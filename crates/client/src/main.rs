use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;
use claviron_tools::{config, init_client_logger};
use dotenvy::dotenv;

mod acme;
mod aliases;
mod application;
mod certificates;
mod client;
mod common;
mod dns;
mod domain;
mod linked;
mod mtls;
mod proto;
mod server;
mod service;
mod task;
mod worker;

use client::create_monitored_channel;
use proto::helloworld;
use proto::helloworld::greeter_client::GreeterClient;

#[derive(Debug, serde::Deserialize)]
struct ClientConfigFile {
    grpc: Option<client::ClientConfig>,
    #[serde(default)]
    profiles: BTreeMap<String, ServerProfile>,
}

#[derive(Debug, serde::Deserialize)]
struct ServerProfile {
    #[serde(default, rename = "default")]
    is_default: bool,
    grpc: Option<client::ClientConfig>,
}

struct SelectedConfig {
    grpc: client::ClientConfig,
}

impl ClientConfigFile {
    async fn load(path: &std::path::Path) -> claviron::Result<Self> {
        let content =
            tokio::fs::read_to_string(path).await.map_err(|error| {
                claviron::Error::Cli(format!(
                    "failed to read client config {}: {}",
                    path.display(),
                    error
                ))
            })?;

        serde_yaml::from_str::<Self>(&content).map_err(|error| {
            claviron::Error::Cli(format!(
                "failed to parse client config {}: {}",
                path.display(),
                error
            ))
        })
    }

    fn select(
        &self,
        server: Option<&str>,
        path: &std::path::Path,
    ) -> claviron::Result<SelectedConfig> {
        let default_profile = self.default_profile()?;
        let address_override = server.filter(|value| {
            value.starts_with("http://") || value.starts_with("https://")
        });
        let selected_name = server
            .filter(|_| address_override.is_none())
            .or(default_profile.map(|(name, _)| name));
        let mut grpc = match selected_name {
            Some(name) => {
                let profile = self.profiles.get(name).ok_or_else(|| {
                    claviron::Error::Cli(format!(
                        "unknown server profile {name:?}; run list-profiles to see available profiles"
                    ))
                })?;
                profile.grpc.clone().ok_or_else(|| claviron::Error::Cli(format!("profile '{name}' has no gRPC connection")))?
            }
            None => self.grpc.clone().ok_or_else(|| {
                    claviron::Error::Cli(
                        "no server selected; configure one default profile or a legacy grpc entry".to_owned()
                    )
                })?,
        };
        if let Some(address) = address_override {
            grpc.server_addr = address.to_owned();
        }
        grpc.resolve_paths_relative_to(path);
        Ok(SelectedConfig { grpc })
    }

    fn default_profile(
        &self
    ) -> claviron::Result<Option<(&str, &ServerProfile)>> {
        if !self.profiles.values().any(|profile| profile.grpc.is_some()) {
            if self.grpc.is_none() {
                return Err(claviron::Error::Cli(
                    "no server configured; add a profile with default: true"
                        .to_owned(),
                ));
            }
            return Ok(None);
        }
        let mut defaults = self.profiles.iter().filter(|(_, profile)| {
            profile.is_default && profile.grpc.is_some()
        });
        let selected = defaults.next().ok_or_else(|| {
            claviron::Error::Cli("no default server profile; mark exactly one profile with default: true".to_owned())
        })?;
        if defaults.next().is_some() {
            return Err(claviron::Error::Cli(
                "multiple default server profiles; mark exactly one profile with default: true".to_owned()
            ));
        }
        Ok(Some((selected.0.as_str(), selected.1)))
    }

