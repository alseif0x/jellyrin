use std::{fmt, str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, ensure};
use serde_json::Value;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    DatabaseDriver, DatabaseRuntimeDiagnostics, DatabaseTelemetryDiagnostics,
    MEDIA_ITEM_FACET_PROJECTION_NAME, MEDIA_ITEM_FACET_PROJECTION_VERSION,
    MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME, MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
    MediaItemFacetProjectionMode, NamedConfigurationPayload, ProviderSecretVault,
    SystemConfigurationPayloads, database_pool_diagnostics, ensure_media_item_facet_projection,
    ensure_media_item_query_filter_projection, normalize_configuration_key,
    telemetry::DatabaseTelemetry,
};

const DEFAULT_MAX_CONNECTIONS: u32 = 6;
const DEFAULT_WORKER_MAX_CONNECTIONS: u32 = 2;
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);
const DEFAULT_API_STATEMENT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_WORKER_STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(3);
/// Start snapshot reads with their transaction characteristics in the same PostgreSQL command.
///
/// Keeping the mode in `BEGIN` is cancellation-resilient: SQLx 0.9 can leave a server-side
/// `BEGIN` accepted when its begin future is cancelled before local transaction depth increments.
/// A later plain `BEGIN` followed by `SET TRANSACTION` would then fail because PostgreSQL treats
/// the duplicate `BEGIN` as a command inside the already-open transaction.
pub(crate) const POSTGRES_REPEATABLE_READ_ONLY_BEGIN: &str =
    "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY";
pub(crate) const POSTGRES_SERIALIZABLE_BEGIN: &str =
    "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE";
// Keep this declaration in the dependency graph whenever a new embedded migration is added.
pub(crate) static POSTGRES_MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("./migrations-postgres");

/// Validated connection settings for the PostgreSQL adapter.
///
/// The URL is deliberately redacted from `Debug`; provider URLs and database credentials must
/// never leak through startup diagnostics.
#[derive(Clone)]
pub struct PostgresSettings {
    database_url: String,
    pub max_connections: u32,
    pub worker_max_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub api_statement_timeout: Duration,
    pub worker_statement_timeout: Duration,
    pub lock_timeout: Duration,
}

impl PostgresSettings {
    pub fn new(database_url: impl Into<String>) -> anyhow::Result<Self> {
        let database_url = database_url.into();
        let has_postgres_scheme = database_url
            .split_once("://")
            .map(|(scheme, _)| {
                scheme.eq_ignore_ascii_case("postgres") || scheme.eq_ignore_ascii_case("postgresql")
            })
            .unwrap_or(false);
        ensure!(
            has_postgres_scheme,
            "DATABASE_URL must use the postgres or postgresql scheme"
        );
        // Do not retain sqlx's parser error here: connection URLs can contain credentials and
        // this public constructor is also used outside DatabaseManager's redaction boundary.
        PgConnectOptions::from_str(&database_url).map_err(|_| {
            anyhow::anyhow!("DATABASE_URL is not a valid PostgreSQL connection URL")
        })?;
        Ok(Self {
            database_url,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            worker_max_connections: DEFAULT_WORKER_MAX_CONNECTIONS,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_lifetime: DEFAULT_MAX_LIFETIME,
            api_statement_timeout: DEFAULT_API_STATEMENT_TIMEOUT,
            worker_statement_timeout: DEFAULT_WORKER_STATEMENT_TIMEOUT,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        })
    }

    pub fn with_max_connections(mut self, max_connections: u32) -> anyhow::Result<Self> {
        ensure!(
            (1..=64).contains(&max_connections),
            "PostgreSQL max_connections must be between 1 and 64"
        );
        self.max_connections = max_connections;
        Ok(self)
    }

    pub fn with_worker_max_connections(
        mut self,
        worker_max_connections: u32,
    ) -> anyhow::Result<Self> {
        ensure!(
            (1..=16).contains(&worker_max_connections),
            "PostgreSQL worker max_connections must be between 1 and 16"
        );
        self.worker_max_connections = worker_max_connections;
        Ok(self)
    }

