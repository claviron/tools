use std::collections::HashSet;
use std::time::Duration;

use tonic::transport::Channel;

use crate::client::{ClientConfig, create_channel};
use crate::common::{CommandResult, format_timestamp};
use crate::proto::services;
use crate::proto::services::service_manager_client::ServiceManagerClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub async fn list(channel: Channel) -> CommandResult {
    let snapshot = fetch(&mut ServiceManagerClient::new(channel)).await?;
    println!(
        "Services: {} | members: {}/{} | captured: {}",
        snapshot.services.len(),
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    for service in snapshot.services {
        print_service(service);
    }
    Ok(())
}

pub async fn status(
    channel: Channel,
    name: String,
) -> CommandResult {
    let snapshot = ServiceManagerClient::new(channel)
        .get_service_status(tonic::Request::new(
            services::ServiceStatusRequest { name },
        ))
        .await?
        .into_inner();
    let service = snapshot
        .service
        .ok_or("service status response did not contain a service")?;

    println!(
        "Service: {} | members: {}/{} | captured: {}",
        service.name,
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    print_service(service);
    Ok(())
}

fn print_service(service: services::ServiceStatus) {
    println!("\n{}", service.name);
    if service.placements.is_empty() {
        println!("  no fresh placement report");
        return;
    }
    for placement in service.placements {
        let member = placement
            .member
            .map(|member| {
                format!("{}:{}", member_role(member.role), member.node_id)
            })
            .unwrap_or_else(|| "unknown-member".to_owned());
        let reloadable =
            if placement.reloadable { "reloadable" } else { "not-reloadable" };
        let execution = match placement.active_executions {
            None => "not-reported",
            Some(n) if n > 0 => {
                if placement.lifecycle
                    == services::ServiceLifecycle::Stopping as i32
                {
                    "draining"
                } else {
                    "executing"
                }
            }
            Some(_)
                if placement.lifecycle
                    == services::ServiceLifecycle::Running as i32 =>
            {
                if matches!(
                    services::ServiceDispatch::try_from(placement.dispatch),
                    Ok(services::ServiceDispatch::One
                        | services::ServiceDispatch::Pool)
                ) {
                    "waiting"
                } else {
                    "starting"
                }
            }
            Some(_) => "not-executing",
        };
        let dispatch =
            match services::ServiceDispatch::try_from(placement.dispatch) {
                Ok(services::ServiceDispatch::Each) => "each",
                Ok(services::ServiceDispatch::One) => "one",
                Ok(services::ServiceDispatch::Pool) => "pool",
                _ => "not-reported",
            };
        println!(
            "  {} | controller={} | execution={} | active={} | dispatch={} | {} | {} | generation={} | reloads={} | observed={}",
            member,
            lifecycle(placement.lifecycle),
            execution,
            placement
                .active_executions
                .map(|count| count.to_string())
                .unwrap_or_else(|| "not-reported".to_owned()),
            dispatch,
            start_policy(placement.start_policy),
            reloadable,
            placement.generation,
            placement.reload_revision,
            format_timestamp(placement.observed_at)
        );
        print_error(
            "last error",
            placement.last_error,
            placement.last_error_truncated,
        );
        print_error(
            "last reload error",
            placement.last_reload_error,
            placement.last_reload_error_truncated,
        );
    }
}

pub async fn start(
    channel: Channel,
    config: &ClientConfig,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, config, name, timeout_secs, Action::Start).await
}

pub async fn stop(
    channel: Channel,
    config: &ClientConfig,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, config, name, timeout_secs, Action::Stop).await
}

pub async fn reload(
    channel: Channel,
    config: &ClientConfig,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, config, name, timeout_secs, Action::Reload).await
}

pub async fn restart(
    channel: Channel,
    config: &ClientConfig,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, config, name, timeout_secs, Action::Restart).await
}