    fn list_profiles(&self) -> claviron::Result<()> {
        self.default_profile()?;
        if let Some(grpc) = &self.grpc {
            let default =
                if self.profiles.is_empty() { " (default)" } else { "" };
            println!("legacy config{default}  {}", grpc.server_addr);
        }
        for (name, profile) in &self.profiles {
            let default = if profile.is_default { " (default)" } else { "" };
            if let Some(grpc) = &profile.grpc {
                println!("{name}{default}  {}", grpc.server_addr);
            } else {
                println!("{name}  (no gRPC connection)");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/profiles.rs"]
mod profile_tests;

#[derive(Debug, Parser)]
#[command(
    name = "claviron-client",
    author,
    version,
    about = "client for runtime claviron services"
)]
pub struct Options {
    #[arg(short = 'f', long = "config", help = "client config file path")]
    pub config_path: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        help = "Server profile name or full gRPC URL; a URL uses the default profile's TLS credentials"
    )]
    pub server: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Parser)]
pub enum Command {
    /// Manage authoritative DNS domains and records
    Dns {
        #[command(subcommand)]
        command: dns::Command,
    },
    /// Registrar-held domains: the store, nameservers and registrar sync
    Domain {
        #[command(subcommand)]
        command: domain::Command,
    },
    /// List configured server profiles without connecting
    ListProfiles,

    /// Simple hello request to gRPC greeter
    Hello {
        #[arg(help = "hello name to greet")]
        name: String,
    },

