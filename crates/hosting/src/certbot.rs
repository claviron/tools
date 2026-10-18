//! One-shot Certbot -> hosting migration. No issuance, renewal or source writes.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use claviron::hosting::{CertificateKind, HostingStore, NewCertificate};
use claviron::ssl::{Algorithm, Aliases, Host, KeyString};
use claviron_acme::{AcmeConfig, AcmeStore, NewAcmeAccount};
use rcgen::PublicKeyData;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug)]
enum Outcome {
    Imported,
    Existing,
    Skipped(String),
}

#[derive(Default)]
struct Counts {
    imported: usize,
    existing: usize,
    skipped: usize,
    errors: usize,
}

impl Counts {
    fn report(
        &mut self,
        kind: &str,
        name: &str,
        result: Result<Outcome>,
    ) {
        match result {
            Ok(Outcome::Imported) => {
                self.imported += 1;
                println!("SUCCESS {kind} {name}: imported");
            }
            Ok(Outcome::Existing) => {
                self.existing += 1;
                println!("SUCCESS {kind} {name}: already present; verified");
            }
            Ok(Outcome::Skipped(reason)) => {
                self.skipped += 1;
                eprintln!("WARNING {kind} {name}: {reason}");
            }
            Err(error) => {
                self.errors += 1;
                eprintln!("ERROR {kind} {name}: {error}");
            }
        }
    }
}

fn field<'a>(
    value: &'a Value,
    key: &str,
) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing JWK field {key}"))
}

/// Retain the exact private key and derive its public JWK, verifying supplied coordinates.
fn account_key(value: &Value) -> Result<(String, Value, Algorithm)> {
    let bytes = |key| {
        URL_SAFE_NO_PAD
            .decode(field(value, key)?)
            .map_err(|_| format!("invalid JWK field {key}"))
    };
    match field(value, "kty")? {
        "RSA" => {
            let integer =
                |key| bytes(key).map(|v| rsa::BigUint::from_bytes_be(&v));
            let key = rsa::RsaPrivateKey::from_components(
                integer("n")?,
                integer("e")?,
                integer("d")?,
                vec![integer("p")?, integer("q")?],
            )
            .map_err(|_| "invalid RSA account key".to_owned())?;
            key.validate()
                .map_err(|_| "RSA account key validation failed".to_owned())?;
            use rsa::traits::PublicKeyParts;
            let public = serde_json::json!({"kty":"RSA", "n":URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()), "e":URL_SAFE_NO_PAD.encode(key.e().to_bytes_be())});
            let pem = key
                .to_pkcs8_pem(LineEnding::LF)
                .map_err(|_| "cannot encode RSA account key".to_owned())?;
            Ok((pem.to_string(), public, Algorithm::RS256))
        }
        "EC" if field(value, "crv")? == "P-256" => {
            let key = p256::SecretKey::from_slice(&bytes("d")?)
                .map_err(|_| "invalid EC account key".to_owned())?;
            use p256::elliptic_curve::sec1::ToEncodedPoint;
            let point = key.public_key().to_encoded_point(false);
            let x = URL_SAFE_NO_PAD.encode(point.x().ok_or("missing EC x")?);
            let y = URL_SAFE_NO_PAD.encode(point.y().ok_or("missing EC y")?);
            if x != field(value, "x")? || y != field(value, "y")? {
                return Err("EC account public/private key mismatch".into());
            }
            let public =
                serde_json::json!({"kty":"EC", "crv":"P-256", "x":x, "y":y});
            let pem = key
                .to_pkcs8_pem(LineEnding::LF)
                .map_err(|_| "cannot encode EC account key".to_owned())?;
            Ok((pem.to_string(), public, Algorithm::ES256))
        }
        _ => Err("unsupported Certbot account key type or curve".into()),
    }
}

fn json(path: &Path) -> Result<Value> {
    let data = std::fs::read(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    serde_json::from_slice(&data)
        .map_err(|_| format!("invalid JSON in {}", path.display()))
}

// Certbot's account tree is accounts/<directory-host>/directory/<account-id>.
fn account_dirs(
    root: &Path,
    counts: &mut Counts,
) -> Vec<PathBuf> {
    fn walk(
        dir: &Path,
        depth: usize,
        found: &mut Vec<PathBuf>,
        counts: &mut Counts,
    ) {
        if dir.join("regr.json").is_file() {
            found.push(dir.to_owned());
            return;
        }
        if depth == 0 {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                counts.report(
                    "account-directory",
                    &dir.display().to_string(),
                    Err(e.to_string()),
                );
                return;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        walk(&entry.path(), depth - 1, found, counts)
                    }
                    Ok(_) => {}
                    Err(e) => counts.report(
                        "account-directory",
                        &entry.path().display().to_string(),
                        Err(e.to_string()),
                    ),
                },
                Err(e) => counts.report(
                    "account-directory",
                    &dir.display().to_string(),
                    Err(e.to_string()),
                ),
            }
        }
    }
    let mut found = Vec::new();
    walk(root, 3, &mut found, counts);
    found.sort();
    found
}

