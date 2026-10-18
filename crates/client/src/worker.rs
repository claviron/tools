use std::time::Duration;

use tonic::transport::Channel;

use crate::client::{ClientConfig, create_channel};
use crate::common::{CommandResult, format_duration, format_timestamp};
use crate::proto::workers;
use crate::proto::workers::worker_manager_client::WorkerManagerClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub async fn list(channel: Channel) -> CommandResult {
    let snapshot = fetch(&mut WorkerManagerClient::new(channel)).await?;
    println!(
        "Workers: {}/{} | captured: {}",
        snapshot.workers.len(),
        snapshot.workers_expected,
        format_timestamp(snapshot.captured_at)
    );
    println!(
        "{}",
        format_header(
            snapshot.master.as_ref(),
            snapshot.fleet_mode,
            snapshot.build.as_ref()
        )
    );
    for worker in snapshot.workers {
        print_worker(&worker);
    }
    Ok(())
}

/// The concise line plus everything the master knows about the slot: the exact
/// process, the two facts its label is derived from and how long each has
/// stood, the operation acting on it, its heartbeat age and, when boot refused
/// it, why. Enough to explain a blocked fleet without the master's log.
pub async fn status(
    channel: Channel,
    worker_id: u8,
) -> CommandResult {
    let snapshot =
        fetch_status(&mut WorkerManagerClient::new(channel), worker_id).await?;
    println!(
        "Worker: #{} | configured: {} | captured: {}",
        worker_id,
        snapshot.workers_expected,
        format_timestamp(snapshot.captured_at)
    );
    println!(
        "{}",
        format_header(
            snapshot.master.as_ref(),
            snapshot.fleet_mode,
            snapshot.build.as_ref()
        )
    );
    let worker = snapshot.worker.ok_or_else(|| {
        format!("worker #{worker_id} is missing from the supervisor snapshot")
    })?;
    print_worker(&worker);
    for line in describe_detail(&worker) {
        println!("  {line}");
    }
    Ok(())
}

