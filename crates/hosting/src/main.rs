use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use acme::AcmeConfig;
use clap::Parser;
use claviron::helpers::base64::base64_standard_encode;
use claviron::hosting::{
    CertificateKind, HostingError, HostingStore, NewCertificate,
};
use claviron::ssl::certstore::{FULLCHAIN_CERT_FILENAME, PRIVKEY_FILENAME};
use claviron::ssl::formatters::parse_text_from_pem;
use claviron::ssl::parsers::{
    concat_chain_bytes_from_pem, take_first_cert_from_der,
};
use claviron::ssl::{Aliases, Host, KeyString, Sni};
use claviron_acme as acme;
use claviron_acme::account::{
    ACCOUNT_FILE_NAME, ACCOUNT_KEY_FILE_NAME, Account, CERT_META_PATH,
    CERT_TEXT_PATH,
};
use claviron_acme::cryptex::jwk::AcmeJwk;
use claviron_acme::{AcmeError, AcmeMode, AcmeStore, CertMeta, NewAcmeAccount};
use claviron_tools::{config, init_client_logger};
use colored::Colorize;
use dialoguer::Confirm;
use dotenvy::dotenv;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use time::OffsetDateTime;
use tracing::{info, warn};

mod ca;
mod certbot;

#[derive(Debug, Parser)]
#[command(
    name = "claviron-hosting",
    author,
    version,
    about = "Hosting database and certificate management"
)]
pub struct Options {
    #[arg(
        long = "server",
        alias = "profile",
        help = "Named client.yaml hosting profile"
    )]
    pub profile: Option<String>,
    #[arg(
        short = 'f',
        long = "config",
        alias = "client-config",
        env = "CLAVIRON_CLIENT_CONFIG_PATH",
        help = "client config file path (default: ~/.config/claviron/client.yaml)"
    )]
    pub config_path: Option<PathBuf>,

    #[arg(
        long = "certstore-config",
        env = "CERTSTORE_CONFIG_PATH",
        help = "Shared certstore.yaml path (used for named certificate imports)"
    )]
    pub certstore_config_path: Option<PathBuf>,

    #[arg(
        long = "server-name",
        env = "CLAVIRON_SERVER_NAME",
        default_value = "claviron",
        help = "server name used in certificate mapping records"
    )]
    pub server_name: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Parser)]
pub enum Command {
    /// Check the selected hosting connection and issuer schema without writing
    Check,
    /// Register this host (`--server-name`) in hosting.servers, once; a
    /// Claviron boot looks the row up and refuses to start without it
    RegisterServer,
    /// Import Certbot accounts and current live certificates, continuing per record
    ImportCertbot {
        #[arg(
            long,
            help = "Certbot store directory (for example /etc/letsencrypt)"
        )]
        dir: PathBuf,
    },
    /// Import a local CA and signing key into the hosting DB without a server
    ImportCa {
        /// CA basename; resolves through the shared certstore.yaml
        #[arg(long = "basename", short = 'b', help = "CA folder name in the configured certstore", required_unless_present_any = ["dir", "cert"])]
        basename: Option<String>,
        #[arg(
            long,
            short = 'd',
            help = "CA directory containing ca.crt and ca.key"
        )]
        dir: Option<PathBuf>,
        #[arg(
            long,
            required_unless_present_any = ["dir", "basename"],
            help = "PEM CA certificate or fullchain path; overrides --dir certificate"
        )]
        cert: Option<PathBuf>,
        #[arg(
            long,
            required_unless_present_any = ["dir", "basename"],
            help = "Unencrypted PEM CA private key path; overrides --dir key"
        )]
        key: Option<PathBuf>,
    },

    /// Import a local server certificate and key (server assignment required)
    #[command(alias = "import-certificate")]
    ImportServer(MaterialInput),
    /// Import a local client certificate and key without a server assignment
    ImportClient(MaterialInput),

    /// Import a saved ACME account without importing a certificate
    #[command(alias = "acme-account-import")]
    ImportAcmeAccount {
        /// Account name in the configured ACME file store
        name: String,
        /// File-store mode (staging or production)
        #[arg(long)]
        mode: AcmeMode,
    },

    /// Import a file-based ACME certificate into DB (from claviron-acme output)
    #[command(name = "import-acme-certificate", aliases = ["acme-import", "import"])]
    AcmeImport {
        #[arg(help = "Common Name (CN) to import")]
        cn: Sni,
    },

    /// Export a certificate from DB to a file-based ACME store
    #[command(aliases = ["export", "dump"])]
    ExportCertificate {
        #[arg(help = "Common Name (CN) to export")]
        cn: Sni,

        #[arg(
            long = "full",
            help = "Also export ACME account metadata (strict URL match)"
        )]
        full: bool,

        #[arg(
            long = "overwrite",
            short = 'f',
            help = "Overwrite existing files",
            default_value = "false"
        )]
        overwrite: bool,

        #[arg(
            long = "dir",
            short = 'd',
            help = "optional certificate directory"
        )]
        output_dir: Option<PathBuf>,
    },
}

