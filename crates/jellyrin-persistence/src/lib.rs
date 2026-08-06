//! Backend-neutral persistence contracts.
//!
//! SQL drivers, pools, rows, placeholders, and dialect-specific errors must stay in adapter
//! crates. Application and API crates consume the small contracts defined here instead.

use std::{fmt, str::FromStr};

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

/// Storage engine selected for one Jellyrin installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersistenceBackend {
    Sqlite,
    Postgres,
}

impl PersistenceBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgresql",
        }
    }
}

impl fmt::Display for PersistenceBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for PersistenceBackend {
    type Err = ParsePersistenceBackendError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" | "postgresql" => Ok(Self::Postgres),
            _ => Err(ParsePersistenceBackendError {
                value: value.trim().to_string(),
            }),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unsupported persistence backend `{value}`; expected `sqlite` or `postgresql`")]
pub struct ParsePersistenceBackendError {
    value: String,
}

/// Operational features exposed for diagnostics and migration tooling.
///
/// Jellyfin-compatible HTTP behavior must not branch on these flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceCapabilities {
    pub transactional_migrations: bool,
    pub concurrent_writes: bool,
    pub online_backup: bool,
}

/// A successful value means the adapter completed a real database round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceHealth {
    pub backend: PersistenceBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaStatus {
    pub latest_applied_migration: Option<i64>,
    pub failed_migrations: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamedConfiguration {
    pub key: String,
    pub payload: Value,
}

/// Backend-neutral persisted user profile.
///
/// Credentials and authentication policy intentionally live outside this record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub id: Uuid,
    pub name: String,
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub sync_play_access: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfileUpdate {
    pub id: Uuid,
    pub name: String,
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub sync_play_access: String,
    pub updated_at: OffsetDateTime,
}

/// Password material persisted by an adapter.
///
/// This value is secret and must never be logged or returned by diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordCredential {
    pub user_id: Uuid,
    pub algorithm: String,
    pub password_hash: String,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemConfiguration {
    pub content_types: Value,
    pub metadata_options: Value,
    pub path_substitutions: Value,
    pub plugin_repositories: Value,
    pub server_options: Value,
}

impl Default for SystemConfiguration {
    fn default() -> Self {
        Self {
            content_types: Value::Array(Vec::new()),
            metadata_options: Value::Array(Vec::new()),
            path_substitutions: Value::Array(Vec::new()),
            plugin_repositories: Value::Array(Vec::new()),
            server_options: Value::Object(Default::default()),
        }
    }
}

/// Stable error categories shared by every storage adapter.
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("{entity} was not found")]
    NotFound { entity: &'static str },
    #[error("persistence conflict: {message}")]
    Conflict { message: String },
    #[error("persistence constraint violation: {message}")]
    Constraint { message: String },
    #[error("persistence backend is busy: {message}")]
    Busy { message: String },
    #[error("persistence operation timed out: {message}")]
    Timeout { message: String },
    #[error("persistence backend is unavailable: {message}")]
    Unavailable { message: String },
    #[error("persistence migration failed: {message}")]
    Migration { message: String },
    #[error("internal persistence failure: {message}")]
    Internal { message: String },
}

impl PersistenceError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

/// Lifecycle and diagnostics surface shared by all persistence adapters.
///
/// Domain repositories will be extracted as separate traits; this deliberately stays small.
#[async_trait]
pub trait PersistenceControl: Send + Sync {
    fn backend(&self) -> PersistenceBackend;

    fn capabilities(&self) -> PersistenceCapabilities;

    async fn health(&self) -> Result<PersistenceHealth, PersistenceError>;

    async fn schema_status(&self) -> Result<SchemaStatus, PersistenceError>;
}

/// Storage operations for the server's named JSON configuration documents.
#[async_trait]
pub trait ConfigurationRepository: Send + Sync {
    async fn system_configuration(&self) -> Result<SystemConfiguration, PersistenceError>;

    async fn update_system_configuration(
        &self,
        configuration: SystemConfiguration,
    ) -> Result<(), PersistenceError>;

    async fn named_configuration(&self, key: &str) -> Result<Option<Value>, PersistenceError>;