    /// List all certificates
    #[command(alias = "ls")]
    ListCertificates {
        #[arg(long, help = "Show only active/enabled certificates")]
        active_only: bool,
        #[arg(
            long,
            alias = "expires",
            short = 'e',
            help = "Show certificates expiring within N days"
        )]
        expiring_days: Option<i32>,
    },

    /// List certificates attached to one server
    ListCertificatesByServer {
        #[arg(help = "Server name")]
        server_name: String,
    },

    /// List certificates with aliases, dates, state, and server attachments
    ListCertificateInventory {
        #[arg(long, help = "Show only active/enabled certificates")]
        active_only: bool,
        #[arg(
            long,
            alias = "expires",
            short = 'e',
            help = "Show certificates expiring within N days"
        )]
        expiring_days: Option<i32>,
    },

    /// Get certificate details or its public fullchain by hostname
    GetCertificate {
        #[arg(help = "Hostname (CN or alias)")]
        hostname: String,
        #[arg(long, help = "Print only the public PEM fullchain")]
        fullchain: bool,
    },

    /// Add a new certificate
    AddCertificate {
        #[arg(help = "Common Name and SANs (comma-separated)")]
        aliases: String,
    },

    /// Renew an existing certificate
    RenewCertificate {
        #[arg(help = "Common Name to renew")]
        cn: String,
        #[arg(
            long,
            alias = "before",
            short = 'b',
            help = "Renew N days before expiration"
        )]
        before: Option<i64>,
    },

    /// Renew a local CA/server/client certificate without rotating its key
    RenewLocalCertificate {
        common_name: String,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        validity_days: u32,
    },

    /// Activate a certificate
    ActivateCertificate { cn: String },

    /// Disable a certificate
    DisableCertificate { cn: String },

    /// Remove a certificate
    RemoveCertificate { cn: String },

    /// Reload certificate resolver
    ReloadResolver,

    /// Create the self-signed root of the server's local CA
    CreateRootCa {
        #[arg(help = "Common name of the root CA")]
        common_name: String,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..), help = "Days the root is valid")]
        validity_days: u32,
        #[arg(long)]
        organization: Option<String>,
        #[arg(long, help = "Two-letter country code")]
        country: Option<String>,
    },

    /// Create a certificate from the server's local CA (not ACME)
    CreateCertificate {
        #[arg(help = "Identity: DNS name, IP or bare name")]
        common_name: String,
        #[arg(
            long,
            value_enum,
            help = "server, client (mTLS) or ca (intermediate)"
        )]
        kind: mtls::Kind,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..), help = "Days the certificate is valid; within the issuing CA's validity")]
        validity_days: u32,
        #[arg(
            long,
            value_delimiter = ',',
            help = "Further names the certificate covers (comma-separated)"
        )]
        aliases: Vec<String>,
        #[arg(
            long,
            help = "Required for --kind ca: maximum intermediate depth below it"
        )]
        ca_path_len: Option<u32>,
        #[arg(
            long,
            help = "A named local_ca issuer; omitted, the configured authority issues"
        )]
        issuer: Option<String>,
    },

    /// Import a certificate and its private key into the hosting database
    ImportCertificate {
        #[arg(
            long,
            value_name = "PATH",
            help = "PEM certificate, optionally followed by its chain"
        )]
        certificate: PathBuf,
        #[arg(
            long,
            value_name = "PATH",
            help = "Unencrypted PEM private key matching the certificate"
        )]
        key: PathBuf,
        #[arg(
            long,
            value_name = "PATH",
            help = "Further PEM chain files, appended in order"
        )]
        chain: Vec<PathBuf>,
        #[arg(
            long,
            help = "Registered local CA issuer; omitted, detected by signature (not ACME)"
        )]
        issuer: Option<String>,
    },

    /// Export a certificate chain, and its private key only when asked
    ExportCertificate {
        #[arg(help = "Exact stored common name (not an alias)")]
        common_name: String,
        #[arg(
            long,
            value_name = "PATH",
            help = "Write the chain here instead of standard output"
        )]
        out: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Also export the private key, written to this file with mode 0600"
        )]
        key_out: Option<PathBuf>,
        #[arg(long, help = "Replace files that already exist")]
        force: bool,
    },

    /// Add a hostname alias to a vhost
    AddAlias { vhost_name: String, hostname: String },

    /// Remove a hostname alias
    RemoveAlias { hostname: String },

    /// Move an alias to another vhost
    ReplaceAlias { hostname: String, vhost_name: String },

    /// List hostname aliases
    ListAliases {
        #[arg(long)]
        vhost_name: Option<String>,
        #[arg(long, help = "Only aliases belonging to enabled vhosts")]
        enabled_only: bool,
    },

    /// List the complete vhost, alias, and server-attachment inventory
    ListHosting {
        #[arg(long)]
        enabled_vhosts_only: bool,
    },

    /// Reload the in-memory vhost table
    ReloadVhosts,

    /// Show server identity, responding process, worker counts and capabilities
    Info,

    /// Reload the complete server process family through the master
    Reload,

    /// Gracefully shut down the process family and wait for its connection to close
    Shutdown,

    /// List process-bound runnable services and their fleet placements
    ListServices,

    /// Show one process-bound runnable service and its fleet placements
    ServiceStatus { name: String },

    /// Start every registered placement of a runnable service
    StartService {
        name: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Stop every registered placement of a runnable service
    StopService {
        name: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Reload a provider without replacing its runnable generation
    ReloadService {
        name: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Drain and replace every registered placement of a runnable service
    RestartService {
        name: String,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// List named applications across every registered runtime
    #[command(alias = "list-apps")]
    ListApplications,

    /// Show one runtime-qualified application
    #[command(alias = "app-status")]
    ApplicationStatus { runtime: String, name: String },

    /// Start a runtime-qualified application
    #[command(alias = "start-app")]
    StartApplication {
        runtime: String,
        name: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Stop a runtime-qualified application
    #[command(alias = "stop-app")]
    StopApplication {
        runtime: String,
        name: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Restart a runtime-qualified application
    #[command(alias = "restart-app")]
    RestartApplication {
        runtime: String,
        name: String,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// List the runners this edge reaches over Orbit Link and their reachability
    #[command(alias = "list-linked")]
    ListLinkedApplications,

    /// Show one linked application, by Link fleet and name
    #[command(alias = "linked-status")]
    LinkedApplicationStatus { fleet: String, name: String },

    /// Start one application inside a linked runner
    #[command(alias = "start-linked")]
    StartLinkedApplication {
        fleet: String,
        link: String,
        runtime: String,
        application: String,
    },

    /// Stop one application inside a linked runner
    #[command(alias = "stop-linked")]
    StopLinkedApplication {
        fleet: String,
        link: String,
        runtime: String,
        application: String,
    },

    /// Restart one application inside a linked runner
    #[command(alias = "restart-linked")]
    RestartLinkedApplication {
        fleet: String,
        link: String,
        runtime: String,
        application: String,
    },
    /// Forward an application-defined command to a linked runner; the runner
    /// decides what it means
    #[command(alias = "send-linked")]
    SendLinkedCommand {
        fleet: String,
        link: String,
        command: String,
        /// Bytes handed to the runner with the command, given as text
        #[arg(long, default_value = "")]
        payload: String,
    },

    /// Attach a vhost to comma-separated server names
    AddVhostToServers { vhost_name: String, server_names: String },

    /// Detach a vhost from comma-separated server names
    RemoveVhostFromServers { vhost_name: String, server_names: String },

    /// List vhosts attached to a server
    ListVhostsByServer { server_name: String },

    /// List servers attached to a vhost
    ListServersByVhost { vhost_name: String },

    /// List configured tasks and their fleet placements
    ListTasks,

    /// Show one configured task and its fleet placements
    TaskStatus { name: String },

    /// Arm a task's configured schedule and let it run
    StartTask {
        name: String,
        #[arg(
            long,
            default_value_t = 30,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to wait for current fleet capacity to report the enabled state"
        )]
        timeout: u64,
    },

    /// Disarm a task's schedule and drain its active placements
    StopTask {
        name: String,
        #[arg(
            long,
            default_value_t = 30,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to wait for current fleet capacity to report the disabled state"
        )]
        timeout: u64,
    },

    /// Reload a logical task according to its configured reload policy
    ReloadTask { name: String },

    /// Drain and replace every eligible placement of a started daemon task
    RestartTask {
        name: String,
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to observe every target placement enter a new generation"
        )]
        timeout: u64,
    },

    /// Execute one task occurrence immediately without changing its trigger
    RunTask {
        name: String,
        #[arg(
            long,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Override the configured execution timeout for this run, in seconds"
        )]
        timeout: Option<u64>,
    },

    /// List fixed worker slots and their process status
    ListWorkers,

    /// Show one configured worker slot and its process status
    WorkerStatus { worker_id: u8 },

    /// Start an existing stopped worker slot
    StartWorker {
        worker_id: u8,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Stop an existing worker slot without changing fleet size
    StopWorker {
        worker_id: u8,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Reload one running worker in place
    ReloadWorker {
        worker_id: u8,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Gracefully replace one running worker with a new incarnation
    RestartWorker {
        worker_id: u8,
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to observe the selected worker enter a new running incarnation"
        )]
        timeout: u64,
    },

    /// Reload every currently running worker in place
    ReloadWorkers {
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Replace every running worker using a safe rolling restart
    RestartWorkers {
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to observe every target worker enter a new running incarnation"
        )]
        timeout: u64,
    },

    /// Add one worker within the immutable fleet capacity
    AddWorker {
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to observe the larger fleet converge"
        )]
        timeout: u64,
    },

    /// Remove the highest worker id while keeping at least one worker
    RemoveWorker {
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(1..),
            help = "Seconds to observe the smaller fleet converge"
        )]
        timeout: u64,
    },
}

