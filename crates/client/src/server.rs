use std::error::Error as _;

use tonic::Code;
use tonic::transport::Channel;

use crate::client::ConnectionMonitor;
use crate::common::CommandResult;
use crate::proto::server::server_manager_client::ServerManagerClient;
use crate::proto::server::{
    InfoRequest, ReloadRequest, RunMode, ShutdownRequest,
};

pub async fn info(channel: Channel) -> CommandResult {
    let info = ServerManagerClient::new(channel)
        .get_info(InfoRequest {})
        .await?
        .into_inner();
    let missing = || "not reported".to_string();
    println!("Server: {}", info.name);
    println!("Environment: {}", info.environment);
    println!("Fleet: {}", info.fleet_name.unwrap_or_else(missing));
    println!("Host: {}", info.hostname);
    println!("Claviron: {}", info.claviron_version);
    println!(
        "Application: {}",
        info.application_version.unwrap_or_else(missing)
    );
    println!("Instance: {}", info.instance_id.unwrap_or_else(missing));
    println!(
        "Workers: {} running / {} desired",
        info.workers_running.map(|n| n.to_string()).unwrap_or_else(missing),
        info.workers_desired.map(|n| n.to_string()).unwrap_or_else(missing)
    );
    println!(
        "Server started: {}",
        crate::common::format_timestamp(info.started_at)
    );
    println!(
        "Server uptime: {}",
        info.uptime.map(crate::common::format_duration).unwrap_or_else(missing)
    );
    println!(
        "Server reload revision: {}",
        info.reload_revision.map(|n| n.to_string()).unwrap_or_else(missing)
    );
    if let Some(process) = info.responding_process {
        let role = RunMode::try_from(process.run_mode)
            .map(|mode| mode.as_str_name())
            .unwrap_or("UNKNOWN");
        println!("Responding process: {role} (PID {})", process.pid);
        if let Some(worker) = process.worker_id {
            println!("Worker ID: {worker}");
        }
        println!(
            "Process started: {}",
            crate::common::format_timestamp(process.started_at)
        );
        println!(
            "Process uptime: {}",
            process
                .uptime
                .map(crate::common::format_duration)
                .unwrap_or_else(missing)
        );
        println!("Process reload revision: {}", process.reload_revision);
    }
    println!("Capabilities: {}", info.capabilities.join(", "));
    Ok(())
}

pub async fn reload(channel: Channel) -> CommandResult {
    let response = ServerManagerClient::new(channel)
        .reload(tonic::Request::new(ReloadRequest {}))
        .await?
        .into_inner();
    println!("✅ Server reload accepted");
    println!("✅ Server reload completed (revision={})", response.revision);
    Ok(())
}

pub async fn shutdown(
    channel: Channel,
    monitor: ConnectionMonitor,
) -> CommandResult {
    let observed_closures = monitor.closure_count();
    let keep_alive = channel.clone();
    let result = ServerManagerClient::new(channel)
        .shutdown(tonic::Request::new(ShutdownRequest {}))
        .await;
    match result {
        Ok(_) => {
            println!(
                "✅ Server shutdown accepted; waiting for connection to close"
            );
            monitor.wait_for_close_after(observed_closures).await;
            drop(keep_alive);
            println!("✅ Server connection closed");
            Ok(())
        }
        Err(status) if is_shutdown_disconnect(&status) => {
            drop(keep_alive);
            println!("✅ Server connection closed");
            Ok(())
        }
        Err(status) => Err(status.into()),
    }
}

fn is_shutdown_disconnect(status: &tonic::Status) -> bool {
    status.source().is_some()
        && matches!(
            status.code(),
            Code::Cancelled
                | Code::Unknown
                | Code::Internal
                | Code::Unavailable
        )
}

#[cfg(test)]
mod tests {
    use super::is_shutdown_disconnect;

    #[test]
    fn only_transport_failures_are_accepted_during_shutdown() {
        let transport =
            tonic::Status::from_error(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            )));
        assert!(is_shutdown_disconnect(&transport));
        assert!(!is_shutdown_disconnect(&tonic::Status::unavailable(
            "server rejected shutdown"
        )));
    }
}
