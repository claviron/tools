//! Operator commands for registrar-held domains (`claviron.domains`).
//!
//! The store view (`list`, `get`) reads what Claviron has recorded; the
//! registrar view (`info`, `nameservers`, `contacts`) is a live call to the
//! registrar named on the command line, the key it has in `domains.yaml`.

use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp, split_csv};
use crate::proto::domains;
use crate::proto::domains::domain_manager_client::DomainManagerClient;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Registrars this server knows and whether each is configured
    ListRegistrars,
    /// Domains in the store
    List {
        #[arg(long, help = "Only this registrar")]
        registrar: Option<String>,
        #[arg(long, help = "Only this status (pending, active, expired, ...)")]
        status: Option<String>,
        #[arg(long, help = "Only domains expiring within this many days")]
        expiring_within: Option<u32>,
    },
    /// One domain as the store records it
    Get { name: String },
    /// Record a domain the registrar already holds, from the registrar's data
    Add { registrar: String, name: String },
    /// Re-read a stored domain from its registrar and replace the store row
    Refresh { name: String },
    /// Sync registrar-hosted DNS records of one domain into the store
    SyncRecords { name: String },
    /// The registrar's live record for a domain
    Info { registrar: String, name: String },
    /// Contacts the registrar holds for a domain
    Contacts { registrar: String, name: String },
    /// Renew at the registrar the store row names
    Renew {
        name: String,
        #[arg(long, default_value_t = 1)]
        years: u32,
    },
    /// Register a new domain from a stored template
    Register {
        registrar: String,
        name: String,
        #[arg(long)]
        template: String,
        #[arg(long, default_value_t = 1)]
        years: u32,
    },
    /// The domain's delegation set at the registrar
    Nameservers { registrar: String, name: String },
    /// Replace the delegation set (comma-separated hostnames)
    SetNameservers { registrar: String, name: String, nameservers: String },
    /// Delegate to the nameservers registered for Claviron's own DNS
    UseClavironDns { registrar: String, name: String },
    /// Registration templates in the store
    Templates,
    /// One template's contacts and nameservers
    TemplateShow { name: String },
    /// Insert or replace a template from a YAML or JSON file
    /// (contacts: {registrant: {...}, admin: ...}, nameservers: [...])
    TemplatePut {
        name: String,
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Remove a template no registrar or domain still references
    TemplateRemove { name: String },
    /// Pull the registrar's domain list into the store
    Sync {
        #[arg(
            long,
            help = "Only this registrar; default is every configured one"
        )]
        registrar: Option<String>,
    },
}

