use tonic::transport::Channel;

use crate::common::{CommandResult, print_operation, split_csv};
use crate::proto::aliases;
use crate::proto::aliases::alias_manager_client::AliasManagerClient;

pub async fn add_alias(
    channel: Channel,
    vhost_name: String,
    hostname: String,
) -> CommandResult {
    let response = AliasManagerClient::new(channel)
        .add_alias(tonic::Request::new(aliases::AddAliasRequest {
            vhost_name,
            hostname,
        }))
        .await?
        .into_inner();
    let alias = response
        .alias
        .ok_or("AliasManager returned an empty alias response")?;
    println!(
        "✅ Added alias '{}' to vhost '{}' (ID: {})",
        alias.hostname, response.vhost_name, alias.id
    );
    Ok(())
}

pub async fn remove_alias(
    channel: Channel,
    hostname: String,
) -> CommandResult {
    let result = AliasManagerClient::new(channel)
        .remove_alias(tonic::Request::new(aliases::RemoveAliasRequest {
            hostname,
        }))
        .await?
        .into_inner();
    print_operation(result.success, result.message, "Failed to remove alias");
    Ok(())
}

pub async fn replace_alias(
    channel: Channel,
    hostname: String,
    vhost_name: String,
) -> CommandResult {
    let result = AliasManagerClient::new(channel)
        .replace_alias(tonic::Request::new(aliases::ReplaceAliasRequest {
            hostname,
            vhost_name,
        }))
        .await?
        .into_inner();
    print_operation(result.success, result.message, "Failed to replace alias");
    Ok(())
}

pub async fn list_aliases(
    channel: Channel,
    vhost_name: Option<String>,
    enabled_only: bool,
) -> CommandResult {
    let aliases = AliasManagerClient::new(channel)
        .list_aliases(tonic::Request::new(aliases::ListAliasesRequest {
            vhost_name,
            enabled_only: Some(enabled_only),
        }))
        .await?
        .into_inner()
        .aliases;
    if aliases.is_empty() {
        println!("No aliases found");
        return Ok(());
    }
    println!("Found {} alias(es):", aliases.len());
    for response in aliases {
        if let Some(alias) = response.alias {
            println!(
                "  {} -> {}{} | {}{}",
                alias.hostname,
                response.vhost_name,
                id_label(alias.id),
                alias_enabled_label(alias.enabled),
                read_only_label(
                    alias.read_only,
                    alias.read_only_reason.as_deref()
                )
            );
        }
    }
    Ok(())
}

pub async fn list_hosting(
    channel: Channel,
    enabled_vhosts_only: bool,
) -> CommandResult {
    let inventory = AliasManagerClient::new(channel)
        .list_hosting(tonic::Request::new(aliases::ListHostingRequest {
            enabled_vhosts_only: Some(enabled_vhosts_only),
        }))
        .await?
        .into_inner()
        .vhosts;
    if inventory.is_empty() {
        println!("No vhosts found");
        return Ok(());
    }

    println!("Hosting: {} vhost(s)", inventory.len());
    for hosted in inventory {
        let vhost = hosted
            .vhost
            .ok_or("AliasManager returned a hosting entry without a vhost")?;
        println!(
            "\n{}{} | {}{}",
            vhost.name,
            id_label(vhost.id),
            if vhost.enabled { "enabled" } else { "disabled" },
            read_only_label(vhost.read_only, vhost.read_only_reason.as_deref())
        );
        if hosted.aliases.is_empty() {
            println!("  aliases: none");
        } else {
            for alias in hosted.aliases {
                println!(
                    "  alias: {}{} | {}{}",
                    alias.hostname,
                    id_label(alias.id),
                    alias_enabled_label(alias.enabled),
                    read_only_label(
                        alias.read_only,
                        alias.read_only_reason.as_deref()
                    )
                );
            }
        }
        if hosted.servers.is_empty() {
            println!("  servers: none");
        } else {
            for server in hosted.servers {
                println!(
                    "  server: {}{} | {}",
                    server.name,
                    id_label(server.id),
                    if server.enabled { "enabled" } else { "disabled" }
                );
            }
        }
    }
    Ok(())
}

