//! Operator commands for the authoritative DNS store (`claviron.dns`).
//!
//! The protocol's updates are full replacements. The commands here take only
//! the fields the operator names, read the current entry, and send the merged
//! configuration back, so `update-zone example.com --refresh 600` does not
//! silently reset the SOA's other fields to proto3 defaults. The read and the
//! write are two requests: a concurrent edit between them is overwritten.

use tonic::transport::Channel;

use crate::common::CommandResult;
use crate::proto::dns;
use crate::proto::dns::dns_manager_client::DnsManagerClient;

/// SOA fields an operator may name; `None` keeps what the zone has.
#[derive(Debug, Default, clap::Args)]
pub struct SoaArgs {
    #[arg(long, help = "Primary name server (SOA MNAME)")]
    pub mname: Option<String>,
    #[arg(long, help = "Responsible mailbox (SOA RNAME)")]
    pub rname: Option<String>,
    #[arg(long, help = "SOA refresh, seconds")]
    pub refresh: Option<u32>,
    #[arg(long, help = "SOA retry, seconds")]
    pub retry: Option<u32>,
    #[arg(long, help = "SOA expire, seconds")]
    pub expire: Option<u32>,
    #[arg(long, help = "SOA minimum / negative-cache TTL, seconds")]
    pub minimum: Option<u32>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    #[command(alias = "list-zones")]
    ListDomains {
        #[arg(long)]
        enabled_only: bool,
    },
    #[command(alias = "get-zone")]
    GetDomain {
        domain: String,
    },
    /// Add a domain; all SOA fields must be explicitly supplied
    #[command(alias = "add-zone")]
    AddDomain {
        domain: String,
        #[command(flatten)]
        soa: SoaArgs,
        #[arg(long)]
        disabled: bool,
    },
    /// Read, merge named fields and replace (concurrent edits can be overwritten)
    #[command(alias = "update-zone")]
    UpdateDomain {
        domain: String,
        #[arg(long)]
        rename: Option<String>,
        #[command(flatten)]
        soa: SoaArgs,
        #[command(flatten)]
        enabled: EnabledArgs,
    },
    /// Refuse a nonempty domain unless --cascade is supplied
    #[command(alias = "remove-zone")]
    RemoveDomain {
        domain: String,
        #[arg(long)]
        cascade: bool,
    },
    ListRecords {
        domain: String,
        #[arg(long)]
        enabled_only: bool,
    },
    AddRecord {
        domain: String,
        #[command(flatten)]
        config: RecordArgs,
    },
    /// Read, merge named fields and replace (concurrent edits can be overwritten)
    UpdateRecord {
        domain: String,
        id: u64,
        #[command(flatten)]
        update: RecordUpdateArgs,
    },
    RemoveRecord {
        id: u64,
    },
    /// Publish a reload notification; completion is asynchronous
    Reload,
}

#[derive(Debug, clap::Args)]
pub struct RecordArgs {
    /// @ for apex, otherwise a zone-relative owner
    pub name: String,
    pub record_type: String,
    pub value: String,
    /// Explicit TTL in seconds (zero is valid)
    #[arg(long)]
    pub ttl: u32,
    #[arg(long)]
    pub priority: Option<u32>,
    #[arg(long)]
    pub weight: Option<u32>,
    #[arg(long)]
    pub port: Option<u32>,
    #[arg(long)]
    pub disabled: bool,
}

pub async fn run(
    channel: Channel,
    command: Command,
) -> CommandResult {
    match command {
        Command::ListDomains { enabled_only } => {
            list_zones(channel, enabled_only).await
        }
        Command::GetDomain { domain } => get_zone(channel, domain).await,
        Command::AddDomain { domain, soa, disabled } => {
            add_zone(channel, domain, soa, disabled).await
        }
        Command::UpdateDomain { domain, rename, soa, enabled } => {
            update_zone(channel, domain, rename, soa, enabled).await
        }
        Command::RemoveDomain { domain, cascade } => {
            remove_zone(channel, domain, cascade).await
        }
        Command::ListRecords { domain, enabled_only } => {
            list_records(channel, domain, enabled_only).await
        }
        Command::AddRecord { domain, config } => {
            add_record(channel, domain, config).await
        }
        Command::UpdateRecord { domain, id, update } => {
            update_record(channel, domain, id, update).await
        }
        Command::RemoveRecord { id } => remove_record(channel, id).await,
        Command::Reload => reload_dns(channel).await,
    }
}