pub async fn start(
    channel: Channel,
    config: &ClientConfig,
    worker_id: u8,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let response = client.start_worker(request(worker_id)).await?.into_inner();
    ensure_accepted("start", &response)?;
    wait_for(
        config,
        worker_id,
        DesiredState::Started,
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn stop(
    channel: Channel,
    config: &ClientConfig,
    worker_id: u8,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let response = client.stop_worker(request(worker_id)).await?.into_inner();
    ensure_accepted("stop", &response)?;
    wait_for(
        config,
        worker_id,
        DesiredState::Stopped,
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn reload(
    channel: Channel,
    config: &ClientConfig,
    worker_id: u8,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let current = fetch_status(&mut client, worker_id)
        .await?
        .worker
        .ok_or_else(|| {
            format!(
                "worker #{worker_id} is missing from the supervisor snapshot"
            )
        })?;
    if !matches!(
        workers::WorkerLifecycle::try_from(current.lifecycle),
        Ok(workers::WorkerLifecycle::Running)
    ) {
        return Err(format!(
            "worker #{worker_id} is {}, not running",
            lifecycle(current.lifecycle)
        )
        .into());
    }
    require_owned_running(&current, false).map_err(|error| {
        format!("worker #{worker_id} cannot be reloaded: {error}")
    })?;
    let baseline = current.reload_revision;
    let incarnation = current.incarnation;
    let response = client.reload_worker(request(worker_id)).await?.into_inner();
    ensure_accepted("reload", &response)?;
    wait_for(
        config,
        worker_id,
        DesiredState::ReloadedAfter { revision: baseline, incarnation },
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn restart(
    channel: Channel,
    config: &ClientConfig,
    worker_id: u8,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let current = fetch_status(&mut client, worker_id)
        .await?
        .worker
        .ok_or_else(|| {
            format!(
                "worker #{worker_id} is missing from the supervisor snapshot"
            )
        })?;
    if !matches!(
        workers::WorkerLifecycle::try_from(current.lifecycle),
        Ok(workers::WorkerLifecycle::Running)
    ) {
        return Err(format!(
            "worker #{worker_id} is {}, not running",
            lifecycle(current.lifecycle)
        )
        .into());
    }
    require_owned_running(&current, false).map_err(|error| {
        format!("worker #{worker_id} cannot be restarted: {error}")
    })?;
    let baseline = current.incarnation;
    let response =
        client.restart_worker(request(worker_id)).await?.into_inner();
    ensure_accepted("restart", &response)?;
    wait_for(
        config,
        worker_id,
        DesiredState::RestartedAfter(baseline),
        Duration::from_secs(timeout_secs),
    )
    .await
}

pub async fn reload_all(
    channel: Channel,
    config: &ClientConfig,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let snapshot = fetch(&mut client).await?;
    let targets = reload_targets(&snapshot)?;
    let response = client
        .reload_workers(tonic::Request::new(workers::ReloadWorkersRequest {}))
        .await?
        .into_inner();
    if !response.accepted {
        return Err("worker reload was not accepted".into());
    }
    println!(
        "✅ Reload workers accepted (submission {}; completion is asynchronous)",
        submission_key(response.submission_key)
    );
    wait_for_reload_all(config, &targets, Duration::from_secs(timeout_secs))
        .await
}

pub async fn restart_all(
    channel: Channel,
    config: &ClientConfig,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let snapshot = fetch(&mut client).await?;
    let targets = restart_targets(&snapshot)?;
    let response = client
        .restart_workers(tonic::Request::new(workers::RestartWorkersRequest {}))
        .await?
        .into_inner();
    if !response.accepted {
        return Err("rolling worker restart was not accepted".into());
    }
    println!(
        "✅ Restart workers accepted (submission {}; completion is asynchronous)",
        submission_key(response.submission_key)
    );
    wait_for_restart_all(config, &targets, Duration::from_secs(timeout_secs))
        .await
}

pub async fn add(
    channel: Channel,
    config: &ClientConfig,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let baseline = fetch(&mut client).await?;
    let response = client
        .add_worker(tonic::Request::new(workers::ResizeWorkersRequest {}))
        .await?
        .into_inner();
    if !response.accepted {
        return Err("worker fleet growth was not accepted".into());
    }
    let expectation = resize_expectation(
        &baseline,
        response.previous_worker_count,
        response.target_worker_count,
    )?;
    println!(
        "✅ Add worker accepted: {} -> {} configured workers (submission {}; completion is asynchronous)",
        response.previous_worker_count,
        response.target_worker_count,
        submission_key(response.submission_key)
    );
    wait_for_resize(config, &expectation, Duration::from_secs(timeout_secs))
        .await
}

pub async fn remove(
    channel: Channel,
    config: &ClientConfig,
    timeout_secs: u64,
) -> CommandResult {
    let mut client = WorkerManagerClient::new(channel);
    let baseline = fetch(&mut client).await?;
    let response = client
        .remove_worker(tonic::Request::new(workers::ResizeWorkersRequest {}))
        .await?
        .into_inner();
    if !response.accepted {
        return Err("worker fleet shrink was not accepted".into());
    }
    let expectation = resize_expectation(
        &baseline,
        response.previous_worker_count,
        response.target_worker_count,
    )?;
    println!(
        "✅ Remove worker accepted: {} -> {} configured workers (submission {}; completion is asynchronous)",
        response.previous_worker_count,
        response.target_worker_count,
        submission_key(response.submission_key)
    );
    wait_for_resize(config, &expectation, Duration::from_secs(timeout_secs))
        .await
}

fn request(worker_id: u8) -> tonic::Request<workers::WorkerCommandRequest> {
    tonic::Request::new(workers::WorkerCommandRequest {
        worker_id: u32::from(worker_id),
    })
}

fn ensure_accepted(
    action: &str,
    response: &workers::WorkerCommandResponse,
) -> CommandResult {
    if !response.accepted {
        return Err(format!(
            "worker #{} was not accepted for {action}",
            response.worker_id
        )
        .into());
    }
    println!(
        "✅ Worker #{} accepted for {} (submission {}; completion is asynchronous)",
        response.worker_id,
        action,
        submission_key(response.submission_key)
    );
    Ok(())
}

/// The key of one submission. While the master runs the operation it opened
/// for it, the operation's management origin carries the same key, so a poll
/// that lands in that window can tell this client's operation from any other
/// acting on the slot. It is not the master's acknowledgement: a refused
/// submission opens nothing, and one that completes between two polls is never
/// seen.
fn submission_key(key: u64) -> String {
    format!("{key:016x}")
}

#[derive(Clone, Copy)]
enum DesiredState {
    Started,
    Stopped,
    ReloadedAfter { revision: u64, incarnation: u64 },
    RestartedAfter(u64),
}

impl DesiredState {
    const fn label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::ReloadedAfter { .. } => "reloaded",
            Self::RestartedAfter(_) => "restarted",
        }
    }
}

async fn wait_for(
    config: &ClientConfig,
    worker_id: u8,
    desired: DesiredState,
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            // The command may intentionally drain the worker that carried its
            // HTTP/2 connection. Never reuse that channel for observation:
            // a fresh authenticated connection lets REUSEPORT select a live
            // worker, and a transient GOAWAY becomes another poll attempt.
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = match fetch_status_fresh(config, worker_id).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_observation = error;
                    continue;
                }
            };
            if u32::from(worker_id) > snapshot.workers_expected {
                return Err(format!(
                    "worker #{worker_id} exceeds configured worker count {}",
                    snapshot.workers_expected
                )
                .into());
            }
            let Some(worker) = snapshot.worker.as_ref() else {
                last_observation =
                    "worker slot is temporarily absent from snapshot"
                        .to_owned();
                continue;
            };
            last_observation = format!(
                "state={}, ownership={}, pid={}, incarnation={}, reloads={}",
                lifecycle(worker.lifecycle),
                ownership(worker.ownership),
                pid_label(worker),
                worker.incarnation,
                worker.reload_revision
            );
            if observe(worker, desired).map_err(
                |error| -> Box<dyn std::error::Error> {
                    format!("worker #{worker_id} {error}").into()
                },
            )? {
                return Ok::<workers::WorkerStatus, Box<dyn std::error::Error>>(
                    *worker,
                );
            }
        }
    })
    .await;

    let worker = match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for worker #{worker_id} to become {}; last observation: {last_observation}",
                timeout.as_secs(),
                desired.label()
            )
            .into());
        }
    };
    println!("{}", completion_message(&worker, desired)?);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReloadTarget {
    worker_id: u32,
    incarnation: u64,
    reload_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RestartTarget {
    worker_id: u32,
    incarnation: u64,
}

/// What the retained running workers are expected to do while the fleet
/// resizes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RetainedWorkers {
    /// Growth converges every retained running worker onto the new count: each
    /// is replaced by a new incarnation.
    Replaced(Vec<RestartTarget>),
    /// Shrink retires only the highest slot: every retained worker keeps its
    /// process, PID and incarnation.
    Unchanged(Vec<RestartTarget>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResizeExpectation {
    previous_worker_count: u32,
    target_worker_count: u32,
    retained: RetainedWorkers,
    added_worker_id: Option<u32>,
    removed_worker_id: Option<u32>,
}

fn reload_targets(
    snapshot: &workers::ListWorkersResponse
) -> Result<Vec<ReloadTarget>, Box<dyn std::error::Error>> {
    let mut targets = Vec::new();
    for worker in &snapshot.workers {
        if matches!(
            workers::WorkerLifecycle::try_from(worker.lifecycle),
            Ok(workers::WorkerLifecycle::Running)
        ) {
            require_owned_running(worker, false).map_err(|error| {
                format!(
                    "worker #{} cannot be reloaded: {error}",
                    worker.worker_id
                )
            })?;
            targets.push(ReloadTarget {
                worker_id: worker.worker_id,
                incarnation: worker.incarnation,
                reload_revision: worker.reload_revision,
            });
        }
    }
    if targets.is_empty() {
        return Err("no running workers are available to reload".into());
    }
    Ok(targets)
}

fn restart_targets(
    snapshot: &workers::ListWorkersResponse
) -> Result<Vec<RestartTarget>, Box<dyn std::error::Error>> {
    let mut targets = Vec::new();
    for worker in &snapshot.workers {
        match workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified)
        {
            workers::WorkerLifecycle::Running => {
                require_owned_running(worker, false).map_err(|error| {
                    format!(
                        "worker #{} cannot be restarted: {error}",
                        worker.worker_id
                    )
                })?;
                targets.push(RestartTarget {
                    worker_id: worker.worker_id,
                    incarnation: worker.incarnation,
                });
            }
            workers::WorkerLifecycle::Stopped => {
                require_stopped_unowned(worker).map_err(|error| {
                    format!(
                        "worker #{} is not stably stopped: {error}",
                        worker.worker_id
                    )
                })?;
            }
            lifecycle => {
                return Err(format!(
                    "worker #{} is {}; rolling restart requires a stable fleet",
                    worker.worker_id,
                    lifecycle_label(lifecycle)
                )
                .into());
            }
        }
    }
    if targets.len() < 2 {
        return Err(format!(
            "rolling restart requires at least two running workers; found {}",
            targets.len()
        )
        .into());
    }
    Ok(targets)
}

fn resize_expectation(
    snapshot: &workers::ListWorkersResponse,
    previous_worker_count: u32,
    target_worker_count: u32,
) -> Result<ResizeExpectation, Box<dyn std::error::Error>> {
    if snapshot.workers_expected != previous_worker_count {
        return Err(format!(
            "fleet changed while resize was accepted: observed {}, command expected {previous_worker_count}",
            snapshot.workers_expected
        )
        .into());
    }
    if snapshot.workers.len() != previous_worker_count as usize {
        return Err(format!(
            "worker snapshot has {} row(s), expected {previous_worker_count}",
            snapshot.workers.len()
        )
        .into());
    }
    if previous_worker_count.abs_diff(target_worker_count) != 1
        || target_worker_count == 0
    {
        return Err(format!(
            "invalid fleet resize response: {previous_worker_count} -> {target_worker_count}"
        )
        .into());
    }
    for worker_id in 1..=previous_worker_count {
        if !snapshot.workers.iter().any(|worker| worker.worker_id == worker_id)
        {
            return Err(format!(
                "worker #{worker_id} is missing from the pre-resize snapshot"
            )
            .into());
        }
    }

    let retained_max = previous_worker_count.min(target_worker_count);
    let mut retained_running = Vec::new();
    for worker in &snapshot.workers {
        let lifecycle = workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified);
        match lifecycle {
            workers::WorkerLifecycle::Running
                if worker.worker_id <= retained_max =>
            {
                require_owned_running(worker, false).map_err(|error| {
                    format!(
                        "worker #{} cannot be resized: {error}",
                        worker.worker_id
                    )
                })?;
                retained_running.push(RestartTarget {
                    worker_id: worker.worker_id,
                    incarnation: worker.incarnation,
                });
            }
            workers::WorkerLifecycle::Running => {
                require_owned_running(worker, false).map_err(|error| {
                    format!(
                        "worker #{} cannot be resized: {error}",
                        worker.worker_id
                    )
                })?;
            }
            workers::WorkerLifecycle::Stopped => {
                require_stopped_unowned(worker).map_err(|error| {
                    format!(
                        "worker #{} is not stably stopped: {error}",
                        worker.worker_id
                    )
                })?;
            }
            lifecycle => {
                return Err(format!(
                    "worker #{} is {}; fleet resize requires a stable fleet",
                    worker.worker_id,
                    lifecycle_label(lifecycle)
                )
                .into());
            }
        }
    }

    let growing = target_worker_count > previous_worker_count;
    Ok(ResizeExpectation {
        previous_worker_count,
        target_worker_count,
        retained: if growing {
            RetainedWorkers::Replaced(retained_running)
        } else {
            RetainedWorkers::Unchanged(retained_running)
        },
        added_worker_id: growing.then_some(target_worker_count),
        removed_worker_id: (!growing).then_some(previous_worker_count),
    })
}