async fn command(
    channel: Channel,
    config: &ClientConfig,
    name: String,
    timeout_secs: u64,
    action: Action,
) -> CommandResult {
    let mut client = ServiceManagerClient::new(channel);
    let baseline = fetch_status(&mut client, &name).await?;
    let targets = command_targets(&baseline, &name, action)?;
    let request = tonic::Request::new(services::ServiceCommandRequest { name });
    let response = match action {
        Action::Start => client.start_service(request).await?,
        Action::Stop => client.stop_service(request).await?,
        Action::Reload => client.reload_service(request).await?,
        Action::Restart => client.restart_service(request).await?,
    }
    .into_inner();
    if !response.accepted {
        return Err(format!(
            "service '{}' was not accepted for {}",
            response.name,
            action.label()
        )
        .into());
    }
    println!(
        "✅ Service '{}' accepted for {} (completion is asynchronous)",
        response.name,
        action.label()
    );
    wait_for(
        config,
        &response.name,
        action,
        &targets,
        Duration::from_secs(timeout_secs),
    )
    .await
}

#[derive(Clone, Copy)]
enum Action {
    Start,
    Stop,
    Reload,
    Restart,
}

impl Action {
    const fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Reload => "reload",
            Self::Restart => "restart",
        }
    }

    const fn completed_label(self) -> &'static str {
        match self {
            Self::Start => "started",
            Self::Stop => "stopped",
            Self::Reload => "reloaded",
            Self::Restart => "restarted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServiceTarget {
    node_id: u32,
    role: i32,
    lifecycle: services::ServiceLifecycle,
    generation: u64,
    reload_revision: u64,
}

fn command_targets(
    snapshot: &services::ServiceStatusResponse,
    name: &str,
    action: Action,
) -> Result<Vec<ServiceTarget>, Box<dyn std::error::Error>> {
    if snapshot.members_reporting == 0
        || snapshot.members_reporting != snapshot.members_expected
    {
        return Err(format!(
            "service fleet snapshot is incomplete: members {}/{}",
            snapshot.members_reporting, snapshot.members_expected
        )
        .into());
    }
    let service = snapshot.service.as_ref().ok_or_else(|| {
        format!("service '{name}' is absent from the fleet snapshot")
    })?;
    if service.placements.is_empty() {
        return Err(format!(
            "service '{name}' has no registered fleet placement"
        )
        .into());
    }

    let mut members = HashSet::with_capacity(service.placements.len());
    service
        .placements
        .iter()
        .map(|placement| {
            let member = placement.member.as_ref().ok_or_else(|| {
                format!("service '{name}' snapshot contains a placement without a fleet member")
            })?;
            if !members.insert((member.node_id, member.role)) {
                return Err(format!(
                    "service '{name}' snapshot contains duplicate member {}:{}",
                    member_role(member.role),
                    member.node_id
                )
                .into());
            }
            let lifecycle = services::ServiceLifecycle::try_from(placement.lifecycle)
                .unwrap_or(services::ServiceLifecycle::Unspecified);
            if lifecycle == services::ServiceLifecycle::Unspecified {
                return Err(
                    format!("service '{name}' snapshot contains an unspecified lifecycle").into(),
                );
            }
            if lifecycle == services::ServiceLifecycle::Stopping {
                return Err(format!(
                    "service '{name}' is stopping on {}:{}",
                    member_role(member.role),
                    member.node_id
                )
                .into());
            }
            if matches!(action, Action::Reload) && !placement.reloadable {
                return Err(format!(
                    "service '{name}' is not reloadable on {}:{}",
                    member_role(member.role),
                    member.node_id
                )
                .into());
            }
            Ok(ServiceTarget {
                node_id: member.node_id,
                role: member.role,
                lifecycle,
                generation: placement.generation,
                reload_revision: placement.reload_revision,
            })
        })
        .collect()
}

