use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::Parser;
use claviron::ssl::certstore::{
    CA_CERT_FILENAME, CA_KEY_FILENAME, CLIENT_CERT_FILENAME,
    CLIENT_KEY_FILENAME, CLIENT_TEXT_FILENAME, CertStore,
    FULLCHAIN_CERT_FILENAME,
};
use claviron::ssl::formatters::parse_text_from_pem;
use claviron::ssl::generators::{
    generate_ca_cert, generate_client_cert, generate_intermediate_ca_cert,
    generate_private_key, generate_server_cert, generate_server_csr,
};
use claviron::ssl::{Aliases, Host, SSLError};
use claviron_tools::init_client_logger;
use dotenvy::dotenv_override;

mod issuer;
use issuer::read_issuer_materials;

#[derive(Debug, Parser)]
#[command(
    name = "claviron-cert",
    author,
    version,
    about = "Claviron Certificate Management"
)]
pub struct Options {
    #[arg(
        short = 'f',
        long = "config",
        env = "CERTSTORE_CONFIG_PATH",
        help = "config file path"
    )]
    pub config_path: Option<PathBuf>,

    /// Hosting client config for database-backed authorities
    #[cfg(feature = "hosting")]
    #[arg(long)]
    pub hosting_config: Option<PathBuf>,

    /// Named hosting database profile
    #[cfg(feature = "hosting")]
    #[arg(long = "server")]
    pub profile: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Parser)]
pub enum Command {
    /// Create a new mTLS server certificate
    #[command(aliases = ["Server","ServerCert"])]
    Server {
        /// Domains to include on the certificate (multiple -d allowed)
        #[arg(
            short = 'd',
            long = "hosts",
            aliases = ["domain","alias","server","host"],
            help = "Hostnames to include on the certificate (multiple -d allowed) First Alias is used as the Common Name (CN) for the certificate"
        )]
        hosts: Vec<Host>,

        #[arg(
            long = "basename",
            short = 'b',
            help = "Certificate folder name (default: first domain)"
        )]
        basename: Option<String>,

        /// intermediate certificate issuer (if not provided, defaults to root CA)
        #[arg(
            short = 'i',
            long = "issuer",
            help = "Intermediate certificate issuer (if not provided, defaults to root CA)"
        )]
        issuer: Option<String>,

        /// Validity days (default: 1825 = 1 year)
        #[arg(
            long,
            default_value_t = 365,
            help = "Validity days (default: 365 = 1 year)"
        )]
        days: u32,

        #[arg(
            long,
            short = 'u',
            help = "Owner user for generated key files (defaults to sudo caller when available)"
        )]
        user: Option<String>,

        #[arg(
            long,
            alias = "gid",
            short = 'g',
            help = "Owner group for generated key files (defaults to --user or sudo caller gid)"
        )]
        group: Option<String>,
    },
    /// Creates a new mTLS client certificate
    #[command(aliases = ["Client","ClientCert"])]
    Client {
        /// Main CN (common name)
        #[arg(help = "Common Name (CN) for the client certificate")]
        cname: String,

        /// intermediate certificate issuer (if not provided, defaults to root CA)
        #[arg(
            short = 'i',
            long = "issuer",
            help = "Intermediate certificate issuer (if not provided, defaults to root CA)"
        )]
        issuer: Option<String>,

        /// Validity days (default: 365 = 1 year)
        #[arg(
            long,
            default_value_t = 365,
            help = "Validity days (default: 365 = 1 year)"
        )]
        days: u32,

        #[arg(
            long,
            short = 'u',
            help = "Owner user for generated key files (defaults to sudo caller when available)"
        )]
        user: Option<String>,

        #[arg(
            long,
            alias = "gid",
            short = 'g',
            help = "Owner group for generated key files (defaults to --user or sudo caller gid)"
        )]
        group: Option<String>,
    },
    /// Generate an intermediate CA certificate
    #[command(aliases = ["Intermediate","intca"])]
    Intermediate {
        #[arg(help = "Common Name (CN) for the intermediate CA")]
        cname: String,
        #[arg(
            long = "basename",
            short = 'b',
            help = "Intermediate CA folder name"
        )]
        basename: String,
        /// Validity days (default: 3650 = 10 years)
        #[arg(
            long,
            default_value_t = 3650,
            help = "Validity days (default: 3650 = 10 years)"
        )]
        days: u32,
    },
    /// Generate or update the Root CA certificate (creates key if missing)
    #[command(aliases = ["CA","Ca","CaCert"])]
    Ca {
        #[arg(help = "Common Name (CN) for the Root CA")]
        cname: String,

        #[arg(long = "basename", short = 'b', help = "CA folder name")]
        basename: String,
        /// Validity days (default: 3650 = 10 years)
        #[arg(
            long,
            default_value_t = 3650,
            help = "Validity days (default: 3650 = 10 years)"
        )]
        days: u32,
        #[arg(long, help = "Organization name (optional)")]
        org: Option<String>,
        #[arg(long, help = "Country code (optional, e.g. TR)")]
        country: Option<String>,
    },
}