async fn wait_for_reload_all(
    config: &ClientConfig,
    targets: &[ReloadTarget],
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = match fetch_fresh(config).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_observation = error;
                    continue;
                }
            };
            last_observation = describe_targets(&snapshot, targets);
            if observe_reload_targets(&snapshot, targets).map_err(
                |error| -> Box<dyn std::error::Error> { error.into() },
            )? {
                return Ok::<
                    workers::ListWorkersResponse,
                    Box<dyn std::error::Error>,
                >(snapshot);
            }
        }
    })
    .await;

    let snapshot = match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for {} worker(s) to reload; last observation: {last_observation}",
                timeout.as_secs(),
                targets.len()
            )
            .into());
        }
    };
    println!(
        "✅ Reloaded {} worker(s): {}",
        targets.len(),
        describe_completed_pids(
            &snapshot,
            targets.iter().map(|target| target.worker_id)
        )?
    );
    Ok(())
}

async fn wait_for_restart_all(
    config: &ClientConfig,
    targets: &[RestartTarget],
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = match fetch_fresh(config).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_observation = error;
                    continue;
                }
            };
            last_observation = describe_restart_targets(&snapshot, targets);
            if observe_restart_targets(&snapshot, targets).map_err(
                |error| -> Box<dyn std::error::Error> { error.into() },
            )? {
                return Ok::<
                    workers::ListWorkersResponse,
                    Box<dyn std::error::Error>,
                >(snapshot);
            }
        }
    })
    .await;

    let snapshot = match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for {} worker(s) to restart; last observation: {last_observation}",
                timeout.as_secs(),
                targets.len()
            )
            .into());
        }
    };
    println!(
        "✅ Restarted {} worker(s): {}",
        targets.len(),
        describe_completed_pids(
            &snapshot,
            targets.iter().map(|target| target.worker_id)
        )?
    );
    Ok(())
}

async fn wait_for_resize(
    config: &ClientConfig,
    expectation: &ResizeExpectation,
    timeout: Duration,
) -> CommandResult {
    let mut last_observation = "no snapshot received".to_owned();
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let snapshot = match fetch_fresh(config).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_observation = error;
                    continue;
                }
            };
            last_observation = describe_resize(&snapshot);
            if observe_resize(&snapshot, expectation).map_err(
                |error| -> Box<dyn std::error::Error> { error.into() },
            )? {
                return Ok::<
                    workers::ListWorkersResponse,
                    Box<dyn std::error::Error>,
                >(snapshot);
            }
        }
    })
    .await;

    let snapshot = match result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "timed out after {}s waiting for worker fleet to resize {} -> {}; last observation: {last_observation}",
                timeout.as_secs(),
                expectation.previous_worker_count,
                expectation.target_worker_count
            )
            .into());
        }
    };
    println!(
        "✅ Worker fleet resized: {} -> {} configured workers | {}",
        expectation.previous_worker_count,
        expectation.target_worker_count,
        describe_completed_pids(
            &snapshot,
            snapshot.workers.iter().filter_map(|worker| {
                matches!(
                    workers::WorkerLifecycle::try_from(worker.lifecycle),
                    Ok(workers::WorkerLifecycle::Running)
                )
                .then_some(worker.worker_id)
            })
        )?
    );
    Ok(())
}

fn observe_reload_targets(
    snapshot: &workers::ListWorkersResponse,
    targets: &[ReloadTarget],
) -> Result<bool, String> {
    for target in targets {
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == target.worker_id)
        else {
            return Ok(false);
        };
        let state = workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified);
        if state == workers::WorkerLifecycle::Unspecified {
            return Err(format!(
                "worker #{} has an unspecified lifecycle",
                target.worker_id
            ));
        }
        if state == workers::WorkerLifecycle::Crashed {
            return Err(format!(
                "worker #{} crashed during reload",
                target.worker_id
            ));
        }
        if worker.incarnation != target.incarnation {
            return Err(format!(
                "worker #{} changed incarnation during reload: expected {}, observed {}",
                target.worker_id, target.incarnation, worker.incarnation
            ));
        }
        if state != workers::WorkerLifecycle::Running {
            return Ok(false);
        }
        require_owned_running(worker, false)
            .map_err(|error| format!("worker #{} {error}", target.worker_id))?;
        if worker.reload_revision <= target.reload_revision {
            return Ok(false);
        }
    }
    Ok(true)
}

fn observe_restart_targets(
    snapshot: &workers::ListWorkersResponse,
    targets: &[RestartTarget],
) -> Result<bool, String> {
    for target in targets {
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == target.worker_id)
        else {
            return Ok(false);
        };
        let state = workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified);
        if state == workers::WorkerLifecycle::Unspecified {
            return Err(format!(
                "worker #{} has an unspecified lifecycle",
                target.worker_id
            ));
        }
        if state == workers::WorkerLifecycle::Crashed {
            return Err(format!(
                "worker #{} crashed during rolling restart",
                target.worker_id
            ));
        }
        if state != workers::WorkerLifecycle::Running
            || worker.incarnation <= target.incarnation
        {
            return Ok(false);
        }
        require_owned_running(worker, true)
            .map_err(|error| format!("worker #{} {error}", target.worker_id))?;
    }
    Ok(true)
}