    pub fn with_acquire_timeout(mut self, acquire_timeout: Duration) -> anyhow::Result<Self> {
        ensure!(
            !acquire_timeout.is_zero() && acquire_timeout <= Duration::from_secs(60),
            "PostgreSQL acquire timeout must be between 1ns and 60s"
        );
        self.acquire_timeout = acquire_timeout;
        Ok(self)
    }

    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> anyhow::Result<Self> {
        ensure!(
            !idle_timeout.is_zero() && idle_timeout <= Duration::from_secs(60 * 60),
            "PostgreSQL idle timeout must be between 1ns and 1h"
        );
        self.idle_timeout = idle_timeout;
        Ok(self)
    }

    pub fn with_max_lifetime(mut self, max_lifetime: Duration) -> anyhow::Result<Self> {
        ensure!(
            !max_lifetime.is_zero() && max_lifetime <= Duration::from_secs(24 * 60 * 60),
            "PostgreSQL max lifetime must be between 1ns and 24h"
        );
        self.max_lifetime = max_lifetime;
        Ok(self)
    }

    pub fn with_statement_timeouts(
        mut self,
        api: Duration,
        worker: Duration,
    ) -> anyhow::Result<Self> {
        ensure!(
            !api.is_zero() && api <= Duration::from_secs(60),
            "PostgreSQL API statement timeout must be between 1ns and 60s"
        );
        ensure!(
            !worker.is_zero() && worker <= Duration::from_secs(30 * 60),
            "PostgreSQL worker statement timeout must be between 1ns and 30m"
        );
        self.api_statement_timeout = api;
        self.worker_statement_timeout = worker;
        Ok(self)
    }

    pub fn with_lock_timeout(mut self, lock_timeout: Duration) -> anyhow::Result<Self> {
        ensure!(
            !lock_timeout.is_zero() && lock_timeout <= Duration::from_secs(60),
            "PostgreSQL lock timeout must be between 1ns and 60s"
        );
        self.lock_timeout = lock_timeout;
        Ok(self)
    }
}

impl fmt::Debug for PostgresSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresSettings")
            .field("database_url", &"[REDACTED]")
            .field("max_connections", &self.max_connections)
            .field("worker_max_connections", &self.worker_max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("max_lifetime", &self.max_lifetime)
            .field("api_statement_timeout", &self.api_statement_timeout)
            .field("worker_statement_timeout", &self.worker_statement_timeout)
            .field("lock_timeout", &self.lock_timeout)
            .finish()
    }
}

/// PostgreSQL lifecycle surface. Domain repositories are ported onto this adapter separately so
/// the server never branches on SQL dialect inside request handlers.
#[derive(Clone)]
pub struct PostgresDatabase {
    pub(crate) pool: PgPool,
    pub(crate) worker_pool: PgPool,
    pub(crate) provider_secret_vault: Option<ProviderSecretVault>,
    pub(crate) telemetry: Arc<DatabaseTelemetry>,
}

