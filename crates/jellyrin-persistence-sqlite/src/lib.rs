//! SQLite connection, migration, and control adapter.
//!
//! Domain queries still move incrementally out of the transitional `jellyrin-db` facade. This
//! crate already owns the SQLite lifecycle so application crates never construct a driver pool.

use anyhow::Context;
use jellyrin_persistence::{
    ConfigurationRepository, CredentialRepository, NamedConfiguration, PasswordCredential,
    PersistenceBackend, PersistenceCapabilities, PersistenceControl, PersistenceError,
    PersistenceHealth, SchemaStatus, SystemConfiguration, UserProfileUpdate, UserRecord,
    UserRepository,
};
use serde_json::Value;
use sqlx::{
    Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

// Keep this declaration in sync with the migration directory; changing it forces Cargo to
// rebuild the embedded migration set when a new migration file is added.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../jellyrin-db/migrations");
const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_MAX_CONNECTIONS: u32 = 5;
const CREDENTIAL_ACTIVITY_BUSY_TIMEOUT_MS: u64 = 100;

#[derive(Clone)]
pub struct SqlitePersistence {
    pool: SqlitePool,
    credential_activity_pool: Option<SqlitePool>,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct PasswordCredentialRow {
    user_id: String,
    algorithm: String,
    password_hash: String,
    updated_at: String,
}

impl SqlitePersistence {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let mut options = database_url
            .parse::<SqliteConnectOptions>()
            .context("failed to parse SQLite database URL")?
            .busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))
            .foreign_keys(true);
        let enable_wal = should_enable_wal(database_url);
        if enable_wal {
            options = options.journal_mode(SqliteJournalMode::Wal);
        }
        let credential_activity_options = enable_wal.then(|| {
            options
                .clone()
                .busy_timeout(std::time::Duration::from_millis(
                    CREDENTIAL_ACTIVITY_BUSY_TIMEOUT_MS,
                ))
        });

        let pool = SqlitePoolOptions::new()
            .max_connections(SQLITE_MAX_CONNECTIONS)
            .after_connect(|connection, _metadata| {
                Box::pin(async move { configure_sqlite_connection(connection).await })
            })
            .connect_with(options)
            .await
            .context("failed to connect SQLite database")?;

        MIGRATOR
            .run(&pool)
            .await
            .context("failed to run SQLite migrations")?;

        let credential_activity_pool = if let Some(options) = credential_activity_options {
            Some(
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await
                    .context("failed to connect SQLite credential activity pool")?,
            )
        } else {
            None
        };

        Ok(Self {
            pool,
            credential_activity_pool,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn credential_activity_pool(&self) -> Option<&SqlitePool> {
        self.credential_activity_pool.as_ref()
    }
}

#[async_trait::async_trait]
impl PersistenceControl for SqlitePersistence {
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
        let result = sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                tracing::error!(?error, "SQLite persistence health check failed");
                PersistenceError::unavailable("health check failed")
            })?;
        if result != 1 {
            return Err(PersistenceError::internal(
                "health check returned an unexpected result",
            ));
        }
        Ok(PersistenceHealth {
            backend: self.backend(),
        })
    }

    async fn schema_status(&self) -> Result<SchemaStatus, PersistenceError> {
        let latest_applied_migration = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(version) FROM _sqlx_migrations WHERE success = TRUE",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            tracing::error!(?error, "failed to read SQLite migration version");
            PersistenceError::unavailable("schema status query failed")
        })?;
        let failed_migrations = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM _sqlx_migrations WHERE success = FALSE",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            tracing::error!(?error, "failed to read failed SQLite migrations");
            PersistenceError::unavailable("schema status query failed")
        })?;

        Ok(SchemaStatus {
            latest_applied_migration,
            failed_migrations: u64::try_from(failed_migrations).map_err(|_| {
                PersistenceError::internal("schema status returned a negative migration count")
            })?,
        })
    }
}