async fn wait_for(
    config: &ClientConfig,
    name: &str,
    action: Action,
    targets: &[ServiceTarget],
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = match fetch_status_fresh(config, name).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_observation = error;
                    continue;
                }
            };
            match observe(&snapshot, name, action, targets) {
                PollStatus::Confirmed => {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                PollStatus::Pending(observation) => {
                    last_observation = observation
                }
                PollStatus::Failed(error) => return Err(error.into()),
            }
        }
    })
    .await;

    match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for service '{name}' to be {}; last observation: {last_observation}",
                timeout.as_secs(),
                action.completed_label()
            )
            .into());
        }
    }
    println!(
        "✅ Service '{name}' is {} on every target fleet member",
        action.completed_label()
    );
    Ok(())
}

enum PollStatus {
    Confirmed,
    Pending(String),
    Failed(String),
}

fn observe(
    snapshot: &services::ServiceStatusResponse,
    name: &str,
    action: Action,
    targets: &[ServiceTarget],
) -> PollStatus {
    let Some(service) = snapshot.service.as_ref() else {
        return PollStatus::Pending(format!(
            "service '{name}' is absent from the latest snapshot"
        ));
    };
    let mut confirmed = 0usize;
    for target in targets {
        let Some(placement) = service.placements.iter().find(|placement| {
            placement.member.as_ref().is_some_and(|member| {
                member.node_id == target.node_id && member.role == target.role
            })
        }) else {
            continue;
        };
        let lifecycle =
            services::ServiceLifecycle::try_from(placement.lifecycle)
                .unwrap_or(services::ServiceLifecycle::Unspecified);
        let member = format!("{}:{}", member_role(target.role), target.node_id);
        let complete = match action {
            Action::Start => {
                if lifecycle == services::ServiceLifecycle::Failed {
                    return PollStatus::Failed(format!(
                        "service '{name}' failed to start on {member}: {}",
                        placement
                            .last_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    ));
                }
                lifecycle == services::ServiceLifecycle::Running
                    && (target.lifecycle == services::ServiceLifecycle::Running
                        || placement.generation > target.generation)
            }
            Action::Stop => matches!(
                lifecycle,
                services::ServiceLifecycle::Stopped
                    | services::ServiceLifecycle::Failed
            ),
            Action::Reload => {
                if placement.reload_revision > target.reload_revision {
                    if let Some(error) = &placement.last_reload_error {
                        return PollStatus::Failed(format!(
                            "service '{name}' failed to reload on {member}: {error}"
                        ));
                    }
                    true
                } else {
                    false
                }
            }
            Action::Restart => {
                if lifecycle == services::ServiceLifecycle::Failed {
                    return PollStatus::Failed(format!(
                        "service '{name}' failed to restart on {member}: {}",
                        placement
                            .last_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    ));
                }
                lifecycle == services::ServiceLifecycle::Running
                    && placement.generation > target.generation
            }
        };
        if complete {
            confirmed += 1;
        }
    }
    if confirmed == targets.len() {
        PollStatus::Confirmed
    } else {
        PollStatus::Pending(format!(
            "target placements reporting completion {confirmed}/{}",
            targets.len()
        ))
    }
}

async fn fetch(
    client: &mut ServiceManagerClient<Channel>
) -> Result<services::ListServicesResponse, tonic::Status> {
    client
        .list_services(tonic::Request::new(services::ListServicesRequest {}))
        .await
        .map(tonic::Response::into_inner)
}

async fn fetch_status(
    client: &mut ServiceManagerClient<Channel>,
    name: &str,
) -> Result<services::ServiceStatusResponse, tonic::Status> {
    client
        .get_service_status(tonic::Request::new(
            services::ServiceStatusRequest { name: name.to_owned() },
        ))
        .await
        .map(tonic::Response::into_inner)
}

async fn fetch_status_fresh(
    config: &ClientConfig,
    name: &str,
) -> Result<services::ServiceStatusResponse, String> {
    let channel =
        create_channel(config).await.map_err(|error| error.to_string())?;
    fetch_status(&mut ServiceManagerClient::new(channel), name)
        .await
        .map_err(|error| error.to_string())
}