async fn import_account(
    dir: &Path,
    store: &AcmeStore,
    cfg: &AcmeConfig,
) -> Result<(i32, String, Outcome)> {
    let registration = json(&dir.join("regr.json"))?;
    let uri = registration
        .get("uri")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or("missing ACME account URI")?;
    let parsed =
        url::Url::parse(uri).map_err(|_| "invalid ACME account URI")?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err("ACME account URI must be HTTPS".into());
    }
    let provider_hosts =
        [claviron_acme::AcmeMode::Production, claviron_acme::AcmeMode::Staging]
            .map(|mode| {
                url::Url::parse(cfg.base_url(&mode))
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
            });
    if !provider_hosts
        .iter()
        .flatten()
        .any(|host| Some(host.as_str()) == parsed.host_str())
    {
        return Err("account provider does not match configured ACME directory; not imported".into());
    }
    let (key, jwk, algorithm) =
        account_key(&json(&dir.join("private_key.json"))?)?;
    if let Some(existing) =
        store.account_by_url(uri).await.map_err(|e| e.to_string())?
    {
        if existing.jwk != jwk
            || existing.private_key.trim_end() != key.trim_end()
            || existing.algorithm != algorithm
        {
            return Err(
                "existing account material differs; not overwritten".into()
            );
        }
        return Ok((existing.id, uri.to_owned(), Outcome::Existing));
    }
    let id = dir
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("invalid account directory name")?;
    let contact = registration
        .pointer("/body/contact")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| cfg.organization().email().to_owned());
    let saved = store
        .insert_account(NewAcmeAccount {
            name: format!("certbot-{id}"),
            url: uri.to_owned(),
            private_key: key,
            jwk,
            algorithm,
            email: contact,
            organization: cfg.organization().name().to_owned(),
            is_default: false,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok((saved.id, uri.to_owned(), Outcome::Imported))
}

fn renewal_account(path: &Path) -> Result<(String, String)> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("reading renewal config: {e}"))?;
    let mut active = false;
    let mut account = None;
    let mut server = None;
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == "[renewalparams]";
            continue;
        }
        if !active || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "account" => account = Some(value.trim().to_owned()),
                "server" => server = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    Ok((
        account.ok_or("renewal config has no account")?,
        server.ok_or("renewal config has no server")?,
    ))
}

