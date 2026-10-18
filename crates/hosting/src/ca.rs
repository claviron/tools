//! Direct certificate imports; no ACME account or gRPC required.
use std::path::{Path, PathBuf};

use claviron::hosting::{CertificateKind, HostingStore, NewCertificate};
use claviron::ssl::certstore::{
    CA_CERT_FILENAME, CA_KEY_FILENAME, CLIENT_CERT_FILENAME,
    CLIENT_KEY_FILENAME, CertStore, FULLCHAIN_CERT_FILENAME, PRIVKEY_FILENAME,
};
use claviron::ssl::transfer::{self, ImportPlan, IssuerCandidate};

pub fn config_path(
    client_config: &Path,
    explicit: Option<&Path>,
) -> PathBuf {
    explicit.map(Path::to_path_buf).unwrap_or_else(|| {
        client_config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("certstore.yaml")
    })
}

#[cfg(test)]
pub fn resolve_input_paths(
    basename: Option<&str>,
    config_path: Option<PathBuf>,
    dir: Option<&Path>,
    certificate: Option<&Path>,
    key: Option<&Path>,
) -> claviron::Result<(PathBuf, PathBuf)> {
    resolve_kind_paths(
        CertificateKind::Ca,
        basename,
        config_path,
        dir,
        certificate,
        key,
    )
}

pub fn resolve_kind_paths(
    kind: CertificateKind,
    basename: Option<&str>,
    config_path: Option<PathBuf>,
    dir: Option<&Path>,
    certificate: Option<&Path>,
    key: Option<&Path>,
) -> claviron::Result<(PathBuf, PathBuf)> {
    let (cert_filename, key_filename) = match kind {
        CertificateKind::Ca => (CA_CERT_FILENAME, CA_KEY_FILENAME),
        CertificateKind::Client => (CLIENT_CERT_FILENAME, CLIENT_KEY_FILENAME),
        _ => (FULLCHAIN_CERT_FILENAME, PRIVKEY_FILENAME),
    };
    let paths = |dir: &Path| -> claviron::Result<_> {
        Ok((
            certificate
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dir.join(cert_filename)),
            key.map(Path::to_path_buf)
                .unwrap_or_else(|| dir.join(key_filename)),
        ))
    };
    if let Some(dir) = dir {
        return paths(dir);
    }
    if let (Some(certificate), Some(key)) = (certificate, key) {
        return Ok((certificate.to_path_buf(), key.to_path_buf()));
    }
    let basename = basename.ok_or_else(|| {
        claviron::Error::Cli(
            "provide --basename, --dir, or --cert and --key".into(),
        )
    })?;
    let mut components = Path::new(basename).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
        || basename.contains(['/', '\\'])
    {
        return Err(claviron::Error::Cli(
            "basename must be a single directory name; use --dir for a path"
                .into(),
        ));
    }
    let config_path = CertStore::resolve_path(config_path);
    let store = CertStore::from_path(&config_path).map_err(|error| {
        claviron::Error::Cli(format!(
            "loading certstore {}: {error}",
            config_path.display()
        ))
    })?;
    let dir = match kind {
        CertificateKind::Ca => store.ca_dir(basename),
        CertificateKind::Server => store.server_dir(basename),
        CertificateKind::Client => store.client_dir(basename),
    };
    paths(&dir)
}

#[cfg(test)]
pub fn input_paths(
    dir: Option<&Path>,
    certificate: Option<&Path>,
    key: Option<&Path>,
) -> claviron::Result<(PathBuf, PathBuf)> {
    let certificate = certificate
        .map(Path::to_path_buf)
        .or_else(|| dir.map(|dir| dir.join(CA_CERT_FILENAME)))
        .ok_or_else(|| {
            claviron::Error::Cli("provide --dir or --cert and --key".into())
        })?;
    let key = key
        .map(Path::to_path_buf)
        .or_else(|| dir.map(|dir| dir.join(CA_KEY_FILENAME)))
        .ok_or_else(|| {
            claviron::Error::Cli("provide --dir or --cert and --key".into())
        })?;
    Ok((certificate, key))
}

fn plan_ca(
    certificate: &[u8],
    key: &[u8],
    issuers: &[IssuerCandidate],
) -> claviron::Result<ImportPlan> {
    let plan = transfer::plan_import(certificate, key, issuers)
        .map_err(|error| claviron::Error::Cli(error.to_string()))?;
    if plan.kind != CertificateKind::Ca {
        return Err(claviron::Error::Cli(
            "import-ca accepts only CA certificates with signing permission"
                .into(),
        ));
    }
    Ok(plan)
}

pub async fn import_kind(
    store: &HostingStore,
    kind: CertificateKind,
    certificate: &Path,
    key: &Path,
    server_name: &str,
) -> claviron::Result<()> {
    let certificate = tokio::fs::read(certificate).await.map_err(|error| {
        claviron::Error::Cli(format!(
            "reading certificate {}: {error}",
            certificate.display()
        ))
    })?;
    let key = tokio::fs::read(key).await.map_err(|error| {
        claviron::Error::Cli(format!(
            "reading private key {}: {error}",
            key.display()
        ))
    })?;
    let issuers = store
        .local_issuer_certificates()
        .await
        .map_err(super::hosting_error)?
        .into_iter()
        .map(|(id, certs_der)| IssuerCandidate { id, certs_der })
        .collect::<Vec<_>>();
    let plan = tokio::task::spawn_blocking(move || {
        if kind == CertificateKind::Ca {
            return plan_ca(&certificate, &key, &issuers);
        }
        let plan = transfer::plan_import(&certificate, &key, &issuers)
            .map_err(|error| claviron::Error::Cli(error.to_string()))?;
        if plan.kind != kind {
            return Err(claviron::Error::Cli(format!(
                "expected {} certificate, found {}",
                kind.as_str(),
                plan.kind.as_str()
            )));
        }
        Ok::<_, claviron::Error>(plan)
    })
    .await
    .map_err(|error| claviron::Error::Other(error.to_string()))??;
    let new = NewCertificate {
        kind: plan.kind,
        issuer_id: plan.issuer_id,
        cn: plan.common_name,
        certs_der: plan.certs_der,
        key_der: plan.key_der,
        expires_at: plan.expires_at,
        issued_at: plan.issued_at,
        enabled: true,
    };
    let saved = if kind == CertificateKind::Server {
        let server = store
            .server_by_name(server_name)
            .await
            .map_err(super::hosting_error)?
            .ok_or_else(|| {
                claviron::Error::Cli(format!(
                    "hosting server '{server_name}' not found"
                ))
            })?;
        store.insert_certificate(new, &plan.aliases, server.id).await
    } else {
        store.insert_unassigned_certificate(new, &plan.aliases).await
    }
    .map_err(super::hosting_error)?;
    println!(
        "{} imported: {} (certificate id: {})",
        kind.as_str(),
        saved.certificate.cn,
        saved.certificate.id
    );
    Ok(())
}

#[cfg(test)]
#[path = "../tests/ca.rs"]
mod tests;