    async fn named_configurations(&self) -> Result<Vec<NamedConfiguration>, PersistenceError>;

    async fn update_named_configuration(
        &self,
        key: &str,
        payload: Value,
    ) -> Result<(), PersistenceError>;
}

/// Storage operations for user profiles and their per-user JSON configuration.
///
/// Password hashing, token issuance, and administrator policy are application responsibilities and
/// are deliberately excluded from this repository.
#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn first_user(&self) -> Result<Option<UserRecord>, PersistenceError>;

    async fn users(&self) -> Result<Vec<UserRecord>, PersistenceError>;

    async fn user_by_id(&self, user_id: Uuid) -> Result<Option<UserRecord>, PersistenceError>;

    async fn user_by_name(&self, name: &str) -> Result<Option<UserRecord>, PersistenceError>;

    async fn user_configuration(&self, user_id: Uuid) -> Result<Option<Value>, PersistenceError>;

    async fn insert_user(&self, user: UserRecord) -> Result<(), PersistenceError>;

    async fn upsert_user_by_name(&self, user: UserRecord) -> Result<(), PersistenceError>;

    async fn update_user_profile(&self, update: UserProfileUpdate) -> Result<(), PersistenceError>;

    async fn delete_user(&self, user_id: Uuid) -> Result<(), PersistenceError>;

    async fn enabled_administrator_count(&self) -> Result<u64, PersistenceError>;

    async fn update_user_configuration(
        &self,
        user_id: Uuid,
        payload: Value,
    ) -> Result<(), PersistenceError>;
}

/// Storage-only password credential operations.
///
/// Hash generation and verification belong to the application layer.
#[async_trait]
pub trait CredentialRepository: Send + Sync {
    async fn credential(
        &self,
        user_id: Uuid,
    ) -> Result<Option<PasswordCredential>, PersistenceError>;

    async fn has_credential(&self, user_id: Uuid) -> Result<bool, PersistenceError>;

    async fn upsert_credential(
        &self,
        credential: PasswordCredential,
    ) -> Result<(), PersistenceError>;