fn observe_resize(
    snapshot: &workers::ListWorkersResponse,
    expectation: &ResizeExpectation,
) -> Result<bool, String> {
    if snapshot.workers_expected != expectation.target_worker_count
        || snapshot.workers.len() != expectation.target_worker_count as usize
    {
        return Ok(false);
    }
    for worker_id in 1..=expectation.target_worker_count {
        if !snapshot.workers.iter().any(|worker| worker.worker_id == worker_id)
        {
            return Ok(false);
        }
    }
    if let Some(worker_id) = expectation.removed_worker_id
        && snapshot.workers.iter().any(|worker| worker.worker_id == worker_id)
    {
        return Ok(false);
    }
    if let Some(worker_id) = expectation.added_worker_id {
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == worker_id)
        else {
            return Ok(false);
        };
        let lifecycle = workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified);
        if lifecycle == workers::WorkerLifecycle::Crashed {
            return Err(format!(
                "new worker #{worker_id} crashed during fleet growth"
            ));
        }
        if lifecycle != workers::WorkerLifecycle::Running {
            return Ok(false);
        }
        require_owned_running(worker, true)
            .map_err(|error| format!("new worker #{worker_id} {error}"))?;
    }
    match &expectation.retained {
        RetainedWorkers::Replaced(targets) => {
            observe_restart_targets(snapshot, targets)
        }
        RetainedWorkers::Unchanged(targets) => {
            observe_unchanged_targets(snapshot, targets)
        }
    }
}

/// Every retained worker still runs under supervisor ownership with the same
/// incarnation it had when the resize was accepted; a replacement would mean
/// the master rolled a worker the shrink was not supposed to touch.
fn observe_unchanged_targets(
    snapshot: &workers::ListWorkersResponse,
    targets: &[RestartTarget],
) -> Result<bool, String> {
    for target in targets {
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == target.worker_id)
        else {
            return Ok(false);
        };
        let state = workers::WorkerLifecycle::try_from(worker.lifecycle)
            .unwrap_or(workers::WorkerLifecycle::Unspecified);
        if state == workers::WorkerLifecycle::Unspecified {
            return Err(format!(
                "worker #{} has an unspecified lifecycle",
                target.worker_id
            ));
        }
        if state == workers::WorkerLifecycle::Crashed {
            return Err(format!(
                "worker #{} crashed during fleet shrink",
                target.worker_id
            ));
        }
        if worker.incarnation != target.incarnation {
            return Err(format!(
                "worker #{} was replaced during fleet shrink: expected incarnation {}, observed {}",
                target.worker_id, target.incarnation, worker.incarnation
            ));
        }
        if state != workers::WorkerLifecycle::Running {
            return Ok(false);
        }
        require_owned_running(worker, false)
            .map_err(|error| format!("worker #{} {error}", target.worker_id))?;
    }
    Ok(true)
}

