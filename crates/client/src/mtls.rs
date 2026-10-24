//! `claviron.mtls`: certificates issued by the server's local CA, apart
//! from ACME. The server keeps all material in its hosting database; these
//! commands report what was issued, never a certificate or a key.

use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp};
use crate::proto::mtls;
use crate::proto::mtls::mtls_manager_client::MtlsManagerClient;

/// A literal CN or a username used as the client certificate's CN.
#[derive(Debug, clap::Args)]
pub struct Identity {
    #[arg(
        value_name = "CN",
        required_unless_present = "user",
        conflicts_with = "user"
    )]
    common_name: Option<String>,
    #[arg(
        long,
        value_name = "USERNAME",
        help = "Use this username as the CN; requires --kind client"
    )]
    user: Option<String>,
}

impl Identity {
    pub fn common_name(
        &self,
        kind: Kind,
    ) -> Result<String, Box<dyn std::error::Error>> {
        match (&self.common_name, &self.user) {
            (Some(name), None) => Ok(name.clone()),
            (None, Some(user)) if kind == Kind::Client => {
                if user.is_empty()
                    || user.trim() != user
                    || user.chars().any(char::is_control)
                {
                    return Err("--user must be a nonempty username without surrounding whitespace or control characters".into());
                }
                let name = user.parse::<claviron::ssl::Host>().map_err(|_| {
                    "the issuing API requires a DNS, IP or bare certificate name for --user"
                })?;
                if user.starts_with("*.")
                    || !name.to_string().eq_ignore_ascii_case(user)
                {
                    return Err("--user must be preserved by the issuing API's certificate-name normalization".into());
                }
                Ok(user.clone())
            }
            (None, Some(_)) => Err("--user requires --kind client".into()),
            _ => Err("supply either a CN or --user USERNAME".into()),
        }
    }
}

/// What a local-CA certificate is for, as the operator names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum Kind {
    /// TLS server identity for an internal host.
    Server,
    /// Client identity, for mutual TLS.
    Client,
    /// An intermediate CA below the configured authority.
    Ca,
}

impl From<Kind> for mtls::CertificateKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Server => Self::Server,
            Kind::Client => Self::Client,
            Kind::Ca => Self::Ca,
        }
    }
}

pub async fn create_root_ca(
    channel: Channel,
    common_name: String,
    validity_days: u32,
    organization: Option<String>,
    country: Option<String>,
) -> CommandResult {
    let info = MtlsManagerClient::new(channel)
        .create_root_ca(mtls::CreateRootCaRequest {
            common_name,
            validity_days,
            organization,
            country,
        })
        .await?
        .into_inner();
    println!("✅ Created local root CA");
    print_info(&info);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_certificate(
    channel: Channel,
    common_name: String,
    aliases: Vec<String>,
    validity_days: u32,
    kind: Kind,
    ca_path_len: Option<u32>,
    issuer: Option<String>,
) -> CommandResult {
    if kind == Kind::Ca && ca_path_len.is_none() {
        return Err("--ca-path-len is required for --kind ca".into());
    }
    let info = MtlsManagerClient::new(channel)
        .create_certificate(mtls::CreateCertificateRequest {
            common_name,
            aliases,
            validity_days,
            kind: mtls::CertificateKind::from(kind) as i32,
            ca_path_len,
            issuer,
        })
        .await?
        .into_inner();
    println!("✅ Created certificate from the local CA");
    print_info(&info);
    Ok(())
}

pub async fn renew_certificate(
    channel: Channel,
    common_name: String,
    validity_days: u32,
) -> CommandResult {
    let info = MtlsManagerClient::new(channel)
        .renew_certificate(mtls::RenewCertificateRequest {
            common_name,
            validity_days,
        })
        .await?
        .into_inner();
    println!("✅ Renewed certificate; existing key and extensions preserved");
    print_info(&info);
    Ok(())
}

fn print_info(info: &mtls::CertificateInfo) {
    println!("   ID: {}", info.id);
    println!("   Common name: {}", info.common_name);
    println!("   Kind: {}", kind_label(info.kind));
    if !info.aliases.is_empty() {
        println!("   Also covers: {}", info.aliases.join(", "));
    }
    match info.issuer_id {
        Some(issuer) => println!("   Issuer: certificate {issuer}"),
        None => println!("   Issuer: self-signed root"),
    }
    println!("   Issued: {}", format_timestamp(info.issued_at));
    println!("   Expires: {}", format_timestamp(info.expires_at));
    println!("   Enabled: {}", if info.enabled { "yes" } else { "no" });
    println!(
        "   Material stays in the hosting database; it is not returned here."
    );
}

fn kind_label(raw: i32) -> String {
    match mtls::CertificateKind::try_from(raw) {
        Ok(mtls::CertificateKind::Server) => "server".to_owned(),
        Ok(mtls::CertificateKind::Client) => "client".to_owned(),
        Ok(mtls::CertificateKind::Ca) => "ca".to_owned(),
        Ok(mtls::CertificateKind::Unspecified) => "not reported".to_owned(),
        Err(_) => format!("unknown ({raw})"),
    }
}

#[cfg(test)]
#[path = "../tests/mtls.rs"]
mod tests;