    async fn delete_credential(&self, user_id: Uuid) -> Result<(), PersistenceError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        ConfigurationRepository, CredentialRepository, NamedConfiguration, PasswordCredential,
        PersistenceBackend, PersistenceCapabilities, PersistenceControl, PersistenceError,
        PersistenceHealth, SchemaStatus, SystemConfiguration, UserProfileUpdate, UserRecord,
        UserRepository,
    };
    use async_trait::async_trait;

    struct StubControl;

    struct StubConfiguration;

    struct StubUsers;

    struct StubCredentials;

    #[async_trait]
    impl PersistenceControl for StubControl {
        fn backend(&self) -> PersistenceBackend {
            PersistenceBackend::Sqlite
        }

        fn capabilities(&self) -> PersistenceCapabilities {
            PersistenceCapabilities {
                transactional_migrations: true,
                concurrent_writes: false,
                online_backup: true,
            }
        }

        async fn health(&self) -> Result<PersistenceHealth, PersistenceError> {
            Ok(PersistenceHealth {
                backend: self.backend(),
            })
        }

        async fn schema_status(&self) -> Result<SchemaStatus, PersistenceError> {
            Ok(SchemaStatus {
                latest_applied_migration: Some(1),
                failed_migrations: 0,
            })
        }
    }

    #[async_trait]
    impl ConfigurationRepository for StubConfiguration {
        async fn system_configuration(&self) -> Result<SystemConfiguration, PersistenceError> {
            Ok(SystemConfiguration::default())
        }

        async fn update_system_configuration(
            &self,
            _configuration: SystemConfiguration,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn named_configuration(
            &self,
            _key: &str,
        ) -> Result<Option<serde_json::Value>, PersistenceError> {
            Ok(None)
        }

        async fn named_configurations(&self) -> Result<Vec<NamedConfiguration>, PersistenceError> {
            Ok(Vec::new())
        }

        async fn update_named_configuration(
            &self,
            _key: &str,
            _payload: serde_json::Value,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }
    }

    #[async_trait]
    impl UserRepository for StubUsers {
        async fn first_user(&self) -> Result<Option<UserRecord>, PersistenceError> {
            Ok(None)
        }

        async fn users(&self) -> Result<Vec<UserRecord>, PersistenceError> {
            Ok(Vec::new())
        }

        async fn user_by_id(
            &self,
            _user_id: uuid::Uuid,
        ) -> Result<Option<UserRecord>, PersistenceError> {
            Ok(None)
        }

        async fn user_by_name(&self, _name: &str) -> Result<Option<UserRecord>, PersistenceError> {
            Ok(None)
        }

        async fn user_configuration(
            &self,
            _user_id: uuid::Uuid,
        ) -> Result<Option<serde_json::Value>, PersistenceError> {
            Ok(None)
        }

        async fn insert_user(&self, _user: UserRecord) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn upsert_user_by_name(&self, _user: UserRecord) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn update_user_profile(
            &self,
            _update: UserProfileUpdate,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn delete_user(&self, _user_id: uuid::Uuid) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn enabled_administrator_count(&self) -> Result<u64, PersistenceError> {
            Ok(0)
        }

        async fn update_user_configuration(
            &self,
            _user_id: uuid::Uuid,
            _payload: serde_json::Value,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }
    }

    #[async_trait]
    impl CredentialRepository for StubCredentials {
        async fn credential(
            &self,
            _user_id: uuid::Uuid,
        ) -> Result<Option<PasswordCredential>, PersistenceError> {
            Ok(None)
        }

        async fn has_credential(&self, _user_id: uuid::Uuid) -> Result<bool, PersistenceError> {
            Ok(false)
        }

        async fn upsert_credential(
            &self,
            _credential: PasswordCredential,
        ) -> Result<(), PersistenceError> {
            Ok(())
        }

        async fn delete_credential(&self, _user_id: uuid::Uuid) -> Result<(), PersistenceError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn persistence_control_is_object_safe() {
        let control: Arc<dyn PersistenceControl> = Arc::new(StubControl);

        assert_eq!(control.backend(), PersistenceBackend::Sqlite);
        assert_eq!(
            control.health().await.unwrap().backend,
            PersistenceBackend::Sqlite
        );
        assert_eq!(
            control.schema_status().await.unwrap(),
            SchemaStatus {
                latest_applied_migration: Some(1),
                failed_migrations: 0,
            }
        );
    }

    #[tokio::test]
    async fn configuration_repository_is_object_safe() {
        let repository: Arc<dyn ConfigurationRepository> = Arc::new(StubConfiguration);

        assert!(
            repository
                .named_configuration("network")
                .await
                .unwrap()
                .is_none()
        );
        repository
            .update_named_configuration("network", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            repository
                .system_configuration()
                .await
                .unwrap()
                .server_options,
            serde_json::json!({})
        );
    }

    #[tokio::test]
    async fn user_repository_is_object_safe() {
        let repository: Arc<dyn UserRepository> = Arc::new(StubUsers);
        let user_id = uuid::Uuid::new_v4();

        assert!(repository.first_user().await.unwrap().is_none());
        assert!(repository.user_by_id(user_id).await.unwrap().is_none());
        assert!(repository.users().await.unwrap().is_empty());
        repository
            .update_user_configuration(user_id, serde_json::json!({}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn credential_repository_is_object_safe() {
        let repository: Arc<dyn CredentialRepository> = Arc::new(StubCredentials);
        let user_id = uuid::Uuid::new_v4();

        assert!(!repository.has_credential(user_id).await.unwrap());
        assert!(repository.credential(user_id).await.unwrap().is_none());
        repository.delete_credential(user_id).await.unwrap();
    }

    #[test]
    fn backend_parser_accepts_supported_names_only() {
        assert_eq!(
            "sqlite".parse::<PersistenceBackend>().unwrap(),
            PersistenceBackend::Sqlite
        );
        assert_eq!(
            "postgres".parse::<PersistenceBackend>().unwrap(),
            PersistenceBackend::Postgres
        );
        assert_eq!(
            "PostgreSQL".parse::<PersistenceBackend>().unwrap(),
            PersistenceBackend::Postgres
        );
        assert!("mysql".parse::<PersistenceBackend>().is_err());
    }
}