fn new_soa(soa: SoaArgs) -> Result<dns::SoaConfig, &'static str> {
    Ok(dns::SoaConfig {
        mname: soa.mname.ok_or("--mname is required for a new zone")?,
        rname: soa.rname.ok_or("--rname is required for a new zone")?,
        refresh: soa.refresh.ok_or("--refresh is required for a new zone")?,
        retry: soa.retry.ok_or("--retry is required for a new zone")?,
        expire: soa.expire.ok_or("--expire is required for a new zone")?,
        minimum: soa.minimum.ok_or("--minimum is required for a new zone")?,
    })
}

/// An `--enable` / `--disable` pair; neither keeps the current state.
#[derive(Debug, Default, clap::Args)]
pub struct EnabledArgs {
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,
    #[arg(long)]
    pub disable: bool,
}

impl EnabledArgs {
    fn apply(
        &self,
        current: bool,
    ) -> bool {
        if self.enable {
            true
        } else if self.disable {
            false
        } else {
            current
        }
    }
}

/// Record fields for `update-record`; `None` keeps what the record has.
#[derive(Debug, Default, clap::Args)]
pub struct RecordUpdateArgs {
    #[arg(long, help = "Owner name")]
    pub name: Option<String>,
    #[arg(
        long = "type",
        help = "Record type (A, AAAA, CNAME, MX, TXT, SRV, ...)"
    )]
    pub record_type: Option<String>,
    #[arg(long)]
    pub value: Option<String>,
    #[arg(long)]
    pub ttl: Option<u32>,
    #[arg(long, conflicts_with = "no_priority")]
    pub priority: Option<u32>,
    #[arg(long, help = "Drop the priority")]
    pub no_priority: bool,
    #[arg(long, conflicts_with = "no_weight")]
    pub weight: Option<u32>,
    #[arg(long, help = "Drop the weight")]
    pub no_weight: bool,
    #[arg(long, conflicts_with = "no_port")]
    pub port: Option<u32>,
    #[arg(long, help = "Drop the port")]
    pub no_port: bool,
    #[command(flatten)]
    pub enabled: EnabledArgs,
}

pub async fn list_zones(
    channel: Channel,
    enabled_only: bool,
) -> CommandResult {
    let zones = DnsManagerClient::new(channel)
        .list_zones(dns::ListZonesRequest { enabled_only })
        .await?
        .into_inner()
        .zones;
    if zones.is_empty() {
        println!("No zones found");
        return Ok(());
    }
    println!("Found {} zone(s):", zones.len());
    for zone in &zones {
        println!("  {}", zone_line(zone));
    }
    Ok(())
}

pub async fn get_zone(
    channel: Channel,
    domain: String,
) -> CommandResult {
    let zone = fetch_zone(&mut DnsManagerClient::new(channel), domain).await?;
    print_zone(&zone);
    Ok(())
}

pub async fn add_zone(
    channel: Channel,
    domain: String,
    soa: SoaArgs,
    disabled: bool,
) -> CommandResult {
    let config = dns::ZoneConfig {
        domain,
        soa: Some(new_soa(soa)?),
        enabled: !disabled,
    };
    let zone = DnsManagerClient::new(channel)
        .add_zone(dns::AddZoneRequest { config: Some(config) })
        .await?
        .into_inner()
        .zone
        .ok_or("DnsManager returned an empty zone response")?;
    println!("✅ Added zone");
    print_zone(&zone);
    Ok(())
}

