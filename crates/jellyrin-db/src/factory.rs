use jellyrin_persistence::PersistenceBackend;
use thiserror::Error;

use crate::{Database, DatabaseConfig};

#[derive(Debug, Error)]
pub enum DatabaseFactoryError {
    #[error("the `{backend}` persistence adapter is not available yet")]
    AdapterUnavailable { backend: PersistenceBackend },
    #[error("failed to initialize the `{backend}` persistence adapter")]
    Initialization {
        backend: PersistenceBackend,
        #[source]
        source: anyhow::Error,
    },
}

/// Connect the adapter selected by validated installation configuration.
pub async fn connect_database(config: &DatabaseConfig) -> Result<Database, DatabaseFactoryError> {
    match config.backend() {
        PersistenceBackend::Sqlite => {
            Database::connect_sqlite(config.url())
                .await
                .map_err(|source| DatabaseFactoryError::Initialization {
                    backend: PersistenceBackend::Sqlite,
                    source,
                })
        }
        PersistenceBackend::Postgres => Err(DatabaseFactoryError::AdapterUnavailable {
            backend: PersistenceBackend::Postgres,
        }),
    }
}

#[cfg(test)]
mod tests {
    use jellyrin_persistence::{PersistenceBackend, PersistenceControl};

    use super::{DatabaseFactoryError, connect_database};
    use crate::DatabaseConfig;

    #[tokio::test]
    async fn sqlite_adapter_is_selected_at_runtime() {
        let config = DatabaseConfig::new(PersistenceBackend::Sqlite, "sqlite::memory:").unwrap();
        let database = connect_database(&config).await.unwrap();

        assert_eq!(database.backend(), PersistenceBackend::Sqlite);
    }

    #[tokio::test]
    async fn postgres_is_not_claimed_before_its_adapter_exists() {
        let config = DatabaseConfig::new(
            PersistenceBackend::Postgres,
            "postgresql://db.example/jellyrin",
        )
        .unwrap();
        let error = match connect_database(&config).await {
            Ok(_) => panic!("PostgreSQL must not be reported as available yet"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            DatabaseFactoryError::AdapterUnavailable {
                backend: PersistenceBackend::Postgres
            }
        ));
    }
}