#[derive(Debug, clap::Args)]
pub struct MaterialInput {
    /// Certificate basename resolved through the shared certstore.yaml
    #[arg(long = "basename", short = 'b', help = "Certificate folder name in the configured certstore", required_unless_present_any = ["dir", "cert"])]
    basename: Option<String>,
    /// Full or cwd-relative certificate directory; overrides the basename
    #[arg(long, short = 'd')]
    dir: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["dir", "basename"])]
    cert: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["dir", "basename"])]
    key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> claviron::Result<()> {
    dotenv().ok();

    init_client_logger();

    let options = Options::parse();
    if let Err(e) = run(options).await {
        eprintln!("{}", format_cli_error(&e).bright_red());
        std::process::exit(1);
    }

    Ok(())
}

async fn run(options: Options) -> claviron::Result<()> {
    let client_config_path =
        config::CliConfig::resolve_path(options.config_path.clone());
    let client_config = config::CliConfig::load(&client_config_path).await?;
    let acme_cfg = client_config.acme_cfg();
    let server_name = options.server_name.clone();
    let certstore_config_path = ca::config_path(
        &client_config_path,
        options.certstore_config_path.as_deref(),
    );
    let material = match &options.command {
        Command::ImportCa { basename, dir, cert, key } => {
            Some((CertificateKind::Ca, basename, dir, cert, key))
        }
        Command::ImportServer(input) => Some((
            CertificateKind::Server,
            &input.basename,
            &input.dir,
            &input.cert,
            &input.key,
        )),
        Command::ImportClient(input) => Some((
            CertificateKind::Client,
            &input.basename,
            &input.dir,
            &input.cert,
            &input.key,
        )),
        _ => None,
    };
    let material_paths = material
        .map(|(kind, basename, dir, cert, key)| {
            ca::resolve_kind_paths(
                kind,
                basename.as_deref(),
                Some(certstore_config_path.clone()),
                dir.as_deref(),
                cert.as_deref(),
                key.as_deref(),
            )
            .map(|paths| (kind, paths))
        })
        .transpose()?;
    let (hosting, acme_store) =
        match client_config.hosting_pool(options.profile.as_deref()).await? {
            config::HostingPool::Postgres(pool) => {
                (HostingStore::new(pool.clone()), AcmeStore::new(pool))
            }
            config::HostingPool::Sqlite(pool) => {
                (HostingStore::sqlite(pool.clone()), AcmeStore::sqlite(pool))
            }
        };
    if let Some((kind, (cert, key))) = material_paths {
        return ca::import_kind(&hosting, kind, &cert, &key, &server_name)
            .await;
    }

    match options.command {
        Command::ImportCertbot { dir } => {
            certbot::import(
                &dir,
                &hosting,
                &acme_store,
                &server_name,
                acme_cfg,
            )
            .await?;
        }
        Command::Check => {
            let issuers = hosting
                .local_issuer_certificates()
                .await
                .map_err(hosting_error)?;
            println!(
                "Hosting database connected; {} enabled local CA issuers",
                issuers.len()
            );
        }
        Command::RegisterServer => {
            let existing = hosting
                .server_by_name(&server_name)
                .await
                .map_err(hosting_error)?;
            let server = hosting
                .find_or_create_server(&server_name)
                .await
                .map_err(hosting_error)?;
            match existing {
                Some(_) => println!(
                    "server '{}' is already registered (id {})",
                    server.name, server.id
                ),
                None => println!(
                    "server '{}' registered (id {})",
                    server.name, server.id
                ),
            }
        }
        Command::ImportCa { .. }
        | Command::ImportServer(_)
        | Command::ImportClient(_) => {
            unreachable!("local certificate import handled above")
        }
        Command::ImportAcmeAccount { name, mode } => {
            if name.contains(['/', '\\'])
                || !matches!(
                    Path::new(&name)
                        .components()
                        .collect::<Vec<_>>()
                        .as_slice(),
                    [std::path::Component::Normal(_)]
                )
            {
                return Err(claviron::Error::Cli(
                    "ACME account name must be a single directory name".into(),
                ));
            }
            let account =
                Account::load(acme_cfg, &mode, &name).map_err(|e| {
                    claviron::Error::Cli(format!(
                        "failed to load ACME account '{name}': {e}"
                    ))
                })?;
            if account.name != name || account.mode != mode {
                return Err(claviron::Error::Cli("ACME account name or mode does not match selected directory".into()));
            }
            let (id, imported) =
                import_acme_account(&acme_store, acme_cfg, &account).await?;
            println!(
                "ACME account {}: {} (account id: {})",
                if imported { "imported" } else { "already present; verified" },
                account.name,
                id
            );
        }
        Command::AcmeImport { cn } => {
            let cert_dir = acme_cfg.cert_dir(cn.as_str());
            let fullchain_path = cert_dir.join(FULLCHAIN_CERT_FILENAME);
            let privkey_path = cert_dir.join(PRIVKEY_FILENAME);
            let meta_path = cert_dir.join(CERT_META_PATH);

            if !fullchain_path.exists() {
                return Err(claviron::Error::Missing(format!(
                    "missing certificate file: {}",
                    fullchain_path.display()
                )));
            }
            if !privkey_path.exists() {
                return Err(claviron::Error::Missing(format!(
                    "missing private key file: {}",
                    privkey_path.display()
                )));
            }
            if !meta_path.exists() {
                return Err(claviron::Error::Missing(format!(
                    "missing meta file: {}",
                    meta_path.display()
                )));
            }

            let meta = CertMeta::from_path(&meta_path).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to read meta {}: {}",
                    meta_path.display(),
                    e
                ))
            })?;
            let meta_cn_norm = Sni::norm(meta.cn.as_str());
            let cn_norm = Sni::norm(cn.as_str());
            if meta_cn_norm != cn_norm {
                return Err(claviron::Error::Cli(format!(
                    "CN mismatch: param='{}' meta='{}'",
                    cn.as_str(),
                    meta.cn
                )));
            }

            let acc_path = acme_cfg.account_path(&meta.account, &meta.mode);
            let account = Account::load(acme_cfg, &meta.mode, &meta.account)
                .map_err(|e| {
                    claviron::Error::Cli(format!(
                        "failed to load account file '{}': {e}",
                        acc_path.display()
                    ))
                })?;

            let (account_id, account_imported) =
                import_acme_account(&acme_store, acme_cfg, &account).await?;

            let existing = hosting
                .certificate_by_cn(cn.as_str())
                .await
                .map_err(hosting_error)?;
            if existing.is_some() {
                return Err(claviron::Error::Cli(format!(
                    "certificate '{}' already exists",
                    cn.as_str()
                )));
            }

            let fullchain_pem = std::fs::read(&fullchain_path)?;
            let chain_bytes = concat_chain_bytes_from_pem(&fullchain_pem)
                .map_err(|e| {
                    claviron::Error::Other(format!(
                        "failed to parse fullchain {}: {}",
                        fullchain_path.display(),
                        e
                    ))
                })?;
            let cert = take_first_cert_from_der(&chain_bytes).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to parse leaf cert {}: {}",
                    fullchain_path.display(),
                    e
                ))
            })?;

            let cert_cn = Host::try_from(&cert).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to extract CN from cert: {}",
                    e
                ))
            })?;
            let cert_cn_norm = Sni::norm(cert_cn.key_string().as_str());
            if cert_cn_norm != cn_norm {
                return Err(claviron::Error::Cli(format!(
                    "CN mismatch: param='{}' cert='{}'",
                    cn.as_str(),
                    cert_cn.key_string()
                )));
            }

            let mut aliases = Aliases::try_from(&cert).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to extract aliases from cert: {}",
                    e
                ))
            })?;
            aliases.push_unique(cert_cn);
            let alias_list: Vec<String> =
                aliases.iter().map(|h| h.key_string()).collect();

            let key_der = load_private_key_der(&privkey_path)?;

            let valid_until = cert.validity().not_after.to_datetime();
            let expires_at = OffsetDateTime::from_unix_timestamp(
                valid_until.unix_timestamp(),
            )
            .map_err(|_| "Invalid expires_at timestamp")?;
            let issued_at = OffsetDateTime::from_unix_timestamp(
                cert.validity().not_before.to_datetime().unix_timestamp(),
            )
            .map_err(|_| "Invalid issued_at timestamp")?;

            let server = hosting
                .find_or_create_server(&server_name)
                .await
                .map_err(hosting_error)?;
            let issuer = hosting
                .ensure_acme_issuer(account_id)
                .await
                .map_err(hosting_error)?;
            let cert_model = hosting
                .insert_certificate(
                    NewCertificate {
                        issuer_id: Some(issuer.id),
                        kind: claviron::hosting::CertificateKind::Server,
                        cn: cn.as_str().to_string(),
                        certs_der: chain_bytes,
                        key_der,
                        expires_at,
                        issued_at,
                        enabled: true,
                    },
                    &alias_list,
                    server.id,
                )
                .await
                .map_err(hosting_error)?;
            info!(
                "Inserted certificate '{}' (id: {})",
                cert_model.certificate.cn, cert_model.certificate.id
            );

            println!("✅ Imported certificate '{}'", cn.as_str());
            if !alias_list.is_empty() {
                println!("   Aliases: {}", alias_list.join(", "));
            }
            if account_imported {
                println!(
                    "   Account imported: {} ({})",
                    account.name, account.url
                );
            }
            warn_if_tls_file_overrides(&hosting, &alias_list).await;
        }
        Command::ExportCertificate { cn, full, overwrite, output_dir } => {
            export_certificate(
                acme_cfg,
                &hosting,
                &acme_store,
                cn,
                full,
                overwrite,
                output_dir,
            )
            .await?;
        }
    }

    Ok(())
}

