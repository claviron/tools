//! `claviron.certificates`: moving certificate material in and out of the
//! server's hosting database, whatever issued it. The server never touches
//! the client's files; these commands read and write them here.
//!
//! A private key is never printed. On export it is requested only when a key
//! file is given, and that file is created with mode 0600.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp};
use crate::proto::certificates;
use crate::proto::certificates::certificate_transfer_client::CertificateTransferClient;

pub async fn import_certificate(
    channel: Channel,
    certificate: PathBuf,
    key: PathBuf,
    chain: Vec<PathBuf>,
    issuer: Option<String>,
) -> CommandResult {
    let mut parts = vec![read_pem(&certificate, "certificate")?];
    for path in &chain {
        parts.push(read_pem(path, "chain")?);
    }
    let certificate_chain_pem = join_pem(&parts);
    let private_key_pem = read_pem(&key, "private key")?;

    let info = CertificateTransferClient::new(channel)
        .import_certificate(certificates::ImportCertificateRequest {
            certificate_chain_pem,
            private_key_pem,
            issuer,
        })
        .await?
        .into_inner();
    println!("✅ Imported certificate");
    print_info(&info);
    Ok(())
}

pub async fn export_certificate(
    channel: Channel,
    common_name: String,
    out: Option<PathBuf>,
    key_out: Option<PathBuf>,
    force: bool,
) -> CommandResult {
    // Refuse before asking the server for anything, least of all the key.
    validate_outputs(out.as_deref(), key_out.as_deref(), force)?;

    let response = CertificateTransferClient::new(channel)
        .export_certificate(certificates::ExportCertificateRequest {
            common_name,
            include_private_key: key_out.is_some(),
        })
        .await?
        .into_inner();

    if let Some(key_out) = &key_out {
        let key = response
            .private_key_pem
            .as_deref()
            .ok_or("the server returned no private key")?;
        write_file(key_out, key, force, true)?;
    }

    match &out {
        Some(out) => {
            write_file(out, &response.certificate_chain_pem, force, false)?
        }
        None => {
            print!("{}", response.certificate_chain_pem);
            if !response.certificate_chain_pem.ends_with('\n') {
                println!();
            }
        }
    }

    // With the chain on standard output, the summary goes to standard error
    // so the output stays a clean PEM stream.
    let report = |line: String| {
        if out.is_some() {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    };
    report("✅ Exported certificate".to_owned());
    if let Some(info) = &response.certificate {
        for line in info_lines(info) {
            report(line);
        }
    }
    if let Some(out) = &out {
        report(format!("   Chain: {}", out.display()));
    }
    if let Some(key_out) = &key_out {
        report(format!("   Private key: {} (mode 0600)", key_out.display()));
    }
    Ok(())
}

fn validate_outputs(
    out: Option<&Path>,
    key_out: Option<&Path>,
    force: bool,
) -> CommandResult {
    let mut identities = Vec::new();
    for path in out.into_iter().chain(key_out) {
        let name = path.file_name().ok_or("output path must name a file")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let identity = fs::canonicalize(parent)?.join(name);
        if identities.contains(&identity) {
            return Err("--out and --key-out must be different files".into());
        }
        identities.push(identity);
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(format!(
                        "{} is not a regular file; refusing to replace it",
                        path.display()
                    )
                    .into());
                }
                if !force {
                    return Err(format!(
                        "{} already exists; pass --force to replace it",
                        path.display()
                    )
                    .into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() > 1 {
                        return Err(format!(
                            "{} has hard links; refusing to replace it",
                            path.display()
                        )
                        .into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_pem(
    path: &Path,
    what: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!("cannot read {what} file {}: {error}", path.display())
    })?;
    if !text.contains("-----BEGIN ") {
        return Err(format!(
            "{} does not contain a PEM {what}",
            path.display()
        )
        .into());
    }
    Ok(text)
}

/// PEM files concatenated so each block starts on its own line.
fn join_pem(parts: &[String]) -> String {
    let mut joined = String::new();
    for part in parts {
        joined.push_str(part.trim_end());
        joined.push('\n');
    }
    joined
}

/// Writes `contents` to `path`, never replacing an existing file unless
/// `force`. A secret file is created, or narrowed before writing, to 0600.
fn write_file(
    path: &Path,
    contents: &str,
    force: bool,
    secret: bool,
) -> CommandResult {
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options
        .open(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    #[cfg(unix)]
    if secret {
        // A replaced file keeps its old mode; narrow it before the key lands.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents.as_bytes())?;
    if !contents.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn print_info(info: &certificates::CertificateInfo) {
    for line in info_lines(info) {
        println!("{line}");
    }
    println!("   Responses carry metadata only; the key is never echoed.");
}

fn info_lines(info: &certificates::CertificateInfo) -> Vec<String> {
    let mut lines = vec![
        format!("   ID: {}", info.id),
        format!("   Common name: {}", info.common_name),
        format!("   Kind: {}", kind_label(info.kind)),
    ];
    if !info.aliases.is_empty() {
        lines.push(format!("   Also covers: {}", info.aliases.join(", ")));
    }
    lines.push(match &info.issuer {
        Some(issuer) => format!("   Issuer: {issuer}"),
        None => "   Issuer: none (root CA)".to_owned(),
    });
    lines.push(format!("   Issued: {}", format_timestamp(info.issued_at)));
    lines.push(format!("   Expires: {}", format_timestamp(info.expires_at)));
    lines.push(format!(
        "   Enabled: {}",
        if info.enabled { "yes" } else { "no" }
    ));
    lines
}

fn kind_label(raw: i32) -> String {
    match certificates::CertificateKind::try_from(raw) {
        Ok(certificates::CertificateKind::Server) => "server".to_owned(),
        Ok(certificates::CertificateKind::Client) => "client".to_owned(),
        Ok(certificates::CertificateKind::Ca) => "ca".to_owned(),
        Ok(certificates::CertificateKind::Unspecified) => {
            "not reported".to_owned()
        }
        Err(_) => format!("unknown ({raw})"),
    }
}
#[cfg(test)]
#[path = "../tests/certificates.rs"]
mod tests;