pub async fn update_zone(
    channel: Channel,
    domain: String,
    rename: Option<String>,
    soa: SoaArgs,
    enabled: EnabledArgs,
) -> CommandResult {
    let mut client = DnsManagerClient::new(channel);
    let current = fetch_zone(&mut client, domain).await?;
    let config = merge_zone(
        current.config.ok_or("DnsManager returned a zone without config")?,
        rename,
        &soa,
        &enabled,
    )?;
    let zone = client
        .update_zone(dns::UpdateZoneRequest {
            id: current.id,
            config: Some(config),
        })
        .await?
        .into_inner()
        .zone
        .ok_or("DnsManager returned an empty zone response")?;
    println!("✅ Updated zone");
    print_zone(&zone);
    Ok(())
}

pub async fn remove_zone(
    channel: Channel,
    domain: String,
    cascade: bool,
) -> CommandResult {
    let mut client = DnsManagerClient::new(channel);
    let zone = fetch_zone(&mut client, domain).await?;
    let result = client
        .remove_zone(dns::RemoveZoneRequest { id: zone.id, cascade })
        .await?
        .into_inner();
    println!("✅ Removed zone '{}'", result.zone);
    Ok(())
}

pub async fn list_records(
    channel: Channel,
    zone: String,
    enabled_only: bool,
) -> CommandResult {
    let records = DnsManagerClient::new(channel)
        .list_records(dns::ListRecordsRequest {
            zone: zone.clone(),
            enabled_only,
        })
        .await?
        .into_inner()
        .records;
    if records.is_empty() {
        println!("No records found in zone '{zone}'");
        return Ok(());
    }
    println!("Found {} record(s) in zone '{zone}':", records.len());
    for record in &records {
        println!("  {}", record_line(record));
    }
    Ok(())
}

pub async fn add_record(
    channel: Channel,
    zone: String,
    args: RecordArgs,
) -> CommandResult {
    let config = dns::RecordConfig {
        name: args.name,
        r#type: args.record_type.to_ascii_uppercase(),
        value: args.value,
        ttl: args.ttl,
        priority: args.priority,
        weight: args.weight,
        port: args.port,
        enabled: !args.disabled,
    };
    let response = DnsManagerClient::new(channel)
        .add_record(dns::AddRecordRequest { zone, config: Some(config) })
        .await?
        .into_inner();
    print_record_response("Added", response)
}

pub async fn update_record(
    channel: Channel,
    zone: String,
    id: u64,
    update: RecordUpdateArgs,
) -> CommandResult {
    let mut client = DnsManagerClient::new(channel);
    let current = client
        .list_records(dns::ListRecordsRequest {
            zone: zone.clone(),
            enabled_only: false,
        })
        .await?
        .into_inner()
        .records
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("record {id} is not in zone '{zone}'"))?;
    let config = merge_record(
        current.config.ok_or("DnsManager returned a record without config")?,
        &update,
    );
    let response = client
        .update_record(dns::UpdateRecordRequest { id, config: Some(config) })
        .await?
        .into_inner();
    print_record_response("Updated", response)
}

pub async fn remove_record(
    channel: Channel,
    id: u64,
) -> CommandResult {
    let result = DnsManagerClient::new(channel)
        .remove_record(dns::RemoveRecordRequest { id })
        .await?
        .into_inner();
    match result.zone_serial {
        Some(serial) => println!(
            "✅ Removed record {id} from zone '{}' (serial {serial})",
            result.zone
        ),
        None => println!("✅ Removed record {id} from zone '{}'", result.zone),
    }
    Ok(())
}

pub async fn reload_dns(channel: Channel) -> CommandResult {
    let accepted = DnsManagerClient::new(channel)
        .reload_dns(dns::ReloadDnsRequest {})
        .await?
        .into_inner()
        .accepted;
    if accepted {
        // Acceptance only: the protocol does not claim fleet-wide completion.
        println!("✅ DNS reload accepted");
    } else {
        return Err("DNS reload was not accepted".into());
    }
    Ok(())
}

async fn fetch_zone(
    client: &mut DnsManagerClient<Channel>,
    domain: String,
) -> Result<dns::ZoneInfo, Box<dyn std::error::Error>> {
    client
        .get_zone(dns::GetZoneRequest { domain })
        .await?
        .into_inner()
        .zone
        .ok_or_else(|| "DnsManager returned an empty zone response".into())
}

