use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp};
use crate::proto::acme;
use crate::proto::acme::acme_manager_client::AcmeManagerClient;

pub async fn list_certificates(
    channel: Channel,
    active_only: bool,
    expiring_days: Option<i32>,
) -> CommandResult {
    let certificates = AcmeManagerClient::new(channel)
        .list_certificates(tonic::Request::new(acme::ListCertificatesRequest {
            active_only: Some(active_only),
            expiring_days,
        }))
        .await?
        .into_inner()
        .certificates;

    print_certificates(certificates, None);
    Ok(())
}

pub async fn list_certificates_by_server(
    channel: Channel,
    server_name: String,
) -> CommandResult {
    let certificates = AcmeManagerClient::new(channel)
        .list_certificates_by_server(tonic::Request::new(
            acme::ListCertificatesByServerRequest {
                server_name: server_name.clone(),
            },
        ))
        .await?
        .into_inner()
        .certificates;

    print_certificates(certificates, Some(&server_name));
    Ok(())
}

fn print_certificates(
    certificates: Vec<acme::CertificateResponse>,
    server_name: Option<&str>,
) {
    if certificates.is_empty() {
        match server_name {
            Some(server_name) => {
                println!("📋 No certificates found for server '{server_name}'")
            }
            None => println!("📋 No certificates found"),
        }
        return;
    }

    match server_name {
        Some(server_name) => println!(
            "📋 Found {} certificate(s) for server '{server_name}':\n",
            certificates.len()
        ),
        None => println!("📋 Found {} certificate(s):\n", certificates.len()),
    }
    for certificate in certificates {
        println!("{}", certificate_summary(&certificate));
        if !certificate.aliases.is_empty() {
            println!("   📎 Aliases: {}", certificate.aliases.join(", "));
        }
        println!();
    }
}

pub async fn list_certificate_inventory(
    channel: Channel,
    active_only: bool,
    expiring_days: Option<i32>,
) -> CommandResult {
    let inventory = AcmeManagerClient::new(channel)
        .list_certificate_inventory(tonic::Request::new(
            acme::ListCertificatesRequest {
                active_only: Some(active_only),
                expiring_days,
            },
        ))
        .await?
        .into_inner()
        .certificates;

    if inventory.is_empty() {
        println!("No certificates found");
        return Ok(());
    }

    println!("Certificate inventory: {} certificate(s)", inventory.len());
    for entry in inventory {
        println!();
        if let Some(certificate) = entry.certificate {
            println!("{}", certificate_summary(&certificate));
            if certificate.aliases.is_empty() {
                println!("  aliases: none");
            } else {
                for alias in certificate.aliases {
                    println!("  alias: {alias}");
                }
            }
        } else {
            println!("Certificate identity and state not reported");
        }

        if entry.servers.is_empty() {
            println!("  servers: none");
        } else {
            for server in entry.servers {
                println!(
                    "  server: {} | id={} | {}",
                    server.name,
                    server.id,
                    if server.enabled { "enabled" } else { "disabled" }
                );
            }
        }
    }
    Ok(())
}

pub async fn get_certificate(
    channel: Channel,
    hostname: String,
    fullchain_only: bool,
) -> CommandResult {
    let certificate = AcmeManagerClient::new(channel)
        .get_certificate(tonic::Request::new(acme::GetCertificateRequest {
            hostname,
        }))
        .await?
        .into_inner();

    if fullchain_only {
        print!("{}", certificate.fullchain_pem);
        return Ok(());
    }

    println!("📜 Certificate Details:");
    println!("   ID: {}", certificate.id);
    println!("   CN: {}", certificate.cname);
    println!("   Enabled: {}", certificate.enabled);
    println!("   Issued: {}", format_timestamp(certificate.issued_at));
    println!("   Expires: {}", format_timestamp(certificate.expires_at));
    println!("   Updated: {}", format_timestamp(certificate.updated_at));
    if !certificate.aliases.is_empty() {
        println!("   Aliases: {}", certificate.aliases.join(", "));
    }
    Ok(())
}

fn certificate_summary(certificate: &acme::CertificateResponse) -> String {
    let status = if certificate.enabled { "✅" } else { "❌" };
    format!(
        "{} ID: {} | CN: {} | Aliases: {} | Issued: {} | Expires: {} | Updated: {}",
        status,
        certificate.id,
        certificate.cname,
        certificate.aliases.len(),
        format_timestamp(certificate.issued_at),
        format_timestamp(certificate.expires_at),
        format_timestamp(certificate.updated_at),
    )
}