fn load_private_key_der(path: &Path) -> claviron::Result<Vec<u8>> {
    let key = PrivateKeyDer::from_pem_file(path).map_err(|e| {
        claviron::Error::Other(format!(
            "failed to read private key {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(key.secret_der().to_vec())
}

async fn export_certificate(
    acme_cfg: &AcmeConfig,
    hosting: &HostingStore,
    acme_store: &AcmeStore,
    cn: Sni,
    full: bool,
    overwrite: bool,
    output_dir: Option<PathBuf>,
) -> claviron::Result<()> {
    let cert = hosting
        .certificate_by_cn(cn.as_str())
        .await
        .map_err(hosting_error)?
        .ok_or_else(|| {
            claviron::Error::Cli(format!(
                "certificate '{}' not found",
                cn.as_str()
            ))
        })?;
    let certificate = &cert.certificate;

    let cert_dir = output_dir.unwrap_or_else(|| acme_cfg.cert_dir(cn.as_str()));

    if !overwrite && cert_dir.exists() {
        let prompt = format!(
            "Certificate directory '{}' already exists. Overwrite contents?",
            cert_dir.display()
        );
        let confirmed = Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()
            .map_err(|e| {
                claviron::Error::Cli(format!("confirmation failed: {}", e))
            })?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut acct_for_meta: Option<Account> = None;

    if full {
        let issuer_id = certificate.issuer_id.ok_or_else(|| {
            claviron::Error::Cli("certificate has no ACME issuer".into())
        })?;
        let issuer = hosting
            .issuer_by_id(issuer_id)
            .await
            .map_err(hosting_error)?
            .ok_or_else(|| {
                claviron::Error::Cli("certificate issuer not found".into())
            })?;
        let account_id = issuer.acme_account_id.ok_or_else(|| {
            claviron::Error::Cli(
                "full ACME export requires an ACME issuer".into(),
            )
        })?;
        let account = acme_store
            .account_by_id(account_id)
            .await
            .map_err(acme_error)?
            .ok_or_else(|| {
                claviron::Error::Cli(format!(
                    "account '{}' not found",
                    account_id
                ))
            })?;

        let mode = infer_account_mode(&account.url, acme_cfg)?;
        let account_dir = acme_cfg.account_dir(&account.name, &mode);
        let account_json_path = account_dir.join(ACCOUNT_FILE_NAME);
        let account_key_path = account_dir.join(ACCOUNT_KEY_FILE_NAME);

        if account_json_path.exists() {
            let existing_url = read_account_url(&account_json_path)?;
            if existing_url != account.url {
                return Err(claviron::Error::Cli(format!(
                    "account URL mismatch for '{}': existing='{}' db='{}'",
                    account.name, existing_url, account.url
                )));
            }
        }

        std::fs::create_dir_all(&account_dir).map_err(|err| {
            claviron::Error::Cli(format!(
                "failed to create account dir '{}': {err}",
                account_dir.to_string_lossy()
            ))
        })?;

        if account_key_path.exists() && !overwrite {
            let existing_key = std::fs::read_to_string(&account_key_path)
                .map_err(|err| {
                    claviron::Error::Cli(format!(
                        "failed to read account key file '{}': {err}",
                        account_key_path.display()
                    ))
                })?;
            if existing_key.trim_end() != account.private_key.trim_end() {
                return Err(claviron::Error::Cli(format!(
                    "account key mismatch for '{}'; use --overwrite to replace the local account from DB",
                    account.name
                )));
            }
        }

        if overwrite || !account_key_path.exists() {
            std::fs::write(&account_key_path, &account.private_key).map_err(
                |err| {
                    claviron::Error::Cli(format!(
                        "failed to write account key file '{}': {err}",
                        account_key_path.to_string_lossy()
                    ))
                },
            )?;
        }

        let jwk: AcmeJwk = serde_json::from_value(account.jwk.clone())
            .map_err(|e| {
                claviron::Error::Other(format!("invalid account JWK: {}", e))
            })?;

        let mut acct = Account::with_config(acme_cfg, &mode, &account.name);
        acct.url = account.url.clone();
        acct.alg = account.algorithm;
        acct.contact = account.email.clone();
        acct.tos_agreed = true;
        acct.mode = mode;
        acct.jwk = jwk;

        let acct_json = serde_json::to_string_pretty(&acct).map_err(|e| {
            claviron::Error::Other(format!(
                "failed to serialize account: {}",
                e
            ))
        })?;
        std::fs::write(&account_json_path, acct_json).map_err(|err| {
            claviron::Error::Cli(format!(
                "failed to write account json file '{}': {err}",
                account_json_path.to_string_lossy()
            ))
        })?;

        println!("   Account: {}", account_json_path.display());
        println!("   Account Key: {}", account_key_path.display());

        acct_for_meta = Some(acct);
    }
    std::fs::create_dir_all(&cert_dir).map_err(|err| {
        claviron::Error::Cli(format!(
            "failed to crate certificate dir '{}': {err}",
            cert_dir.to_string_lossy()
        ))
    })?;

    let fullchain_pem = der_chain_to_pem(&certificate.certs_der)?;
    let fullchain_path = cert_dir.join(FULLCHAIN_CERT_FILENAME);
    let key_path = cert_dir.join(PRIVKEY_FILENAME);
    let meta_path = cert_dir.join(CERT_META_PATH);

    std::fs::write(&fullchain_path, fullchain_pem.as_bytes()).map_err(
        |err| {
            claviron::Error::Cli(format!(
                "failed to crate certificate file '{}': {err}",
                fullchain_path.to_string_lossy()
            ))
        },
    )?;

    let key_pem = der_to_pem(&certificate.key_der, "PRIVATE KEY");
    std::fs::write(&key_path, key_pem.as_bytes()).map_err(|err| {
        claviron::Error::Cli(format!(
            "failed to crate key file '{}': {err}",
            key_path.display()
        ))
    })?;

    if let Ok(text) = parse_text_from_pem(fullchain_pem.as_bytes()) {
        let text_path = cert_dir.join(CERT_TEXT_PATH);
        if let Err(e) = std::fs::write(&text_path, text) {
            warn!("failed to write certificate text: {}", e);
        } else {
            println!("   Text: {}", text_path.display());
        }
    } else {
        warn!("failed to parse certificate text");
    }

    if let Some(acct) = acct_for_meta {
        let meta = CertMeta::from_cert(&acct, fullchain_pem.as_bytes())
            .map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to build cert meta: {}",
                    e
                ))
            })?;
        let meta_json = meta.as_json().map_err(|e| {
            claviron::Error::Other(format!(
                "failed to serialize cert meta: {}",
                e
            ))
        })?;
        match std::fs::write(&meta_path, meta_json) {
            Ok(_) => println!("   Meta: {}", meta_path.display()),
            Err(err) => eprintln!(
                "{} `{}` : {}",
                "failed to write meta file".bright_red().bold(),
                meta_path.display(),
                err
            ),
        }
    }

    println!("✅ Exported certificate for '{}'", cn.as_str());
    println!("   Cert: {}", fullchain_path.display());
    println!("   Key : {}", key_path.display());

    Ok(())
}

fn der_to_pem(
    der: &[u8],
    label: &str,
) -> String {
    let b64 = base64_standard_encode(der);
    let mut out = String::new();
    out.push_str("-----BEGIN ");
    out.push_str(label);
    out.push_str("-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}

fn der_chain_to_pem(chain: &[u8]) -> claviron::Result<String> {
    use x509_parser::prelude::{FromDer, X509Certificate};
    let mut out = String::new();
    let mut data = chain;
    while !data.is_empty() {
        let (rem, _cert) = X509Certificate::from_der(data).map_err(|e| {
            claviron::Error::Other(format!("invalid certificate DER: {}", e))
        })?;
        let used = data.len() - rem.len();
        out.push_str(&der_to_pem(&data[..used], "CERTIFICATE"));
        data = rem;
    }
    Ok(out)
}

fn read_account_url(path: &Path) -> claviron::Result<String> {
    let data = std::fs::read(path)?;
    let v: serde_json::Value = serde_json::from_slice(&data)?;
    let url = v.get("url").and_then(|u| u.as_str()).ok_or_else(|| {
        claviron::Error::Cli(format!(
            "account file missing url: {}",
            path.display()
        ))
    })?;
    Ok(url.to_string())
}

fn infer_account_mode(
    account_url: &str,
    cfg: &AcmeConfig,
) -> claviron::Result<AcmeMode> {
    let acc_host = url_host(account_url).ok_or_else(|| {
        claviron::Error::Cli(format!("invalid account url: {}", account_url))
    })?;
    let prod_host =
        url_host(cfg.base_url(&AcmeMode::Production)).ok_or_else(|| {
            claviron::Error::Cli("invalid production url".to_string())
        })?;
    let stag_host =
        url_host(cfg.base_url(&AcmeMode::Staging)).ok_or_else(|| {
            claviron::Error::Cli("invalid staging url".to_string())
        })?;

    if acc_host == prod_host {
        Ok(AcmeMode::Production)
    } else if acc_host == stag_host {
        Ok(AcmeMode::Staging)
    } else {
        Err(claviron::Error::Cli(format!(
            "account url does not match configured ACME endpoints: {}",
            account_url
        )))
    }
}

fn url_host(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1)?;
    let host_port = after_scheme.split('/').next()?;
    if host_port.is_empty() { None } else { Some(host_port.to_string()) }
}

#[derive(Debug, Default, serde::Deserialize)]
struct TlsFileOverrideConfig {
    #[serde(default, alias = "certs")]
    sni_certs: Vec<TlsFileOverrideEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TlsFileOverrideEntry {
    cert_path: Option<PathBuf>,
}

async fn warn_if_tls_file_overrides(
    hosting: &HostingStore,
    imported_hosts: &[String],
) {
    let wanted: BTreeSet<String> =
        imported_hosts.iter().map(|host| Sni::norm(host)).collect();
    if wanted.is_empty() {
        return;
    }

    let Some(tls_path) = find_tls_yaml_path() else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&tls_path) else {
        return;
    };
    let yaml = match claviron::helpers::yaml::expand_env_with(&raw, |key| {
        std::env::var(key).ok()
    }) {
        Ok(yaml) => yaml,
        Err(err) => {
            warn!(
                "failed to expand {} while checking file cert overrides: {err}",
                tls_path.display()
            );
            return;
        }
    };
    let cfg: TlsFileOverrideConfig = match serde_yaml::from_str(&yaml) {
        Ok(cfg) => cfg,
        Err(err) => {
            warn!(
                "failed to parse {} while checking file cert overrides: {err}",
                tls_path.display()
            );
            return;
        }
    };

    let mut matches = BTreeSet::new();
    let mut file_hosts_all = BTreeSet::new();
    for entry in cfg.sni_certs {
        let Some(cert_path) = entry.cert_path else {
            continue;
        };
        let Ok(cert_bytes) = std::fs::read(&cert_path) else {
            continue;
        };
        let mut file_hosts = BTreeSet::new();
        if let Ok(chain) = concat_chain_bytes_from_pem(&cert_bytes)
            && let Ok(cert) = take_first_cert_from_der(&chain)
        {
            if let Ok(cn) = Host::try_from(&cert) {
                file_hosts.insert(Sni::norm(cn.key_string().as_str()));
            }
            if let Ok(aliases) = Aliases::try_from(&cert) {
                for alias in aliases.iter() {
                    file_hosts.insert(Sni::norm(alias.key_string().as_str()));
                }
            }
        }

        file_hosts_all.extend(file_hosts.iter().cloned());
        for host in wanted.intersection(&file_hosts) {
            matches.insert(host.clone());
        }
    }

    if file_hosts_all.is_empty() {
        return;
    }

    let db_hosts = db_managed_tls_file_hosts(hosting, &file_hosts_all).await;

    if matches.is_empty() {
        println!(
            "{}",
            "⚠️  tls.yaml explicit file SNI certificate override(s) active:"
                .bright_yellow()
                .bold()
        );
    } else {
        println!(
            "{}",
            "⚠️  imported DB-managed certificate is still overridden by tls.yaml:"
                .bright_yellow()
                .bold()
        );
        println!(
            "   imported hosts: {}",
            matches.into_iter().collect::<Vec<_>>().join(", ")
        );
        println!(
            "   action: remove these hostnames from tls.yaml certs/sni_certs"
        );
    }
    if !db_hosts.is_empty() {
        println!(
            "{}",
            "⚠️  tls.yaml contains host(s) already managed in DB:"
                .bright_yellow()
                .bold()
        );
        println!(
            "   db hosts: {}",
            db_hosts.into_iter().collect::<Vec<_>>().join(", ")
        );
        println!(
            "   action: remove these hostnames from tls.yaml certs/sni_certs"
        );
    }
    println!(
        "   file hosts: {}",
        file_hosts_all.into_iter().collect::<Vec<_>>().join(", ")
    );
    println!("   file : {}", tls_path.display());
    println!(
        "   hint : remove these hostnames from tls.yaml certs/sni_certs if DB-managed ACME renewals should take effect."
    );
}

async fn db_managed_tls_file_hosts(
    hosting: &HostingStore,
    file_hosts: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut managed = BTreeSet::new();
    for host in file_hosts {
        let found = hosting
            .certificate_by_hostname(host)
            .await
            .ok()
            .flatten()
            .is_some();
        if found {
            managed.insert(host.clone());
        }
    }
    managed
}

fn find_tls_yaml_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("CLAVIRON_TLS_CONFIG_PATH") {
        candidates.push(PathBuf::from(path));
    }
    for key in ["CLAVIRON_CONFIG_DIR", "CONFIG_DIR"] {
        if let Ok(dir) = std::env::var(key) {
            candidates.push(PathBuf::from(dir).join("tls.yaml"));
        }
    }
    candidates.push(PathBuf::from("config").join("tls.yaml"));
    candidates.push(PathBuf::from("tls.yaml"));

    candidates.into_iter().find(|path| path.is_file())
}

fn hosting_error(err: HostingError) -> claviron::Error {
    claviron::Error::Cli(format!("hosting database operation failed: {err}"))
}

fn acme_error(err: AcmeError) -> claviron::Error {
    claviron::Error::Cli(format!("ACME database operation failed: {err}"))
}

fn format_cli_error(err: &claviron::Error) -> String {
    match err {
        claviron::Error::Cli(msg) => msg.to_string(),
        _ => err.to_string(),
    }
}

/// Reuse matching account material; never overwrite an existing identity or default.
async fn import_acme_account(
    acme_store: &AcmeStore,
    acme_cfg: &AcmeConfig,
    account: &Account,
) -> claviron::Result<(i32, bool)> {
    let mut account_imported = false;
    let account_id = match acme_store
        .account_by_url(&account.url)
        .await
        .map_err(acme_error)?
    {
        Some(existing) => {
            let file_jwk = serde_json::to_value(&account.jwk).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to serialize JWK: {}",
                    e
                ))
            })?;
            if existing.jwk != file_jwk
                || existing.private_key.trim_end()
                    != account.private_key().trim_end()
            {
                return Err(claviron::Error::Cli(format!(
                    "account material mismatch for URL '{}'",
                    account.url
                )));
            }
            existing.id
        }
        None => {
            warn!(
                "ACME account not found in DB; importing from file (name={}, url={})",
                account.name, account.url
            );
            let jwk = serde_json::to_value(&account.jwk).map_err(|e| {
                claviron::Error::Other(format!(
                    "failed to serialize JWK: {}",
                    e
                ))
            })?;

            let email = if !account.contact.is_empty() {
                account.contact.clone()
            } else {
                acme_cfg.organization().email().to_string()
            };

            let wants_default = account.name == acme_cfg.default_account();
            let existing_default = if wants_default {
                acme_store.default_account().await.map_err(acme_error)?
            } else {
                None
            };
            let is_default = should_import_account_as_default(
                wants_default,
                existing_default.is_some(),
            );
            if let Some(existing) = existing_default {
                warn!(
                    existing_name = %existing.name,
                    existing_url = %existing.url,
                    imported_name = %account.name,
                    "preserving existing default ACME account"
                );
            }

            let created = acme_store
                .insert_account(NewAcmeAccount {
                    name: account.name.clone(),
                    url: account.url.clone(),
                    private_key: account.private_key().to_string(),
                    jwk,
                    algorithm: account.alg,
                    email,
                    organization: acme_cfg.organization().name().to_string(),
                    is_default,
                })
                .await
                .map_err(acme_error)?;
            info!(
                "Created ACME account in DB: {} (id: {})",
                created.name, created.id
            );
            account_imported = true;
            created.id
        }
    };
    Ok((account_id, account_imported))
}

fn should_import_account_as_default(
    configured_as_default: bool,
    database_has_default: bool,
) -> bool {
    configured_as_default && !database_has_default
}

#[cfg(test)]
#[path = "../tests/accounts.rs"]
mod tests;