#[async_trait::async_trait]
impl ConfigurationRepository for SqlitePersistence {
    async fn system_configuration(&self) -> Result<SystemConfiguration, PersistenceError> {
        let row = sqlx::query(
            r#"
            SELECT content_types_json, metadata_options_json, path_substitutions_json,
                plugin_repositories_json, server_options_json
            FROM system_configuration_payloads
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read system configuration"))?;

        let Some(row) = row else {
            return Ok(SystemConfiguration::default());
        };
        Ok(SystemConfiguration {
            content_types: array_payload(row.get("content_types_json"))?,
            metadata_options: array_payload(row.get("metadata_options_json"))?,
            path_substitutions: array_payload(row.get("path_substitutions_json"))?,
            plugin_repositories: array_payload(row.get("plugin_repositories_json"))?,
            server_options: object_payload(row.get("server_options_json"))?,
        })
    }

    async fn update_system_configuration(
        &self,
        configuration: SystemConfiguration,
    ) -> Result<(), PersistenceError> {
        let content_types = serialize_configuration(&configuration.content_types)?;
        let metadata_options = serialize_configuration(&configuration.metadata_options)?;
        let path_substitutions = serialize_configuration(&configuration.path_substitutions)?;
        let plugin_repositories = serialize_configuration(&configuration.plugin_repositories)?;
        let server_options = serialize_configuration(&configuration.server_options)?;
        let now = persistence_timestamp()?;

        sqlx::query(
            r#"
            INSERT INTO system_configuration_payloads (
                id, content_types_json, metadata_options_json, path_substitutions_json,
                plugin_repositories_json, server_options_json, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
                content_types_json = excluded.content_types_json,
                metadata_options_json = excluded.metadata_options_json,
                path_substitutions_json = excluded.path_substitutions_json,
                plugin_repositories_json = excluded.plugin_repositories_json,
                server_options_json = excluded.server_options_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(content_types)
        .bind(metadata_options)
        .bind(path_substitutions)
        .bind(plugin_repositories)
        .bind(server_options)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "update system configuration"))?;
        Ok(())
    }

    async fn named_configuration(&self, key: &str) -> Result<Option<Value>, PersistenceError> {
        let row = sqlx::query(
            r#"
            SELECT payload_json
            FROM named_configurations
            WHERE key = ?1
            "#,
        )
        .bind(normalize_configuration_key(key))
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read named configuration"))?;

        row.map(|row| {
            serde_json::from_str(row.get::<&str, _>("payload_json")).map_err(|error| {
                tracing::error!(?error, "invalid named configuration JSON in SQLite");
                PersistenceError::internal("stored named configuration is invalid")
            })
        })
        .transpose()
    }

    async fn named_configurations(&self) -> Result<Vec<NamedConfiguration>, PersistenceError> {
        let rows = sqlx::query(
            r#"
            SELECT key, payload_json
            FROM named_configurations
            ORDER BY key
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "list named configurations"))?;

        rows.into_iter()
            .map(|row| {
                let payload =
                    serde_json::from_str(row.get::<&str, _>("payload_json")).map_err(|error| {
                        tracing::error!(?error, "invalid named configuration JSON in SQLite");
                        PersistenceError::internal("stored named configuration is invalid")
                    })?;
                Ok(NamedConfiguration {
                    key: row.get("key"),
                    payload,
                })
            })
            .collect()
    }

    async fn update_named_configuration(
        &self,
        key: &str,
        payload: Value,
    ) -> Result<(), PersistenceError> {
        let key = normalize_configuration_key(key);
        if key.is_empty() {
            return Err(PersistenceError::Constraint {
                message: "configuration key must not be empty".to_string(),
            });
        }
        let payload_json = serialize_configuration(&payload)?;
        let now = persistence_timestamp()?;

        sqlx::query(
            r#"
            INSERT INTO named_configurations (key, payload_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(payload_json)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "update named configuration"))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl UserRepository for SqlitePersistence {
    async fn first_user(&self) -> Result<Option<UserRecord>, PersistenceError> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            FROM users
            ORDER BY created_at
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read first user"))?;

        row.map(user_record_from_row).transpose()
    }

    async fn users(&self) -> Result<Vec<UserRecord>, PersistenceError> {
        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            FROM users
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "list users"))?;