impl PostgresDatabase {
    /// Connects a production runtime and verifies that the externally managed schema is current.
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let settings = PostgresSettings::new(database_url)?;
        let database = Self::connect_with_settings(&settings).await?;
        if let Err(error) = database.schema_health().await {
            database.close().await;
            return Err(error);
        }
        Ok(database)
    }

    /// Connects pools without requiring an existing schema. Migration jobs and integration tests
    /// use this entry point before calling [`Self::migrate`].
    pub async fn connect_with_settings(settings: &PostgresSettings) -> anyhow::Result<Self> {
        let pool = connect_pool(
            settings,
            settings.max_connections,
            settings.api_statement_timeout,
            "jellyrin-api",
        )
        .await?;
        let worker_pool = match connect_pool(
            settings,
            settings.worker_max_connections,
            settings.worker_statement_timeout,
            "jellyrin-worker",
        )
        .await
        {
            Ok(pool) => pool,
            Err(error) => {
                pool.close().await;
                return Err(error);
            }
        };
        let database = Self {
            pool,
            worker_pool,
            provider_secret_vault: None,
            telemetry: Arc::new(DatabaseTelemetry::default()),
        };
        database.health().await?;
        Ok(database)
    }

    pub fn with_provider_secret_vault(mut self, vault: ProviderSecretVault) -> Self {
        self.provider_secret_vault = Some(vault);
        self
    }

    /// Performs a real round trip; callers can use this for readiness, not liveness.
    pub async fn health(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("PostgreSQL API pool health check failed")?;
        sqlx::query("SELECT 1")
            .execute(&self.worker_pool)
            .await
            .context("PostgreSQL worker pool health check failed")?;
        Ok(())
    }

    pub fn runtime_diagnostics(&self) -> DatabaseRuntimeDiagnostics {
        DatabaseRuntimeDiagnostics {
            driver: DatabaseDriver::PostgreSql,
            api_pool: database_pool_diagnostics(&self.pool),
            worker_pool: Some(database_pool_diagnostics(&self.worker_pool)),
        }
    }

    pub fn telemetry_diagnostics(&self) -> DatabaseTelemetryDiagnostics {
        self.telemetry.snapshot(true)
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        // PostgreSQL extensions are database-global even when each test or
        // deployment migration targets a separate schema. Serialize the whole
        // migration run so concurrent starters cannot race while creating
        // pg_trgm or SQLx's history table. A transaction-scoped lock is
        // cancellation-safe: dropping this future rolls the transaction back
        // and releases the lock automatically.
        let mut migration_lock = self
            .pool
            .begin()
            .await
            .context("failed to start PostgreSQL migration lock transaction")?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('jellyrin:schema:migration', 0))",
        )
        .execute(&mut *migration_lock)
        .await
        .context("failed to acquire PostgreSQL migration lock")?;
        // Catalogue index expressions may catch malformed provider metadata
        // with PL/pgSQL exception handlers. Such handlers open subtransactions,
        // which PostgreSQL forbids inside parallel CREATE INDEX workers.
        let mut migration_connection = self
            .worker_pool
            .acquire()
            .await
            .context("failed to acquire PostgreSQL migration connection")?;
        sqlx::query("SET max_parallel_maintenance_workers = 0")
            .execute(&mut *migration_connection)
            .await
            .context("failed to disable parallel PostgreSQL migration workers")?;
        POSTGRES_MIGRATOR
            .run_direct(None, &mut *migration_connection, false)
            .await
            .context("failed to migrate PostgreSQL schema")?;
        drop(migration_connection);
        ensure_media_item_facet_projection(
            &mut migration_lock,
            MediaItemFacetProjectionMode::EnsureCurrent,
        )
        .await
        .context("failed to ensure PostgreSQL media item facet projection")?;
        ensure_media_item_query_filter_projection(
            &mut migration_lock,
            MediaItemFacetProjectionMode::EnsureCurrent,
        )
        .await
        .context("failed to ensure PostgreSQL media item query-filter projection")?;
        migration_lock
            .commit()
            .await
            .context("failed to release PostgreSQL migration lock")?;
        Ok(())
    }

    /// Readiness check for the externally managed schema. The application deliberately does not
    /// migrate on startup, so multiple replicas never race with DDL under the runtime role.
    pub async fn schema_health(&self) -> anyhow::Result<()> {
        let expected = POSTGRES_MIGRATOR
            .iter()
            .filter(|migration| !migration.migration_type.is_down_migration())
            .collect::<Vec<_>>();
        ensure!(
            !expected.is_empty(),
            "PostgreSQL migrator contains no migrations"
        );
        let applied = sqlx::query_as::<_, PostgresAppliedMigrationRow>(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await
        .context(
            "PostgreSQL schema metadata is unavailable; run the migration job before Jellyrin",
        )?;
        ensure!(
            applied.len() == expected.len(),
            "PostgreSQL schema migration count differs (expected {}, found {})",
            expected.len(),
            applied.len()
        );
        for (expected, applied) in expected.into_iter().zip(applied) {
            ensure!(
                applied.version == expected.version,
                "PostgreSQL schema has migration {} where {} was expected",
                applied.version,
                expected.version
            );
            ensure!(
                applied.success,
                "PostgreSQL migration {} is marked failed",
                applied.version
            );
            ensure!(
                applied.checksum.as_slice() == expected.checksum.as_ref(),
                "PostgreSQL migration {} checksum does not match this Jellyrin build",
                applied.version
            );
        }
        let projection_version = sqlx::query_scalar::<_, i32>(
            "SELECT extractor_version FROM jellyrin_derived_projection_versions \
             WHERE projection_name = $1",
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .fetch_optional(&self.pool)
        .await
        .context(
            "PostgreSQL media item facet projection metadata is unavailable; run the migration job before Jellyrin",
        )?;
        ensure!(
            projection_version == Some(MEDIA_ITEM_FACET_PROJECTION_VERSION),
            "PostgreSQL media item facet projection is not current (expected version {}); run the migration job before Jellyrin",
            MEDIA_ITEM_FACET_PROJECTION_VERSION
        );
        let query_filter_marker = sqlx::query_as::<_, (i32, i64, i64)>(
            "SELECT extractor_version, source_item_count, projected_facet_count \
             FROM jellyrin_derived_projection_versions WHERE projection_name = $1",
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
        .fetch_optional(&self.pool)
        .await
        .context("PostgreSQL media item query-filter projection metadata is unavailable")?;
        ensure!(
            query_filter_marker.is_some_and(|marker| {
                marker.0 == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION
                    && marker.1 >= 0
                    && marker.2 >= 0
            }),
            "PostgreSQL media item query-filter projection is not current; run the migration job before Jellyrin"
        );
        Ok(())
    }

    pub async fn system_configuration_payloads(
        &self,
    ) -> anyhow::Result<SystemConfigurationPayloads> {
        let row = sqlx::query_as::<_, PostgresSystemConfigurationRow>(
            r#"
            SELECT content_types, metadata_options, path_substitutions,
                   plugin_repositories, server_options
            FROM system_configuration_payloads
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into).unwrap_or_default())
    }

    pub async fn update_system_configuration_payloads(
        &self,
        payloads: SystemConfigurationPayloads,
    ) -> anyhow::Result<()> {
        let payloads = normalize_system_configuration_payloads(payloads);
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.worker_pool.begin().await?;
        super::postgres_plugins::lock_platform_exclusive(&mut transaction).await?;
        sqlx::query(
            r#"
            INSERT INTO system_configuration_payloads (
                id, content_types, metadata_options, path_substitutions,
                plugin_repositories, server_options, updated_at
            )
            VALUES (1, $1, $2, $3, $4, $5, $6)
            ON CONFLICT (id) DO UPDATE SET
                content_types = excluded.content_types,
                metadata_options = excluded.metadata_options,
                path_substitutions = excluded.path_substitutions,
                plugin_repositories = excluded.plugin_repositories,
                server_options = excluded.server_options,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&payloads.content_types)
        .bind(&payloads.metadata_options)
        .bind(&payloads.path_substitutions)
        .bind(&payloads.plugin_repositories)
        .bind(&payloads.server_options)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        super::postgres_plugins::replace_catalog_from_configuration(
            &mut transaction,
            &payloads.plugin_repositories,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn named_configuration(&self, key: &str) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar("SELECT payload FROM named_configurations WHERE key = $1")
            .bind(normalize_configuration_key(key))
            .fetch_optional(&self.pool)
            .await
            .context("failed to load PostgreSQL named configuration")
    }

    pub async fn named_configurations(&self) -> anyhow::Result<Vec<NamedConfigurationPayload>> {
        let rows = sqlx::query_as::<_, PostgresNamedConfigurationRow>(
            "SELECT key, payload FROM named_configurations ORDER BY key",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| NamedConfigurationPayload {
                key: row.key,
                payload: row.payload,
            })
            .collect())
    }

    pub async fn update_named_configuration(
        &self,
        key: &str,
        mut payload: Value,
    ) -> anyhow::Result<()> {
        let key = normalize_configuration_key(key);
        ensure!(!key.is_empty(), "configuration key must not be empty");
        let mut transaction = self.pool.begin().await?;
        if key == "livetv" {
            super::postgres_provider_secrets::lock_provider_configuration_mutation(
                &mut transaction,
                "named",
                &key,
            )
            .await?;
            let existing = sqlx::query_scalar::<_, Value>(
                "SELECT payload FROM named_configurations WHERE key = $1 FOR UPDATE",
            )
            .bind(&key)
            .fetch_optional(&mut *transaction)
            .await?;
            payload = self
                .protect_live_tv_named_configuration_in_connection(
                    &mut transaction,
                    payload,
                    existing.as_ref(),
                )
                .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO named_configurations (key, payload, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (key) DO UPDATE SET
                payload = excluded.payload,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(payload)
        .bind(OffsetDateTime::now_utc())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn user_configuration(&self, user_id: Uuid) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar("SELECT payload FROM user_configurations WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to load PostgreSQL user configuration")
    }

    pub async fn update_user_configuration(
        &self,
        user_id: Uuid,
        payload: Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO user_configurations (user_id, payload, created_at, updated_at)
            VALUES ($1, $2, $3, $3)
            ON CONFLICT (user_id) DO UPDATE SET
                payload = excluded.payload,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user_id)
        .bind(payload)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
        self.worker_pool.close().await;
    }
}

async fn connect_pool(
    settings: &PostgresSettings,
    max_connections: u32,
    statement_timeout: Duration,
    application_name: &'static str,
) -> anyhow::Result<PgPool> {
    let options = PgConnectOptions::from_str(&settings.database_url)
        .context("failed to parse PostgreSQL connection options")?
        .application_name(application_name);
    let statement_timeout = duration_as_postgres_milliseconds(statement_timeout);
    let lock_timeout = duration_as_postgres_milliseconds(settings.lock_timeout);

    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .idle_timeout(Some(settings.idle_timeout))
        .max_lifetime(Some(settings.max_lifetime))
        .after_connect(move |connection, _metadata| {
            let statement_timeout = statement_timeout.clone();
            let lock_timeout = lock_timeout.clone();
            Box::pin(async move {
                sqlx::query(
                    r#"
                    SELECT set_config('TimeZone', 'UTC', false),
                           set_config('statement_timeout', $1, false),
                           set_config('lock_timeout', $2, false)
                    "#,
                )
                .bind(statement_timeout)
                .bind(lock_timeout)
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .with_context(|| format!("failed to connect PostgreSQL {application_name} pool"))
}

fn duration_as_postgres_milliseconds(duration: Duration) -> String {
    format!("{}ms", duration.as_millis().max(1))
}

#[derive(sqlx::FromRow)]
struct PostgresSystemConfigurationRow {
    content_types: Value,
    metadata_options: Value,
    path_substitutions: Value,
    plugin_repositories: Value,
    server_options: Value,
}

impl From<PostgresSystemConfigurationRow> for SystemConfigurationPayloads {
    fn from(row: PostgresSystemConfigurationRow) -> Self {
        normalize_system_configuration_payloads(Self {
            content_types: row.content_types,
            metadata_options: row.metadata_options,
            path_substitutions: row.path_substitutions,
            plugin_repositories: row.plugin_repositories,
            server_options: row.server_options,
        })
    }
}

fn normalize_system_configuration_payloads(
    payloads: SystemConfigurationPayloads,
) -> SystemConfigurationPayloads {
    SystemConfigurationPayloads {
        content_types: normalize_array(payloads.content_types),
        metadata_options: normalize_array(payloads.metadata_options),
        path_substitutions: normalize_array(payloads.path_substitutions),
        plugin_repositories: normalize_array(payloads.plugin_repositories),
        server_options: normalize_object(payloads.server_options),
    }
}

fn normalize_array(value: Value) -> Value {
    match value {
        Value::Array(_) => value,
        _ => Value::Array(Vec::new()),
    }
}

fn normalize_object(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        _ => Value::Object(Default::default()),
    }
}

#[derive(sqlx::FromRow)]
struct PostgresNamedConfigurationRow {
    key: String,
    payload: Value,
}

#[derive(sqlx::FromRow)]
struct PostgresAppliedMigrationRow {
    version: i64,
    success: bool,
    checksum: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{PostgresDatabase, PostgresSettings, SystemConfigurationPayloads};
    use serde_json::json;
    use sqlx::PgPool;
    use time::OffsetDateTime;
    use uuid::Uuid;

    const POOL_RESPONSIVENESS_LIMIT: Duration = Duration::from_millis(500);
    const LOAD_REQUESTS: usize = 8;

    async fn configured_test_database(
        api_connections: u32,
        worker_connections: u32,
    ) -> Option<PostgresDatabase> {
        let database_url = std::env::var("JELLYRIN_TEST_POSTGRES_URL").ok()?;
        let settings = PostgresSettings::new(database_url)
            .unwrap()
            .with_max_connections(api_connections)
            .unwrap()
            .with_worker_max_connections(worker_connections)
            .unwrap();
        Some(
            PostgresDatabase::connect_with_settings(&settings)
                .await
                .unwrap(),
        )
    }

    async fn wait_until_backend_is_sleeping(observer: &PgPool, backend_pid: i32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let sleeping: bool = sqlx::query_scalar(
                    r#"
                    SELECT EXISTS (
                        SELECT 1
                        FROM pg_stat_activity
                        WHERE pid = $1
                          AND state = 'active'
                          AND query LIKE 'SELECT pg_sleep(%'
                    )
                    "#,
                )
                .bind(backend_pid)
                .fetch_one(observer)
                .await
                .unwrap();
                if sleeping {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sleeping PostgreSQL backend was not observable in time");
    }

    async fn assert_busy_pool_does_not_block_other_pool(busy: &PgPool, responsive: &PgPool) {
        let mut busy_connection = busy.acquire().await.unwrap();
        let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *busy_connection)
            .await
            .unwrap();
        let sleeper = tokio::spawn(async move {
            sqlx::query("SELECT pg_sleep(1)")
                .execute(&mut *busy_connection)
                .await
        });

        wait_until_backend_is_sleeping(responsive, backend_pid).await;
        let started = Instant::now();
        let result: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(responsive)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(result, 1);
        assert!(
            elapsed < POOL_RESPONSIVENESS_LIMIT,
            "independent pool took {elapsed:?} while the other pool was saturated"
        );
        sleeper.await.unwrap().unwrap();
    }

    struct LoadSample {
        latencies_millis: Vec<f64>,
        elapsed: Duration,
        errors: usize,
    }

    impl LoadSample {
        fn percentile_millis(&self, percentile: f64) -> f64 {
            if self.latencies_millis.is_empty() {
                return 0.0;
            }
            let mut values = self.latencies_millis.clone();
            values.sort_by(f64::total_cmp);
            let index = ((values.len() - 1) as f64 * percentile).ceil() as usize;
            values[index]
        }

        fn throughput_per_second(&self) -> f64 {
            self.latencies_millis.len() as f64 / self.elapsed.as_secs_f64().max(f64::EPSILON)
        }

        fn as_json(&self) -> serde_json::Value {
            json!({
                "requests": LOAD_REQUESTS,
                "completed": self.latencies_millis.len(),
                "errors": self.errors,
                "p50_ms": self.percentile_millis(0.50),
                "p95_ms": self.percentile_millis(0.95),
                "p99_ms": self.percentile_millis(0.99),
                "throughput_per_second": self.throughput_per_second(),
            })
        }
    }

    async fn run_api_load(pool: &PgPool) -> LoadSample {
        let started = Instant::now();
        let mut tasks = Vec::with_capacity(LOAD_REQUESTS);
        for _ in 0..LOAD_REQUESTS {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let request_started = Instant::now();
                sqlx::query("SELECT pg_sleep(0.02)")
                    .execute(&pool)
                    .await
                    .map(|_| request_started.elapsed())
            }));
        }

        let mut latencies_millis = Vec::with_capacity(LOAD_REQUESTS);
        let mut errors = 0;
        for task in tasks {
            match task.await {
                Ok(Ok(latency)) => latencies_millis.push(latency.as_secs_f64() * 1_000.0),
                Ok(Err(_)) | Err(_) => errors += 1,
            }
        }
        LoadSample {
            latencies_millis,
            elapsed: started.elapsed(),
            errors,
        }
    }

    #[test]
    fn postgres_settings_validate_scheme_limits_and_redact_secrets() {
        let settings = PostgresSettings::new("postgresql://jellyrin:secret@db/jellyrin")
            .unwrap()
            .with_max_connections(4)
            .unwrap()
            .with_acquire_timeout(Duration::from_secs(3))
            .unwrap();

        let debug = format!("{settings:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
        assert_eq!(settings.max_connections, 4);
        assert_eq!(settings.acquire_timeout, Duration::from_secs(3));

        // URI schemes are case-insensitive; the public selector and adapter must agree.
        assert!(PostgresSettings::new("POSTGRESQL://db/jellyrin").is_ok());
    }

    #[test]
    fn postgres_settings_reject_non_postgres_urls_and_unsafe_pool_sizes() {
        assert!(PostgresSettings::new("sqlite://jellyrin.db").is_err());
        let parse_error =
            PostgresSettings::new("postgresql://jellyrin:super-secret@[invalid-host/jellyrin")
                .unwrap_err()
                .to_string();
        assert!(!parse_error.contains("super-secret"));
        assert!(!parse_error.contains("postgresql://"));
        assert!(
            PostgresSettings::new("postgres://db/jellyrin")
                .unwrap()
                .with_max_connections(0)
                .is_err()
        );
        assert!(
            PostgresSettings::new("postgres://db/jellyrin")
                .unwrap()
                .with_max_connections(65)
                .is_err()
        );
    }

    #[tokio::test]
    async fn postgres_api_and_worker_pools_remain_isolated_when_saturated() {
        let Some(database) = configured_test_database(1, 1).await else {
            return;
        };

        let api_backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let worker_backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&database.worker_pool)
            .await
            .unwrap();
        assert_ne!(api_backend_pid, worker_backend_pid);

        assert_busy_pool_does_not_block_other_pool(&database.worker_pool, &database.pool).await;
        assert_busy_pool_does_not_block_other_pool(&database.pool, &database.worker_pool).await;
        database.close().await;
    }

    #[tokio::test]
    #[ignore = "local PostgreSQL load runner; set JELLYRIN_TEST_POSTGRES_URL to execute"]
    async fn postgres_pool_local_load() {
        let Some(database) = configured_test_database(2, 1).await else {
            return;
        };

        let baseline = run_api_load(&database.pool).await;

        let mut worker_connection = database.worker_pool.acquire().await.unwrap();
        let worker_backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *worker_connection)
            .await
            .unwrap();
        let worker_sleeper = tokio::spawn(async move {
            sqlx::query("SELECT pg_sleep(2)")
                .execute(&mut *worker_connection)
                .await
        });
        wait_until_backend_is_sleeping(&database.pool, worker_backend_pid).await;

        let worker_saturated = run_api_load(&database.pool).await;
        worker_sleeper.await.unwrap().unwrap();

        let baseline_p95 = baseline.percentile_millis(0.95);
        let p95_ratio = if baseline_p95 > 0.0 {
            worker_saturated.percentile_millis(0.95) / baseline_p95
        } else {
            0.0
        };
        let report = json!({
            "benchmark": "postgres_pool_local_load",
            "api_max_connections": 2,
            "worker_max_connections": 1,
            "baseline": baseline.as_json(),
            "worker_saturated": worker_saturated.as_json(),
            "p95_ratio_worker_saturated_to_baseline": p95_ratio,
        });
        println!("{report}");

        assert_eq!(baseline.errors, 0);
        assert_eq!(worker_saturated.errors, 0);
        database.close().await;
    }

    #[tokio::test]
    async fn postgres_migrator_builds_the_baseline_schema_when_configured() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();

        let required_tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = 'public' \
             AND table_name IN ('users', 'media_items', 'transcode_sessions', 'live_tv_channels')",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(required_tables, 4);

        let metadata_type: String = sqlx::query_scalar(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'media_items' \
             AND column_name = 'metadata'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(metadata_type, "jsonb");

        let required_foreign_key_indexes: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_indexes
            WHERE schemaname = 'public'
              AND indexname IN (
                  'activity_log_entries_user_idx',
                  'display_preferences_user_idx',
                  'media_lists_owner_user_idx'
              )
            "#,
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(required_foreign_key_indexes, 3);

        let system_payloads = SystemConfigurationPayloads {
            content_types: json!([{"Name": "Movies"}]),
            metadata_options: json!([]),
            path_substitutions: json!([]),
            plugin_repositories: json!([]),
            server_options: json!({"EnableMetrics": true}),
        };
        database
            .update_system_configuration_payloads(system_payloads.clone())
            .await
            .unwrap();
        assert_eq!(
            database.system_configuration_payloads().await.unwrap(),
            system_payloads
        );

        database
            .update_system_configuration_payloads(SystemConfigurationPayloads {
                content_types: json!({"invalid": true}),
                metadata_options: json!(null),
                path_substitutions: json!("invalid"),
                plugin_repositories: json!({}),
                server_options: json!([]),
            })
            .await
            .unwrap();
        assert_eq!(
            database.system_configuration_payloads().await.unwrap(),
            SystemConfigurationPayloads::default()
        );

        let named_key = format!("network-{}", Uuid::new_v4());
        database
            .update_named_configuration(&named_key, json!({"EnableHttps": false}))
            .await
            .unwrap();
        assert_eq!(
            database.named_configuration(&named_key).await.unwrap(),
            Some(json!({"EnableHttps": false}))
        );
        assert!(
            database
                .named_configurations()
                .await
                .unwrap()
                .iter()
                .any(|configuration| configuration.key == named_key)
        );

        let user_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)")
            .bind(user_id)
            .bind(format!("postgres-test-{user_id}"))
            .bind(now)
            .execute(&database.pool)
            .await
            .unwrap();
        database
            .update_user_configuration(user_id, json!({"AudioLanguagePreference": "es"}))
            .await
            .unwrap();
        assert_eq!(
            database.user_configuration(user_id).await.unwrap(),
            Some(json!({"AudioLanguagePreference": "es"}))
        );
        database.close().await;
    }
}