fn main() -> claviron::Result<()> {
    dotenv_override().ok();

    init_client_logger();

    let options = Options::parse();
    let store_path = CertStore::resolve_path(options.config_path.clone());
    let certstore = CertStore::from_path(&store_path).map_err(|e| {
        claviron::Error::other(format!("failed to load certificate store: {e}"))
    })?;

    /// Write a human-readable certificate dump to a file
    fn write_certificate_text(
        cert_pem: &[u8],
        out_path: &Path,
    ) -> Result<(), SSLError> {
        let txt = parse_text_from_pem(cert_pem)?;
        fs::write(out_path, txt).map_err(|e| SSLError::Io {
            path: out_path.to_path_buf(),
            source: e,
        })
    }

    fn write_with_user_only_access(
        path: &Path,
        data: &[u8],
    ) -> Result<(), SSLError> {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| SSLError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        f.write_all(data)
            .map_err(|e| SSLError::Io { path: path.to_path_buf(), source: e })
    }

    #[cfg(unix)]
    #[derive(Clone, Debug)]
    struct FileOwner {
        uid: nix::unistd::Uid,
        gid: nix::unistd::Gid,
        label: String,
    }

    #[cfg(unix)]
    fn resolve_file_owner(
        user: &Option<String>,
        group: &Option<String>,
    ) -> Result<Option<FileOwner>, SSLError> {
        if let Some(user_name) = user.as_deref() {
            let group_name = group.as_deref().unwrap_or(user_name);

            let user_entry = nix::unistd::User::from_name(user_name)
                .map_err(|e| {
                    SSLError::Other(format!(
                        "failed to resolve user '{user_name}': {e}"
                    ))
                })?
                .ok_or_else(|| {
                    SSLError::Other(format!("User not found: {user_name}"))
                })?;

            let group_entry = nix::unistd::Group::from_name(group_name)
                .map_err(|e| {
                    SSLError::Other(format!(
                        "failed to resolve group '{group_name}': {e}"
                    ))
                })?
                .ok_or_else(|| {
                    SSLError::Other(format!("Group not found: {group_name}"))
                })?;

            return Ok(Some(FileOwner {
                uid: user_entry.uid,
                gid: group_entry.gid,
                label: format!("{user_name}:{group_name}"),
            }));
        }

        if nix::unistd::Uid::effective().is_root()
            && let (Ok(uid), Ok(gid)) =
                (std::env::var("SUDO_UID"), std::env::var("SUDO_GID"))
        {
            let uid = u32::from_str(&uid).map_err(|e| {
                SSLError::Other(format!("invalid SUDO_UID value: {e}"))
            })?;
            let gid = u32::from_str(&gid).map_err(|e| {
                SSLError::Other(format!("invalid SUDO_GID value: {e}"))
            })?;
            let user_label =
                std::env::var("SUDO_USER").unwrap_or_else(|_| uid.to_string());
            return Ok(Some(FileOwner {
                uid: nix::unistd::Uid::from_raw(uid),
                gid: nix::unistd::Gid::from_raw(gid),
                label: format!("{user_label}:{gid}"),
            }));
        }

        Ok(None)
    }

    fn chown_if_requested(
        path: &Path,
        owner: &Option<FileOwner>,
    ) -> Result<(), SSLError> {
        let Some(owner) = owner else {
            return Ok(());
        };

        #[cfg(unix)]
        {
            nix::unistd::chown(path, Some(owner.uid), Some(owner.gid))
                .map_err(|e| {
                    SSLError::Other(format!(
                        "failed to chown '{}' to {}: {e}",
                        path.display(),
                        owner.label
                    ))
                })?;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = (path, owner);
            Err(SSLError::Other(
                "chown is only supported on Unix platforms".to_string(),
            ))
        }
    }

    fn run_server(
        aliases: Aliases,
        basename: Option<String>,
        cfg: &CertStore,
        days: u32,
        material: &(Vec<u8>, Vec<u8>),
        include_chain: bool,
        owner: &Option<FileOwner>,
    ) -> Result<(), SSLError> {
        let cname: std::borrow::Cow<'_, str> = aliases
            .first()
            .map(|host| host.to_cow_str())
            .ok_or(SSLError::NoDomainsProvided)?;

        let basename: &str = match basename.as_deref() {
            Some(name) => name,
            None => cname.as_ref(),
        };
        let dir = cfg.server_dir(basename);

        fs::create_dir_all(&dir)
            .map_err(|e| SSLError::Io { path: dir.clone(), source: e })?;

        // subject key
        let subject_key = generate_private_key()?; // ECDSA P-256
        let key_pem = subject_key.serialize_pem();
        let key_path = dir.join("server.key");
        let legacy_key_path = dir.join("privkey.pem");

        // issue cert using issuer
        let (issuer_cert_pem, issuer_key_pem) = material;

        let csr = generate_server_csr(
            &subject_key,
            cname.as_ref(),
            &aliases,
            cfg.organization(),
        )?;

        // Debug: round-trip aliases from CSR to catch any empty SANs
        // if let Ok(csr_pem_dbg) = csr.pem() {
        // 	if let Ok(csr_aliases) = Aliases::from_csr_content(csr_pem_dbg.as_bytes()) {
        // 		debug!(?csr_aliases, "csr_aliases (round-trip)");
        // 	} else {
        // 		debug!("csr_aliases parse failed (round-trip)");
        // 	}
        // }

        let cert_pem = generate_server_cert(
            &csr,
            key_pem.as_bytes(),
            issuer_cert_pem,
            issuer_key_pem,
            days,
        )?;

        write_with_user_only_access(&key_path, key_pem.as_bytes())?;
        chown_if_requested(&key_path, owner)?;

        write_with_user_only_access(&legacy_key_path, key_pem.as_bytes())?;
        chown_if_requested(&legacy_key_path, owner)?;

        // save cert
        let crt_path = dir.join("certificate.crt");
        fs::write(&crt_path, &cert_pem)
            .map_err(|e| SSLError::Io { path: crt_path.clone(), source: e })?;

        if include_chain {
            let fullchain_path = dir.join("fullchain.pem");
            let mut fullchain =
                Vec::with_capacity(cert_pem.len() + issuer_cert_pem.len());
            fullchain.extend_from_slice(&cert_pem);
            fullchain.extend_from_slice(issuer_cert_pem);
            fs::write(&fullchain_path, &fullchain).map_err(|e| {
                SSLError::Io { path: fullchain_path.clone(), source: e }
            })?;
        }

        let txt_path = dir.join("certificate.txt");
        write_certificate_text(&cert_pem, &txt_path)?;

        // create a CSR and store for renewal reference
        let csr_path = dir.join("request.csr");
        let csr_pem = csr.pem()?;
        fs::write(&csr_path, csr_pem.as_bytes())
            .map_err(|e| SSLError::Io { path: csr_path.clone(), source: e })?;

        println!(
            "[server] CN={} basename={} -> {}",
            cname,
            basename,
            crt_path.display()
        );
        Ok(())
    }

    fn run_client(
        cname: &str,
        cfg: &CertStore,
        material: &(Vec<u8>, Vec<u8>),
        days: u32,
        include_chain: bool,
        owner: &Option<FileOwner>,
    ) -> Result<(), SSLError> {
        let dir = cfg.client_dir(cname);

        // TODO: Review and standardize permissions for all certificate directories.
        fs::create_dir_all(&dir)
            .map_err(|e| SSLError::Io { path: dir.clone(), source: e })?;
        #[cfg(unix)]
        {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o711))
                .map_err(|e| SSLError::Io { path: dir.clone(), source: e })?;
        }

        // client key (new)
        let client_key = generate_private_key()?;
        let client_key_pem = client_key.serialize_pem();
        let key_path = dir.join(CLIENT_KEY_FILENAME);

        // issuer
        let (issuer_cert_pem, issuer_key_pem) = material;

        // minimal client cert (no extra SANs)
        let cert_pem = generate_client_cert(
            cname,
            client_key_pem.as_bytes(),
            issuer_cert_pem,
            issuer_key_pem,
            days,
            &[] as &[&str],
        )?;
        let crt_path = dir.join(CLIENT_CERT_FILENAME);
        fs::write(&crt_path, &cert_pem)
            .map_err(|e| SSLError::Io { path: crt_path.clone(), source: e })?;

        if include_chain {
            let fullchain_path = dir.join(FULLCHAIN_CERT_FILENAME);
            let mut fullchain =
                Vec::with_capacity(cert_pem.len() + issuer_cert_pem.len());
            fullchain.extend_from_slice(&cert_pem);
            fullchain.extend_from_slice(issuer_cert_pem);
            fs::write(&fullchain_path, &fullchain).map_err(|e| {
                SSLError::Io { path: fullchain_path.clone(), source: e }
            })?;
        }

        let txt_path = dir.join(CLIENT_TEXT_FILENAME);
        write_certificate_text(&cert_pem, &txt_path)?;
        write_with_user_only_access(&key_path, client_key_pem.as_bytes())?;
        chown_if_requested(&key_path, owner)?;

        println!("[client] CN={cname} -> {}", crt_path.display());
        Ok(())
    }

    fn run_intermediate(
        cname: &str,
        basename: &str,
        cfg: &CertStore,
        days: u32,
        material: &(Vec<u8>, Vec<u8>),
    ) -> Result<(), SSLError> {
        // generate intermediate key and cert in cfg.root()

        let ca_dir = cfg.ca_dir(basename);

        if ca_dir.exists() {
            return Err(SSLError::Exists(
                "Intermediate CA directory already exists".to_string(),
                ca_dir.clone(),
            ));
        }

        fs::create_dir_all(&ca_dir)
            .map_err(|e| SSLError::Io { path: ca_dir.clone(), source: e })?;

        let key_path = ca_dir.join("ca.key");
        let crt_path = ca_dir.join("ca.crt");
        let txt_path = ca_dir.join("ca.txt");

        let key = generate_private_key()?;

        // sign by root CA
        let (issuer_cert_pem, issuer_key_pem) = material;

        let org = cfg.organization();
        let cert_pem = generate_intermediate_ca_cert(
            key.serialize_pem().as_bytes(),
            issuer_cert_pem,
            issuer_key_pem,
            cname,
            days,
            org,
            Some(0),
        )?;

        write_with_user_only_access(&key_path, key.serialize_pem().as_bytes())?;

        fs::write(&crt_path, &cert_pem)
            .map_err(|e| SSLError::Io { path: crt_path.clone(), source: e })?;

        write_certificate_text(&cert_pem, &txt_path)?;

        println!("[intermediate] CN={cname} -> {}", crt_path.display());
        Ok(())
    }

    fn run_ca(
        cname: &str,
        basename: &str,
        cfg: &CertStore,
        days: u32,
        org: &Option<String>,
        country: &Option<String>,
    ) -> Result<(), SSLError> {
        // Load or create CA key
        let ca_dir = cfg.ca_dir(basename);

        if ca_dir.exists() {
            return Err(SSLError::Exists(
                "CA directory already exists".to_string(),
                ca_dir.clone(),
            ));
        }

        fs::create_dir_all(&ca_dir)
            .map_err(|e| SSLError::Io { path: ca_dir.clone(), source: e })?;

        let key_path = ca_dir.join(CA_KEY_FILENAME);
        let crt_path = ca_dir.join(CA_CERT_FILENAME);
        let txt_path = ca_dir.join("ca.txt");

        let kp = generate_private_key()?; // ECDSA P-256
        let ca_key_pem = kp.serialize_pem().into_bytes();

        // Generate CA certificate
        let cert_pem = generate_ca_cert(
            cname,
            &ca_key_pem,
            days,
            org.as_deref(),
            country.as_deref(),
        )?;

        // Write CA key securely (mode 0o600)
        write_with_user_only_access(&key_path, &ca_key_pem)?;

        fs::write(&crt_path, &cert_pem)
            .map_err(|e| SSLError::Io { path: crt_path.clone(), source: e })?;

        write_certificate_text(&cert_pem, &txt_path)?;

        println!("[ca] CN={cname} -> {}", crt_path.display());
        Ok(())
    }

    let material = match &options.command {
        Command::Server { issuer, .. } | Command::Client { issuer, .. } => {
            Some(
                read_issuer_materials(&certstore, issuer, &options)
                    .map_err(|e| claviron::Error::Cli(e.to_string()))?,
            )
        }
        Command::Intermediate { .. } => Some(
            read_issuer_materials(&certstore, &None, &options)
                .map_err(|e| claviron::Error::Cli(e.to_string()))?,
        ),
        Command::Ca { .. } => None,
    };
    match options.command {
        Command::Server { hosts, basename, issuer, days, user, group } => {
            let aliases = Aliases::from(hosts);
            let owner = resolve_file_owner(&user, &group)
                .map_err(|e| claviron::Error::Cli(e.to_string()))?;
            // debug!(?aliases, "aliases");
            run_server(
                aliases,
                basename,
                &certstore,
                days,
                material.as_ref().expect("resolved above"),
                issuer.is_some(),
                &owner,
            )
        }
        Command::Client { cname, issuer, days, user, group } => {
            let owner = resolve_file_owner(&user, &group)
                .map_err(|e| claviron::Error::Cli(e.to_string()))?;
            run_client(
                &cname,
                &certstore,
                material.as_ref().expect("resolved above"),
                days,
                issuer.is_some(),
                &owner,
            )
        }
        Command::Intermediate { cname, basename, days } => run_intermediate(
            &cname,
            &basename,
            &certstore,
            days,
            material.as_ref().expect("resolved above"),
        ),
        Command::Ca { cname, basename, days, org, country } => {
            run_ca(&cname, &basename, &certstore, days, &org, &country)
        }
    }
    .map_err(|e| claviron::Error::Cli(e.to_string()))?;

    Ok(())
}