fn lifecycle(value: i32) -> &'static str {
    match services::ServiceLifecycle::try_from(value) {
        Ok(services::ServiceLifecycle::Running) => "running",
        Ok(services::ServiceLifecycle::Stopping) => "stopping",
        Ok(services::ServiceLifecycle::Stopped) => "stopped",
        Ok(services::ServiceLifecycle::Failed) => "failed",
        Ok(services::ServiceLifecycle::Unspecified) | Err(_) => "unknown",
    }
}

fn start_policy(value: i32) -> &'static str {
    match services::ServiceStartPolicy::try_from(value)
        .unwrap_or(services::ServiceStartPolicy::Unspecified)
    {
        services::ServiceStartPolicy::Automatic => "automatic",
        services::ServiceStartPolicy::Manual => "manual",
        services::ServiceStartPolicy::Unavailable => "unavailable",
        services::ServiceStartPolicy::Unspecified => "unspecified-policy",
    }
}

fn member_role(value: i32) -> &'static str {
    match services::ServiceMemberRole::try_from(value) {
        Ok(services::ServiceMemberRole::Master) => "master",
        Ok(services::ServiceMemberRole::Worker) => "worker",
        Ok(services::ServiceMemberRole::Standalone) => "standalone",
        Ok(services::ServiceMemberRole::Unspecified) | Err(_) => "unknown",
    }
}

fn print_error(
    label: &str,
    error: Option<String>,
    truncated: bool,
) {
    if let Some(error) = error {
        let suffix = if truncated { " (truncated)" } else { "" };
        println!("    {label}: {error}{suffix}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        lifecycle: services::ServiceLifecycle,
        generation: u64,
        reload_revision: u64,
        last_reload_error: Option<&str>,
    ) -> services::ServiceStatusResponse {
        services::ServiceStatusResponse {
            captured_at: None,
            members_expected: 1,
            members_reporting: 1,
            service: Some(services::ServiceStatus {
                name: "HttpsServer".to_owned(),
                placements: vec![services::ServicePlacement {
                    member: Some(services::ServiceMember {
                        node_id: 1,
                        role: services::ServiceMemberRole::Worker as i32,
                    }),
                    lifecycle: lifecycle as i32,
                    reloadable: true,
                    generation,
                    reload_revision,
                    last_error: None,
                    last_error_truncated: false,
                    last_reload_error: last_reload_error.map(ToOwned::to_owned),
                    last_reload_error_truncated: false,
                    observed_at: None,
                    start_policy: services::ServiceStartPolicy::Automatic
                        as i32,
                    active_executions: None,
                    dispatch: services::ServiceDispatch::Unspecified as i32,
                }],
            }),
        }
    }

    #[test]
    fn restart_requires_a_new_running_generation() {
        let baseline =
            snapshot(services::ServiceLifecycle::Running, 4, 0, None);
        let targets =
            command_targets(&baseline, "HttpsServer", Action::Restart).unwrap();

        assert!(matches!(
            observe(&baseline, "HttpsServer", Action::Restart, &targets),
            PollStatus::Pending(_)
        ));
        let completed =
            snapshot(services::ServiceLifecycle::Running, 5, 0, None);
        assert!(matches!(
            observe(&completed, "HttpsServer", Action::Restart, &targets),
            PollStatus::Confirmed
        ));
    }

    #[test]
    fn reload_revision_exposes_the_remote_failure() {
        let baseline =
            snapshot(services::ServiceLifecycle::Running, 1, 2, None);
        let targets =
            command_targets(&baseline, "HttpsServer", Action::Reload).unwrap();
        let failed = snapshot(
            services::ServiceLifecycle::Running,
            1,
            3,
            Some("certificate reload failed"),
        );

        match observe(&failed, "HttpsServer", Action::Reload, &targets) {
            PollStatus::Failed(error) => {
                assert!(error.contains("certificate reload failed"))
            }
            _ => panic!("failed reload must be terminal"),
        }
    }
}
