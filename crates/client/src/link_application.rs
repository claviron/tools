//! Independently started applications reached through an edge's Orbit Link.
//!
//! A command response means the Link inbox accepted a frame. The runner owns
//! its application lifecycle; the edge does not launch or stop its process.

use tonic::transport::Channel;

use crate::common::{CommandResult, format_timestamp};
use crate::proto::applications;
use crate::proto::applications::application_manager_client::ApplicationManagerClient;
use crate::proto::applications::{
    LinkApplicationCommandRequest, LinkApplicationId, LinkApplicationStatus,
    LinkApplicationStatusRequest, LinkReachability,
    ListLinkApplicationsRequest,
};

pub async fn list(channel: Channel) -> CommandResult {
    let snapshot = ApplicationManagerClient::new(channel)
        .list_link_applications(tonic::Request::new(
            ListLinkApplicationsRequest {},
        ))
        .await?
        .into_inner();
    println!(
        "Link applications: {} | captured: {}",
        snapshot.applications.len(),
        format_timestamp(snapshot.captured_at)
    );
    for application in &snapshot.applications {
        println!("{}", describe(application));
    }
    Ok(())
}

pub async fn status(
    channel: Channel,
    fleet: String,
    name: String,
) -> CommandResult {
    let snapshot = ApplicationManagerClient::new(channel)
        .get_link_application_status(tonic::Request::new(
            LinkApplicationStatusRequest { id: Some(id(fleet, name)) },
        ))
        .await?
        .into_inner();
    let application = snapshot
        .application
        .ok_or("the status response did not contain the application")?;
    println!("captured: {}", format_timestamp(snapshot.captured_at));
    println!("{}", describe(&application));
    for lane in &application.lanes {
        println!(
            "  lane {}: pid {}, incarnation {}",
            lane.lane,
            lane.pid.map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
            lane.incarnation
        );
    }
    if let Some(hosted) = &application.hosted {
        println!(
            "  hosted application members: {}/{}",
            hosted.members_reporting, hosted.members_expected
        );
        for app in &hosted.applications {
            let id = app.id.as_ref().map_or_else(
                || "<unnamed>".to_owned(),
                |id| format!("{}/{}", id.runtime, id.name),
            );
            println!("  {id}");
            for placement in &app.placements {
                let lifecycle = applications::ApplicationLifecycle::try_from(
                    placement.lifecycle,
                )
                .unwrap_or(applications::ApplicationLifecycle::Unspecified);
                println!(
                    "    node {}: {}",
                    placement
                        .member
                        .as_ref()
                        .map_or(0, |member| member.node_id),
                    lifecycle.as_str_name()
                );
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    const fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

pub async fn command(
    channel: Channel,
    fleet: String,
    link: String,
    runtime: String,
    application: String,
    action: Action,
) -> CommandResult {
    let mut client = ApplicationManagerClient::new(channel);
    let request = tonic::Request::new(LinkApplicationCommandRequest {
        id: Some(id(fleet, link)),
        command: String::new(),
        payload: Vec::new(),
        application: Some(applications::ApplicationId {
            runtime,
            name: application,
        }),
    });
    let response = match action {
        Action::Start => client.start_link_application(request).await?,
        Action::Stop => client.stop_link_application(request).await?,
        Action::Restart => client.restart_link_application(request).await?,
    }
    .into_inner();
    let shown = display(response.id.as_ref());
    if !response.accepted {
        return Err(format!(
            "Link application '{shown}' was not accepted for {}",
            action.label()
        )
        .into());
    }
    println!(
        "✅ Link application '{shown}' accepted Link command {}; the runner handles it",
        action.label()
    );
    Ok(())
}

/// A named command the runner defines; the edge only forwards it.
pub async fn send(
    channel: Channel,
    fleet: String,
    link: String,
    command: String,
    payload: Vec<u8>,
) -> CommandResult {
    let response = ApplicationManagerClient::new(channel)
        .send_link_command(tonic::Request::new(LinkApplicationCommandRequest {
            id: Some(id(fleet, link)),
            command: command.clone(),
            payload,
            application: None,
        }))
        .await?
        .into_inner();
    let shown = display(response.id.as_ref());
    if !response.accepted {
        return Err(format!(
            "Link runner '{shown}' did not accept command '{command}'"
        )
        .into());
    }
    println!(
        "✅ Link runner '{shown}' accepted command '{command}' into its Link inbox; the runner handles it"
    );
    Ok(())
}

fn id(
    fleet_name: String,
    name: String,
) -> LinkApplicationId {
    LinkApplicationId { fleet_name, name, transport: "orbit".to_owned() }
}

fn display(id: Option<&LinkApplicationId>) -> String {
    id.map_or_else(
        || "<unnamed>".to_owned(),
        |id| format!("{}/{}", id.fleet_name, id.name),
    )
}

/// One line: advertised runtime and current Link reachability.
fn describe(application: &LinkApplicationStatus) -> String {
    let link = match application.link() {
        LinkReachability::Connected => "connected",
        LinkReachability::Disconnected => "disconnected",
        LinkReachability::Unspecified => "unknown",
    };
    let info = application.info.as_ref();
    let runtime = info
        .map(|info| info.runtime.as_str())
        .filter(|runtime| !runtime.is_empty())
        .unwrap_or("-");
    format!(
        "- {} [{runtime}] link: {link} ({} worker lanes)",
        display(info.and_then(|info| info.id.as_ref())),
        application.connected_lanes
    )
}