#[tokio::main]
async fn main() -> Result<(), common::CliError> {
    dotenv().ok();
    init_client_logger();

    let options = Options::parse();
    let config_path = config::CliConfig::resolve_path(options.config_path);
    tracing::debug!(?config_path);

    let config_file = ClientConfigFile::load(&config_path).await?;
    if matches!(options.command, Command::ListProfiles) {
        config_file.list_profiles()?;
        return Ok(());
    }
    let config = config_file.select(options.server.as_deref(), &config_path)?;
    let (channel, connection_monitor) =
        create_monitored_channel(&config.grpc).await.map_err(|error| {
            claviron::Error::Cli(format!("channel creation failed: {error}"))
        })?;
    tracing::debug!(server_address = %config.grpc.server_addr);

    match options.command {
        Command::Dns { command } => dns::run(channel, command).await?,
        Command::Domain { command } => domain::run(channel, command).await?,
        Command::ListProfiles => unreachable!("handled before connecting"),
        Command::Hello { name } => {
            let response = GreeterClient::new(channel)
                .say_hello(tonic::Request::new(helloworld::HelloRequest {
                    name,
                }))
                .await?;
            println!("Response: {}", response.into_inner().message);
        }
        Command::ListCertificates { active_only, expiring_days } => {
            acme::list_certificates(channel, active_only, expiring_days).await?
        }
        Command::ListCertificatesByServer { server_name } => {
            acme::list_certificates_by_server(channel, server_name).await?
        }
        Command::ListCertificateInventory { active_only, expiring_days } => {
            acme::list_certificate_inventory(
                channel,
                active_only,
                expiring_days,
            )
            .await?
        }
        Command::GetCertificate { hostname, fullchain } => {
            acme::get_certificate(channel, hostname, fullchain).await?
        }
        Command::AddCertificate { aliases } => {
            acme::add_certificate(channel, aliases).await?
        }
        Command::RenewCertificate { cn, before } => {
            acme::renew_certificate(channel, cn, before).await?
        }
        Command::RenewLocalCertificate { common_name, validity_days } => {
            mtls::renew_certificate(channel, common_name, validity_days).await?
        }
        Command::ActivateCertificate { cn } => {
            acme::activate_certificate(channel, cn).await?
        }
        Command::DisableCertificate { cn } => {
            acme::disable_certificate(channel, cn).await?
        }
        Command::RemoveCertificate { cn } => {
            acme::remove_certificate(channel, cn).await?
        }
        Command::ReloadResolver => acme::reload_resolver(channel).await?,
        Command::CreateRootCa {
            common_name,
            validity_days,
            organization,
            country,
        } => {
            mtls::create_root_ca(
                channel,
                common_name,
                validity_days,
                organization,
                country,
            )
            .await?
        }
        Command::CreateCertificate {
            common_name,
            kind,
            validity_days,
            aliases,
            ca_path_len,
            issuer,
        } => {
            mtls::create_certificate(
                channel,
                common_name,
                aliases,
                validity_days,
                kind,
                ca_path_len,
                issuer,
            )
            .await?
        }
        Command::ImportCertificate { certificate, key, chain, issuer } => {
            certificates::import_certificate(
                channel,
                certificate,
                key,
                chain,
                issuer,
            )
            .await?
        }
        Command::ExportCertificate { common_name, out, key_out, force } => {
            certificates::export_certificate(
                channel,
                common_name,
                out,
                key_out,
                force,
            )
            .await?
        }
        Command::AddAlias { vhost_name, hostname } => {
            aliases::add_alias(channel, vhost_name, hostname).await?
        }
        Command::RemoveAlias { hostname } => {
            aliases::remove_alias(channel, hostname).await?
        }
        Command::ReplaceAlias { hostname, vhost_name } => {
            aliases::replace_alias(channel, hostname, vhost_name).await?
        }
        Command::ListAliases { vhost_name, enabled_only } => {
            aliases::list_aliases(channel, vhost_name, enabled_only).await?
        }
        Command::ListHosting { enabled_vhosts_only } => {
            aliases::list_hosting(channel, enabled_vhosts_only).await?
        }
        Command::ReloadVhosts => aliases::reload_vhosts(channel).await?,
        Command::Reload => server::reload(channel).await?,
        Command::Info => server::info(channel).await?,
        Command::Shutdown => {
            server::shutdown(channel, connection_monitor).await?
        }
        Command::ListServices => service::list(channel).await?,
        Command::ServiceStatus { name } => {
            service::status(channel, name).await?
        }
        Command::StartService { name, timeout } => {
            service::start(channel, &config.grpc, name, timeout).await?
        }
        Command::StopService { name, timeout } => {
            service::stop(channel, &config.grpc, name, timeout).await?
        }
        Command::ReloadService { name, timeout } => {
            service::reload(channel, &config.grpc, name, timeout).await?
        }
        Command::RestartService { name, timeout } => {
            service::restart(channel, &config.grpc, name, timeout).await?
        }
        Command::ListApplications => application::list(channel).await?,
        Command::ApplicationStatus { runtime, name } => {
            application::status(channel, runtime, name).await?
        }
        Command::StartApplication { runtime, name, timeout } => {
            application::start(channel, runtime, name, timeout).await?
        }
        Command::StopApplication { runtime, name, timeout } => {
            application::stop(channel, runtime, name, timeout).await?
        }
        Command::RestartApplication { runtime, name, timeout } => {
            application::restart(channel, runtime, name, timeout).await?
        }
        Command::ListLinkedApplications => linked::list(channel).await?,
        Command::LinkedApplicationStatus { fleet, name } => {
            linked::status(channel, fleet, name).await?
        }
        Command::StartLinkedApplication {
            fleet,
            link,
            runtime,
            application,
        } => {
            linked::command(
                channel,
                fleet,
                link,
                runtime,
                application,
                linked::Action::Start,
            )
            .await?
        }
        Command::StopLinkedApplication {
            fleet,
            link,
            runtime,
            application,
        } => {
            linked::command(
                channel,
                fleet,
                link,
                runtime,
                application,
                linked::Action::Stop,
            )
            .await?
        }
        Command::RestartLinkedApplication {
            fleet,
            link,
            runtime,
            application,
        } => {
            linked::command(
                channel,
                fleet,
                link,
                runtime,
                application,
                linked::Action::Restart,
            )
            .await?
        }
        Command::SendLinkedCommand { fleet, link, command, payload } => {
            linked::send(channel, fleet, link, command, payload.into_bytes())
                .await?
        }
        Command::AddVhostToServers { vhost_name, server_names } => {
            aliases::add_vhost_to_servers(channel, vhost_name, server_names)
                .await?
        }
        Command::RemoveVhostFromServers { vhost_name, server_names } => {
            aliases::remove_vhost_from_servers(
                channel,
                vhost_name,
                server_names,
            )
            .await?
        }
        Command::ListVhostsByServer { server_name } => {
            aliases::list_vhosts_by_server(channel, server_name).await?
        }
        Command::ListServersByVhost { vhost_name } => {
            aliases::list_servers_by_vhost(channel, vhost_name).await?
        }
        Command::ListTasks => task::list(channel).await?,
        Command::TaskStatus { name } => task::status(channel, name).await?,
        Command::StartTask { name, timeout } => {
            task::start(channel, name, timeout).await?
        }
        Command::StopTask { name, timeout } => {
            task::stop(channel, name, timeout).await?
        }
        Command::ReloadTask { name } => task::reload(channel, name).await?,
        Command::RestartTask { name, timeout } => {
            task::restart(channel, name, timeout).await?
        }
        Command::RunTask { name, timeout } => {
            task::run(channel, name, timeout).await?
        }
        Command::ListWorkers => worker::list(channel).await?,
        Command::WorkerStatus { worker_id } => {
            worker::status(channel, worker_id).await?
        }
        Command::StartWorker { worker_id, timeout } => {
            worker::start(channel, &config.grpc, worker_id, timeout).await?
        }
        Command::StopWorker { worker_id, timeout } => {
            worker::stop(channel, &config.grpc, worker_id, timeout).await?
        }
        Command::ReloadWorker { worker_id, timeout } => {
            worker::reload(channel, &config.grpc, worker_id, timeout).await?
        }
        Command::RestartWorker { worker_id, timeout } => {
            worker::restart(channel, &config.grpc, worker_id, timeout).await?
        }
        Command::ReloadWorkers { timeout } => {
            worker::reload_all(channel, &config.grpc, timeout).await?
        }
        Command::RestartWorkers { timeout } => {
            worker::restart_all(channel, &config.grpc, timeout).await?
        }
        Command::AddWorker { timeout } => {
            worker::add(channel, &config.grpc, timeout).await?
        }
        Command::RemoveWorker { timeout } => {
            worker::remove(channel, &config.grpc, timeout).await?
        }
    }

    Ok(())
}
