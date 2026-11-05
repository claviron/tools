//! File-first issuer selection, with opt-in hosting CN resolution.
use std::path::PathBuf;

use claviron::ssl::certstore::{CA_KEY_FILENAME, CertStore, CertificateSource};
use claviron::ssl::generators::{KeyPair, read_private_key_from_pem};
use claviron::ssl::parsers::concat_chain_bytes_from_pem;
use claviron::ssl::{Basename, CommonName, SSLError};

pub(crate) struct SigningMaterial {
    pub certificates: Vec<u8>,
    pub key: KeyPair,
}

/// Return certificate first, private key second. DB identities are managed by
/// the hosting/control-plane tools, never silently converted to local paths.
pub(crate) fn resolve_issuer_paths(
    store: &CertStore,
    issuer: Option<Basename>,
) -> Result<(PathBuf, PathBuf), SSLError> {
    let configured = store.authority_entry();
    if let Some(name) =
        issuer.or_else(|| configured.and_then(CertificateSource::basename))
    {
        return Ok((store.ca_cert_path(name), store.ca_key_path(name)));
    }
    match configured {
        Some(CertificateSource::Path(path)) => {
            let certificate = store.dir().join(path);
            let key = certificate.parent()
                .ok_or_else(|| SSLError::Other("authority path must name a certificate file".into()))?
                .join(CA_KEY_FILENAME);
            Ok((certificate, key))
        }
        Some(CertificateSource::Paths(paths)) => {
            let certificate = paths.cert_path().ok_or_else(|| SSLError::Other("no CA certificate path configured".into()))?;
            let key = paths.key_path().ok_or_else(|| SSLError::Other("no CA key path configured".into()))?;
            Ok((certificate.clone(), key.clone()))
        }
        Some(_) => Err(SSLError::Other(
            "database authority cannot be used by the file-only claviron-cert tool; use claviron-client or select a file authority".into()
        )),
        None => Err(SSLError::Other("no certificate authority configured".into()))
    }
}

#[cfg(test)]
#[path = "../tests/issuer.rs"]
mod tests;

pub(crate) fn read_issuer_materials(
    store: &CertStore,
    issuer: Option<&CommonName>,
    options: &crate::Options,
) -> Result<SigningMaterial, SSLError> {
    let (basename, fallback, use_files) =
        match (issuer, store.authority_entry()) {
            (Some(name), _) | (None, Some(CertificateSource::Name(name))) => {
                let basename = Basename::new(name.as_str());
                (basename, Some(name), basename.is_some())
            }
            (None, Some(CertificateSource::Cn(name))) => {
                (None, Some(name), false)
            }
            (None, _) => (None, None, true),
        };
    if use_files {
        let (cert, key) = resolve_issuer_paths(store, basename)?;
        let exists = |path: &PathBuf| {
            path.try_exists()
                .map_err(|source| SSLError::Io { path: path.clone(), source })
        };
        let cert_exists = exists(&cert)?;
        let key_exists = exists(&key)?;
        // Only a named source missing both files may fall back to hosting.
        if cert_exists || key_exists || fallback.is_none() {
            let read = |path: PathBuf| {
                std::fs::read(&path)
                    .map_err(|source| SSLError::Io { path, source })
            };
            let cert = read(cert)?;
            let key = read(key)?;
            return Ok(SigningMaterial {
                certificates: concat_chain_bytes_from_pem(&cert)?,
                key: read_private_key_from_pem(&key)?,
            });
        }
    }
    #[cfg(feature = "hosting")]
    {
        read_hosting_materials(
            fallback.ok_or_else(|| {
                SSLError::Other("no certificate authority configured".into())
            })?,
            options,
        )
    }
    #[cfg(not(feature = "hosting"))]
    {
        let _ = options;
        Err(SSLError::Other(
            "authority files not found or CN requires the hosting feature"
                .into(),
        ))
    }
}

#[cfg(feature = "hosting")]
fn read_hosting_materials(
    name: &CommonName,
    options: &crate::Options,
) -> Result<SigningMaterial, SSLError> {
    use claviron::hosting::{CertificateKind, HostingStore};
    use claviron::ssl::generators::read_private_key_from_der;
    use claviron_tools::config::{CliConfig, HostingPool};
    let error = |e: &dyn std::fmt::Display| SSLError::Other(e.to_string());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| error(&e))?;
    runtime.block_on(async {
        let config = CliConfig::load(&CliConfig::resolve_path(
            options.hosting_config.clone(),
        ))
        .await
        .map_err(|e| error(&e))?;
        let hosting = match config
            .hosting_pool(options.profile.as_deref())
            .await
            .map_err(|e| error(&e))?
        {
            HostingPool::Postgres(pool) => HostingStore::new(pool),
            #[cfg(feature = "sqlite")]
            HostingPool::Sqlite(pool) => HostingStore::sqlite(pool),
            #[cfg(not(feature = "sqlite"))]
            #[allow(unreachable_patterns)]
            _ => {
                return Err(SSLError::Other(
                    "SQLite authorities require the sqlite feature".into(),
                ));
            }
        };
        let cert = hosting
            .certificate_by_cn(name.as_str())
            .await
            .map_err(|e| error(&e))?
            .ok_or_else(|| {
                SSLError::Other(format!("CA '{name}' not found in hosting"))
            })?
            .certificate;
        if !cert.enabled || cert.kind != CertificateKind::Ca {
            return Err(SSLError::Other(format!(
                "hosting certificate '{name}' is not an enabled CA"
            )));
        }
        Ok(SigningMaterial {
            certificates: cert.certs_der,
            key: read_private_key_from_der(&cert.key_der)?,
        })
    })
}
