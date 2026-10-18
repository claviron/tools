//! Shared client configuration, with optional ACME and hosting database support.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use claviron::Error;
#[cfg(feature = "db")]
mod hosting;
#[cfg(feature = "acme")]
use claviron_acme::AcmeConfig;
#[cfg(feature = "db")]
pub use hosting::{HostingDatabase, HostingPool, HostingProfile};
#[cfg(feature = "db")]
use sqlx::PgPool;
#[cfg(feature = "db")]
use sqlx::postgres::PgPoolOptions;

#[cfg(feature = "db")]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct CliPostgresConfig {
    #[serde(default)]
    pub url: Option<String>,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[cfg(feature = "db")]
impl Default for CliPostgresConfig {
    fn default() -> Self {
        Self {
            url: None,
            host: "127.0.0.1".to_string(),
            port: 5432,
            database: "roma".to_string(),
            username: "claviron".to_string(),
            password: String::new(),
        }
    }
}

#[cfg(feature = "db")]
impl CliPostgresConfig {
    pub fn url(&self) -> String {
        if let Some(url) = &self.url {
            return url.clone();
        }
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.username, self.password, self.host, self.port, self.database
        )
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CliConfig {
    #[cfg(feature = "db")]
    #[serde(default)]
    pub profiles: std::collections::BTreeMap<String, HostingProfile>,
    #[cfg(feature = "acme")]
    pub acme: AcmeConfig,
    #[cfg(feature = "db")]
    #[serde(default)]
    pub postgres: CliPostgresConfig,
}

impl CliConfig {
    pub fn resolve_path(path: Option<PathBuf>) -> PathBuf {
        path.unwrap_or_else(default_path)
    }

    pub async fn load(path: &Path) -> claviron::Result<Self> {
        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            Error::Cli(format!(
                "failed to read client config {}: {}",
                path.display(),
                e
            ))
        })?;

        let config =
            serde_yaml::from_str::<Self>(&content).map_err(|primary| {
                Error::Cli(format!(
                    "failed to parse client config {}: {}",
                    path.display(),
                    primary
                ))
            })?;
        #[cfg(feature = "db")]
        let mut config = config;
        #[cfg(feature = "db")]
        for profile in config.profiles.values_mut() {
            if let Some(HostingDatabase::Sqlite { path: db_path }) =
                &mut profile.hosting
                && db_path.is_relative()
            {
                *db_path = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(&*db_path);
            }
        }
        Ok(config)
    }

    #[cfg(feature = "acme")]
    pub fn acme_cfg(&self) -> &AcmeConfig {
        &self.acme
    }

    #[cfg(feature = "db")]
    pub async fn db_pool(&self) -> claviron::Result<PgPool> {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| self.postgres.url());
        let connect = PgPoolOptions::new()
            .max_connections(2)
            .min_connections(0)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(&database_url);
        tokio::time::timeout(std::time::Duration::from_secs(5), connect)
            .await
            .map_err(|_| {
                Error::Cli("postgres connect timed out after 5s".to_string())
            })?
            .map_err(|e| Error::Cli(format!("postgres connect failed: {}", e)))
    }
}

fn default_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("claviron").join("client.yaml"))
        .filter(|path| path.is_file())
        .or_else(|| {
            dirs::home_dir()
                .map(|home| {
                    home.join(".config").join("claviron").join("client.yaml")
                })
                .filter(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from(".").join("client.yaml"))
}
