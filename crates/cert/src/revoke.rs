//! Local file revocation. Remote management writes the hosting database instead.
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use claviron::ssl::certstore::{
    CLIENT_CERT_FILENAME, CertStore, FULLCHAIN_CERT_FILENAME,
};
use claviron::ssl::parsers::{
    sanitize_pem_content_to_der, take_first_cert_from_der,
};
use claviron::ssl::{Basename, CommonName, SSLError, crl};

pub(crate) fn run(
    store: &CertStore,
    name: &CommonName,
    issuer: Option<Basename>,
) -> Result<(), SSLError> {
    let name = crate::store_entry_name(name)?;
    let fullchain = store.client_cert_path(name);
    let client = if fullchain
        .try_exists()
        .map_err(|e| SSLError::io_error(&fullchain, e))?
    {
        fullchain
    } else {
        store.client_dir(name).join(CLIENT_CERT_FILENAME)
    };
    let candidates = [
        Some(client),
        Some(store.server_dir(name).join(FULLCHAIN_CERT_FILENAME)),
        Basename::new(name).map(|name| store.ca_cert_path(name)),
    ];
    let mut found = Vec::new();
    for path in candidates.into_iter().flatten() {
        if path.try_exists().map_err(|e| SSLError::io_error(&path, e))? {
            found.push(path);
        }
    }
    let path = match found.as_slice() {
        [path] => path,
        [] => {
            return Err(SSLError::Other(format!(
                "certificate '{name}' not found"
            )));
        }
        _ => {
            return Err(SSLError::Other(format!(
                "certificate name '{name}' is ambiguous"
            )));
        }
    };
    let certificate =
        fs::read(path).map_err(|e| SSLError::io_error(path, e))?;
    let der = sanitize_pem_content_to_der(&certificate);
    let parsed = take_first_cert_from_der(&der)?;
    let issuer = match issuer {
        Some(issuer) => Some(issuer),
        None => CommonName::read(parsed.issuer())?
            .and_then(Basename::new)
            .filter(|name| store.ca_cert_path(*name).is_file()),
    };
    let (ca, key) = crate::issuer::resolve_issuer_paths(store, issuer)?;
    let ca = fs::read(&ca).map_err(|e| SSLError::io_error(&ca, e))?;
    let key = fs::read(&key).map_err(|e| SSLError::io_error(&key, e))?;
    let output = store.crl_path();
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut lock_name = output.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = std::path::PathBuf::from(lock_name);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| SSLError::io_error(&lock_path, e))?;
    lock.lock().map_err(|e| SSLError::io_error(&lock_path, e))?;
    let previous = match fs::read(&output) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(SSLError::io_error(&output, e)),
    };
    let pem = crl::revoke(
        &certificate,
        &ca,
        &key,
        &previous,
        time::OffsetDateTime::now_utc(),
    )?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| SSLError::io_error(parent, e))?;
    temporary
        .write_all(pem.as_bytes())
        .map_err(|e| SSLError::io_error(&output, e))?;
    // CRLs are public trust material, readable by the service consuming the store.
    temporary
        .as_file()
        .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))
        .map_err(|e| SSLError::io_error(&output, e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| SSLError::io_error(&output, e))?;
    temporary
        .persist(&output)
        .map_err(|e| SSLError::io_error(&output, e.error))?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|e| SSLError::io_error(parent, e))?;
    println!("Revoked '{name}'; signed CRL written to {}", output.display());
    println!(
        "Reload the certificate source to publish the updated CRL. Existing connections remain open."
    );
    Ok(())
}