fn merge_zone(
    mut config: dns::ZoneConfig,
    rename: Option<String>,
    soa: &SoaArgs,
    enabled: &EnabledArgs,
) -> Result<dns::ZoneConfig, &'static str> {
    let current =
        config.soa.as_mut().ok_or("DnsManager returned a zone without SOA")?;
    if let Some(mname) = &soa.mname {
        current.mname.clone_from(mname);
    }
    if let Some(rname) = &soa.rname {
        current.rname.clone_from(rname);
    }
    current.refresh = soa.refresh.unwrap_or(current.refresh);
    current.retry = soa.retry.unwrap_or(current.retry);
    current.expire = soa.expire.unwrap_or(current.expire);
    current.minimum = soa.minimum.unwrap_or(current.minimum);
    if let Some(domain) = rename {
        config.domain = domain;
    }
    config.enabled = enabled.apply(config.enabled);
    Ok(config)
}

fn merge_record(
    mut config: dns::RecordConfig,
    update: &RecordUpdateArgs,
) -> dns::RecordConfig {
    if let Some(name) = &update.name {
        config.name.clone_from(name);
    }
    if let Some(record_type) = &update.record_type {
        config.r#type = record_type.to_ascii_uppercase();
    }
    if let Some(value) = &update.value {
        config.value.clone_from(value);
    }
    config.ttl = update.ttl.unwrap_or(config.ttl);
    config.priority =
        optional(config.priority, update.priority, update.no_priority);
    config.weight = optional(config.weight, update.weight, update.no_weight);
    config.port = optional(config.port, update.port, update.no_port);
    config.enabled = update.enabled.apply(config.enabled);
    config
}

/// Presence is meaningful (SRV weight 0 is valid), so dropping a field is an
/// explicit `--no-*`, never a zero.
fn optional(
    current: Option<u32>,
    set: Option<u32>,
    clear: bool,
) -> Option<u32> {
    if clear { None } else { set.or(current) }
}

fn print_record_response(
    verb: &str,
    response: dns::RecordResponse,
) -> CommandResult {
    let record = response
        .record
        .ok_or("DnsManager returned an empty record response")?;
    println!(
        "✅ {verb} record in zone '{}' (serial {})",
        record.zone, response.zone_serial
    );
    println!("  {}", record_line(&record));
    Ok(())
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn zone_line(zone: &dns::ZoneInfo) -> String {
    match &zone.config {
        Some(config) => format!(
            "{} | id={} | serial={} | {}",
            config.domain,
            zone.id,
            zone.serial,
            enabled_label(config.enabled)
        ),
        None => {
            format!("<no config> | id={} | serial={}", zone.id, zone.serial)
        }
    }
}

fn print_zone(zone: &dns::ZoneInfo) {
    println!("{}", zone_line(zone));
    if let Some(soa) =
        zone.config.as_ref().and_then(|config| config.soa.as_ref())
    {
        println!("  SOA mname={} rname={}", soa.mname, soa.rname);
        println!(
            "      refresh={} retry={} expire={} minimum={}",
            soa.refresh, soa.retry, soa.expire, soa.minimum
        );
    }
}

fn record_line(record: &dns::RecordInfo) -> String {
    let Some(config) = &record.config else {
        return format!("<no config> | id={}", record.id);
    };
    let mut extras = Vec::new();
    if let Some(priority) = config.priority {
        extras.push(format!("priority={priority}"));
    }
    if let Some(weight) = config.weight {
        extras.push(format!("weight={weight}"));
    }
    if let Some(port) = config.port {
        extras.push(format!("port={port}"));
    }
    let extras = if extras.is_empty() {
        String::new()
    } else {
        format!(" {}", extras.join(" "))
    };
    format!(
        "{} {} {} {}{} | id={} | {}",
        config.name,
        config.ttl,
        config.r#type,
        config.value,
        extras,
        record.id,
        enabled_label(config.enabled)
    )
}

#[cfg(test)]
#[path = "../tests/dns.rs"]
mod tests;
