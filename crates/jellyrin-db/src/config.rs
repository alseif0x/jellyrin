use std::fmt;

use jellyrin_persistence::PersistenceBackend;
use thiserror::Error;

/// Validated database selection for one server process.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    backend: PersistenceBackend,
    url: String,
}

impl DatabaseConfig {
    pub fn new(
        backend: PersistenceBackend,
        url: impl Into<String>,
    ) -> Result<Self, DatabaseConfigError> {
        let url = url.into();
        let url_backend = backend_from_url(&url)?;
        if backend != url_backend {
            return Err(DatabaseConfigError::BackendUrlMismatch {
                backend,
                url_backend,
            });
        }
        Ok(Self { backend, url })
    }

    pub fn from_url(url: impl Into<String>) -> Result<Self, DatabaseConfigError> {
        let url = url.into();
        let backend = backend_from_url(&url)?;
        Ok(Self { backend, url })
    }

    pub const fn backend(&self) -> PersistenceBackend {
        self.backend
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("backend", &self.backend)
            .field("url", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DatabaseConfigError {
    #[error("database URL must start with sqlite:, postgres://, or postgresql://")]
    UnsupportedUrlScheme,
    #[error("configured database backend `{backend}` does not match URL backend `{url_backend}`")]
    BackendUrlMismatch {
        backend: PersistenceBackend,
        url_backend: PersistenceBackend,
    },
}

fn backend_from_url(url: &str) -> Result<PersistenceBackend, DatabaseConfigError> {
    let normalized = url.trim().to_ascii_lowercase();
    if normalized.starts_with("sqlite:") {
        Ok(PersistenceBackend::Sqlite)
    } else if normalized.starts_with("postgres://") || normalized.starts_with("postgresql://") {
        Ok(PersistenceBackend::Postgres)
    } else {
        Err(DatabaseConfigError::UnsupportedUrlScheme)
    }
}

#[cfg(test)]
mod tests {
    use jellyrin_persistence::PersistenceBackend;

    use super::{DatabaseConfig, DatabaseConfigError};

    #[test]
    fn infers_backend_from_supported_urls() {
        assert_eq!(
            DatabaseConfig::from_url("sqlite::memory:")
                .unwrap()
                .backend(),
            PersistenceBackend::Sqlite
        );
        assert_eq!(
            DatabaseConfig::from_url("postgresql://db.example/jellyrin")
                .unwrap()
                .backend(),
            PersistenceBackend::Postgres
        );
    }

    #[test]
    fn rejects_backend_url_mismatch() {
        assert_eq!(
            DatabaseConfig::new(PersistenceBackend::Sqlite, "postgres://db/jellyrin").unwrap_err(),
            DatabaseConfigError::BackendUrlMismatch {
                backend: PersistenceBackend::Sqlite,
                url_backend: PersistenceBackend::Postgres,
            }
        );
    }

    #[test]
    fn debug_output_redacts_database_credentials() {
        let config = DatabaseConfig::from_url("postgres://user:secret@db/jellyrin").unwrap();
        let debug = format!("{config:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }
}
