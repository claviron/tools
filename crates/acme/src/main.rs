use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, process};

use clap::Parser;
use claviron::Error;
use claviron::ssl::certstore::{FULLCHAIN_CERT_FILENAME, PRIVKEY_FILENAME};
use claviron::ssl::{KeyString, Sni};
use claviron_acme::account::{Account, CERT_META_PATH};
use claviron_acme::actions::account::new_account;
use claviron_acme::actions::certificate::new_cert;
use claviron_acme::actions::renew::renew_cert;
use claviron_acme::{AcmeMode, CertMeta};
use claviron_tools::config;
use colored::Colorize;
use dotenvy::dotenv_override;
use time::OffsetDateTime;

#[derive(Debug, Parser)]
#[command(
    name = "claviron-acme",
    author,
    version,
    about = "claviron acme utility"
)]
pub struct Options {
    #[arg(
        short = 'f',
        long = "config",
        env = "CLAVIRON_CLIENT_CONFIG_PATH",
        help = "client config file path (default: ~/.config/claviron/client.yaml)"
    )]
    pub config_path: Option<PathBuf>,

    #[arg(long= "staging", aliases = ["local","test"],short = 's',help = "Use ACME staging environment"
    )]
    staging: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Parser)]
pub enum Command {
    /// Create and register a new account key
    #[command(aliases = ["account","Account","register"])]
    NewAccount {
        #[arg(
            long = "basename",
            short = 'b',
            help = "ACME account folder name",
            default_value = "claviron"
        )]
        basename: String,
    },
    /// Create a new ACME certificate
    #[command(aliases = ["create","add"])]
    New {
        /// Hosts to include on the certificate (multiple -d allowed)
        #[arg(
            help = "Hostnames to include on the certificate (multiple -d allowed) Fist Alias is used as the Common Name (CN) for the certificate"
        )]
        hosts: Vec<Sni>,
    },
    /// Renew an ACME certificate
    Renew {
        /// Main CN (common name) domain to renew
        #[arg(help = "Main hostname to renew")]
        cn: Sni,

        #[arg(
            long = "before",
            short = 'b',
            help = "overwrite the config renewal days value for until validity date "
        )]
        days_before: Option<u32>,
    },
}

#[tokio::main]
async fn main() -> claviron::Result<()> {
    dotenv_override().ok();

    let options = Options::parse();
    let config_path = config::CliConfig::resolve_path(options.config_path);
    let config = config::CliConfig::load(&config_path).await?;

    let cfg = config.acme_cfg();

    let mode =
        if options.staging { AcmeMode::Staging } else { AcmeMode::Production };

    match options.command {
        Command::NewAccount { basename } => {
            println!("registering new account with name {}", basename);

            let mut account = Account::with_config(cfg, &mode, &basename);

            new_account(&mut account).await.unwrap_or_else(|e| {
                eprintln!("FAILED: {}", e.to_string().bright_red());
                process::exit(1);
            });

            let account_url = &account.url;
            let algorithm = account.alg.to_string();
            let key_file = account.key_path.to_string_lossy();

            let title = "✅ ACME ACCOUNT CREATED".bright_green().bold();
            let bar =
                "=========================================================="
                    .bright_green()
                    .bold();

            println!(
                "\n{}\n{}\n{}\n{}\n{}\n",
                bar,
                title,
                format!("Account Url : {}", account_url)
                    .bright_green()
                    .underline(),
                format!("Algorithm   : {}", algorithm).white(),
                format!("Key File    : {}", key_file).white(),
            );
        }
        Command::New { hosts } => {
            let cn = hosts
                .first()
                .cloned()
                .ok_or_else(|| Error::from("No host provided"))?;

            let account_name = cfg.default_account();
            let account =
                Account::load(cfg, &mode, account_name).map_err(|e| {
                    let msg = "Account could not be loaded".bright_red();
                    eprintln!("{msg}: {e}");
                    Error::from(format!("{msg}: {e}"))
                })?;

            let cert = new_cert(
                Arc::new(account),
                &cn,
                &hosts,
                cfg.organization(),
                cfg.challenges_dir(),
                cfg.pre_validation,
            )
            .await
            .map_err(|e| {
                eprintln!("FAILED: {}", e.to_string().bright_red());
                Error::from(format!("Certificate creation failed: {e}"))
            })?;
            println!("Certificate:\n{}", cert);
            let cert_path = cfg
                .cert_dir(cn.key_str().as_ref())
                .join(FULLCHAIN_CERT_FILENAME);
            let key_path =
                cfg.cert_dir(cn.key_str().as_ref()).join(PRIVKEY_FILENAME);
            println!("Certificate Path: {}", cert_path.to_string_lossy());
            println!("Key Path: {}", key_path.to_string_lossy());
        }
        Command::Renew { cn, days_before } => {
            let meta_path = cfg.cert_dir(cn.as_str()).join(CERT_META_PATH);

            let meta_content = fs::read(&meta_path).inspect_err(|e| {
                eprintln!("Failed to read certificate metadata err:{e}");
            })?;

            let mut meta: CertMeta = serde_json::from_slice(&meta_content)?;

            let account =
                Account::load(cfg, &mode, &meta.account).map_err(|e| {
                    let msg = "Account file not found".bright_red();
                    eprintln!("{msg}: {e}");
                    Error::from(format!("{msg}: {e}"))
                })?;

            let renew_before = days_before.unwrap_or(cfg.renew_before);

            if !meta.should_be_renewed(renew_before) {
                let now = OffsetDateTime::now_utc();
                let remaining = meta.valid_until - now;
                eprintln!(
                    "Certificate renewal not required: {} remaining",
                    humantime::format_duration(std::time::Duration::from_secs(
                        remaining.as_seconds_f64() as u64
                    ))
                );
                return Ok(());
            }

            renew_cert(Arc::new(account), &mut meta, cfg.challenges_dir())
                .await
                .map_err(|e| {
                    eprintln!("RENEW FAILED: {}", e.to_string().bright_red());
                    Error::from(format!("Certificate renew  failed: {e}"))
                })?;
            println!("Certificate renewed for {}", cn);
        }
    }
    Ok(())
}