        rows.into_iter().map(user_record_from_row).collect()
    }

    async fn user_by_id(&self, user_id: Uuid) -> Result<Option<UserRecord>, PersistenceError> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            FROM users
            WHERE id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read user by id"))?;

        row.map(user_record_from_row).transpose()
    }

    async fn user_by_name(&self, name: &str) -> Result<Option<UserRecord>, PersistenceError> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            FROM users
            WHERE name = ?1 COLLATE NOCASE
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read user by name"))?;

        row.map(user_record_from_row).transpose()
    }

    async fn user_configuration(&self, user_id: Uuid) -> Result<Option<Value>, PersistenceError> {
        let payload_json = sqlx::query_scalar::<_, String>(
            r#"
            SELECT payload_json
            FROM user_configurations
            WHERE user_id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read user configuration"))?;

        payload_json
            .map(|payload_json| {
                serde_json::from_str(&payload_json).map_err(|error| {
                    tracing::error!(?error, "invalid user configuration JSON in SQLite");
                    PersistenceError::internal("stored user configuration is invalid")
                })
            })
            .transpose()
    }

    async fn insert_user(&self, user: UserRecord) -> Result<(), PersistenceError> {
        sqlx::query(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(user.id.to_string())
        .bind(user.name)
        .bind(user.is_administrator)
        .bind(user.is_disabled)
        .bind(user.sync_play_access)
        .bind(format_timestamp(user.created_at, "user created_at")?)
        .bind(format_timestamp(user.updated_at, "user updated_at")?)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "insert user"))?;
        Ok(())
    }

    async fn upsert_user_by_name(&self, user: UserRecord) -> Result<(), PersistenceError> {
        sqlx::query(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(name) DO UPDATE SET
                is_administrator = excluded.is_administrator,
                is_disabled = excluded.is_disabled,
                sync_play_access = excluded.sync_play_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user.id.to_string())
        .bind(user.name)
        .bind(user.is_administrator)
        .bind(user.is_disabled)
        .bind(user.sync_play_access)
        .bind(format_timestamp(user.created_at, "user created_at")?)
        .bind(format_timestamp(user.updated_at, "user updated_at")?)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "upsert user by name"))?;
        Ok(())
    }

    async fn update_user_profile(&self, update: UserProfileUpdate) -> Result<(), PersistenceError> {
        sqlx::query(
            r#"
            UPDATE users
            SET name = ?1, is_administrator = ?2, is_disabled = ?3,
                sync_play_access = ?4, updated_at = ?5
            WHERE id = ?6
            "#,
        )
        .bind(update.name)
        .bind(update.is_administrator)
        .bind(update.is_disabled)
        .bind(update.sync_play_access)
        .bind(format_timestamp(update.updated_at, "user updated_at")?)
        .bind(update.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "update user profile"))?;
        Ok(())
    }

    async fn delete_user(&self, user_id: Uuid) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM users WHERE id = ?1")
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|error| map_sqlite_error(error, "delete user"))?;
        Ok(())
    }

    async fn enabled_administrator_count(&self) -> Result<u64, PersistenceError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE is_administrator = 1 AND is_disabled = 0",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "count enabled administrators"))?;
        u64::try_from(count)
            .map_err(|_| PersistenceError::internal("administrator count is negative"))
    }

    async fn update_user_configuration(
        &self,
        user_id: Uuid,
        payload: Value,
    ) -> Result<(), PersistenceError> {
        let payload_json = serialize_configuration(&payload)?;
        let now = persistence_timestamp()?;
        sqlx::query(
            r#"
            INSERT INTO user_configurations (
                user_id, payload_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user_id.to_string())
        .bind(payload_json)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "update user configuration"))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl CredentialRepository for SqlitePersistence {
    async fn credential(
        &self,
        user_id: Uuid,
    ) -> Result<Option<PasswordCredential>, PersistenceError> {
        let row = sqlx::query_as::<_, PasswordCredentialRow>(
            r#"
            SELECT user_id, algorithm, password_hash, updated_at
            FROM user_passwords
            WHERE user_id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "read password credential"))?;
        row.map(password_credential_from_row).transpose()
    }

    async fn has_credential(&self, user_id: Uuid) -> Result<bool, PersistenceError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_passwords WHERE user_id = ?1")
                .bind(user_id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(|error| map_sqlite_error(error, "check password credential"))?;
        Ok(count > 0)
    }

    async fn upsert_credential(
        &self,
        credential: PasswordCredential,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r#"
            INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(user_id) DO UPDATE SET
                algorithm = excluded.algorithm,
                password_hash = excluded.password_hash,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(credential.user_id.to_string())
        .bind(credential.algorithm)
        .bind(credential.password_hash)
        .bind(format_timestamp(
            credential.updated_at,
            "password credential updated_at",
        )?)
        .execute(&self.pool)
        .await
        .map_err(|error| map_sqlite_error(error, "upsert password credential"))?;
        Ok(())
    }

    async fn delete_credential(&self, user_id: Uuid) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM user_passwords WHERE user_id = ?1")
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|error| map_sqlite_error(error, "delete password credential"))?;
        Ok(())
    }
}