pub async fn run(
    channel: Channel,
    command: Command,
) -> CommandResult {
    let mut client = DomainManagerClient::new(channel);
    match command {
        Command::ListRegistrars => {
            let response = client
                .list_registrars(domains::ListRegistrarsRequest {})
                .await?
                .into_inner();
            if response.registrars.is_empty() {
                println!("No registrar is built into this server");
                return Ok(());
            }
            for registrar in response.registrars {
                let mut features = Vec::new();
                if registrar.supports_registration {
                    features.push("registration");
                }
                if registrar.supports_dns {
                    features.push("dns");
                }
                if registrar.supports_glue {
                    features.push("glue");
                }
                println!(
                    "  {:<12} {:<14} {}",
                    registrar.name,
                    if registrar.configured {
                        "configured"
                    } else {
                        "not configured"
                    },
                    features.join(", ")
                );
            }
        }
        Command::List { registrar, status, expiring_within } => {
            let status = match status.as_deref() {
                None => domains::DomainStatus::Unspecified,
                Some(word) => parse_status(word)?,
            };
            let response = client
                .list_domains(domains::ListDomainsRequest {
                    registrar: registrar.unwrap_or_default(),
                    status: status.into(),
                    expiring_within_days: expiring_within.unwrap_or_default(),
                })
                .await?
                .into_inner();
            if response.domains.is_empty() {
                println!("No domains found");
                return Ok(());
            }
            println!("Found {} domain(s):", response.domains.len());
            for domain in response.domains {
                println!("  {}", domain_line(&domain));
            }
        }
        Command::Get { name } => {
            let response = client
                .get_domain(domains::GetDomainRequest { name })
                .await?
                .into_inner();
            print_domain(response.domain.as_ref());
        }
        Command::Add { registrar, name } => {
            let response = client
                .add_domain(domains::AddDomainRequest { registrar, name })
                .await?
                .into_inner();
            println!("✅ Domain recorded");
            print_domain(response.domain.as_ref());
        }
        Command::Refresh { name } => {
            let response = client
                .refresh_domain(domains::RefreshDomainRequest { name })
                .await?
                .into_inner();
            println!("✅ Domain refreshed from its registrar");
            print_domain(response.domain.as_ref());
        }
        Command::SyncRecords { name } => {
            let response = client
                .sync_dns_records(domains::SyncDnsRecordsRequest { name })
                .await?
                .into_inner();
            println!(
                "found={} updated={} failed={}",
                response.found,
                response.updated.len(),
                response.failed.len()
            );
            for failure in response.failed {
                println!("  ❌ {}: {}", failure.domain, failure.message);
            }
        }
        Command::Info { registrar, name } => {
            let response = client
                .get_domain_info(domains::GetDomainInfoRequest {
                    registrar,
                    name,
                })
                .await?
                .into_inner();
            let Some(info) = response.info else {
                println!("Registrar returned no information");
                return Ok(());
            };
            println!("Name:        {}", info.name);
            println!("Registrar:   {}", info.registrar);
            println!("Reference:   {}", info.registrar_ref);
            println!("Status:      {}", info.registrar_status);
            println!("Registered:  {}", format_timestamp(info.registered_at));
            println!("Expires:     {}", format_timestamp(info.expires_at));
            println!("Auto-renew:  {}", optional_flag(info.auto_renew));
            println!("Privacy:     {}", optional_flag(info.whois_privacy));
            println!("Nameservers: {}", list_or_none(&info.nameservers));
        }
        Command::Contacts { registrar, name } => {
            let response = client
                .get_contacts(domains::GetContactsRequest { registrar, name })
                .await?
                .into_inner();
            let Some(contacts) = response.contacts else {
                println!("Registrar returned no contacts");
                return Ok(());
            };
            print_contact("Registrant", contacts.registrant.as_ref());
            print_contact("Admin", contacts.admin.as_ref());
            print_contact("Tech", contacts.tech.as_ref());
            print_contact("Billing", contacts.billing.as_ref());
        }
        Command::Renew { name, years } => {
            let response = client
                .renew_domain(domains::RenewDomainRequest { name, years })
                .await?
                .into_inner();
            println!("✅ Renewed");
            print_domain(response.domain.as_ref());
        }
        Command::Register { registrar, name, template, years } => {
            let response = client
                .register_domain(domains::RegisterDomainRequest {
                    registrar,
                    name,
                    template,
                    years,
                })
                .await?
                .into_inner();
            println!("✅ Registered");
            print_domain(response.domain.as_ref());
        }
        Command::Nameservers { registrar, name } => {
            let response = client
                .get_nameservers(domains::GetNameserversRequest {
                    registrar,
                    name,
                })
                .await?
                .into_inner();
            print_nameservers(&response);
        }
        Command::SetNameservers { registrar, name, nameservers } => {
            let response = client
                .set_nameservers(domains::SetNameserversRequest {
                    registrar,
                    name,
                    nameservers: split_csv(&nameservers),
                })
                .await?
                .into_inner();
            println!("✅ Delegation replaced");
            print_nameservers(&response);
        }
        Command::UseClavironDns { registrar, name } => {
            let response = client
                .use_claviron_nameservers(
                    domains::UseClavironNameserversRequest { registrar, name },
                )
                .await?
                .into_inner();
            println!("✅ Delegated to Claviron DNS");
            print_nameservers(&response);
        }
        Command::Templates => {
            let response = client
                .list_templates(domains::ListTemplatesRequest {})
                .await?
                .into_inner();
            if response.templates.is_empty() {
                println!("No templates");
                return Ok(());
            }
            for template in response.templates {
                let registrant = template
                    .contacts
                    .as_ref()
                    .and_then(|c| c.registrant.as_ref())
                    .map(|c| {
                        format!(
                            "{} {} <{}>",
                            c.first_name, c.last_name, c.email
                        )
                    })
                    .unwrap_or_default();
                println!(
                    "  {:<24} {:<40} ns={}",
                    template.name,
                    registrant,
                    list_or_none(&template.nameservers)
                );
            }
        }
        Command::TemplateShow { name } => {
            let response = client
                .list_templates(domains::ListTemplatesRequest {})
                .await?
                .into_inner();
            let Some(template) =
                response.templates.into_iter().find(|t| t.name == name)
            else {
                return Err(format!("template `{name}` not found").into());
            };
            print_template(&template);
        }
        Command::TemplatePut { name, file } => {
            let text = std::fs::read_to_string(&file)?;
            let input: TemplateFile = serde_yaml::from_str(&text)?;
            let response = client
                .put_template(domains::PutTemplateRequest {
                    name,
                    contacts: Some(input.contacts.into()),
                    nameservers: input.nameservers.unwrap_or_default(),
                })
                .await?
                .into_inner();
            println!("✅ Template stored");
            if let Some(template) = response.template.as_ref() {
                print_template(template);
            }
        }
        Command::TemplateRemove { name } => {
            let response = client
                .remove_template(domains::RemoveTemplateRequest { name })
                .await?
                .into_inner();
            println!(
                "{}",
                if response.removed {
                    "✅ Template removed"
                } else {
                    "No such template"
                }
            );
        }
        Command::Sync { registrar } => {
            let response = client
                .sync_domains(domains::SyncDomainsRequest {
                    registrar: registrar.unwrap_or_default(),
                })
                .await?
                .into_inner();
            println!(
                "found={} created={} updated={} skipped={} failed={}",
                response.found,
                response.created.len(),
                response.updated.len(),
                response.skipped,
                response.failed.len()
            );
            for failure in response.failed {
                println!("  ❌ {}: {}", failure.domain, failure.message);
            }
        }
    }
    Ok(())
}

