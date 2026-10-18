//! Hosting database selection and connections for client configuration profiles.

use std::path::PathBuf;

use claviron::Error;

use super::{CliConfig, CliPostgresConfig};

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct HostingProfile {
    #[serde(default)]
    pub default: bool,
    pub hosting: Option<HostingDatabase>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "driver", rename_all = "lowercase")]
pub enum HostingDatabase {
    Postgres {
        #[serde(flatten)]
        connection: CliPostgresConfig,
    },
    Sqlite {
        path: PathBuf,
    },
}

pub enum HostingPool {
    Postgres(sqlx::PgPool),
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
}

impl CliConfig {
    pub fn hosting_database(
        &self,
        name: Option<&str>,
    ) -> claviron::Result<&HostingDatabase> {
        if let Some(name) = name {
            return self
                .profiles
                .get(name)
                .and_then(|p| p.hosting.as_ref())
                .ok_or_else(|| {
                    Error::Cli(format!(
                        "profile '{name}' has no hosting database"
                    ))
                });
        }
        let profiles = self
            .profiles
            .values()
            .filter(|p| p.hosting.is_some())
            .collect::<Vec<_>>();
        let defaults =
            profiles.iter().filter(|p| p.default).collect::<Vec<_>>();
        let selected = match (defaults.as_slice(), profiles.as_slice()) {
            ([profile], _) => *profile,
            ([], [profile]) => profile,
            _ => return Err(Error::Cli("select a hosting profile with --server; configure profiles.<name>.hosting".into()))
        };
        Ok(selected.hosting.as_ref().expect("filtered above"))
    }

    /// Explicit profile connections never inherit an unrelated DATABASE_URL.
    pub async fn hosting_pool(
        &self,
        name: Option<&str>,
    ) -> claviron::Result<HostingPool> {
        match self.hosting_database(name)? {
            HostingDatabase::Postgres { connection } => {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .connect(&connection.url())
                    .await
                    .map_err(|_| {
                        Error::Cli(
                            "could not connect to hosting PostgreSQL profile"
                                .into(),
                        )
                    })?;
                Ok(HostingPool::Postgres(pool))
            }
            HostingDatabase::Sqlite { path } => {
                #[cfg(feature = "sqlite")]
                {
                    let options = sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(path)
                        .create_if_missing(false)
                        .foreign_keys(true);
                    let pool = sqlx::sqlite::SqlitePoolOptions::new().connect_with(options).await
                        .map_err(|_| Error::Cli(format!("could not open existing hosting SQLite database {}", path.display())))?;
                    Ok(HostingPool::Sqlite(pool))
                }
                #[cfg(not(feature = "sqlite"))]
                Err(Error::Cli(format!(
                    "SQLite support is not enabled for {}",
                    path.display()
                )))
            }
        }
    }
}