pub async fn reload_vhosts(channel: Channel) -> CommandResult {
    let result = AliasManagerClient::new(channel)
        .reload_vhosts(tonic::Request::new(aliases::ReloadRequest {}))
        .await?
        .into_inner();
    print_operation(result.success, result.message, "Failed to reload vhosts");
    Ok(())
}

pub async fn add_vhost_to_servers(
    channel: Channel,
    vhost_name: String,
    server_names: String,
) -> CommandResult {
    let result = AliasManagerClient::new(channel)
        .add_vhost_to_servers(tonic::Request::new(
            aliases::AddVhostToServersRequest {
                vhost_name,
                server_names: split_csv(&server_names),
            },
        ))
        .await?
        .into_inner();
    print_operation(
        result.success,
        result.message,
        "Failed to add vhost to servers",
    );
    Ok(())
}

pub async fn remove_vhost_from_servers(
    channel: Channel,
    vhost_name: String,
    server_names: String,
) -> CommandResult {
    let result = AliasManagerClient::new(channel)
        .remove_vhost_from_servers(tonic::Request::new(
            aliases::RemoveVhostFromServersRequest {
                vhost_name,
                server_names: split_csv(&server_names),
            },
        ))
        .await?
        .into_inner();
    print_operation(
        result.success,
        result.message,
        "Failed to remove vhost from servers",
    );
    Ok(())
}

pub async fn list_vhosts_by_server(
    channel: Channel,
    server_name: String,
) -> CommandResult {
    let vhosts = AliasManagerClient::new(channel)
        .list_vhosts_by_server(tonic::Request::new(
            aliases::ListVhostsByServerRequest { server_name },
        ))
        .await?
        .into_inner()
        .vhosts;
    if vhosts.is_empty() {
        println!("No vhosts found");
        return Ok(());
    }
    for vhost in vhosts {
        println!(
            "{} | id={} | {}",
            vhost.name,
            vhost.id,
            if vhost.enabled { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}

pub async fn list_servers_by_vhost(
    channel: Channel,
    vhost_name: String,
) -> CommandResult {
    let servers = AliasManagerClient::new(channel)
        .list_servers_by_vhost(tonic::Request::new(
            aliases::ListServersByVhostRequest { vhost_name },
        ))
        .await?
        .into_inner()
        .servers;
    if servers.is_empty() {
        println!("No servers found");
        return Ok(());
    }
    for server in servers {
        println!(
            "{} | id={} | {}",
            server.name,
            server.id,
            if server.enabled { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}

/// Runtime lists carry no persistence IDs (they arrive as zero); show an ID
/// only where the server assigned one.
fn id_label(id: i32) -> String {
    if id == 0 { String::new() } else { format!(" | id={id}") }
}

/// Static entries cannot be changed through management; say so, and why.
fn read_only_label(
    read_only: bool,
    reason: Option<&str>,
) -> String {
    match (read_only, reason) {
        (false, _) => String::new(),
        (true, Some(reason)) => format!(" | read-only: {reason}"),
        (true, None) => " | read-only".to_owned(),
    }
}

fn alias_enabled_label(enabled: Option<bool>) -> &'static str {
    match enabled {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "state not reported",
    }
}

#[cfg(test)]
mod tests {
    use super::{alias_enabled_label, id_label, read_only_label};

    #[test]
    fn runtime_zero_ids_are_not_presented() {
        assert_eq!(id_label(0), "");
        assert_eq!(id_label(7), " | id=7");
    }

    #[test]
    fn read_only_entries_say_why() {
        assert_eq!(read_only_label(false, Some("ignored")), "");
        assert_eq!(read_only_label(true, None), " | read-only");
        assert_eq!(
            read_only_label(true, Some("declared in vhost.yaml")),
            " | read-only: declared in vhost.yaml"
        );
    }

    #[test]
    fn absent_alias_state_is_not_presented_as_enabled() {
        assert_eq!(alias_enabled_label(None), "state not reported");
        assert_eq!(alias_enabled_label(Some(true)), "enabled");
        assert_eq!(alias_enabled_label(Some(false)), "disabled");
    }
}