fn parse_status(word: &str) -> Result<domains::DomainStatus, String> {
    Ok(match word.to_ascii_lowercase().as_str() {
        "pending" => domains::DomainStatus::Pending,
        "active" => domains::DomainStatus::Active,
        "expired" => domains::DomainStatus::Expired,
        "suspended" => domains::DomainStatus::Suspended,
        "transferring" => domains::DomainStatus::Transferring,
        "failed" => domains::DomainStatus::Failed,
        other => return Err(format!("unknown domain status `{other}`")),
    })
}

fn status_word(status: domains::DomainStatus) -> &'static str {
    match status {
        domains::DomainStatus::Unspecified => "unspecified",
        domains::DomainStatus::Pending => "pending",
        domains::DomainStatus::Active => "active",
        domains::DomainStatus::Expired => "expired",
        domains::DomainStatus::Suspended => "suspended",
        domains::DomainStatus::Transferring => "transferring",
        domains::DomainStatus::Failed => "failed",
    }
}

fn domain_line(domain: &domains::Domain) -> String {
    format!(
        "{:<32} {:<10} {:<12} dns={:<10} expires {}",
        domain.name,
        domain.registrar,
        status_word(domain.status()),
        if domain.dns_provider.is_empty() {
            "-"
        } else {
            domain.dns_provider.as_str()
        },
        format_timestamp(domain.expires_at)
    )
}