pub async fn add_certificate(
    channel: Channel,
    aliases: String,
) -> CommandResult {
    let aliases: Vec<String> =
        aliases.split(',').map(|alias| alias.trim().to_owned()).collect();
    let result = AcmeManagerClient::new(channel)
        .add_certificate(tonic::Request::new(acme::AddCertificateRequest {
            aliases,
        }))
        .await?
        .into_inner();

    if result.attached_existing {
        println!("✅ Existing certificate attached to this server!");
    } else {
        println!("✅ Certificate created successfully!");
    }
    println!("   ID: {}", result.id);
    println!("   Content length: {} bytes", result.content.len());
    Ok(())
}

pub async fn renew_certificate(
    channel: Channel,
    cn: String,
    renew_before: Option<i64>,
) -> CommandResult {
    let request = tonic::Request::new(acme::RenewCertificateRequest {
        cn: cn.clone(),
        renew_before,
    });

    match AcmeManagerClient::new(channel).renew_certificate(request).await {
        Ok(response) => {
            let result = response.into_inner();
            println!("✅ Certificate renewed successfully!");
            println!("   Content length: {} bytes", result.content.len());
            if let Some(expires) = result.expires_at {
                println!(
                    "   New expiration: {}",
                    format_timestamp(Some(expires))
                );
            }
        }
        Err(status) if status.code() == tonic::Code::FailedPrecondition => {
            println!("ℹ️ Renewal skipped");
            println!("   message: {}", status.message());
        }
        Err(status) => {
            println!("❌ gRPC call failed");
            println!("   code: {:?}", status.code());
            println!("   message: {}", status.message());
            if !status.details().is_empty() {
                println!("   details: {:?}", status.details());
            }
            if !status.metadata().is_empty() {
                println!("   metadata: {:?}", status.metadata());
            }
        }
    }
    Ok(())
}

pub async fn activate_certificate(
    channel: Channel,
    cn: String,
) -> CommandResult {
    let result = AcmeManagerClient::new(channel)
        .activate_certificate(tonic::Request::new(
            acme::UpdateCertificateRequest { cn: cn.clone() },
        ))
        .await?
        .into_inner();
    if result.success {
        println!("✅ {}", result.message.unwrap_or_default());
    } else {
        println!("❌ Failed to activate certificate");
    }
    Ok(())
}

pub async fn disable_certificate(
    channel: Channel,
    cn: String,
) -> CommandResult {
    let result = AcmeManagerClient::new(channel)
        .disable_certificate(tonic::Request::new(
            acme::UpdateCertificateRequest { cn: cn.clone() },
        ))
        .await?
        .into_inner();
    if result.success {
        println!("✅ {}", result.message.unwrap_or_default());
    } else {
        println!("❌ Failed to disable certificate");
    }
    Ok(())
}

pub async fn remove_certificate(
    channel: Channel,
    cn: String,
) -> CommandResult {
    let result = AcmeManagerClient::new(channel)
        .remove_certificate(tonic::Request::new(
            acme::UpdateCertificateRequest { cn: cn.clone() },
        ))
        .await?
        .into_inner();
    if result.success {
        println!("✅ {}", result.message.unwrap_or_default());
    } else {
        println!("❌ Failed to remove certificate");
    }
    Ok(())
}

pub async fn reload_resolver(channel: Channel) -> CommandResult {
    let result = AcmeManagerClient::new(channel)
        .reload_resolver(tonic::Request::new(acme::ReloadRequest {}))
        .await?
        .into_inner();
    crate::common::print_operation(
        result.success,
        result.message,
        "Failed to reload resolver",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::certificate_summary;
    use crate::proto::acme::CertificateResponse;

    #[test]
    fn certificate_summary_keeps_missing_update_date_explicit() {
        let summary = certificate_summary(&CertificateResponse {
            id: 7,
            cname: "example.test".to_owned(),
            aliases: vec!["www.example.test".to_owned()],
            issued_at: None,
            expires_at: None,
            enabled: false,
            updated_at: None,
            fullchain_pem: String::new(),
        });

        assert!(
            summary.starts_with("❌ ID: 7 | CN: example.test | Aliases: 1")
        );
        assert!(summary.contains("Updated: N/A"));
    }
}
