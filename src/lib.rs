//! Shared configuration and logging for independently packaged operator tools.

#[cfg(feature = "config")]
pub mod config;

use tracing_subscriber::EnvFilter;

/// Initializes the standard logger used by Claviron client tools.
pub fn init_client_logger() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(true)
        .without_time()
        .init();
}