async fn import_certificate(
    dir: &Path,
    root: &Path,
    accounts: &HashMap<(String, String), i32>,
    hosting: &HostingStore,
    server_name: &str,
) -> Result<Outcome> {
    let name = dir
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("invalid live directory name")?;
    let pem = std::fs::read(
        dir.join(claviron::ssl::certstore::FULLCHAIN_CERT_FILENAME),
    )
    .map_err(|e| format!("reading fullchain: {e}"))?;
    let chain = claviron::ssl::parsers::concat_chain_bytes_from_pem(&pem)
        .map_err(|e| e.to_string())?;
    let cert = claviron::ssl::parsers::take_first_cert_from_der(&chain)
        .map_err(|e| e.to_string())?;
    if !cert.validity().is_valid() {
        return Ok(Outcome::Skipped(
            "certificate expired or not yet valid".into(),
        ));
    }
    let key = std::fs::read_to_string(
        dir.join(claviron::ssl::certstore::PRIVKEY_FILENAME),
    )
    .map_err(|e| format!("reading private key: {e}"))?;
    let key = rcgen::KeyPair::from_pem(&key)
        .map_err(|_| "invalid certificate private key")?;
    if key.subject_public_key_info() != cert.public_key().raw {
        return Err("certificate/private key mismatch".into());
    }
    if cert
        .basic_constraints()
        .map_err(|e| e.to_string())?
        .is_some_and(|ext| ext.value.ca)
    {
        return Err("live certificate is a CA, not a server certificate".into());
    }
    if cert
        .extended_key_usage()
        .map_err(|e| e.to_string())?
        .is_some_and(|ext| !ext.value.server_auth)
    {
        return Err("certificate does not permit server authentication".into());
    }
    // Validate all supplied issuer links; a missing root is normal for public fullchains.
    let mut remaining = chain.as_slice();
    let mut certificates = Vec::new();
    while !remaining.is_empty() {
        let (rest, parsed) = x509_parser::parse_x509_certificate(remaining)
            .map_err(|_| "invalid certificate chain")?;
        certificates.push(parsed);
        remaining = rest;
    }
    for pair in certificates.windows(2) {
        let parent = &pair[1];
        if pair[0].issuer() != parent.subject()
            || !parent.validity().is_valid()
            || !parent
                .basic_constraints()
                .map_err(|e| e.to_string())?
                .is_some_and(|ext| ext.value.ca)
            || parent
                .key_usage()
                .map_err(|e| e.to_string())?
                .is_some_and(|ext| !ext.value.key_cert_sign())
        {
            return Err("invalid or expired issuer chain".into());
        }
        pair[0]
            .verify_signature(Some(parent.public_key()))
            .map_err(|_| "certificate chain signature verification failed")?;
    }
    let cn = Host::try_from(&cert).map_err(|e| e.to_string())?.key_string();
    let (account, server) =
        renewal_account(&root.join("renewal").join(format!("{name}.conf")))?;
    let directory = url::Url::parse(&server)
        .map_err(|_| "invalid renewal ACME directory URL")?;
    let host =
        directory.host_str().ok_or("ACME directory has no host")?.to_owned();
    let account_id = *accounts
        .get(&(host, account))
        .ok_or("renewal account was not imported; certificate skipped")?;
    let issuer = hosting
        .ensure_acme_issuer(account_id)
        .await
        .map_err(|e| e.to_string())?;
    let server = hosting
        .find_or_create_server(server_name)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(existing) =
        hosting.certificate_by_cn(&cn).await.map_err(|e| e.to_string())?
    {
        if existing.certificate.certs_der != chain
            || existing.certificate.key_der != key.serialize_der()
            || existing.certificate.issuer_id != Some(issuer.id)
            || !existing.certificate.enabled
        {
            return Err("existing certificate material, issuer or state differs; not overwritten".into());
        }
        hosting
            .attach_certificate_to_server(existing.certificate.id, server.id)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(Outcome::Existing);
    }
    let aliases = Aliases::try_from(&cert)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|h| h.key_string())
        .filter(|name| name != &cn)
        .collect::<Vec<_>>();
    hosting
        .insert_certificate(
            NewCertificate {
                kind: CertificateKind::Server,
                issuer_id: Some(issuer.id),
                cn,
                certs_der: chain.clone(),
                key_der: key.serialize_der(),
                expires_at: cert.validity().not_after.to_datetime(),
                issued_at: cert.validity().not_before.to_datetime(),
                enabled: true,
            },
            &aliases,
            server.id,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(Outcome::Imported)
}

pub async fn import(
    root: &Path,
    hosting: &HostingStore,
    store: &AcmeStore,
    server_name: &str,
    cfg: &AcmeConfig,
) -> claviron::Result<()> {
    if !root.is_dir() {
        return Err(claviron::Error::Cli(format!(
            "Certbot directory not found: {}",
            root.display()
        )));
    }
    let mut counts = Counts::default();
    let mut accounts = HashMap::new();
    for dir in account_dirs(&root.join("accounts"), &mut counts) {
        let name = dir.display().to_string();
        match import_account(&dir, store, cfg).await {
            Ok((id, uri, outcome)) => {
                let host = url::Url::parse(&uri)
                    .expect("validated above")
                    .host_str()
                    .expect("validated above")
                    .to_owned();
                let key = dir
                    .file_name()
                    .expect("validated above")
                    .to_string_lossy()
                    .into_owned();
                accounts.insert((host, key), id);
                counts.report("account", &name, Ok(outcome));
            }
            Err(error) => counts.report("account", &name, Err(error)),
        }
    }
    match std::fs::read_dir(root.join("live")) {
        Ok(entries) => {
            let mut dirs = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) if entry.path().is_dir() => {
                        dirs.push(entry.path())
                    }
                    Ok(_) => {}
                    Err(e) => counts.report(
                        "certificate-directory",
                        "live",
                        Err(e.to_string()),
                    ),
                }
            }
            dirs.sort();
            for dir in dirs {
                let result = import_certificate(
                    &dir,
                    root,
                    &accounts,
                    hosting,
                    server_name,
                )
                .await;
                counts.report(
                    "certificate",
                    &dir.display().to_string(),
                    result,
                );
            }
        }
        Err(e) => {
            counts.report("certificate-directory", "live", Err(e.to_string()))
        }
    }
    println!(
        "Certbot import summary: {} imported, {} existing, {} skipped, {} errors",
        counts.imported, counts.existing, counts.skipped, counts.errors
    );
    Ok(())
}

#[cfg(test)]
#[path = "../tests/certbot.rs"]
mod tests;
