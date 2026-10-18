use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tonic::transport::Channel;

use crate::common::{CommandResult, format_duration, format_timestamp};
use crate::proto::tasks;
use crate::proto::tasks::task_manager_client::TaskManagerClient;

pub async fn list(channel: Channel) -> CommandResult {
    let snapshot = TaskManagerClient::new(channel)
        .list_tasks(tonic::Request::new(tasks::ListTasksRequest {}))
        .await?
        .into_inner();

    println!(
        "Tasks: {} | members: {}/{} | captured: {}",
        snapshot.tasks.len(),
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    for task in snapshot.tasks {
        print_task(task);
    }
    Ok(())
}

pub async fn status(
    channel: Channel,
    name: String,
) -> CommandResult {
    let snapshot =
        fetch_status(&mut TaskManagerClient::new(channel), &name).await?;
    let task =
        snapshot.task.ok_or("task status response did not contain a task")?;

    println!(
        "Task: {} | members: {}/{} | captured: {}",
        task.name,
        snapshot.members_reporting,
        snapshot.members_expected,
        format_timestamp(snapshot.captured_at)
    );
    print_task(task);
    Ok(())
}

fn print_task(task: tasks::TaskStatus) {
    println!("{}", render_task(task));
}

fn render_task(task: tasks::TaskStatus) -> String {
    let mut lines = Vec::new();
    let in_flight = task
        .placements
        .iter()
        .map(|placement| placement.in_flight)
        .sum::<u64>();
    lines.push(format!(
        "\n{} | {} | {} | {} | {} | concurrency={} | running={}",
        task.name,
        task_kind(task.kind),
        task_scope(task.scope),
        task_dispatch(task.dispatch),
        task_timing(task.timing),
        task.concurrency,
        in_flight
    ));
    // Retain completed execution reports as well as current slot ownership.
    // A scheduled task owns its slot for the whole period and runs for a
    // fraction of it, so filtering on `in_flight` alone reported "nobody has
    // this" at almost every moment an operator looks.
    let owned = task
        .placements
        .iter()
        .map(|placement| placement.owned_slots)
        .sum::<u64>();
    if in_flight == 0 && owned == 0 {
        lines.push("  no running instances".to_owned());
    }
    for placement in task.placements {
        let member = placement
            .member
            .map(|member| {
                format!("{}:{}", member_role(member.role), member.node_id)
            })
            .unwrap_or_else(|| "unknown-member".to_owned());
        let execution = placement
            .last_execution
            .map(|outcome| {
                format!(
                    "{} at {}",
                    execution_result(outcome.result),
                    format_timestamp(outcome.finished_at)
                )
            })
            .unwrap_or_else(|| "never".to_owned());
        lines.push(format!(
            "  {} | controller={} | execution={} | owned_slots={} | generation={} | last={} | observed={}",
            member,
            lifecycle(placement.lifecycle),
            if placement.in_flight > 0 {
                "running"
            } else if placement.owned_slots > 0 {
                "armed"
            } else if placement.lifecycle == tasks::TaskLifecycle::Active as i32 {
                if matches!(tasks::TaskDispatch::try_from(task.dispatch),
                    Ok(tasks::TaskDispatch::One | tasks::TaskDispatch::Pool)) {
                    "waiting"
                } else {
                    "idle"
                }
            } else {
                "not-running"
            },
            placement.owned_slots,
            placement.generation,
            execution,
            format_timestamp(placement.observed_at)
        ));
        if placement.owned_slots > 0 {
            lines.push(format!(
                "    owns {} slot(s) | {}",
                placement.owned_slots,
                if placement.in_flight > 0 { "running" } else { "armed, idle" }
            ));
        }
        for instance in 1..=placement.in_flight {
            lines.push(format!(
                "    instance={}/{} | running",
                instance, placement.in_flight
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "../tests/task_status.rs"]
mod status_tests;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RUN_RESPONSE_ALLOWANCE: Duration = Duration::from_secs(30);

pub async fn start(
    channel: Channel,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = TaskManagerClient::new(channel);
    let response = client
        .start_task(tonic::Request::new(tasks::TaskCommandRequest { name }))
        .await?
        .into_inner();
    ensure_accepted("start", &response)?;
    wait_for_state(
        &mut client,
        &response.name,
        DesiredState::Enabled,
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn stop(
    channel: Channel,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = TaskManagerClient::new(channel);
    let response = client
        .stop_task(tonic::Request::new(tasks::TaskCommandRequest { name }))
        .await?
        .into_inner();
    ensure_accepted("stop", &response)?;
    wait_for_state(
        &mut client,
        &response.name,
        DesiredState::Disabled,
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn reload(
    channel: Channel,
    name: String,
) -> CommandResult {
    let response = TaskManagerClient::new(channel)
        .reload_task(tonic::Request::new(tasks::TaskCommandRequest { name }))
        .await?
        .into_inner();
    ensure_accepted("reload", &response)
}

pub async fn restart(
    channel: Channel,
    name: String,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = TaskManagerClient::new(channel);
    let baseline = fetch_status(&mut client, &name).await?;
    let targets = restart_targets(&baseline, &name)
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let response = client
        .restart_task(tonic::Request::new(tasks::TaskCommandRequest { name }))
        .await?
        .into_inner();
    ensure_accepted("restart", &response)?;
    wait_for_restart(
        &mut client,
        &response.name,
        &targets,
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn run(
    channel: Channel,
    name: String,
    timeout_secs: Option<u64>,
) -> CommandResult {
    let mut client = TaskManagerClient::new(channel);
    let (execution_timeout, response_timeout) = match timeout_secs {
        Some(seconds) => {
            let execution_timeout = prost_types::Duration {
                seconds: i64::try_from(seconds).map_err(
                    |_| "timeout seconds exceed protobuf duration range",
                )?,
                nanos: 0,
            };
            (Some(execution_timeout), Some(Duration::from_secs(seconds)))
        }
        None => {
            let snapshot = fetch_status(&mut client, &name).await?;
            let task = snapshot
                .task
                .ok_or("task status response did not contain a task")?;
            (None, task_execution_timeout(&task)?)
        }
    };
    let mut request =
        tonic::Request::new(tasks::RunTaskRequest { name, execution_timeout });
    if let Some(response_timeout) = response_timeout {
        request.set_timeout(run_response_timeout(response_timeout)?);
    }
    let response = client.run_task(request).await?.into_inner();

    match response.result {
        Some(tasks::run_task_response::Result::Success(_)) => {
            println!(
                "✅ Task '{}' completed successfully (run-id={})",
                response.name, response.run_id
            );
            Ok(())
        }
        Some(tasks::run_task_response::Result::Failure(failure)) => {
            let kind = run_failure_kind(failure.kind);
            Err(format!(
                "task '{}' failed (run-id={}, kind={}): {}",
                response.name, response.run_id, kind, failure.message
            )
            .into())
        }
        None => Err(format!(
            "task '{}' returned no execution result (run-id={})",
            response.name, response.run_id
        )
        .into()),
    }
}

fn run_response_timeout(
    execution_timeout: Duration
) -> Result<Duration, String> {
    execution_timeout.checked_add(RUN_RESPONSE_ALLOWANCE).ok_or_else(|| {
        "task run response timeout exceeds supported duration".to_owned()
    })
}

fn task_execution_timeout(
    task: &tasks::TaskStatus
) -> Result<Option<Duration>, String> {
    let timeout = match task
        .timing
        .as_ref()
        .and_then(|timing| timing.schedule.as_ref())
    {
        Some(tasks::task_timing::Schedule::Manual(manual)) => {
            manual.execution_timeout.as_ref()
        }
        Some(tasks::task_timing::Schedule::Interval(interval)) => {
            interval.execution_timeout.as_ref()
        }
        Some(tasks::task_timing::Schedule::Cron(cron)) => {
            cron.execution_timeout.as_ref()
        }
        Some(tasks::task_timing::Schedule::Daemon(_)) | None => None,
    };

    timeout.map(protobuf_duration).transpose()
}

fn protobuf_duration(
    value: &prost_types::Duration
) -> Result<Duration, String> {
    let seconds = u64::try_from(value.seconds)
        .map_err(|_| "task execution timeout has negative seconds")?;
    let nanos = u32::try_from(value.nanos)
        .ok()
        .filter(|nanos| *nanos < 1_000_000_000)
        .ok_or("task execution timeout has invalid nanoseconds")?;
    Ok(Duration::new(seconds, nanos))
}

fn ensure_accepted(
    action: &str,
    response: &tasks::TaskCommandResponse,
) -> CommandResult {
    if response.accepted {
        println!(
            "✅ Task '{}' accepted for {} (completion is asynchronous)",
            response.name, action
        );
        return Ok(());
    }

    Err(format!("task '{}' was not accepted for {action}", response.name)
        .into())
}

#[derive(Clone, Copy)]
enum DesiredState {
    Enabled,
    Disabled,
}

impl DesiredState {
    const fn label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

enum PollStatus {
    Confirmed,
    Pending(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RestartTarget {
    node_id: u32,
    generation: u64,
}

fn restart_targets(
    snapshot: &tasks::TaskStatusResponse,
    name: &str,
) -> Result<Vec<RestartTarget>, String> {
    if snapshot.members_reporting == 0 {
        return Err(
            "cannot restart a task while no fleet members are reporting"
                .to_owned(),
        );
    }
    let task = snapshot.task.as_ref().ok_or_else(|| {
        format!("task '{name}' is absent from the fleet snapshot")
    })?;
    match task.timing.as_ref().and_then(|timing| timing.schedule.as_ref()) {
        Some(tasks::task_timing::Schedule::Daemon(daemon))
            if daemon.enabled => {}
        Some(tasks::task_timing::Schedule::Daemon(_)) => {
            return Err(format!(
                "task '{name}' is stopped; start it before restarting"
            ));
        }
        _ => {
            return Err(format!(
                "task '{name}' is not a daemon; only daemon tasks can restart"
            ));
        }
    }

    let expected = snapshot.members_reporting as usize;
    if task.placements.len() != expected {
        return Err(format!(
            "task '{name}' has an incomplete baseline snapshot: placements {}/{} reporting members",
            task.placements.len(),
            expected
        ));
    }

    let mut nodes = HashSet::with_capacity(expected);
    let mut targets = Vec::with_capacity(expected);
    for placement in &task.placements {
        let member = placement.member.as_ref().ok_or_else(|| {
            format!("task '{name}' snapshot contains a placement without a fleet member")
        })?;
        if !nodes.insert(member.node_id) {
            return Err(format!(
                "task '{name}' snapshot contains duplicate member {}",
                member.node_id
            ));
        }
        let state = tasks::TaskLifecycle::try_from(placement.lifecycle)
            .unwrap_or(tasks::TaskLifecycle::Unspecified);
        match state {
            tasks::TaskLifecycle::Unspecified => {
                return Err(format!(
                    "task '{name}' snapshot contains an unspecified lifecycle"
                ));
            }
            tasks::TaskLifecycle::Disabled => {
                return Err(format!(
                    "task '{name}' is disabled on {}:{}",
                    member_role(member.role),
                    member.node_id
                ));
            }
            tasks::TaskLifecycle::MissingRunner => {
                return Err(format!(
                    "task '{name}' has no runner on {}:{}",
                    member_role(member.role),
                    member.node_id
                ));
            }
            tasks::TaskLifecycle::Ineligible => continue,
            _ => targets.push(RestartTarget {
                node_id: member.node_id,
                generation: placement.generation,
            }),
        }
    }
    if targets.is_empty() {
        return Err(format!(
            "task '{name}' has no eligible fleet placements to restart"
        ));
    }
    Ok(targets)
}

async fn wait_for_restart(
    client: &mut TaskManagerClient<Channel>,
    name: &str,
    targets: &[RestartTarget],
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    match tokio::time::timeout(
        timeout,
        poll_until_restarted(client, name, targets, &mut last_observation),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for task '{name}' to restart across current fleet capacity; last observation: {last_observation}",
                timeout.as_secs()
            )
            .into());
        }
    }

    println!("✅ Task '{name}' restarted across current fleet capacity");
    Ok(())
}

async fn poll_until_restarted(
    client: &mut TaskManagerClient<Channel>,
    name: &str,
    targets: &[RestartTarget],
    last_observation: &mut String,
) -> CommandResult {
    loop {
        let snapshot = fetch_status(client, name).await?;
        match observe_restart(&snapshot, name, targets) {
            PollStatus::Confirmed => return Ok(()),
            PollStatus::Pending(observation) => *last_observation = observation,
            PollStatus::Failed(error) => return Err(error.into()),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn observe_restart(
    snapshot: &tasks::TaskStatusResponse,
    name: &str,
    targets: &[RestartTarget],
) -> PollStatus {
    if snapshot.members_reporting == 0 {
        return PollStatus::Pending("no fleet members reporting".to_owned());
    }
    let Some(task) = snapshot.task.as_ref() else {
        return PollStatus::Failed(format!(
            "task '{name}' disappeared from the fleet snapshot"
        ));
    };
    match task.timing.as_ref().and_then(|timing| timing.schedule.as_ref()) {
        Some(tasks::task_timing::Schedule::Daemon(daemon))
            if daemon.enabled => {}
        Some(tasks::task_timing::Schedule::Daemon(_)) => {
            return PollStatus::Failed(format!(
                "task '{name}' became disabled during restart"
            ));
        }
        _ => {
            return PollStatus::Failed(format!(
                "task '{name}' is no longer a daemon"
            ));
        }
    }

    let mut placements = HashMap::with_capacity(task.placements.len());
    for placement in &task.placements {
        let Some(member) = placement.member.as_ref() else {
            return PollStatus::Failed(format!(
                "task '{name}' snapshot contains a placement without a fleet member"
            ));
        };
        if placements.insert(member.node_id, placement).is_some() {
            return PollStatus::Failed(format!(
                "task '{name}' snapshot contains duplicate member {}",
                member.node_id
            ));
        }
    }

    let mut states = Vec::with_capacity(targets.len());
    let mut complete = true;
    for target in targets {
        let Some(placement) = placements.get(&target.node_id) else {
            complete = false;
            states.push(format!("member:{}=not-reporting", target.node_id));
            continue;
        };
        let member =
            placement.member.as_ref().expect("indexed placements have members");
        let state = tasks::TaskLifecycle::try_from(placement.lifecycle)
            .unwrap_or(tasks::TaskLifecycle::Unspecified);
        states.push(format!(
            "{}:{}={} generation={}->{}",
            member_role(member.role),
            member.node_id,
            lifecycle(placement.lifecycle),
            target.generation,
            placement.generation
        ));
        match state {
            tasks::TaskLifecycle::Unspecified => {
                return PollStatus::Failed(format!(
                    "task '{name}' snapshot contains an unspecified lifecycle"
                ));
            }
            tasks::TaskLifecycle::Disabled => {
                return PollStatus::Failed(format!(
                    "task '{name}' became disabled on {}:{} during restart",
                    member_role(member.role),
                    member.node_id
                ));
            }
            tasks::TaskLifecycle::MissingRunner => {
                return PollStatus::Failed(format!(
                    "task '{name}' has no runner on {}:{}",
                    member_role(member.role),
                    member.node_id
                ));
            }
            tasks::TaskLifecycle::Ineligible => {
                return PollStatus::Failed(format!(
                    "task '{name}' became ineligible on {}:{} during restart",
                    member_role(member.role),
                    member.node_id
                ));
            }
            _ => {}
        }

        if placement.generation <= target.generation {
            complete = false;
            continue;
        }
        if state == tasks::TaskLifecycle::Finished
            && !enable_confirmed(task, state, placement)
        {
            return PollStatus::Failed(format!(
                "task '{name}' replacement finished unsuccessfully on {}:{}",
                member_role(member.role),
                member.node_id
            ));
        }
        complete &= enable_confirmed(task, state, placement);
    }

    if complete {
        PollStatus::Confirmed
    } else {
        PollStatus::Pending(states.join(", "))
    }
}

async fn wait_for_state(
    client: &mut TaskManagerClient<Channel>,
    name: &str,
    desired: DesiredState,
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    match tokio::time::timeout(
        timeout,
        poll_until_confirmed(client, name, desired, &mut last_observation),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for task '{name}' to become {} across current fleet capacity; last observation: {last_observation}",
                timeout.as_secs(),
                desired.label(),
            )
            .into());
        }
    }

    println!(
        "✅ Task '{name}' is {} across current fleet capacity",
        desired.label()
    );
    Ok(())
}

async fn poll_until_confirmed(
    client: &mut TaskManagerClient<Channel>,
    name: &str,
    desired: DesiredState,
    last_observation: &mut String,
) -> CommandResult {
    loop {
        let snapshot = fetch_status(client, name).await?;

        match observe(&snapshot, name, desired) {
            PollStatus::Confirmed => return Ok(()),
            PollStatus::Pending(observation) => *last_observation = observation,
            PollStatus::Failed(error) => return Err(error.into()),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn observe(
    snapshot: &tasks::TaskStatusResponse,
    name: &str,
    desired: DesiredState,
) -> PollStatus {
    if snapshot.members_reporting == 0 {
        return PollStatus::Pending("no fleet members reporting".to_owned());
    }

    let Some(task) = snapshot.task.as_ref() else {
        return PollStatus::Failed(format!(
            "task '{name}' disappeared from the fleet snapshot"
        ));
    };
    let expected = snapshot.members_reporting as usize;
    if task.placements.len() != expected {
        return PollStatus::Pending(format!(
            "task placements {}/{} reporting members ({} configured)",
            task.placements.len(),
            expected,
            snapshot.members_expected
        ));
    }

    let mut nodes = HashSet::with_capacity(expected);
    let mut states = Vec::with_capacity(expected);
    let mut complete = true;
    for placement in &task.placements {
        let Some(member) = placement.member.as_ref() else {
            return PollStatus::Failed(format!(
                "task '{name}' snapshot contains a placement without a fleet member"
            ));
        };
        if !nodes.insert(member.node_id) {
            return PollStatus::Failed(format!(
                "task '{name}' snapshot contains duplicate member {}",
                member.node_id
            ));
        }

        let state = tasks::TaskLifecycle::try_from(placement.lifecycle)
            .unwrap_or(tasks::TaskLifecycle::Unspecified);
        states.push(format!(
            "{}:{}={}({})",
            member_role(member.role),
            member.node_id,
            lifecycle(placement.lifecycle),
            placement.in_flight
        ));

        if state == tasks::TaskLifecycle::Unspecified {
            return PollStatus::Failed(format!(
                "task '{name}' snapshot contains an unspecified lifecycle"
            ));
        }
        if state == tasks::TaskLifecycle::MissingRunner {
            return PollStatus::Failed(format!(
                "task '{name}' has no runner on {}:{}",
                member_role(member.role),
                member.node_id
            ));
        }

        let placement_complete = match desired {
            DesiredState::Enabled => enable_confirmed(task, state, placement),
            DesiredState::Disabled => matches!(
                state,
                tasks::TaskLifecycle::Ineligible
                    | tasks::TaskLifecycle::Disabled
            ),
        };
        complete &= placement_complete;

        if matches!(desired, DesiredState::Enabled)
            && state == tasks::TaskLifecycle::Finished
            && !enable_confirmed(task, state, placement)
        {
            return PollStatus::Failed(format!(
                "task '{name}' finished without a successful enable on {}:{}",
                member_role(member.role),
                member.node_id
            ));
        }
    }

    if nodes.len() != expected {
        return PollStatus::Pending(format!(
            "unique task members {}/{expected}",
            nodes.len()
        ));
    }
    if complete {
        PollStatus::Confirmed
    } else {
        PollStatus::Pending(states.join(", "))
    }
}

async fn fetch_status(
    client: &mut TaskManagerClient<Channel>,
    name: &str,
) -> Result<tasks::TaskStatusResponse, tonic::Status> {
    client
        .get_task_status(tonic::Request::new(tasks::TaskStatusRequest {
            name: name.to_owned(),
        }))
        .await
        .map(tonic::Response::into_inner)
}

fn enable_confirmed(
    task: &tasks::TaskStatus,
    state: tasks::TaskLifecycle,
    placement: &tasks::TaskPlacementStatus,
) -> bool {
    if state == tasks::TaskLifecycle::Ineligible
        || state == tasks::TaskLifecycle::Active
    {
        return true;
    }

    match task.timing.as_ref().and_then(|timing| timing.schedule.as_ref()) {
        Some(tasks::task_timing::Schedule::Daemon(_)) => {
            state == tasks::TaskLifecycle::Finished
                && placement.last_execution.as_ref().is_some_and(|outcome| {
                    tasks::TaskExecutionResult::try_from(outcome.result)
                        == Ok(tasks::TaskExecutionResult::Succeeded)
                })
        }
        _ => false,
    }
}

fn task_kind(value: i32) -> &'static str {
    match tasks::TaskKind::try_from(value)
        .unwrap_or(tasks::TaskKind::Unspecified)
    {
        tasks::TaskKind::Builtin => "builtin",
        tasks::TaskKind::Command => "command",
        tasks::TaskKind::Php => "php",
        tasks::TaskKind::Js => "js",
        tasks::TaskKind::Lua => "lua",
        tasks::TaskKind::Unspecified => "unspecified",
    }
}

fn task_scope(value: i32) -> &'static str {
    match tasks::TaskScope::try_from(value)
        .unwrap_or(tasks::TaskScope::Unspecified)
    {
        tasks::TaskScope::Master => "master",
        tasks::TaskScope::Workers => "workers",
        tasks::TaskScope::Fleet => "fleet",
        tasks::TaskScope::Unspecified => "unspecified",
    }
}

fn task_dispatch(value: i32) -> &'static str {
    match tasks::TaskDispatch::try_from(value)
        .unwrap_or(tasks::TaskDispatch::Unspecified)
    {
        tasks::TaskDispatch::Each => "each",
        tasks::TaskDispatch::One => "one",
        tasks::TaskDispatch::Pool => "pool",
        tasks::TaskDispatch::Unspecified => "unspecified",
    }
}

fn task_timing(timing: Option<tasks::TaskTiming>) -> String {
    match timing.and_then(|timing| timing.schedule) {
        Some(tasks::task_timing::Schedule::Manual(manual)) => format!(
            "manual:timeout={}",
            optional_duration(manual.execution_timeout)
        ),
        Some(tasks::task_timing::Schedule::Daemon(daemon)) => {
            format!("daemon:{}", trigger_state(daemon.enabled))
        }
        Some(tasks::task_timing::Schedule::Interval(interval)) => {
            format!(
                "interval:{} timeout={} {}",
                optional_duration(interval.every),
                optional_duration(interval.execution_timeout),
                trigger_state(interval.enabled)
            )
        }
        Some(tasks::task_timing::Schedule::Cron(cron)) => format!(
            "cron:{} timeout={} {}",
            cron.expression,
            optional_duration(cron.execution_timeout),
            trigger_state(cron.enabled)
        ),
        None => "unspecified".to_owned(),
    }
}

fn trigger_state(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn optional_duration(duration: Option<prost_types::Duration>) -> String {
    duration.map(format_duration).unwrap_or_else(|| "unspecified".to_owned())
}

fn run_failure_kind(value: i32) -> &'static str {
    match tasks::RunTaskFailureKind::try_from(value)
        .unwrap_or(tasks::RunTaskFailureKind::Unspecified)
    {
        tasks::RunTaskFailureKind::Io => "io",
        tasks::RunTaskFailureKind::Cancelled => "cancelled",
        tasks::RunTaskFailureKind::Shutdown => "shutdown",
        tasks::RunTaskFailureKind::GracefulTimeout => "graceful-timeout",
        tasks::RunTaskFailureKind::ExecutionTimeout => "execution-timeout",
        tasks::RunTaskFailureKind::Task => "task",
        tasks::RunTaskFailureKind::Unspecified => "unspecified",
    }
}

fn member_role(value: i32) -> &'static str {
    match tasks::TaskMemberRole::try_from(value)
        .unwrap_or(tasks::TaskMemberRole::Unspecified)
    {
        tasks::TaskMemberRole::Master => "master",
        tasks::TaskMemberRole::Worker => "worker",
        tasks::TaskMemberRole::Standalone => "standalone",
        tasks::TaskMemberRole::Unspecified => "unknown",
    }
}

fn lifecycle(value: i32) -> &'static str {
    match tasks::TaskLifecycle::try_from(value)
        .unwrap_or(tasks::TaskLifecycle::Unspecified)
    {
        tasks::TaskLifecycle::Ineligible => "ineligible",
        tasks::TaskLifecycle::Disabled => "disabled",
        tasks::TaskLifecycle::Inactive => "inactive",
        tasks::TaskLifecycle::Active => "active",
        tasks::TaskLifecycle::Stopping => "stopping",
        tasks::TaskLifecycle::Finished => "finished",
        tasks::TaskLifecycle::MissingRunner => "missing-runner",
        tasks::TaskLifecycle::Unspecified => "unspecified",
    }
}

fn execution_result(value: i32) -> &'static str {
    match tasks::TaskExecutionResult::try_from(value)
        .unwrap_or(tasks::TaskExecutionResult::Unspecified)
    {
        tasks::TaskExecutionResult::Succeeded => "succeeded",
        tasks::TaskExecutionResult::Failed => "failed",
        tasks::TaskExecutionResult::Cancelled => "cancelled",
        tasks::TaskExecutionResult::TimedOut => "timed-out",
        tasks::TaskExecutionResult::GaveUp => "gave-up",
        tasks::TaskExecutionResult::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        DesiredState, PollStatus, RUN_RESPONSE_ALLOWANCE, observe,
        observe_restart, restart_targets, run_response_timeout,
        task_execution_timeout,
    };
    use crate::proto::tasks;

    #[test]
    fn enable_uses_current_reporting_capacity() {
        let snapshot = snapshot(vec![
            placement(
                0,
                tasks::TaskMemberRole::Master,
                tasks::TaskLifecycle::Ineligible,
                0,
            ),
            placement(
                1,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Active,
                0,
            ),
            placement(
                2,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Active,
                0,
            ),
        ]);

        assert!(matches!(
            observe(&snapshot, "queue", DesiredState::Enabled),
            PollStatus::Confirmed
        ));

        let mut reduced_capacity = snapshot.clone();
        reduced_capacity.members_reporting = 2;
        task_mut(&mut reduced_capacity).placements.pop();
        assert!(matches!(
            observe(&reduced_capacity, "queue", DesiredState::Enabled),
            PollStatus::Confirmed
        ));

        let mut incomplete = snapshot;
        task_mut(&mut incomplete).placements.pop();
        assert!(matches!(
            observe(&incomplete, "queue", DesiredState::Enabled),
            PollStatus::Pending(_)
        ));
    }

    #[test]
    fn disabled_requires_every_eligible_member_to_disable_its_trigger() {
        let snapshot = snapshot(vec![
            placement(
                0,
                tasks::TaskMemberRole::Master,
                tasks::TaskLifecycle::Ineligible,
                0,
            ),
            placement(
                1,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Disabled,
                0,
            ),
            placement(
                2,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Disabled,
                0,
            ),
        ]);

        assert!(matches!(
            observe(&snapshot, "queue", DesiredState::Disabled),
            PollStatus::Confirmed
        ));

        let mut stopping = snapshot.clone();
        task_mut(&mut stopping).placements[1].lifecycle =
            tasks::TaskLifecycle::Stopping as i32;
        assert!(matches!(
            observe(&stopping, "queue", DesiredState::Disabled),
            PollStatus::Pending(_)
        ));

        let mut explicit_run = snapshot;
        task_mut(&mut explicit_run).placements[1].in_flight = 1;
        assert!(matches!(
            observe(&explicit_run, "queue", DesiredState::Disabled),
            PollStatus::Confirmed
        ));
    }

    #[test]
    fn enable_waits_while_the_previous_disabled_snapshot_is_still_visible() {
        let snapshot = snapshot(vec![
            placement(
                0,
                tasks::TaskMemberRole::Master,
                tasks::TaskLifecycle::Disabled,
                0,
            ),
            placement(
                1,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Disabled,
                0,
            ),
            placement(
                2,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Disabled,
                0,
            ),
        ]);

        assert!(matches!(
            observe(&snapshot, "queue", DesiredState::Enabled),
            PollStatus::Pending(_)
        ));
    }

    #[test]
    fn missing_runner_is_an_error() {
        let snapshot = snapshot(vec![
            placement(
                0,
                tasks::TaskMemberRole::Master,
                tasks::TaskLifecycle::Ineligible,
                0,
            ),
            placement(
                1,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::MissingRunner,
                0,
            ),
            placement(
                2,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Active,
                0,
            ),
        ]);

        assert!(matches!(
            observe(&snapshot, "queue", DesiredState::Enabled),
            PollStatus::Failed(_)
        ));
    }

    #[test]
    fn restart_requires_every_eligible_member_to_advance_generation() {
        let mut baseline = snapshot(vec![
            placement(
                0,
                tasks::TaskMemberRole::Master,
                tasks::TaskLifecycle::Ineligible,
                0,
            ),
            placement(
                1,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Active,
                2,
            ),
            placement(
                2,
                tasks::TaskMemberRole::Worker,
                tasks::TaskLifecycle::Active,
                1,
            ),
        ]);
        task_mut(&mut baseline).timing = Some(tasks::TaskTiming {
            schedule: Some(tasks::task_timing::Schedule::Daemon(
                tasks::DaemonTiming { enabled: true },
            )),
        });
        task_mut(&mut baseline).placements[1].generation = 8;
        task_mut(&mut baseline).placements[2].generation = 5;
        let targets =
            restart_targets(&baseline, "queue").expect("restart baseline");

        assert!(matches!(
            observe_restart(&baseline, "queue", &targets),
            PollStatus::Pending(_)
        ));

        let mut restarted = baseline;
        task_mut(&mut restarted).placements[1].generation = 9;
        task_mut(&mut restarted).placements[2].generation = 6;
        assert!(matches!(
            observe_restart(&restarted, "queue", &targets),
            PollStatus::Confirmed
        ));
    }

    #[test]
    fn run_deadline_uses_the_observed_execution_timeout() {
        let execution_timeout =
            prost_types::Duration { seconds: 120, nanos: 250_000_000 };
        let task = tasks::TaskStatus {
            timing: Some(tasks::TaskTiming {
                schedule: Some(tasks::task_timing::Schedule::Manual(
                    tasks::ManualTiming {
                        execution_timeout: Some(execution_timeout),
                    },
                )),
            }),
            ..Default::default()
        };

        assert_eq!(
            task_execution_timeout(&task).expect("valid timeout"),
            Some(Duration::new(120, 250_000_000))
        );
        assert_eq!(
            run_response_timeout(Duration::new(120, 250_000_000))
                .expect("representable response timeout"),
            Duration::new(150, 250_000_000)
        );
        assert_eq!(RUN_RESPONSE_ALLOWANCE, Duration::from_secs(30));
    }

    fn snapshot(
        placements: Vec<tasks::TaskPlacementStatus>
    ) -> tasks::TaskStatusResponse {
        tasks::TaskStatusResponse {
            captured_at: None,
            members_expected: 3,
            members_reporting: 3,
            task: Some(tasks::TaskStatus {
                name: "queue".to_owned(),
                kind: tasks::TaskKind::Command as i32,
                scope: tasks::TaskScope::Workers as i32,
                dispatch: tasks::TaskDispatch::Pool as i32,
                timing: None,
                concurrency: 2,
                placements,
            }),
        }
    }

    fn task_mut(
        snapshot: &mut tasks::TaskStatusResponse
    ) -> &mut tasks::TaskStatus {
        snapshot.task.as_mut().expect("test snapshot contains a task")
    }

    fn placement(
        node_id: u32,
        role: tasks::TaskMemberRole,
        lifecycle: tasks::TaskLifecycle,
        in_flight: u64,
    ) -> tasks::TaskPlacementStatus {
        tasks::TaskPlacementStatus {
            member: Some(tasks::TaskMember { node_id, role: role as i32 }),
            lifecycle: lifecycle as i32,
            generation: 0,
            in_flight,
            owned_slots: 0,
            last_execution: None,
            observed_at: None,
        }
    }
}