fn describe_targets(
    snapshot: &workers::ListWorkersResponse,
    targets: &[ReloadTarget],
) -> String {
    targets
        .iter()
        .map(|target| {
            snapshot
                .workers
                .iter()
                .find(|worker| worker.worker_id == target.worker_id)
                .map(|worker| {
                    format!(
                        "worker:{}={}/ownership:{}/pid:{}/reloads:{}",
                        worker.worker_id,
                        lifecycle(worker.lifecycle),
                        ownership(worker.ownership),
                        pid_label(worker),
                        worker.reload_revision
                    )
                })
                .unwrap_or_else(|| {
                    format!("worker:{}=missing", target.worker_id)
                })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_restart_targets(
    snapshot: &workers::ListWorkersResponse,
    targets: &[RestartTarget],
) -> String {
    targets
        .iter()
        .map(|target| {
            snapshot
                .workers
                .iter()
                .find(|worker| worker.worker_id == target.worker_id)
                .map(|worker| {
                    format!(
                        "worker:{}={}/ownership:{}/pid:{}/incarnation:{}",
                        worker.worker_id,
                        lifecycle(worker.lifecycle),
                        ownership(worker.ownership),
                        pid_label(worker),
                        worker.incarnation
                    )
                })
                .unwrap_or_else(|| {
                    format!("worker:{}=missing", target.worker_id)
                })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_resize(snapshot: &workers::ListWorkersResponse) -> String {
    let workers = snapshot
        .workers
        .iter()
        .map(|worker| {
            format!(
                "worker:{}={}/ownership:{}/pid:{}/incarnation:{}",
                worker.worker_id,
                lifecycle(worker.lifecycle),
                ownership(worker.ownership),
                pid_label(worker),
                worker.incarnation
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("configured={}, {workers}", snapshot.workers_expected)
}

fn observe(
    worker: &workers::WorkerStatus,
    desired: DesiredState,
) -> Result<bool, String> {
    let state = workers::WorkerLifecycle::try_from(worker.lifecycle)
        .unwrap_or(workers::WorkerLifecycle::Unspecified);
    if state == workers::WorkerLifecycle::Unspecified {
        return Err("has an unspecified lifecycle".to_string());
    }
    if state == workers::WorkerLifecycle::Crashed {
        return Err(format!("crashed while waiting for {}", desired.label()));
    }
    Ok(match desired {
        DesiredState::Started => {
            state == workers::WorkerLifecycle::Running
                && require_owned_running(worker, true)?
        }
        DesiredState::Stopped => {
            state == workers::WorkerLifecycle::Stopped
                && require_stopped_unowned(worker)?
        }
        DesiredState::ReloadedAfter { revision, incarnation } => {
            if worker.incarnation != incarnation {
                return Err(format!(
                    "changed incarnation during reload: expected {incarnation}, observed {}",
                    worker.incarnation
                ));
            }
            state == workers::WorkerLifecycle::Running
                && require_owned_running(worker, false)?
                && worker.reload_revision > revision
        }
        DesiredState::RestartedAfter(incarnation) => {
            state == workers::WorkerLifecycle::Running
                && worker.incarnation > incarnation
                && require_owned_running(worker, true)?
        }
    })
}

fn require_owned_running(
    worker: &workers::WorkerStatus,
    spawned_only: bool,
) -> Result<bool, String> {
    let ownership = workers::WorkerOwnership::try_from(worker.ownership)
        .unwrap_or(workers::WorkerOwnership::Unspecified);
    match ownership {
        workers::WorkerOwnership::Unspecified => {
            return Err("has an unspecified ownership".to_owned());
        }
        workers::WorkerOwnership::Unowned => {
            return Err(
                "is running without supervisor process ownership".to_owned()
            );
        }
        workers::WorkerOwnership::Adopted if spawned_only => {
            return Err("is adopted rather than spawned by the current master"
                .to_owned());
        }
        workers::WorkerOwnership::Spawned
        | workers::WorkerOwnership::Adopted => {}
    }
    if pid(worker).is_none() {
        return Err("is owned and running without a PID".to_owned());
    }
    Ok(true)
}

fn require_stopped_unowned(
    worker: &workers::WorkerStatus
) -> Result<bool, String> {
    let ownership = workers::WorkerOwnership::try_from(worker.ownership)
        .unwrap_or(workers::WorkerOwnership::Unspecified);
    if ownership == workers::WorkerOwnership::Unspecified {
        return Err("has an unspecified ownership".to_owned());
    }
    if ownership != workers::WorkerOwnership::Unowned {
        return Err(format!(
            "is stopped but still has {} process ownership",
            ownership_label(ownership)
        ));
    }
    if let Some(pid) = pid(worker) {
        return Err(format!("is stopped but still reports PID {pid}"));
    }
    Ok(true)
}

fn completion_message(
    worker: &workers::WorkerStatus,
    desired: DesiredState,
) -> Result<String, String> {
    if matches!(desired, DesiredState::Stopped) {
        return Ok(format!("✅ Worker #{} stopped", worker.worker_id));
    }
    let pid = pid(worker).ok_or_else(|| {
        format!("worker #{} completed without a PID", worker.worker_id)
    })?;
    Ok(format!(
        "✅ Worker #{} {} with PID {pid}",
        worker.worker_id,
        desired.label()
    ))
}

fn describe_completed_pids(
    snapshot: &workers::ListWorkersResponse,
    worker_ids: impl Iterator<Item = u32>,
) -> Result<String, String> {
    worker_ids
        .map(|worker_id| {
            let worker = snapshot
                .workers
                .iter()
                .find(|worker| worker.worker_id == worker_id)
                .ok_or_else(|| {
                    format!("completed worker #{worker_id} is missing")
                })?;
            let pid = pid(worker).ok_or_else(|| {
                format!("completed worker #{worker_id} has no PID")
            })?;
            Ok(format!("worker #{worker_id} PID {pid}"))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|workers| workers.join(", "))
}

async fn fetch(
    client: &mut WorkerManagerClient<Channel>
) -> Result<workers::ListWorkersResponse, Box<dyn std::error::Error>> {
    Ok(client
        .list_workers(tonic::Request::new(workers::ListWorkersRequest {}))
        .await?
        .into_inner())
}

async fn fetch_status(
    client: &mut WorkerManagerClient<Channel>,
    worker_id: u8,
) -> Result<workers::WorkerStatusResponse, Box<dyn std::error::Error>> {
    Ok(client
        .get_worker_status(tonic::Request::new(workers::WorkerStatusRequest {
            worker_id: u32::from(worker_id),
        }))
        .await?
        .into_inner())
}

async fn fetch_fresh(
    config: &ClientConfig
) -> Result<workers::ListWorkersResponse, String> {
    let channel = create_channel(config)
        .await
        .map_err(|error| format!("fresh connection failed: {error}"))?;
    fetch(&mut WorkerManagerClient::new(channel))
        .await
        .map_err(|error| format!("fresh status request failed: {error}"))
}

async fn fetch_status_fresh(
    config: &ClientConfig,
    worker_id: u8,
) -> Result<workers::WorkerStatusResponse, String> {
    let channel = create_channel(config)
        .await
        .map_err(|error| format!("fresh connection failed: {error}"))?;
    fetch_status(&mut WorkerManagerClient::new(channel), worker_id)
        .await
        .map_err(|error| format!("fresh status request failed: {error}"))
}

fn print_worker(worker: &workers::WorkerStatus) {
    println!("{}", format_worker(worker));
}

fn format_worker(worker: &workers::WorkerStatus) -> String {
    format!(
        "worker:{} | {} | ownership={} | pid={} | uptime={} | incarnation={} | recovery-attempts={} | reloads={}",
        worker.worker_id,
        lifecycle(worker.lifecycle),
        ownership(worker.ownership),
        pid_label(worker),
        worker.uptime.map(format_duration).unwrap_or_else(|| "-".to_owned()),
        worker.incarnation,
        worker.recovery_attempts,
        worker.reload_revision,
    )
}

fn pid(worker: &workers::WorkerStatus) -> Option<u32> {
    worker.process.as_ref().map(|process| process.pid)
}

fn pid_label(worker: &workers::WorkerStatus) -> String {
    pid(worker).map(|pid| pid.to_string()).unwrap_or_else(|| "-".to_owned())
}

fn format_header(
    master: Option<&workers::MasterIdentity>,
    fleet_mode: i32,
    build: Option<&workers::BuildIdentity>,
) -> String {
    let master = master
        .and_then(|master| {
            master.process.as_ref().map(|process| {
                format!(
                    "pid:{}/start_stamp:{:#x}/generation:{}",
                    process.pid, process.started_at, master.generation
                )
            })
        })
        .unwrap_or_else(|| "-".to_owned());
    let build = build
        .map(|build| {
            let revision = if build.revision.is_empty() {
                "unknown"
            } else {
                build.revision.as_str()
            };
            if build.dirty {
                format!("{revision}-dirty")
            } else {
                revision.to_owned()
            }
        })
        .unwrap_or_else(|| "-".to_owned());
    format!(
        "master={master} | fleet={} | build={build}",
        fleet_mode_label(
            workers::FleetMode::try_from(fleet_mode)
                .unwrap_or(workers::FleetMode::Unspecified)
        )
    )
}

/// One line per fact the concise line leaves out.
fn describe_detail(worker: &workers::WorkerStatus) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(match &worker.process {
        Some(process) => format!(
            "process: pid={} start_stamp={:#x}",
            process.pid, process.started_at
        ),
        None => "process: -".to_owned(),
    });
    lines.push(format!(
        "phase: {} for {}",
        phase_label(
            workers::WorkerPhase::try_from(worker.phase)
                .unwrap_or(workers::WorkerPhase::Unspecified)
        ),
        age(worker.phase_age)
    ));
    lines.push(format!(
        "desired: {} for {}",
        worker
            .desired
            .as_ref()
            .map(desired_label)
            .unwrap_or_else(|| "unspecified".to_owned()),
        age(worker.desired_age)
    ));
    lines.push(match &worker.operation {
        Some(operation) => format!(
            "operation: #{} {} by {}, running for {}",
            operation.id,
            operation_kind_label(operation),
            origin_label(operation),
            age(operation.age)
        ),
        None => "operation: -".to_owned(),
    });
    lines.push(match worker.heartbeat_age {
        Some(heartbeat) => {
            format!("heartbeat: {} ago", format_duration(heartbeat))
        }
        None => "heartbeat: -".to_owned(),
    });
    let block_reason =
        workers::WorkerBlockReason::try_from(worker.block_reason)
            .unwrap_or(workers::WorkerBlockReason::Unspecified);
    if block_reason != workers::WorkerBlockReason::Unspecified {
        lines.push(format!("blocked: {}", block_reason_label(block_reason)));
    }
    lines
}

fn age(value: Option<prost_types::Duration>) -> String {
    value.map(format_duration).unwrap_or_else(|| "-".to_owned())
}

fn desired_label(desired: &workers::WorkerDesiredState) -> String {
    use workers::worker_desired_state::State;

    match &desired.state {
        Some(State::EnsureRunning(_)) => "ensure_running".to_owned(),
        Some(State::Stopped(_)) => "stopped".to_owned(),
        Some(State::Drain(drain)) => format!(
            "drain({} -> {})",
            drain_reason_label(
                workers::WorkerDrainReason::try_from(drain.reason)
                    .unwrap_or(workers::WorkerDrainReason::Unspecified)
            ),
            exit_disposition_label(
                workers::WorkerExitDisposition::try_from(drain.after_exit)
                    .unwrap_or(workers::WorkerExitDisposition::Unspecified)
            )
        ),
        None => "unspecified".to_owned(),
    }
}

fn operation_kind_label(operation: &workers::WorkerOperation) -> String {
    match workers::WorkerOperationKind::try_from(operation.kind)
        .unwrap_or(workers::WorkerOperationKind::Unspecified)
    {
        workers::WorkerOperationKind::Start => "start".to_owned(),
        workers::WorkerOperationKind::Stop => "stop".to_owned(),
        workers::WorkerOperationKind::Restart => "restart".to_owned(),
        workers::WorkerOperationKind::Reload => "reload".to_owned(),
        workers::WorkerOperationKind::Rotation => "rotation".to_owned(),
        workers::WorkerOperationKind::HeartbeatRecovery => {
            "heartbeat_recovery".to_owned()
        }
        workers::WorkerOperationKind::CrashRecovery => {
            "crash_recovery".to_owned()
        }
        workers::WorkerOperationKind::ReloadAll => "reload_all".to_owned(),
        workers::WorkerOperationKind::RestartAll => "restart_all".to_owned(),
        workers::WorkerOperationKind::Resize => {
            format!("resize {}->{}", operation.resize_from, operation.resize_to)
        }
        workers::WorkerOperationKind::ConfigReload => {
            "config_reload".to_owned()
        }
        workers::WorkerOperationKind::FleetShutdown => {
            "fleet_shutdown".to_owned()
        }
        workers::WorkerOperationKind::Unspecified => "unspecified".to_owned(),
    }
}

fn origin_label(operation: &workers::WorkerOperation) -> String {
    use workers::worker_operation_origin::Origin;

    match operation.origin.as_ref().and_then(|origin| origin.origin.as_ref()) {
        Some(Origin::Management(management)) => {
            let via = management
                .via_worker
                .map(|worker| format!(" via worker {worker}"))
                .unwrap_or_default();
            format!(
                "management{via} (submission {})",
                submission_key(management.submission_key)
            )
        }
        Some(Origin::Supervisor(_)) => "supervisor".to_owned(),
        Some(Origin::Signal(_)) => "signal".to_owned(),
        None => "unspecified".to_owned(),
    }
}

fn fleet_mode_label(value: workers::FleetMode) -> &'static str {
    match value {
        workers::FleetMode::Running => "running",
        workers::FleetMode::ShuttingDown => "shutting_down",
        workers::FleetMode::Unspecified => "unspecified",
    }
}

fn phase_label(value: workers::WorkerPhase) -> &'static str {
    match value {
        workers::WorkerPhase::Unclaimed => "unclaimed",
        workers::WorkerPhase::OwnershipPending => "ownership_pending",
        workers::WorkerPhase::ReconciliationBlocked => "reconciliation_blocked",
        workers::WorkerPhase::Starting => "starting",
        workers::WorkerPhase::Running => "running",
        workers::WorkerPhase::Reloading => "reloading",
        workers::WorkerPhase::DrainAcknowledged => "drain_acknowledged",
        workers::WorkerPhase::TeardownCompleted => "teardown_completed",
        workers::WorkerPhase::ExitedPlanned => "exited_planned",
        workers::WorkerPhase::ExitedFailed => "exited_failed",
        workers::WorkerPhase::Unspecified => "unspecified",
    }
}

fn drain_reason_label(value: workers::WorkerDrainReason) -> &'static str {
    match value {
        workers::WorkerDrainReason::Shutdown => "shutdown",
        workers::WorkerDrainReason::ScaleDown => "scale_down",
        workers::WorkerDrainReason::Rotation => "rotation",
        workers::WorkerDrainReason::HeartbeatTimeout => "heartbeat_timeout",
        workers::WorkerDrainReason::ManualStop => "manual_stop",
        workers::WorkerDrainReason::RollingRestart => "rolling_restart",
        workers::WorkerDrainReason::Unspecified => "unspecified",
    }
}

fn exit_disposition_label(
    value: workers::WorkerExitDisposition
) -> &'static str {
    match value {
        workers::WorkerExitDisposition::StayStopped => "stay_stopped",
        workers::WorkerExitDisposition::Replace => "replace",
        workers::WorkerExitDisposition::RemoveSlot => "remove_slot",
        workers::WorkerExitDisposition::FinishFleetShutdown => {
            "finish_fleet_shutdown"
        }
        workers::WorkerExitDisposition::Unspecified => "unspecified",
    }
}

fn block_reason_label(value: workers::WorkerBlockReason) -> &'static str {
    match value {
        workers::WorkerBlockReason::StalePresence => "stale_presence",
        workers::WorkerBlockReason::WorkerNotRunning => "worker_not_running",
        workers::WorkerBlockReason::WorkerBusy => "worker_busy",
        workers::WorkerBlockReason::UnknownMaster => "unknown_master",
        workers::WorkerBlockReason::MasterStillAlive => "master_still_alive",
        workers::WorkerBlockReason::MasterNotOlder => "master_not_older",
        workers::WorkerBlockReason::IdentityChanged => "identity_changed",
        workers::WorkerBlockReason::Unspecified => "unspecified",
    }
}

fn lifecycle(value: i32) -> &'static str {
    lifecycle_label(
        workers::WorkerLifecycle::try_from(value)
            .unwrap_or(workers::WorkerLifecycle::Unspecified),
    )
}

fn lifecycle_label(value: workers::WorkerLifecycle) -> &'static str {
    match value {
        workers::WorkerLifecycle::Starting => "starting",
        workers::WorkerLifecycle::Running => "running",
        workers::WorkerLifecycle::Reloading => "reloading",
        workers::WorkerLifecycle::Stopping => "stopping",
        workers::WorkerLifecycle::Stopped => "stopped",
        workers::WorkerLifecycle::Exited => "exited",
        workers::WorkerLifecycle::Crashed => "crashed",
        workers::WorkerLifecycle::Unclaimed => "unclaimed",
        workers::WorkerLifecycle::Unspecified => "unspecified",
    }
}

fn ownership(value: i32) -> &'static str {
    ownership_label(
        workers::WorkerOwnership::try_from(value)
            .unwrap_or(workers::WorkerOwnership::Unspecified),
    )
}

fn ownership_label(value: workers::WorkerOwnership) -> &'static str {
    match value {
        workers::WorkerOwnership::Unowned => "unowned",
        workers::WorkerOwnership::Spawned => "spawned",
        workers::WorkerOwnership::Adopted => "adopted",
        workers::WorkerOwnership::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DesiredState, ReloadTarget, RestartTarget, RetainedWorkers,
        completion_message, describe_detail, format_header, format_worker,
        observe, observe_reload_targets, observe_resize,
        observe_restart_targets, reload_targets, resize_expectation,
        restart_targets,
    };
    use crate::proto::workers::worker_desired_state::State;
    use crate::proto::workers::worker_operation_origin::Origin;
    use crate::proto::workers::{
        BuildIdentity, FleetMode, ListWorkersResponse, MasterIdentity,
        ProcessIdentity, WorkerBlockReason, WorkerDesiredState, WorkerDrain,
        WorkerDrainReason, WorkerExitDisposition, WorkerLifecycle,
        WorkerManagementOrigin, WorkerOperation, WorkerOperationKind,
        WorkerOperationOrigin, WorkerOwnership, WorkerPhase, WorkerStatus,
    };

    fn process(pid: u32) -> ProcessIdentity {
        ProcessIdentity { pid, started_at: 0x0a0b }
    }

    #[test]
    fn stopped_requires_no_live_pid() {
        let mut worker = status(WorkerLifecycle::Stopped, 0);
        worker.process = Some(process(123));
        assert!(observe(&worker, DesiredState::Stopped).is_err());
        worker.process = None;
        assert!(observe(&worker, DesiredState::Stopped).unwrap());
    }

    #[test]
    fn stopped_requires_supervisor_to_release_process_ownership() {
        let mut worker = status(WorkerLifecycle::Stopped, 0);
        worker.ownership = WorkerOwnership::Adopted as i32;
        assert!(observe(&worker, DesiredState::Stopped).is_err());
    }

    #[test]
    fn reload_requires_a_new_completed_revision() {
        let mut worker = status(WorkerLifecycle::Running, 4);
        let desired =
            DesiredState::ReloadedAfter { revision: 4, incarnation: 1 };
        assert!(!observe(&worker, desired).unwrap());
        worker.reload_revision = 5;
        assert!(observe(&worker, desired).unwrap());
    }

    #[test]
    fn reload_accepts_the_same_adopted_incarnation() {
        let mut worker = status(WorkerLifecycle::Running, 5);
        worker.ownership = WorkerOwnership::Adopted as i32;
        assert!(
            observe(
                &worker,
                DesiredState::ReloadedAfter { revision: 4, incarnation: 1 }
            )
            .unwrap()
        );
    }

    #[test]
    fn reload_rejects_a_different_incarnation() {
        let mut worker = status(WorkerLifecycle::Running, 5);
        worker.incarnation = 2;
        assert!(
            observe(
                &worker,
                DesiredState::ReloadedAfter { revision: 4, incarnation: 1 }
            )
            .is_err()
        );
    }

    #[test]
    fn restart_requires_a_new_running_incarnation() {
        let mut worker = status(WorkerLifecycle::Starting, 0);
        worker.incarnation = 5;
        assert!(!observe(&worker, DesiredState::RestartedAfter(4)).unwrap());
        worker.lifecycle = WorkerLifecycle::Running as i32;
        assert!(observe(&worker, DesiredState::RestartedAfter(4)).unwrap());
    }

    #[test]
    fn restart_requires_current_master_spawned_ownership() {
        let mut worker = status(WorkerLifecycle::Running, 0);
        worker.incarnation = 5;
        worker.ownership = WorkerOwnership::Adopted as i32;
        assert!(observe(&worker, DesiredState::RestartedAfter(4)).is_err());
    }

    #[test]
    fn restart_result_names_the_pid_observed_from_the_supervisor() {
        let mut worker = status(WorkerLifecycle::Running, 0);
        worker.incarnation = 5;
        worker.process = Some(process(6666));

        assert_eq!(
            completion_message(&worker, DesiredState::RestartedAfter(4))
                .unwrap(),
            "✅ Worker #1 restarted with PID 6666"
        );
    }

    #[test]
    fn crash_is_a_terminal_poll_error() {
        let worker = status(WorkerLifecycle::Crashed, 0);
        assert!(observe(&worker, DesiredState::Started).is_err());
    }

    #[test]
    fn reload_all_tracks_only_workers_running_at_acceptance() {
        let snapshot = snapshot(vec![
            status_with_id(1, WorkerLifecycle::Running, 4),
            status_with_id(2, WorkerLifecycle::Stopped, 9),
            status_with_id(3, WorkerLifecycle::Running, 7),
        ]);
        assert_eq!(
            reload_targets(&snapshot).unwrap(),
            vec![
                ReloadTarget {
                    worker_id: 1,
                    incarnation: 1,
                    reload_revision: 4
                },
                ReloadTarget {
                    worker_id: 3,
                    incarnation: 1,
                    reload_revision: 7
                },
            ]
        );
    }

    #[test]
    fn reload_all_requires_every_target_revision_to_advance() {
        let targets = vec![
            ReloadTarget { worker_id: 1, incarnation: 1, reload_revision: 4 },
            ReloadTarget { worker_id: 2, incarnation: 1, reload_revision: 7 },
        ];
        let pending = snapshot(vec![
            status_with_id(1, WorkerLifecycle::Running, 5),
            status_with_id(2, WorkerLifecycle::Running, 7),
        ]);
        assert!(!observe_reload_targets(&pending, &targets).unwrap());

        let complete = snapshot(vec![
            status_with_id(1, WorkerLifecycle::Running, 5),
            status_with_id(2, WorkerLifecycle::Running, 8),
        ]);
        assert!(observe_reload_targets(&complete, &targets).unwrap());
    }

    #[test]
    fn reload_all_fails_when_a_target_crashes() {
        let targets = vec![ReloadTarget {
            worker_id: 1,
            incarnation: 1,
            reload_revision: 4,
        }];
        let snapshot =
            snapshot(vec![status_with_id(1, WorkerLifecycle::Crashed, 4)]);
        assert!(observe_reload_targets(&snapshot, &targets).is_err());
    }

    #[test]
    fn restart_all_tracks_running_incarnations_and_preserves_stopped_slots() {
        let mut first = status_with_id(1, WorkerLifecycle::Running, 0);
        first.incarnation = 4;
        let mut stopped = status_with_id(2, WorkerLifecycle::Stopped, 0);
        stopped.incarnation = 3;
        let mut third = status_with_id(3, WorkerLifecycle::Running, 0);
        third.incarnation = 7;
        let snapshot = snapshot(vec![first, stopped, third]);
        assert_eq!(
            restart_targets(&snapshot).unwrap(),
            vec![
                RestartTarget { worker_id: 1, incarnation: 4 },
                RestartTarget { worker_id: 3, incarnation: 7 },
            ]
        );
    }

    #[test]
    fn restart_all_requires_every_target_incarnation_to_advance() {
        let targets = vec![
            RestartTarget { worker_id: 1, incarnation: 4 },
            RestartTarget { worker_id: 2, incarnation: 7 },
        ];
        let mut first = status_with_id(1, WorkerLifecycle::Running, 0);
        first.incarnation = 5;
        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 7;
        assert!(
            !observe_restart_targets(&snapshot(vec![first, second]), &targets)
                .unwrap()
        );
        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 8;
        assert!(
            observe_restart_targets(&snapshot(vec![first, second]), &targets)
                .unwrap()
        );
    }

    #[test]
    fn fleet_growth_waits_for_new_worker_and_retained_incarnations() {
        let mut first = status_with_id(1, WorkerLifecycle::Running, 0);
        first.incarnation = 4;
        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 7;
        let baseline = snapshot(vec![first, second]);
        let expectation = resize_expectation(&baseline, 2, 3).unwrap();

        let mut first = status_with_id(1, WorkerLifecycle::Running, 0);
        first.incarnation = 5;
        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 7;
        let third = status_with_id(3, WorkerLifecycle::Running, 0);
        assert!(
            !observe_resize(
                &snapshot(vec![first, second, third]),
                &expectation
            )
            .unwrap()
        );

        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 8;
        assert!(
            observe_resize(&snapshot(vec![first, second, third]), &expectation)
                .unwrap()
        );
    }

    /// Shrink retires only the highest slot. The retained workers keep their
    /// incarnations; one that changed was rolled, which the shrink must not do.
    #[test]
    fn fleet_shrink_drops_the_highest_slot_and_keeps_retained_workers() {
        let mut first = status_with_id(1, WorkerLifecycle::Running, 0);
        first.incarnation = 4;
        let mut second = status_with_id(2, WorkerLifecycle::Running, 0);
        second.incarnation = 7;
        let third = status_with_id(3, WorkerLifecycle::Running, 0);
        let baseline = snapshot(vec![first, second, third]);
        let expectation = resize_expectation(&baseline, 3, 2).unwrap();
        assert!(
            matches!(&expectation.retained, RetainedWorkers::Unchanged(targets) if targets.len() == 2)
        );

        assert!(
            !observe_resize(
                &snapshot(vec![first, second, third]),
                &expectation
            )
            .unwrap(),
            "the removed slot is still present"
        );
        assert!(
            observe_resize(&snapshot(vec![first, second]), &expectation)
                .unwrap()
        );

        let mut rolled = status_with_id(2, WorkerLifecycle::Running, 0);
        rolled.incarnation = 8;
        assert!(
            observe_resize(&snapshot(vec![first, rolled]), &expectation)
                .is_err(),
            "a retained worker with a new incarnation was replaced, not retained"
        );
    }

    #[test]
    fn status_detail_explains_the_slot_without_the_master_log() {
        let mut worker = status_with_id(1, WorkerLifecycle::Stopping, 2);
        worker.phase = WorkerPhase::DrainAcknowledged as i32;
        worker.phase_age = Some(prost_types::Duration { seconds: 3, nanos: 0 });
        worker.desired = Some(WorkerDesiredState {
            state: Some(State::Drain(WorkerDrain {
                reason: WorkerDrainReason::RollingRestart as i32,
                after_exit: WorkerExitDisposition::FinishFleetShutdown as i32,
            })),
        });
        worker.desired_age =
            Some(prost_types::Duration { seconds: 12, nanos: 0 });
        worker.operation = Some(WorkerOperation {
            id: 9,
            kind: WorkerOperationKind::Resize as i32,
            resize_from: 3,
            resize_to: 4,
            origin: Some(WorkerOperationOrigin {
                origin: Some(Origin::Management(WorkerManagementOrigin {
                    via_worker: Some(2),
                    submission_key: 0x0200_0197_0000_0001,
                })),
            }),
            age: Some(prost_types::Duration { seconds: 0, nanos: 250_000_000 }),
        });
        worker.heartbeat_age =
            Some(prost_types::Duration { seconds: 0, nanos: 700_000_000 });

        assert_eq!(
            describe_detail(&worker),
            [
                "process: pid=6001 start_stamp=0xa0b",
                "phase: drain_acknowledged for 3s",
                "desired: drain(rolling_restart -> finish_fleet_shutdown) for 12s",
                "operation: #9 resize 3->4 by management via worker 2 (submission 0200019700000001), running for 0.250000000s",
                "heartbeat: 0.700000000s ago",
            ]
        );

        let mut blocked = status_with_id(3, WorkerLifecycle::Unclaimed, 0);
        blocked.ownership = WorkerOwnership::Unowned as i32;
        blocked.process = Some(process(31_337));
        blocked.phase = WorkerPhase::ReconciliationBlocked as i32;
        blocked.block_reason = WorkerBlockReason::MasterStillAlive as i32;
        let detail = describe_detail(&blocked);
        assert_eq!(detail[0], "process: pid=31337 start_stamp=0xa0b");
        assert_eq!(detail.last().unwrap(), "blocked: master_still_alive");

        assert_eq!(
            format_header(
                Some(&MasterIdentity {
                    process: Some(process(4242)),
                    generation: 7
                }),
                FleetMode::ShuttingDown as i32,
                Some(&BuildIdentity {
                    revision: "016ff434".to_owned(),
                    dirty: true
                })
            ),
            "master=pid:4242/start_stamp:0xa0b/generation:7 | fleet=shutting_down | build=016ff434-dirty"
        );
        assert_eq!(
            format_header(None, FleetMode::Unspecified as i32, None),
            "master=- | fleet=unspecified | build=-"
        );
    }

    #[test]
    fn worker_output_exposes_typed_ownership() {
        let mut worker = status(WorkerLifecycle::Running, 2);
        worker.worker_id = 4;
        worker.process = Some(process(6666));
        worker.ownership = WorkerOwnership::Adopted as i32;

        assert_eq!(
            format_worker(&worker),
            "worker:4 | running | ownership=adopted | pid=6666 | uptime=- | incarnation=1 | recovery-attempts=0 | reloads=2"
        );
    }

    fn status(
        lifecycle: WorkerLifecycle,
        reload_revision: u64,
    ) -> WorkerStatus {
        status_with_id(1, lifecycle, reload_revision)
    }

    fn status_with_id(
        worker_id: u32,
        lifecycle: WorkerLifecycle,
        reload_revision: u64,
    ) -> WorkerStatus {
        let live = matches!(
            lifecycle,
            WorkerLifecycle::Starting
                | WorkerLifecycle::Running
                | WorkerLifecycle::Reloading
                | WorkerLifecycle::Stopping
                | WorkerLifecycle::Crashed
                | WorkerLifecycle::Unclaimed
        );
        WorkerStatus {
            worker_id,
            lifecycle: lifecycle as i32,
            uptime: None,
            recovery_attempts: 0,
            reload_revision,
            incarnation: 1,
            ownership: if live {
                WorkerOwnership::Spawned as i32
            } else {
                WorkerOwnership::Unowned as i32
            },
            process: live.then(|| process(6_000 + worker_id)),
            phase: 0,
            phase_age: None,
            desired: None,
            desired_age: None,
            operation: None,
            heartbeat_age: None,
            block_reason: 0,
        }
    }

    fn snapshot(workers: Vec<WorkerStatus>) -> ListWorkersResponse {
        ListWorkersResponse {
            captured_at: None,
            workers_expected: workers.len() as u32,
            workers,
            master: None,
            fleet_mode: 0,
            build: None,
        }
    }
}
