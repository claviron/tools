use std::collections::HashSet;
use std::time::Duration;

use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp};
use crate::proto::applications;
use crate::proto::applications::application_manager_client::ApplicationManagerClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub async fn list(channel: Channel) -> CommandResult {
    let snapshot = ApplicationManagerClient::new(channel)
        .list_applications(tonic::Request::new(
            applications::ListApplicationsRequest {},
        ))
        .await?
        .into_inner();
    println!(
        "Applications: {} | members: {}/{} | captured: {}",
        snapshot.applications.len(),
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    for application in snapshot.applications {
        print_application(application);
    }
    Ok(())
}

pub async fn status(
    channel: Channel,
    runtime: String,
    name: String,
) -> CommandResult {
    let snapshot = ApplicationManagerClient::new(channel)
        .get_application_status(tonic::Request::new(
            applications::ApplicationStatusRequest {
                id: Some(application_id(runtime, name)),
            },
        ))
        .await?
        .into_inner();
    let application = snapshot
        .application
        .ok_or("application status response did not contain an application")?;
    println!(
        "Application: {} | members: {}/{} | captured: {}",
        display_id(application.id.as_ref()),
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    print_application(application);
    Ok(())
}

pub async fn start(
    channel: Channel,
    runtime: String,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, runtime, name, ApplicationAction::Start, timeout_secs)
        .await
}

pub async fn stop(
    channel: Channel,
    runtime: String,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, runtime, name, ApplicationAction::Stop, timeout_secs).await
}

pub async fn restart(
    channel: Channel,
    runtime: String,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    command(channel, runtime, name, ApplicationAction::Restart, timeout_secs)
        .await
}

#[derive(Clone, Copy)]
enum ApplicationAction {
    Start,
    Stop,
    Restart,
}

impl ApplicationAction {
    const fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }

    const fn completed_label(self) -> &'static str {
        match self {
            Self::Start => "running",
            Self::Stop => "stopped",
            Self::Restart => "restarted",
        }
    }
}