fn should_enable_wal(database_url: &str) -> bool {
    !database_url.contains(":memory:")
}

fn normalize_configuration_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

fn serialize_configuration(value: &Value) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|error| {
        tracing::error!(?error, "failed to serialize configuration");
        PersistenceError::internal("configuration serialization failed")
    })
}

fn persistence_timestamp() -> Result<String, PersistenceError> {
    format_timestamp(OffsetDateTime::now_utc(), "persistence timestamp")
}

fn format_timestamp(
    timestamp: OffsetDateTime,
    field: &'static str,
) -> Result<String, PersistenceError> {
    timestamp.format(&Rfc3339).map_err(|error| {
        tracing::error!(?error, field, "failed to format timestamp for SQLite");
        PersistenceError::internal("timestamp formatting failed")
    })
}

fn user_record_from_row(row: UserRow) -> Result<UserRecord, PersistenceError> {
    Ok(UserRecord {
        id: Uuid::parse_str(&row.id).map_err(|error| {
            tracing::error!(?error, "invalid user id in SQLite");
            PersistenceError::internal("stored user id is invalid")
        })?,
        name: row.name,
        is_administrator: row.is_administrator,
        is_disabled: row.is_disabled,
        sync_play_access: row.sync_play_access,
        created_at: stored_timestamp(&row.created_at, "user created_at")?,
        updated_at: stored_timestamp(&row.updated_at, "user updated_at")?,
    })
}

fn password_credential_from_row(
    row: PasswordCredentialRow,
) -> Result<PasswordCredential, PersistenceError> {
    Ok(PasswordCredential {
        user_id: Uuid::parse_str(&row.user_id).map_err(|error| {
            tracing::error!(?error, "invalid credential user id in SQLite");
            PersistenceError::internal("stored credential user id is invalid")
        })?,
        algorithm: row.algorithm,
        password_hash: row.password_hash,
        updated_at: stored_timestamp(&row.updated_at, "password credential updated_at")?,
    })
}

fn stored_timestamp(raw: &str, field: &'static str) -> Result<OffsetDateTime, PersistenceError> {
    let trimmed = raw.trim();
    if let Ok(timestamp) = OffsetDateTime::parse(trimmed, &Rfc3339) {
        return Ok(timestamp);
    }

    let mut normalized = trimmed.replacen(' ', "T", 1);
    if !normalized.ends_with('Z') && !normalized.get(10..).is_some_and(|tail| tail.contains('+')) {
        normalized.push('Z');
    }
    OffsetDateTime::parse(&normalized, &Rfc3339).map_err(|error| {
        tracing::error!(?error, field, "invalid timestamp in SQLite");
        PersistenceError::internal("stored timestamp is invalid")
    })
}

fn array_payload(raw: &str) -> Result<Value, PersistenceError> {
    let value = stored_configuration_value(raw)?;
    match value {
        Value::Array(_) => Ok(value),
        _ => Ok(Value::Array(Vec::new())),
    }
}

fn object_payload(raw: &str) -> Result<Value, PersistenceError> {
    let value = stored_configuration_value(raw)?;
    match value {
        Value::Object(_) => Ok(value),
        _ => Ok(Value::Object(Default::default())),
    }
}

fn stored_configuration_value(raw: &str) -> Result<Value, PersistenceError> {
    serde_json::from_str(raw).map_err(|error| {
        tracing::error!(?error, "invalid system configuration JSON in SQLite");
        PersistenceError::internal("stored system configuration is invalid")
    })
}