fn print_domain(domain: Option<&domains::Domain>) {
    let Some(domain) = domain else {
        println!("No domain in the response");
        return;
    };
    println!("Name:          {}", domain.name);
    println!("Registrar:     {}", domain.registrar);
    println!("Reference:     {}", domain.registrar_ref);
    println!(
        "DNS provider:  {}",
        if domain.dns_provider.is_empty() { "-" } else { &domain.dns_provider }
    );
    println!("Status:        {}", status_word(domain.status()));
    println!("Registered:    {}", format_timestamp(domain.registered_at));
    println!("Expires:       {}", format_timestamp(domain.expires_at));
    println!("Auto-renew:    {}", domain.auto_renew);
    println!("Renew before:  {} day(s)", domain.renew_before_days);
    println!("Privacy:       {}", domain.whois_privacy);
    println!("Updated:       {}", format_timestamp(domain.updated_at));
}

fn print_nameservers(response: &domains::NameserversResponse) {
    println!("{}:", response.name);
    if response.nameservers.is_empty() {
        println!("  (no nameservers reported)");
    }
    for server in &response.nameservers {
        println!("  {server}");
    }
}

fn print_contact(
    role: &str,
    contact: Option<&domains::Contact>,
) {
    let Some(contact) = contact else {
        return;
    };
    println!("{role}:");
    println!("  {} {}", contact.first_name, contact.last_name);
    println!("  {}", contact.address1);
    if !contact.address2.is_empty() {
        println!("  {}", contact.address2);
    }
    println!(
        "  {} {} {} {}",
        contact.postal_code,
        contact.city,
        contact.state_province,
        contact.country
    );
    println!("  {}  {}", contact.phone, contact.email);
}

fn optional_flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not reported",
    }
}

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "not reported".to_owned()
    } else {
        values.join(", ")
    }
}

/// A template file: the same shape the store holds, YAML or JSON.
#[derive(serde::Deserialize)]
struct TemplateFile {
    contacts: ContactsFile,
    #[serde(default)]
    nameservers: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct ContactsFile {
    registrant: ContactFile,
    #[serde(default)]
    admin: Option<ContactFile>,
    #[serde(default)]
    tech: Option<ContactFile>,
    #[serde(default)]
    billing: Option<ContactFile>,
}

#[derive(serde::Deserialize)]
struct ContactFile {
    first_name: String,
    last_name: String,
    address1: String,
    #[serde(default)]
    address2: Option<String>,
    city: String,
    state_province: String,
    postal_code: String,
    country: String,
    phone: String,
    email: String,
}

impl From<ContactsFile> for domains::Contacts {
    fn from(value: ContactsFile) -> Self {
        Self {
            registrant: Some(value.registrant.into()),
            admin: value.admin.map(Into::into),
            tech: value.tech.map(Into::into),
            billing: value.billing.map(Into::into),
        }
    }
}

impl From<ContactFile> for domains::Contact {
    fn from(value: ContactFile) -> Self {
        Self {
            first_name: value.first_name,
            last_name: value.last_name,
            address1: value.address1,
            address2: value.address2.unwrap_or_default(),
            city: value.city,
            state_province: value.state_province,
            postal_code: value.postal_code,
            country: value.country,
            phone: value.phone,
            email: value.email,
        }
    }
}

fn print_template(template: &domains::DomainTemplate) {
    println!("Template:    {}", template.name);
    println!("Updated:     {}", format_timestamp(template.updated_at));
    println!("Nameservers: {}", list_or_none(&template.nameservers));
    if let Some(contacts) = template.contacts.as_ref() {
        print_contact("Registrant", contacts.registrant.as_ref());
        print_contact("Admin", contacts.admin.as_ref());
        print_contact("Tech", contacts.tech.as_ref());
        print_contact("Billing", contacts.billing.as_ref());
    }
}