async fn command(
    channel: Channel,
    runtime: String,
    name: String,
    action: ApplicationAction,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = ApplicationManagerClient::new(channel);
    let id = application_id(runtime, name);
    let baseline = fetch_status(&mut client, &id).await?;
    let targets = command_targets(&baseline, &id)?;
    let request =
        tonic::Request::new(applications::ApplicationCommandRequest {
            id: Some(id.clone()),
        });
    let response = match action {
        ApplicationAction::Start => client.start_application(request).await?,
        ApplicationAction::Stop => client.stop_application(request).await?,
        ApplicationAction::Restart => {
            client.restart_application(request).await?
        }
    }
    .into_inner();
    if !response.accepted {
        return Err(format!(
            "application '{}' was not accepted for {}",
            display_id(response.id.as_ref()),
            action.label()
        )
        .into());
    }
    println!(
        "✅ Application '{}' accepted for {} (completion is asynchronous)",
        display_id(response.id.as_ref()),
        action.label()
    );
    wait_for(
        &mut client,
        &id,
        action,
        &targets,
        Duration::from_secs(timeout_secs),
    )
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ApplicationTarget {
    node_id: u32,
    role: i32,
    lifecycle: applications::ApplicationLifecycle,
    enabled: bool,
    generation: u64,
}

fn command_targets(
    snapshot: &applications::ApplicationStatusResponse,
    id: &applications::ApplicationId,
) -> Result<Vec<ApplicationTarget>, Box<dyn std::error::Error>> {
    let display = display_id(Some(id));
    if snapshot.members_reporting == 0
        || snapshot.members_reporting != snapshot.members_expected
    {
        return Err(format!(
            "application fleet snapshot is incomplete: members {}/{}",
            snapshot.members_reporting, snapshot.members_expected
        )
        .into());
    }
    let application = snapshot.application.as_ref().ok_or_else(|| {
        format!("application '{display}' is absent from the fleet snapshot")
    })?;
    if application.placements.len() != snapshot.members_reporting as usize {
        return Err(format!(
            "application '{display}' has an incomplete placement snapshot: placements {}/{} reporting members",
            application.placements.len(), snapshot.members_reporting
        )
        .into());
    }

    let mut members = HashSet::with_capacity(application.placements.len());
    application
        .placements
        .iter()
        .map(|placement| {
            let member = placement.member.as_ref().ok_or_else(|| {
                format!(
                    "application '{display}' snapshot contains a placement without a fleet member"
                )
            })?;
            if !members.insert((member.node_id, member.role)) {
                return Err(format!(
                    "application '{display}' snapshot contains duplicate member {}:{}",
                    member_role(member.role),
                    member.node_id
                )
                .into());
            }
            let lifecycle = applications::ApplicationLifecycle::try_from(placement.lifecycle)
                .unwrap_or(applications::ApplicationLifecycle::Unspecified);
            if lifecycle == applications::ApplicationLifecycle::Unspecified {
                return Err(format!(
                    "application '{display}' snapshot contains an unspecified lifecycle"
                )
                .into());
            }
            Ok(ApplicationTarget {
                node_id: member.node_id,
                role: member.role,
                lifecycle,
                enabled: placement.enabled,
                generation: placement.generation,
            })
        })
        .collect()
}

async fn wait_for(
    client: &mut ApplicationManagerClient<Channel>,
    id: &applications::ApplicationId,
    action: ApplicationAction,
    targets: &[ApplicationTarget],
    timeout: Duration,
) -> CommandResult {
    let display = display_id(Some(id));
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = fetch_status(client, id).await?;
            match observe(&snapshot, &display, action, targets) {
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
                "timed out after {}s waiting for application '{display}' to be {}; last observation: {last_observation}",
                timeout.as_secs(),
                action.completed_label()
            )
            .into());
        }
    }
    println!(
        "✅ Application '{display}' is {} across current fleet capacity",
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
    snapshot: &applications::ApplicationStatusResponse,
    display: &str,
    action: ApplicationAction,
    targets: &[ApplicationTarget],
) -> PollStatus {
    if snapshot.members_reporting != snapshot.members_expected {
        return PollStatus::Pending(format!(
            "fleet members reporting {}/{}",
            snapshot.members_reporting, snapshot.members_expected
        ));
    }
    let Some(application) = snapshot.application.as_ref() else {
        return PollStatus::Pending(format!(
            "application '{display}' is absent from the latest snapshot"
        ));
    };

    let mut confirmed = 0usize;
    for target in targets {
        let Some(placement) = application.placements.iter().find(|placement| {
            placement.member.as_ref().is_some_and(|member| {
                member.node_id == target.node_id && member.role == target.role
            })
        }) else {
            continue;
        };
        let lifecycle =
            applications::ApplicationLifecycle::try_from(placement.lifecycle)
                .unwrap_or(applications::ApplicationLifecycle::Unspecified);
        let member = format!("{}:{}", member_role(target.role), target.node_id);
        let complete = match action {
            ApplicationAction::Start => {
                if lifecycle == applications::ApplicationLifecycle::Failed {
                    return PollStatus::Failed(format!(
                        "application '{display}' failed to start on {member}: {}",
                        placement
                            .last_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    ));
                }
                lifecycle == applications::ApplicationLifecycle::Running
                    && placement.enabled
                    && ((target.lifecycle
                        == applications::ApplicationLifecycle::Running
                        && target.enabled)
                        || placement.generation > target.generation)
            }
            ApplicationAction::Stop => {
                !placement.enabled
                    && matches!(
                        lifecycle,
                        applications::ApplicationLifecycle::Stopped
                            | applications::ApplicationLifecycle::Failed
                    )
            }
            ApplicationAction::Restart => {
                if lifecycle == applications::ApplicationLifecycle::Failed {
                    return PollStatus::Failed(format!(
                        "application '{display}' failed to restart on {member}: {}",
                        placement
                            .last_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    ));
                }
                lifecycle == applications::ApplicationLifecycle::Running
                    && placement.enabled
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

async fn fetch_status(
    client: &mut ApplicationManagerClient<Channel>,
    id: &applications::ApplicationId,
) -> Result<applications::ApplicationStatusResponse, tonic::Status> {
    client
        .get_application_status(tonic::Request::new(
            applications::ApplicationStatusRequest { id: Some(id.clone()) },
        ))
        .await
        .map(tonic::Response::into_inner)
}

fn application_id(
    runtime: String,
    name: String,
) -> applications::ApplicationId {
    applications::ApplicationId { runtime, name }
}

fn print_application(application: applications::ApplicationStatus) {
    println!("\n{}", display_id(application.id.as_ref()));
    for placement in application.placements {
        println!(
            "  {} | {} | {} | generation={} | observed={}",
            placement
                .member
                .as_ref()
                .map(member)
                .unwrap_or_else(|| "unknown-member".into()),
            lifecycle(placement.lifecycle),
            if placement.enabled { "enabled" } else { "disabled" },
            placement.generation,
            format_timestamp(placement.observed_at)
        );
        if let Some(pool) = placement.pool {
            println!(
                "    accepting={} instances={} ready={} busy={} retiring={} backing-off={} exhausted={}",
                optional_bool(pool.accepting),
                pool.instances,
                optional_count(pool.ready),
                optional_count(pool.busy),
                optional_count(pool.retiring),
                optional_count(pool.backing_off),
                optional_count(pool.exhausted)
            );
        }
        if let Some(error) = placement.last_error {
            let suffix = if placement.last_error_truncated {
                " [truncated]"
            } else {
                ""
            };
            println!("    last error: {error}{suffix}");
        }
    }
}

fn display_id(value: Option<&applications::ApplicationId>) -> String {
    value
        .map(|id| format!("{}:{}", id.runtime, id.name))
        .unwrap_or_else(|| "unknown-application".into())
}

fn member(member: &applications::FleetMember) -> String {
    let role = applications::FleetMemberRole::try_from(member.role)
        .map(|role| {
            role.as_str_name()
                .trim_start_matches("FLEET_MEMBER_ROLE_")
                .to_lowercase()
        })
        .unwrap_or_else(|_| "unknown".into());
    format!("{}:{}", role, member.node_id)
}

fn member_role(value: i32) -> &'static str {
    match applications::FleetMemberRole::try_from(value) {
        Ok(applications::FleetMemberRole::Master) => "master",
        Ok(applications::FleetMemberRole::Worker) => "worker",
        Ok(applications::FleetMemberRole::Standalone) => "standalone",
        Ok(applications::FleetMemberRole::Unspecified) | Err(_) => "unknown",
    }
}

fn lifecycle(value: i32) -> &'static str {
    match applications::ApplicationLifecycle::try_from(value) {
        Ok(applications::ApplicationLifecycle::Starting) => "starting",
        Ok(applications::ApplicationLifecycle::Running) => "running",
        Ok(applications::ApplicationLifecycle::Stopping) => "stopping",
        Ok(applications::ApplicationLifecycle::Stopped) => "stopped",
        Ok(applications::ApplicationLifecycle::Failed) => "failed",
        _ => "unknown",
    }
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "-",
    }
}

fn optional_count(value: Option<u32>) -> String {
    value.map_or_else(|| "-".into(), |count| count.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        lifecycle: applications::ApplicationLifecycle,
        enabled: bool,
        generation: u64,
        last_error: Option<&str>,
    ) -> applications::ApplicationStatusResponse {
        applications::ApplicationStatusResponse {
            captured_at: None,
            members_expected: 1,
            members_reporting: 1,
            application: Some(applications::ApplicationStatus {
                id: Some(application_id("php".into(), "shop".into())),
                placements: vec![applications::ApplicationPlacement {
                    member: Some(applications::FleetMember {
                        node_id: 1,
                        role: applications::FleetMemberRole::Worker as i32,
                    }),
                    lifecycle: lifecycle as i32,
                    enabled,
                    generation,
                    pool: None,
                    last_error: last_error.map(ToOwned::to_owned),
                    last_error_truncated: false,
                    observed_at: None,
                }],
            }),
        }
    }

    #[test]
    fn restart_requires_a_new_running_generation() {
        let id = application_id("php".into(), "shop".into());
        let baseline = snapshot(
            applications::ApplicationLifecycle::Running,
            true,
            4,
            None,
        );
        let targets = command_targets(&baseline, &id).unwrap();

        assert!(matches!(
            observe(
                &baseline,
                "php:shop",
                ApplicationAction::Restart,
                &targets
            ),
            PollStatus::Pending(_)
        ));
        let completed = snapshot(
            applications::ApplicationLifecycle::Running,
            true,
            5,
            None,
        );
        assert!(matches!(
            observe(
                &completed,
                "php:shop",
                ApplicationAction::Restart,
                &targets
            ),
            PollStatus::Confirmed
        ));
    }

    #[test]
    fn start_exposes_runtime_failure() {
        let id = application_id("php".into(), "shop".into());
        let baseline = snapshot(
            applications::ApplicationLifecycle::Stopped,
            false,
            2,
            None,
        );
        let targets = command_targets(&baseline, &id).unwrap();
        let failed = snapshot(
            applications::ApplicationLifecycle::Failed,
            true,
            3,
            Some("bootstrap failed"),
        );

        match observe(&failed, "php:shop", ApplicationAction::Start, &targets) {
            PollStatus::Failed(error) => {
                assert!(error.contains("bootstrap failed"))
            }
            _ => panic!("failed application start must be terminal"),
        }
    }

    #[test]
    fn stop_requires_every_target_to_be_disabled_and_stopped() {
        let id = application_id("php".into(), "shop".into());
        let baseline = snapshot(
            applications::ApplicationLifecycle::Running,
            true,
            7,
            None,
        );
        let targets = command_targets(&baseline, &id).unwrap();
        let stopped = snapshot(
            applications::ApplicationLifecycle::Stopped,
            false,
            7,
            None,
        );

        assert!(matches!(
            observe(&stopped, "php:shop", ApplicationAction::Stop, &targets),
            PollStatus::Confirmed
        ));
    }
}