fn map_sqlite_error(error: sqlx::Error, operation: &'static str) -> PersistenceError {
    let persistence_error = match &error {
        sqlx::Error::PoolTimedOut => PersistenceError::Timeout {
            message: operation.to_string(),
        },
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            PersistenceError::Conflict {
                message: operation.to_string(),
            }
        }
        sqlx::Error::Database(database_error)
            if database_error
                .code()
                .and_then(|code| code.parse::<i64>().ok())
                .is_some_and(|code| matches!(code & 0xff, 5 | 6)) =>
        {
            PersistenceError::Busy {
                message: operation.to_string(),
            }
        }
        sqlx::Error::Database(database_error)
            if database_error
                .code()
                .and_then(|code| code.parse::<i64>().ok())
                .is_some_and(|code| code & 0xff == 19) =>
        {
            PersistenceError::Constraint {
                message: operation.to_string(),
            }
        }
        _ => PersistenceError::Unavailable {
            message: operation.to_string(),
        },
    };
    tracing::error!(?error, operation, "SQLite persistence operation failed");
    persistence_error
}

async fn configure_sqlite_connection(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(&mut *connection)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use jellyrin_persistence::{
        ConfigurationRepository, CredentialRepository, PasswordCredential, PersistenceBackend,
        PersistenceControl, PersistenceError, SystemConfiguration, UserProfileUpdate, UserRecord,
        UserRepository,
    };
    use serde_json::json;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    use uuid::Uuid;

    use super::SqlitePersistence;

    #[tokio::test]
    async fn connects_migrates_and_reports_control_contract() {
        let persistence = SqlitePersistence::connect("sqlite::memory:").await.unwrap();

        assert_eq!(
            persistence.health().await.unwrap().backend,
            PersistenceBackend::Sqlite
        );
        assert!(
            persistence
                .schema_status()
                .await
                .unwrap()
                .latest_applied_migration
                .is_some()
        );
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(persistence.pool())
            .await
            .unwrap();
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(persistence.pool())
            .await
            .unwrap();
        assert_eq!(busy_timeout, 5_000);
        assert_eq!(foreign_keys, 1);
    }

    #[tokio::test]
    async fn named_configuration_contract_normalizes_orders_and_replaces() {
        let persistence = SqlitePersistence::connect("sqlite::memory:").await.unwrap();

        assert!(
            persistence
                .named_configuration("network")
                .await
                .unwrap()
                .is_none()
        );
        persistence
            .update_named_configuration(" Network ", json!({"Port": 8097}))
            .await
            .unwrap();
        persistence
            .update_named_configuration("livetv", json!({"Enabled": true}))
            .await
            .unwrap();
        persistence
            .update_named_configuration("NETWORK", json!({"Port": 8098}))
            .await
            .unwrap();

        assert_eq!(
            persistence
                .named_configuration(" network ")
                .await
                .unwrap()
                .unwrap(),
            json!({"Port": 8098})
        );
        let configurations = persistence.named_configurations().await.unwrap();
        assert_eq!(configurations.len(), 2);
        assert_eq!(configurations[0].key, "livetv");
        assert_eq!(configurations[1].key, "network");

        assert!(matches!(
            persistence
                .update_named_configuration("  ", json!({}))
                .await
                .unwrap_err(),
            PersistenceError::Constraint { .. }
        ));
    }

    #[tokio::test]
    async fn system_configuration_contract_defaults_and_sanitizes_shapes() {
        let persistence = SqlitePersistence::connect("sqlite::memory:").await.unwrap();

        assert_eq!(
            persistence.system_configuration().await.unwrap(),
            SystemConfiguration::default()
        );
        persistence
            .update_system_configuration(SystemConfiguration {
                content_types: json!({"invalid": true}),
                metadata_options: json!(null),
                path_substitutions: json!([{"From": "/a", "To": "/b"}]),
                plugin_repositories: json!([{"Name": "Repository"}]),
                server_options: json!(["invalid"]),
            })
            .await
            .unwrap();

        let stored = persistence.system_configuration().await.unwrap();
        assert_eq!(stored.content_types, json!([]));
        assert_eq!(stored.metadata_options, json!([]));
        assert_eq!(stored.path_substitutions.as_array().unwrap().len(), 1);
        assert_eq!(stored.plugin_repositories[0]["Name"], "Repository");
        assert_eq!(stored.server_options, json!({}));
    }

    #[tokio::test]
    async fn user_repository_reads_profiles_and_round_trips_configuration() {
        let persistence = SqlitePersistence::connect("sqlite::memory:").await.unwrap();
        let first_id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let second_id = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();

        assert!(persistence.first_user().await.unwrap().is_none());
        assert!(persistence.users().await.unwrap().is_empty());

        insert_test_user(&persistence, first_id, "Zulu", true, "2026-01-01 00:00:00").await;
        let second_created_at = test_timestamp("2026-01-02T00:00:00Z");
        persistence
            .insert_user(UserRecord {
                id: second_id,
                name: "alpha".to_string(),
                is_administrator: false,
                is_disabled: false,
                sync_play_access: "CreateAndJoinGroups".to_string(),
                created_at: second_created_at,
                updated_at: second_created_at,
            })
            .await
            .unwrap();

        assert_eq!(
            persistence.first_user().await.unwrap().unwrap().id,
            first_id
        );
        assert_eq!(
            persistence
                .user_by_id(second_id)
                .await
                .unwrap()
                .unwrap()
                .name,
            "alpha"
        );
        assert_eq!(
            persistence.user_by_name("ALPHA").await.unwrap().unwrap().id,
            second_id
        );
        assert!(persistence.user_by_name("missing").await.unwrap().is_none());

        let users = persistence.users().await.unwrap();
        assert_eq!(
            users
                .iter()
                .map(|user| user.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "Zulu"]
        );
        assert_eq!(persistence.enabled_administrator_count().await.unwrap(), 1);

        let profile_updated_at = test_timestamp("2026-01-03T00:00:00Z");
        persistence
            .update_user_profile(UserProfileUpdate {
                id: second_id,
                name: "beta".to_string(),
                is_administrator: true,
                is_disabled: false,
                sync_play_access: "JoinGroups".to_string(),
                updated_at: profile_updated_at,
            })
            .await
            .unwrap();
        let updated = persistence.user_by_id(second_id).await.unwrap().unwrap();
        assert_eq!(updated.name, "beta");
        assert_eq!(updated.sync_play_access, "JoinGroups");
        assert_eq!(updated.updated_at, profile_updated_at);
        assert_eq!(persistence.enabled_administrator_count().await.unwrap(), 2);

        let first = persistence.user_by_id(first_id).await.unwrap().unwrap();
        persistence
            .upsert_user_by_name(UserRecord {
                is_disabled: true,
                updated_at: profile_updated_at,
                ..first.clone()
            })
            .await
            .unwrap();
        let upserted = persistence.user_by_id(first_id).await.unwrap().unwrap();
        assert!(upserted.is_disabled);
        assert_eq!(upserted.created_at, first.created_at);
        assert_eq!(persistence.enabled_administrator_count().await.unwrap(), 1);

        assert!(
            persistence
                .user_configuration(second_id)
                .await
                .unwrap()
                .is_none()
        );
        persistence
            .update_user_configuration(second_id, json!({"AudioLanguagePreference": "es"}))
            .await
            .unwrap();
        assert_eq!(
            persistence
                .user_configuration(second_id)
                .await
                .unwrap()
                .unwrap(),
            json!({"AudioLanguagePreference": "es"})
        );

        assert!(!persistence.has_credential(second_id).await.unwrap());
        let credential_updated_at = test_timestamp("2026-01-04T00:00:00Z");
        persistence
            .upsert_credential(PasswordCredential {
                user_id: second_id,
                algorithm: "argon2id".to_string(),
                password_hash: "$argon2id$fixture".to_string(),
                updated_at: credential_updated_at,
            })
            .await
            .unwrap();
        assert!(persistence.has_credential(second_id).await.unwrap());
        let credential = persistence.credential(second_id).await.unwrap().unwrap();
        assert_eq!(credential.algorithm, "argon2id");
        assert_eq!(credential.password_hash, "$argon2id$fixture");
        assert_eq!(credential.updated_at, credential_updated_at);
        persistence.delete_credential(second_id).await.unwrap();
        assert!(persistence.credential(second_id).await.unwrap().is_none());

        persistence.delete_user(second_id).await.unwrap();
        assert!(persistence.user_by_id(second_id).await.unwrap().is_none());
    }

    async fn insert_test_user(
        persistence: &SqlitePersistence,
        id: Uuid,
        name: &str,
        is_administrator: bool,
        timestamp: &str,
    ) {
        sqlx::query(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 0, 'CreateAndJoinGroups', ?4, ?4)
            "#,
        )
        .bind(id.to_string())
        .bind(name)
        .bind(is_administrator)
        .bind(timestamp)
        .execute(persistence.pool())
        .await
        .unwrap();
    }

    fn test_timestamp(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).unwrap()
    }
}
