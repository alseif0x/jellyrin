mod report;
mod runtime_hygiene;
mod spec;
mod value;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    str::FromStr,
    time::Instant,
};

use anyhow::Context;
use futures_util::TryStreamExt;
use jellyrin_db::{
    MediaItemFacetProjectionMode, ensure_media_item_facet_projection,
    ensure_media_item_query_filter_projection,
};
use report::{MigrationReport, OmittedTableReport, TableReport, ValidationReport};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, PgPool, Postgres, QueryBuilder, SqliteConnection, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
    sqlite::SqliteConnectOptions,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub use report::{
    FailureReport, ProviderUrlRetentionCounts, ProviderUrlRetentionReport, SchemaReport,
};
pub use runtime_hygiene::{
    RuntimeHygieneAuditOptions, RuntimeHygieneCounts, RuntimeHygieneReport, audit_runtime_hygiene,
};
use spec::{
    ColumnKind, MIGRATED_TABLES, OMITTED_TABLES, SOURCE_INFRASTRUCTURE_TABLES,
    TARGET_INFRASTRUCTURE_TABLES, TARGET_ONLY_OMITTED_TABLES, TableSpec,
};
use value::{TypedValue, parse_uuid};

pub const SOURCE_SCHEMA_VERSION: i64 = 202_608_220_001;
pub const TARGET_SCHEMA_VERSION: i64 = 202_608_220_001;
const MIN_POSTGRES_VERSION_NUM: i64 = 160_000;
const MIGRATION_BATCH_ROWS: usize = 500;
const TARGET_APPLICATION_LOCK_TIMEOUT: &str = "10s";
const SCHEMA_MIGRATION_LOCK_NAME: &str = "jellyrin:schema:migration";
const CASE_INSENSITIVE_PLUGIN_ID_TABLES: &[&str] = &[
    "installed_plugins",
    "plugin_manifests",
    "plugin_configurations",
    "plugin_permissions",
];
// Recompile this one-shot binary whenever the embedded migration set changes.
static POSTGRES_MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("../jellyrin-db/migrations-postgres");
static SQLITE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../jellyrin-db/migrations");

#[derive(Debug, Clone)]
pub struct MigrationOptions {
    pub source: std::path::PathBuf,
    pub target_url: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderUrlAuditOptions {
    pub source: Option<std::path::PathBuf>,
    pub target_url: String,
}

/// Fail-closed rollout audit for credential-bearing legacy provider locations.
///
/// The report contains counts only. Neither SQL nor errors select URL values or metadata, so the
/// command is safe to retain as deployment evidence. A non-zero count is an error: re-import the
/// affected provider catalogue before promotion instead of redacting durable rows in place.
pub async fn audit_provider_url_retention(
    options: ProviderUrlAuditOptions,
) -> anyhow::Result<ProviderUrlRetentionReport> {
    let started = OffsetDateTime::now_utc();
    let timer = Instant::now();
    let target = open_postgres(&options.target_url).await?;
    let mut snapshot = target
        .begin()
        .await
        .context("failed to start provider URL audit")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *snapshot)
        .await
        .context("failed to make provider URL audit read-only")?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *snapshot)
        .await
        .context("failed to bound provider URL audit statements")?;
    let postgres = postgres_provider_url_retention_counts(&mut snapshot).await?;
    snapshot
        .rollback()
        .await
        .context("failed to close provider URL audit snapshot")?;
    let sqlite = if let Some(source) = options.source {
        validate_source_path(&source)?;
        let mut connection = open_read_only_sqlite(&source).await?;
        Some(sqlite_provider_url_retention_counts(&mut connection).await?)
    } else {
        None
    };
    let finished = OffsetDateTime::now_utc();
    let clean = postgres.is_clean()
        && sqlite
            .as_ref()
            .is_none_or(ProviderUrlRetentionCounts::is_clean);
    Ok(ProviderUrlRetentionReport {
        report_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        status: if clean {
            "provider_url_retention_clean"
        } else {
            "provider_url_retention_findings"
        },
        postgres,
        sqlite,
        started_at: started.format(&Rfc3339)?,
        finished_at: finished.format(&Rfc3339)?,
        duration_ms: timer.elapsed().as_millis(),
    })
}

pub async fn apply_schema(target_url: &str) -> anyhow::Result<SchemaReport> {
    let started = OffsetDateTime::now_utc();
    let timer = Instant::now();
    let target = open_postgres(target_url).await?;
    let server_version_num = postgres_server_version_num(&target).await?;
    anyhow::ensure!(
        server_version_num >= MIN_POSTGRES_VERSION_NUM,
        "PostgreSQL 16 or newer is required"
    );
    // Extensions and migration history are database-global even when callers
    // target different schemas. Serialize schema application across CLI,
    // systemd and QA invocations; the transaction-scoped lock is released on
    // cancellation or any early error.
    let mut migration_lock = target
        .begin()
        .await
        .context("failed to start PostgreSQL schema lock transaction")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(SCHEMA_MIGRATION_LOCK_NAME)
        .execute(&mut *migration_lock)
        .await
        .context("failed to acquire PostgreSQL schema migration lock")?;
    ensure_local_migration_table(&target).await?;
    let schema_version_before = postgres_schema_version(&target).await?;
    let applied_before = postgres_applied_migration_count(&target).await?;
    // Some immutable catalogue-index expressions intentionally catch invalid
    // provider metadata with PL/pgSQL exception handlers. PostgreSQL exception
    // handlers open subtransactions, which parallel CREATE INDEX workers cannot
    // do. Keep migration DDL on one backend without changing an already-published
    // migration (and therefore its SQLx checksum).
    let mut migration_connection = target
        .acquire()
        .await
        .context("failed to acquire PostgreSQL schema migration connection")?;
    sqlx::query("SET max_parallel_maintenance_workers = 0")
        .execute(&mut *migration_connection)
        .await
        .context("failed to disable parallel PostgreSQL migration workers")?;
    POSTGRES_MIGRATOR
        .run_direct(None, &mut *migration_connection, false)
        .await
        .context("failed to apply embedded PostgreSQL schema migrations")?;
    drop(migration_connection);
    let facet_projection = ensure_media_item_facet_projection(
        &mut migration_lock,
        MediaItemFacetProjectionMode::EnsureCurrent,
    )
    .await
    .context("failed to ensure PostgreSQL media item facet projection")?;
    let query_filter_projection = ensure_media_item_query_filter_projection(
        &mut migration_lock,
        MediaItemFacetProjectionMode::EnsureCurrent,
    )
    .await
    .context("failed to ensure PostgreSQL media item query-filter projection")?;
    let schema_version_after = postgres_schema_version(&target)
        .await?
        .context("PostgreSQL migration history was not created")?;
    anyhow::ensure!(
        schema_version_after == TARGET_SCHEMA_VERSION,
        "embedded migrations produced schema version {schema_version_after}; expected {TARGET_SCHEMA_VERSION}"
    );
    let applied_after = postgres_applied_migration_count(&target).await?;
    let extension_available: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')")
            .fetch_one(&target)
            .await?;
    anyhow::ensure!(
        extension_available,
        "PostgreSQL extension pg_trgm is missing"
    );
    migration_lock
        .commit()
        .await
        .context("failed to release PostgreSQL schema migration lock")?;
    target.close().await;
    let finished = OffsetDateTime::now_utc();
    let applied_migrations = applied_after.saturating_sub(applied_before) as u64;
    Ok(SchemaReport {
        report_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        status: if applied_migrations == 0
            && !facet_projection.rebuilt
            && !query_filter_projection.rebuilt
        {
            "schema_current"
        } else {
            "schema_migrated"
        },
        postgres_server_version_num: server_version_num,
        schema_version_before,
        schema_version_after,
        embedded_migrations: POSTGRES_MIGRATOR.iter().count(),
        applied_migrations,
        started_at: format_timestamp(started)?,
        finished_at: format_timestamp(finished)?,
        duration_ms: timer.elapsed().as_millis(),
    })
}

pub async fn execute(options: MigrationOptions) -> anyhow::Result<MigrationReport> {
    validate_source_path(&options.source)?;
    let started = OffsetDateTime::now_utc();
    let timer = Instant::now();
    let mut source = open_read_only_sqlite(&options.source).await?;
    sqlx::query("BEGIN")
        .execute(&mut source)
        .await
        .context("failed to open a stable SQLite read transaction")?;

    let target = open_postgres(&options.target_url).await?;
    let mut transaction = target
        .begin()
        .await
        .context("failed to start PostgreSQL data migration transaction")?;
    // Each emptiness query must take a fresh snapshot after ACCESS EXCLUSIVE
    // lock acquisition, including when the cluster default is stricter.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED, READ WRITE")
        .execute(&mut *transaction)
        .await
        .context("failed to configure PostgreSQL data migration transaction")?;
    // Use the same transaction-scoped lock as every schema migrator. This
    // prevents DDL and concurrent data imports from racing any preflight, and
    // remains held until the import commits or a dry run rolls back.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(SCHEMA_MIGRATION_LOCK_NAME)
        .execute(&mut *transaction)
        .await
        .context("failed to acquire PostgreSQL data migration lock")?;

    let source_preflight = preflight_source(&mut source).await?;
    let target_preflight = preflight_target(&target).await?;
    let item_reference_count = validate_durable_item_references(&mut source).await?;
    lock_target_application_tables(&mut transaction).await?;
    ensure_target_application_tables_are_empty(&mut transaction).await?;

    let mut table_reports = Vec::with_capacity(MIGRATED_TABLES.len());
    let mut overall_digest = Sha256::new();
    for table in MIGRATED_TABLES {
        let report = migrate_table(&mut source, &mut transaction, table).await?;
        overall_digest.update(table.target.as_bytes());
        overall_digest.update(report.migrated_rows.to_be_bytes());
        overall_digest.update(report.source_normalized_digest_sha256.as_bytes());
        table_reports.push(report);
    }

    ensure_media_item_facet_projection(&mut transaction, MediaItemFacetProjectionMode::Force)
        .await
        .context("failed to rebuild PostgreSQL media item facets from migrated media items")?;

    let omitted = omitted_table_reports(&mut source).await?;
    validate_no_foreign_key_violations(&mut transaction).await?;
    sqlx::query("ROLLBACK")
        .execute(&mut source)
        .await
        .context("failed to close SQLite read transaction")?;
    source.close().await?;

    if options.dry_run {
        transaction.rollback().await?;
    } else {
        reset_activity_log_sequence(&mut transaction).await?;
        transaction.commit().await?;
    }
    target.close().await;

    let finished = OffsetDateTime::now_utc();
    Ok(MigrationReport {
        report_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        status: if options.dry_run {
            "dry_run_validated"
        } else {
            "committed"
        },
        dry_run: options.dry_run,
        source_schema_version: source_preflight.schema_version,
        target_schema_version: target_preflight.schema_version,
        started_at: format_timestamp(started)?,
        finished_at: format_timestamp(finished)?,
        duration_ms: timer.elapsed().as_millis(),
        validation: ValidationReport {
            sqlite_integrity_check: "ok",
            sqlite_foreign_key_violations: source_preflight.foreign_key_violations,
            postgres_server_version_num: target_preflight.server_version_num,
            target_required_tables_checked: target_preflight.required_tables_checked,
            durable_item_references_checked: item_reference_count,
            durable_item_references_missing: 0,
            provider_secret_references_checked: source_preflight.provider_secret_references_checked,
            provider_secret_references_missing: 0,
            target_was_empty_for_application_tables: true,
            transaction_outcome: if options.dry_run {
                "rolled_back"
            } else {
                "committed"
            },
        },
        tables: table_reports,
        omitted,
        overall_digest_sha256: format!("{:x}", overall_digest.finalize()),
    })
}

fn validate_source_path(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path).context("SQLite source does not exist")?;
    anyhow::ensure!(metadata.is_file(), "SQLite source must be a regular file");
    Ok(())
}

async fn open_read_only_sqlite(path: &Path) -> anyhow::Result<SqliteConnection> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .foreign_keys(true);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .context("failed to open SQLite source read-only")?;
    sqlx::query("PRAGMA query_only = ON")
        .execute(&mut connection)
        .await?;
    Ok(connection)
}

async fn open_postgres(target_url: &str) -> anyhow::Result<PgPool> {
    let options = PgConnectOptions::from_str(target_url)
        .context("target must be a valid PostgreSQL connection URL")?
        .application_name("jellyrin-migrate");
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect_with(options)
        .await
        .context("failed to connect to PostgreSQL target")
}

async fn postgres_provider_url_retention_counts(
    target: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<ProviderUrlRetentionCounts> {
    // Keep each full-catalog JSON inspection in its own bounded statement. Combining these
    // independent scans into one SELECT makes PostgreSQL evaluate all three before returning and
    // exceeded the fail-closed 10 s statement budget on the 494k-item staging catalogue.
    let remote_source_url_rows: i64 = sqlx::query_scalar(
        r#"
            SELECT count(*)
            FROM media_items
            WHERE EXISTS (
                SELECT 1 FROM jsonb_object_keys(metadata) AS key
                WHERE lower(key) = lower('RemoteSourceUrl')
            )
            "#,
    )
    .fetch_one(&mut **target)
    .await
    .context("failed to audit PostgreSQL media provider URL retention")?;
    let remote_probe_source_url_rows: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM media_items
        WHERE EXISTS (
            SELECT 1 FROM jsonb_each(metadata) AS probe(key, value)
            WHERE lower(probe.key) = lower('RemoteMediaProbe')
              AND jsonb_typeof(probe.value) = 'object'
              AND EXISTS (
                  SELECT 1 FROM jsonb_object_keys(probe.value) AS nested_key
                  WHERE lower(nested_key) = lower('SourceUrl')
              )
        )
        "#,
    )
    .fetch_one(&mut **target)
    .await
    .context("failed to audit PostgreSQL remote probe URL retention")?;
    let invalid_remote_probe_rows: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM media_items
        WHERE EXISTS (
            SELECT 1 FROM jsonb_each(metadata) AS probe(key, value)
            WHERE lower(probe.key) = lower('RemoteMediaProbe')
              AND jsonb_typeof(probe.value) <> 'object'
        )
        "#,
    )
    .fetch_one(&mut **target)
    .await
    .context("failed to audit malformed PostgreSQL remote probes")?;
    let live_tv_stream_url_rows: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM live_tv_channels AS channel
        JOIN live_tv_tuners AS tuner USING (tuner_id)
        WHERE NULLIF(btrim(channel.stream_url), '') IS NOT NULL
          AND (
              lower(tuner.provider_type) = 'xtream'
              OR lower(tuner.provider_type) LIKE 'plugin:%'
          )
        "#,
    )
    .fetch_one(&mut **target)
    .await
    .context("failed to audit PostgreSQL Live TV provider URL retention")?;
    provider_url_retention_counts(
        remote_source_url_rows,
        remote_probe_source_url_rows,
        invalid_remote_probe_rows,
        live_tv_stream_url_rows,
    )
}

async fn sqlite_provider_url_retention_counts(
    source: &mut SqliteConnection,
) -> anyhow::Result<ProviderUrlRetentionCounts> {
    let (remote_source_url_rows, remote_probe_source_url_rows, invalid_remote_probe_rows): (
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        r#"
            SELECT
                coalesce(sum(CASE
                    WHEN json_valid(metadata_json)
                     AND EXISTS (
                         SELECT 1 FROM json_each(metadata_json)
                         WHERE lower(key) = lower('RemoteSourceUrl')
                     )
                    THEN 1 ELSE 0 END
                ), 0),
                coalesce(sum(CASE
                    WHEN json_valid(metadata_json)
                     AND EXISTS (
                         SELECT 1 FROM json_each(metadata_json) AS probe
                         WHERE lower(probe.key) = lower('RemoteMediaProbe')
                           AND probe.type = 'object'
                           AND EXISTS (
                               SELECT 1 FROM json_each(probe.value)
                               WHERE lower(key) = lower('SourceUrl')
                           )
                     )
                    THEN 1 ELSE 0 END
                ), 0),
                coalesce(sum(CASE
                    WHEN json_valid(metadata_json)
                     AND EXISTS (
                         SELECT 1 FROM json_each(metadata_json) AS probe
                         WHERE lower(probe.key) = lower('RemoteMediaProbe')
                           AND probe.type <> 'object'
                     )
                    THEN 1 ELSE 0 END
                ), 0)
            FROM media_items
            "#,
    )
    .fetch_one(&mut *source)
    .await
    .context("failed to audit SQLite media provider URL retention")?;
    let live_tv_stream_url_rows: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM live_tv_channels AS channel
        JOIN live_tv_tuners AS tuner USING (tuner_id)
        WHERE NULLIF(trim(channel.stream_url), '') IS NOT NULL
          AND (
              lower(tuner.provider_type) = 'xtream'
              OR lower(tuner.provider_type) LIKE 'plugin:%'
          )
        "#,
    )
    .fetch_one(&mut *source)
    .await
    .context("failed to audit SQLite Live TV provider URL retention")?;
    provider_url_retention_counts(
        remote_source_url_rows,
        remote_probe_source_url_rows,
        invalid_remote_probe_rows,
        live_tv_stream_url_rows,
    )
}

fn provider_url_retention_counts(
    remote_source_url_rows: i64,
    remote_probe_source_url_rows: i64,
    invalid_remote_probe_rows: i64,
    live_tv_stream_url_rows: i64,
) -> anyhow::Result<ProviderUrlRetentionCounts> {
    Ok(ProviderUrlRetentionCounts {
        remote_source_url_rows: u64::try_from(remote_source_url_rows)
            .context("negative RemoteSourceUrl count")?,
        remote_probe_source_url_rows: u64::try_from(remote_probe_source_url_rows)
            .context("negative RemoteMediaProbe.SourceUrl count")?,
        invalid_remote_probe_rows: u64::try_from(invalid_remote_probe_rows)
            .context("negative malformed RemoteMediaProbe count")?,
        live_tv_stream_url_rows: u64::try_from(live_tv_stream_url_rows)
            .context("negative live_tv stream_url count")?,
    })
}

#[derive(Debug)]
struct SourcePreflight {
    schema_version: i64,
    foreign_key_violations: u64,
    provider_secret_references_checked: usize,
}

async fn preflight_source(source: &mut SqliteConnection) -> anyhow::Result<SourcePreflight> {
    let integrity = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
        .fetch_all(&mut *source)
        .await
        .context("SQLite integrity_check failed to execute")?;
    anyhow::ensure!(
        integrity.len() == 1 && integrity[0].eq_ignore_ascii_case("ok"),
        "SQLite integrity_check did not return ok"
    );
    let foreign_key_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *source)
        .await?
        .len() as u64;
    anyhow::ensure!(
        foreign_key_violations == 0,
        "SQLite foreign_key_check found {foreign_key_violations} violation(s)"
    );

    let migration_table_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(&mut *source)
    .await?;
    anyhow::ensure!(
        migration_table_exists == 1,
        "SQLite source has no SQLx migration history"
    );
    let failed_migrations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 0")
            .fetch_one(&mut *source)
            .await?;
    anyhow::ensure!(failed_migrations == 0, "SQLite has a failed migration");
    validate_sqlite_migration_history(source).await?;
    let schema_version: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
    )
    .fetch_one(&mut *source)
    .await?;
    anyhow::ensure!(
        schema_version == SOURCE_SCHEMA_VERSION,
        "unsupported SQLite schema version {schema_version}; expected {SOURCE_SCHEMA_VERSION}"
    );

    let source_tables =
        sqlx::query_scalar::<_, String>("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&mut *source)
            .await?
            .into_iter()
            .collect::<HashSet<_>>();
    for table in MIGRATED_TABLES {
        anyhow::ensure!(
            source_tables.contains(table.source),
            "SQLite source is missing required table {}",
            table.source
        );
        validate_sqlite_table_shape(source, table).await?;
    }
    for table in OMITTED_TABLES {
        anyhow::ensure!(
            source_tables.contains(table.table),
            "SQLite source is missing required table {}",
            table.table
        );
    }
    for table in SOURCE_INFRASTRUCTURE_TABLES {
        anyhow::ensure!(
            source_tables.contains(*table),
            "SQLite source is missing required infrastructure table {table}"
        );
    }
    validate_case_insensitive_plugin_ids(source).await?;
    let provider_secret_references_checked = validate_provider_secret_references(source).await?;
    Ok(SourcePreflight {
        schema_version,
        foreign_key_violations,
        provider_secret_references_checked,
    })
}

#[derive(Debug)]
struct ProviderSecretReferenceValue {
    id: String,
    provider_type: String,
    revision: i64,
}

fn collect_provider_secret_references(
    value: &serde_json::Value,
    references: &mut Vec<ProviderSecretReferenceValue>,
) -> anyhow::Result<()> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_provider_secret_references(value, references)?;
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("JellyrinProviderSecretRef") {
                let reference = reference
                    .as_object()
                    .context("JellyrinProviderSecretRef must be an object")?;
                let id = reference
                    .get("Id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("JellyrinProviderSecretRef.Id must be a non-empty string")?;
                let provider_type = reference
                    .get("Provider")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("JellyrinProviderSecretRef.Provider must be a non-empty string")?;
                let revision = reference
                    .get("Revision")
                    .and_then(serde_json::Value::as_i64)
                    .filter(|revision| *revision > 0)
                    .context("JellyrinProviderSecretRef.Revision must be positive")?;
                references.push(ProviderSecretReferenceValue {
                    id: id.to_owned(),
                    provider_type: provider_type.to_owned(),
                    revision,
                });
            }
            for value in object.values() {
                collect_provider_secret_references(value, references)?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn validate_provider_secret_references(
    source: &mut SqliteConnection,
) -> anyhow::Result<usize> {
    let secrets = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT secret_id, provider_type, revision FROM provider_secrets",
    )
    .fetch_all(&mut *source)
    .await?
    .into_iter()
    .map(|(secret_id, provider_type, revision)| (secret_id, (provider_type, revision)))
    .collect::<HashMap<_, _>>();
    let mut references_checked = 0usize;
    for (table, identity_column, configuration_column) in [
        ("plugin_configurations", "plugin_id", "configuration_json"),
        ("live_tv_tuners", "tuner_id", "configuration_json"),
        ("named_configurations", "key", "payload_json"),
    ] {
        let query = format!("SELECT {identity_column}, {configuration_column} FROM {table}");
        let configurations =
            sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(query.as_str()))
                .fetch_all(&mut *source)
                .await?;
        for (_identity, configuration) in configurations {
            let configuration = serde_json::from_str::<serde_json::Value>(&configuration)
                .with_context(|| {
                    format!("SQLite {table}.{configuration_column} is invalid JSON")
                })?;
            let mut references = Vec::new();
            collect_provider_secret_references(&configuration, &mut references).with_context(
                || format!("SQLite {table} has an invalid provider secret reference"),
            )?;
            for reference in references {
                let Some((provider_type, revision)) = secrets.get(&reference.id) else {
                    anyhow::bail!(
                        "SQLite {table} has a provider secret reference without a matching envelope"
                    );
                };
                anyhow::ensure!(
                    provider_type.eq_ignore_ascii_case(&reference.provider_type)
                        && *revision == reference.revision,
                    "SQLite {table} has a provider secret reference whose provider or revision does not match its envelope"
                );
                references_checked = references_checked
                    .checked_add(1)
                    .context("provider secret reference count overflow")?;
            }
        }
    }
    Ok(references_checked)
}

async fn validate_case_insensitive_plugin_ids(source: &mut SqliteConnection) -> anyhow::Result<()> {
    let mut canonical_ids = HashMap::<String, (&str, String)>::new();
    for table in CASE_INSENSITIVE_PLUGIN_ID_TABLES {
        let query = format!("SELECT plugin_id FROM {table}");
        let plugin_ids = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query.as_str()))
            .fetch_all(&mut *source)
            .await?;
        let mut normalized_ids = HashMap::with_capacity(plugin_ids.len());
        for plugin_id in plugin_ids {
            let normalized = plugin_id.to_lowercase();
            if let Some(first_plugin_id) = normalized_ids.get(&normalized) {
                anyhow::bail!(
                    "SQLite table {table} has plugin_id values {first_plugin_id:?} and {plugin_id:?} that collide case-insensitively"
                );
            }
            if let Some((first_table, first_plugin_id)) = canonical_ids.get(&normalized) {
                if first_plugin_id != &plugin_id {
                    anyhow::bail!(
                        "SQLite plugin_id casing is inconsistent across tables: {first_table} has {first_plugin_id:?} and {table} has {plugin_id:?} for normalized id {normalized:?}"
                    );
                }
            } else {
                canonical_ids.insert(normalized.clone(), (*table, plugin_id.clone()));
            }
            normalized_ids.insert(normalized, plugin_id);
        }
    }
    Ok(())
}

async fn validate_sqlite_table_shape(
    source: &mut SqliteConnection,
    table: &TableSpec,
) -> anyhow::Result<()> {
    let actual =
        sqlx::query_as::<_, (String, String)>("SELECT name, type FROM pragma_table_info(?1)")
            .bind(table.source)
            .fetch_all(&mut *source)
            .await?
            .into_iter()
            .collect::<HashMap<_, _>>();
    let expected = table
        .columns
        .iter()
        .map(|column| {
            (
                column.source.to_owned(),
                expected_sqlite_type(column.kind).to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    anyhow::ensure!(
        actual == expected,
        "SQLite table {} does not match the embedded migration schema",
        table.source
    );
    Ok(())
}

struct TargetPreflight {
    schema_version: i64,
    server_version_num: i64,
    required_tables_checked: usize,
}

async fn preflight_target(target: &PgPool) -> anyhow::Result<TargetPreflight> {
    let server_version_num = postgres_server_version_num(target).await?;
    anyhow::ensure!(
        server_version_num >= MIN_POSTGRES_VERSION_NUM,
        "PostgreSQL 16 or newer is required"
    );
    let extension_available: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')")
            .fetch_one(target)
            .await?;
    anyhow::ensure!(
        extension_available,
        "PostgreSQL extension pg_trgm is missing"
    );

    anyhow::ensure!(
        postgres_has_local_table(target, "_sqlx_migrations").await?,
        "PostgreSQL target has no SQLx migration history"
    );
    let failed_migrations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = false")
            .fetch_one(target)
            .await?;
    anyhow::ensure!(failed_migrations == 0, "PostgreSQL has a failed migration");
    validate_postgres_migration_history(target).await?;
    let schema_version = postgres_schema_version(target)
        .await?
        .context("PostgreSQL target has no SQLx migration history")?;
    anyhow::ensure!(
        schema_version == TARGET_SCHEMA_VERSION,
        "unsupported PostgreSQL schema version {schema_version}; expected {TARGET_SCHEMA_VERSION}"
    );

    let required_tables = target_application_tables()
        .chain(TARGET_INFRASTRUCTURE_TABLES.iter().copied())
        .collect::<HashSet<_>>();
    let target_tables = sqlx::query_scalar::<_, String>(
        r#"
        SELECT table_name
        FROM information_schema.tables
        WHERE table_schema = current_schema() AND table_type = 'BASE TABLE'
        "#,
    )
    .fetch_all(target)
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    for table in &required_tables {
        anyhow::ensure!(
            target_tables.contains(*table),
            "PostgreSQL target is missing required table {table}"
        );
    }
    validate_postgres_table_shapes(target).await?;
    Ok(TargetPreflight {
        schema_version,
        server_version_num,
        required_tables_checked: required_tables.len(),
    })
}

async fn validate_postgres_table_shapes(target: &PgPool) -> anyhow::Result<()> {
    let columns = sqlx::query_as::<_, (String, String, String, String)>(
        r#"
        SELECT table_name, column_name, data_type, is_nullable
        FROM information_schema.columns
        WHERE table_schema = current_schema()
        "#,
    )
    .fetch_all(target)
    .await?;
    let mut actual = HashMap::<String, HashMap<String, (String, bool)>>::new();
    for (table, column, data_type, nullable) in columns {
        actual
            .entry(table)
            .or_default()
            .insert(column, (data_type, nullable == "YES"));
    }
    for table in MIGRATED_TABLES {
        let expected = table
            .columns
            .iter()
            .map(|column| {
                (
                    column.target.to_owned(),
                    (
                        expected_postgres_type(column.kind).to_owned(),
                        column.nullable,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        anyhow::ensure!(
            actual.get(table.target) == Some(&expected),
            "PostgreSQL table {} does not match the embedded migration schema",
            table.target
        );
    }
    Ok(())
}

async fn postgres_server_version_num(target: &PgPool) -> anyhow::Result<i64> {
    let version_raw: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(target)
        .await?;
    version_raw
        .parse::<i64>()
        .context("PostgreSQL returned an invalid server_version_num")
}

async fn validate_sqlite_migration_history(source: &mut SqliteConnection) -> anyhow::Result<()> {
    let actual = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = 1 ORDER BY version",
    )
    .fetch_all(&mut *source)
    .await?;
    let expected = SQLITE_MIGRATOR.iter().collect::<Vec<_>>();
    anyhow::ensure!(
        actual.len() == expected.len(),
        "SQLite migration history is incomplete or contains unexpected migrations"
    );
    for ((actual_version, actual_checksum), expected_migration) in actual.iter().zip(expected) {
        anyhow::ensure!(
            *actual_version == expected_migration.version
                && actual_checksum.as_slice() == expected_migration.checksum.as_ref(),
            "SQLite migration history does not match the embedded migration set"
        );
    }
    Ok(())
}

async fn validate_postgres_migration_history(target: &PgPool) -> anyhow::Result<()> {
    let actual = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = true ORDER BY version",
    )
    .fetch_all(target)
    .await?;
    let expected = POSTGRES_MIGRATOR.iter().collect::<Vec<_>>();
    anyhow::ensure!(
        actual.len() == expected.len(),
        "PostgreSQL migration history is incomplete or contains unexpected migrations"
    );
    for ((actual_version, actual_checksum), expected_migration) in actual.iter().zip(expected) {
        anyhow::ensure!(
            *actual_version == expected_migration.version
                && actual_checksum.as_slice() == expected_migration.checksum.as_ref(),
            "PostgreSQL migration history does not match the embedded migration set"
        );
    }
    Ok(())
}

async fn postgres_schema_version(target: &PgPool) -> anyhow::Result<Option<i64>> {
    if !postgres_has_local_table(target, "_sqlx_migrations").await? {
        return Ok(None);
    }
    sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = true")
        .fetch_one(target)
        .await
        .map_err(Into::into)
}

async fn postgres_applied_migration_count(target: &PgPool) -> anyhow::Result<i64> {
    if postgres_schema_version(target).await?.is_none() {
        return Ok(0);
    }
    sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = true")
        .fetch_one(target)
        .await
        .map_err(Into::into)
}

async fn postgres_has_local_table(target: &PgPool, table: &str) -> anyhow::Result<bool> {
    sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM information_schema.tables
            WHERE table_schema = current_schema() AND table_name = $1
        )
        "#,
    )
    .bind(table)
    .fetch_one(target)
    .await
    .map_err(Into::into)
}

async fn ensure_local_migration_table(target: &PgPool) -> anyhow::Result<()> {
    if postgres_has_local_table(target, "_sqlx_migrations").await? {
        return Ok(());
    }
    let schema: String = sqlx::query_scalar("SELECT quote_ident(current_schema())")
        .fetch_one(target)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"
        CREATE TABLE IF NOT EXISTS {schema}._sqlx_migrations (
            version bigint PRIMARY KEY,
            description text NOT NULL,
            installed_on timestamptz NOT NULL DEFAULT now(),
            success boolean NOT NULL,
            checksum bytea NOT NULL,
            execution_time bigint NOT NULL
        )
        "#
    )))
    .execute(target)
    .await
    .context("failed to create migration history in the target schema")?;
    Ok(())
}

async fn validate_durable_item_references(source: &mut SqliteConnection) -> anyhow::Result<usize> {
    const REFERENCES: &str = r#"
        SELECT item_id FROM playback_states
        UNION ALL SELECT item_id FROM media_list_items
        UNION ALL SELECT item_id FROM media_item_lyrics
        UNION ALL SELECT item_id FROM activity_log_entries WHERE item_id IS NOT NULL
    "#;
    let missing: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"
        SELECT COUNT(*)
        FROM ({REFERENCES}) AS durable_references
        LEFT JOIN media_items ON media_items.id = durable_references.item_id
        WHERE media_items.id IS NULL
        "#
    )))
    .fetch_one(&mut *source)
    .await?;
    anyhow::ensure!(
        missing == 0,
        "SQLite has {missing} durable reference(s) to missing media items"
    );

    let reference_query = format!("SELECT item_id FROM ({REFERENCES}) AS durable_references");
    let mut references =
        sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(reference_query.as_str()))
            .fetch(&mut *source);
    let mut reference_count = 0_usize;
    while let Some(raw) = references.try_next().await? {
        parse_uuid(&raw).context("invalid durable media item reference")?;
        reference_count = reference_count
            .checked_add(1)
            .context("durable media item reference count overflow")?;
    }
    Ok(reference_count)
}

async fn ensure_target_application_tables_are_empty(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    for table in target_application_tables() {
        let has_rows: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM {table})"
        )))
        .fetch_one(&mut **transaction)
        .await?;
        anyhow::ensure!(
            !has_rows,
            "PostgreSQL application table {table} is not empty; refusing to merge or overwrite"
        );
    }
    Ok(())
}

fn target_application_tables() -> impl Iterator<Item = &'static str> {
    MIGRATED_TABLES
        .iter()
        .map(|table| table.target)
        .chain(OMITTED_TABLES.iter().map(|table| table.table))
        .chain(TARGET_ONLY_OMITTED_TABLES.iter().map(|table| table.table))
}

fn target_application_lock_sql() -> String {
    format!(
        "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        target_application_tables().collect::<Vec<_>>().join(", ")
    )
}

async fn lock_target_application_tables(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(TARGET_APPLICATION_LOCK_TIMEOUT)
        .execute(&mut **transaction)
        .await
        .context("failed to configure PostgreSQL application table lock timeout")?;
    if let Err(error) = sqlx::query(sqlx::AssertSqlSafe(target_application_lock_sql()))
        .execute(&mut **transaction)
        .await
    {
        let timed_out = error
            .as_database_error()
            .and_then(|database_error| database_error.code())
            .is_some_and(|code| code.as_ref() == "55P03");
        if timed_out {
            anyhow::bail!(
                "timed out after {TARGET_APPLICATION_LOCK_TIMEOUT} waiting for exclusive PostgreSQL application table locks; stop every Jellyrin runtime and retry"
            );
        }
        return Err(error)
            .context("failed to lock PostgreSQL application tables for exclusive data migration");
    }
    Ok(())
}

async fn migrate_table(
    source: &mut SqliteConnection,
    transaction: &mut Transaction<'_, Postgres>,
    table: &'static TableSpec,
) -> anyhow::Result<TableReport> {
    let source_columns = table
        .columns
        .iter()
        .map(|column| column.source)
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!(
        "SELECT {source_columns} FROM {} ORDER BY {}",
        table.source, table.order_by
    );
    let mut stream = sqlx::query(sqlx::AssertSqlSafe(select.as_str())).fetch(&mut *source);
    let mut batch = Vec::with_capacity(MIGRATION_BATCH_ROWS);
    let mut digest = NormalizedTableDigest::new(table.target);
    let mut source_rows = 0_u64;

    while let Some(row) = stream.try_next().await? {
        let mut values = Vec::with_capacity(table.columns.len());
        for column in table.columns {
            let value = TypedValue::from_sqlite(&row, column).with_context(|| {
                format!(
                    "failed to normalize {}.{} as {:?}",
                    table.source, column.source, column.kind
                )
            })?;
            values.push(value);
        }
        digest.add_row(&values);
        batch.push(values);
        source_rows += 1;
        if batch.len() == MIGRATION_BATCH_ROWS {
            insert_batch(transaction, table, &batch).await?;
            batch.clear();
        }
    }
    drop(stream);
    if !batch.is_empty() {
        insert_batch(transaction, table, &batch).await?;
    }

    let source_digest = digest.finalize();
    let (target_rows, target_digest) = target_table_digest(transaction, table).await?;
    anyhow::ensure!(
        target_rows == source_rows,
        "row count mismatch for {}: source {source_rows}, target {target_rows}",
        table.target
    );
    anyhow::ensure!(
        target_digest == source_digest,
        "normalized digest mismatch for {} after insertion",
        table.target
    );
    Ok(TableReport {
        table: table.target,
        source_rows,
        migrated_rows: source_rows,
        target_rows_in_transaction: target_rows,
        source_normalized_digest_sha256: source_digest,
        target_normalized_digest_sha256: target_digest,
        validation: "source_target_normalized_multiset_digest_and_count_matched",
    })
}

async fn target_table_digest(
    transaction: &mut Transaction<'_, Postgres>,
    table: &TableSpec,
) -> anyhow::Result<(u64, String)> {
    let target_columns = table
        .columns
        .iter()
        .map(|column| column.target)
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!("SELECT {target_columns} FROM {}", table.target);
    let mut stream = sqlx::query(sqlx::AssertSqlSafe(select.as_str())).fetch(&mut **transaction);
    let mut digest = NormalizedTableDigest::new(table.target);
    while let Some(row) = stream.try_next().await? {
        let mut values = Vec::with_capacity(table.columns.len());
        for column in table.columns {
            let value = TypedValue::from_postgres(&row, column).with_context(|| {
                format!(
                    "failed to normalize {}.{} as {:?}",
                    table.target, column.target, column.kind
                )
            })?;
            values.push(value);
        }
        digest.add_row(&values);
    }
    Ok((digest.rows(), digest.finalize()))
}

struct NormalizedTableDigest {
    table: &'static str,
    rows: u64,
    xor: [u8; 32],
    sum: [u64; 4],
    sum_of_squares: [u64; 4],
}

impl NormalizedTableDigest {
    fn new(table: &'static str) -> Self {
        Self {
            table,
            rows: 0,
            xor: [0; 32],
            sum: [0; 4],
            sum_of_squares: [0; 4],
        }
    }

    fn add_row(&mut self, values: &[TypedValue]) {
        let mut row_digest = Sha256::new();
        row_digest.update(b"jellyrin-normalized-row-v1\0");
        row_digest.update((self.table.len() as u64).to_be_bytes());
        row_digest.update(self.table.as_bytes());
        row_digest.update((values.len() as u64).to_be_bytes());
        for value in values {
            value.update_digest(&mut row_digest);
        }
        let row_hash: [u8; 32] = row_digest.finalize().into();
        for (target, value) in self.xor.iter_mut().zip(row_hash) {
            *target ^= value;
        }
        for (index, bytes) in row_hash.as_chunks::<8>().0.iter().enumerate() {
            let value = u64::from_be_bytes(*bytes);
            self.sum[index] = self.sum[index].wrapping_add(value);
            self.sum_of_squares[index] =
                self.sum_of_squares[index].wrapping_add(value.wrapping_mul(value));
        }
        self.rows += 1;
    }

    fn rows(&self) -> u64 {
        self.rows
    }

    fn finalize(self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"jellyrin-normalized-multiset-v1\0");
        digest.update((self.table.len() as u64).to_be_bytes());
        digest.update(self.table.as_bytes());
        digest.update(self.rows.to_be_bytes());
        digest.update(self.xor);
        for value in self.sum.into_iter().chain(self.sum_of_squares) {
            digest.update(value.to_be_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}

async fn insert_batch(
    transaction: &mut Transaction<'_, Postgres>,
    table: &TableSpec,
    rows: &[Vec<TypedValue>],
) -> anyhow::Result<()> {
    let target_columns = table
        .columns
        .iter()
        .map(|column| column.target)
        .collect::<Vec<_>>()
        .join(", ");
    let mut builder =
        QueryBuilder::<Postgres>::new(format!("INSERT INTO {} ({target_columns}) ", table.target));
    builder.push_values(rows, |mut values, row| {
        for value in row {
            match value {
                TypedValue::Text(value) => {
                    values.push_bind(value);
                }
                TypedValue::Bytes(value) => {
                    values.push_bind(value.as_deref());
                }
                TypedValue::Uuid(value) => {
                    values.push_bind(*value);
                }
                TypedValue::Timestamp(value) => {
                    values.push_bind(*value);
                }
                TypedValue::Bool(value) => {
                    values.push_bind(*value);
                }
                TypedValue::I16(value) => {
                    values.push_bind(*value);
                }
                TypedValue::I32(value) => {
                    values.push_bind(*value);
                }
                TypedValue::I64(value) => {
                    values.push_bind(*value);
                }
                TypedValue::F64(value) => {
                    values.push_bind(*value);
                }
                TypedValue::Json(value) => {
                    values.push_bind(value);
                }
            }
        }
    });
    builder
        .build()
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("failed to insert normalized rows into {}", table.target))?;
    Ok(())
}

async fn omitted_table_reports(
    source: &mut SqliteConnection,
) -> anyhow::Result<Vec<OmittedTableReport>> {
    let mut reports = Vec::with_capacity(OMITTED_TABLES.len() + TARGET_ONLY_OMITTED_TABLES.len());
    for table in OMITTED_TABLES {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {}",
            table.table
        )))
        .fetch_one(&mut *source)
        .await?;
        reports.push(OmittedTableReport {
            table: table.table,
            source_rows: count.max(0) as u64,
            strategy: table.strategy,
            reason: table.reason,
        });
    }
    reports.extend(
        TARGET_ONLY_OMITTED_TABLES
            .iter()
            .map(|table| OmittedTableReport {
                table: table.table,
                source_rows: 0,
                strategy: table.strategy,
                reason: table.reason,
            }),
    );
    Ok(reports)
}

async fn validate_no_foreign_key_violations(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let violations: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM (
            SELECT conrelid
            FROM pg_constraint
            WHERE contype = 'f'
              AND connamespace = (SELECT oid FROM pg_namespace WHERE nspname = current_schema())
              AND NOT convalidated
        ) AS invalid_constraints
        "#,
    )
    .fetch_one(&mut **transaction)
    .await?;
    anyhow::ensure!(violations == 0, "PostgreSQL has unvalidated foreign keys");
    Ok(())
}

async fn reset_activity_log_sequence(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        SELECT setval(
            pg_get_serial_sequence('activity_log_entries', 'id'),
            GREATEST(COALESCE(MAX(id), 1), 1),
            COUNT(*) > 0
        )
        FROM activity_log_entries
        "#,
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn format_timestamp(value: OffsetDateTime) -> anyhow::Result<String> {
    value
        .format(&Rfc3339)
        .context("failed to format report timestamp")
}

fn expected_sqlite_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Text | ColumnKind::Uuid | ColumnKind::Timestamp | ColumnKind::Json(_) => "TEXT",
        ColumnKind::Bytes => "BLOB",
        ColumnKind::Bool | ColumnKind::I16 | ColumnKind::I32 | ColumnKind::I64 => "INTEGER",
        ColumnKind::F64 => "REAL",
    }
}

fn expected_postgres_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Text => "text",
        ColumnKind::Bytes => "bytea",
        ColumnKind::Uuid => "uuid",
        ColumnKind::Timestamp => "timestamp with time zone",
        ColumnKind::Bool => "boolean",
        ColumnKind::I16 => "smallint",
        ColumnKind::I32 => "integer",
        ColumnKind::I64 => "bigint",
        ColumnKind::F64 => "double precision",
        ColumnKind::Json(_) => "jsonb",
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr};

    use super::*;
    use sqlx::{
        SqlitePool,
        postgres::PgPoolOptions,
        sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn sqlite_provider_url_audit_is_counts_only_and_provider_aware() {
        let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE media_items (metadata_json TEXT NOT NULL)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE live_tv_tuners (tuner_id TEXT PRIMARY KEY, provider_type TEXT NOT NULL)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE live_tv_channels (tuner_id TEXT NOT NULL, stream_url TEXT NOT NULL)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query("INSERT INTO media_items(metadata_json) VALUES ('{}')")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO live_tv_tuners VALUES ('m3u', 'm3u')")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO live_tv_channels VALUES ('m3u', 'https://legacy.invalid/live.m3u8')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let clean = sqlite_provider_url_retention_counts(&mut connection)
            .await
            .unwrap();
        assert!(clean.is_clean());

        for metadata in [
            r#"{"remotesourceurl":null}"#,
            r#"{"RemoteMediaProbe":{"sourceurl":"canary://must-not-be-reported"}}"#,
            r#"{"RemoteMediaProbe":"malformed-canary"}"#,
        ] {
            sqlx::query("INSERT INTO media_items(metadata_json) VALUES (?1)")
                .bind(metadata)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO live_tv_tuners VALUES ('xtream', 'XTREAM')")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO live_tv_tuners VALUES ('plugin', 'plugin:magstv')")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO live_tv_channels VALUES ('xtream', 'canary://user/password')")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO live_tv_channels VALUES ('plugin', 'canary://token')")
            .execute(&mut connection)
            .await
            .unwrap();

        assert_eq!(
            sqlite_provider_url_retention_counts(&mut connection)
                .await
                .unwrap(),
            ProviderUrlRetentionCounts {
                remote_source_url_rows: 1,
                remote_probe_source_url_rows: 1,
                invalid_remote_probe_rows: 1,
                live_tv_stream_url_rows: 2,
            }
        );
    }

    #[test]
    fn source_path_requires_an_existing_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_source_path(directory.path()).is_err());
        assert!(validate_source_path(&directory.path().join("missing.db")).is_err());
        let file = directory.path().join("source.db");
        std::fs::write(&file, []).unwrap();
        assert!(validate_source_path(&file).is_ok());
    }

    #[test]
    fn migration_specs_have_unique_safe_identifiers() {
        let mut tables = HashSet::new();
        for table in MIGRATED_TABLES {
            assert!(tables.insert(table.target));
            assert!(safe_identifier(table.source));
            assert!(safe_identifier(table.target));
            assert!(!table.columns.is_empty());
            let mut source_columns = HashSet::new();
            let mut target_columns = HashSet::new();
            for column in table.columns {
                assert!(safe_identifier(column.source));
                assert!(safe_identifier(column.target));
                assert!(source_columns.insert(column.source));
                assert!(target_columns.insert(column.target));
            }
            for order_column in table.order_by.split(',').map(str::trim) {
                assert!(safe_identifier(order_column));
                assert!(source_columns.contains(order_column));
            }
        }
    }

    #[test]
    fn normalized_table_digest_is_order_independent_and_content_sensitive() {
        let first = vec![
            TypedValue::Uuid(Some(
                Uuid::parse_str("12345678-f000-4000-8000-000000000001").unwrap(),
            )),
            TypedValue::Text(Some("first".to_owned())),
        ];
        let second = vec![
            TypedValue::Uuid(Some(
                Uuid::parse_str("12345678-0000-4000-8000-000000000002").unwrap(),
            )),
            TypedValue::Text(Some("second".to_owned())),
        ];
        let digest = |rows: &[&[TypedValue]]| {
            let mut digest = NormalizedTableDigest::new("media_items");
            for row in rows {
                digest.add_row(row);
            }
            digest.finalize()
        };
        assert_eq!(
            digest(&[first.as_slice(), second.as_slice()]),
            digest(&[second.as_slice(), first.as_slice()])
        );
        assert_ne!(digest(&[first.as_slice()]), digest(&[second.as_slice()]));
        assert_ne!(
            digest(&[first.as_slice()]),
            digest(&[first.as_slice(), first.as_slice()])
        );
    }

    #[test]
    fn omitted_tables_are_explicit_and_disjoint() {
        let migrated = MIGRATED_TABLES
            .iter()
            .map(|table| table.source)
            .collect::<HashSet<_>>();
        let mut omitted = HashSet::new();
        for table in OMITTED_TABLES {
            assert!(omitted.insert(table.table));
            assert!(!migrated.contains(table.table));
            assert!(!table.strategy.is_empty());
            assert!(!table.reason.is_empty());
        }
        for table in TARGET_ONLY_OMITTED_TABLES {
            assert!(omitted.insert(table.table));
            assert!(!migrated.contains(table.table));
            assert!(!table.strategy.is_empty());
            assert!(!table.reason.is_empty());
        }
        for table in TARGET_INFRASTRUCTURE_TABLES {
            assert!(!migrated.contains(table));
            assert!(!omitted.contains(table));
            assert!(safe_identifier(table));
        }
        for required in [
            "live_tv_programs",
            "active_playback_sessions",
            "transcode_sessions",
        ] {
            assert!(omitted.contains(required));
        }
        for preserved in ["virtual_folders", "media_items"] {
            assert!(migrated.contains(preserved));
        }
        assert!(omitted.contains("catalog_sync_runs"));
    }

    #[test]
    fn exclusive_target_lock_covers_every_application_table_once() {
        let tables = target_application_tables().collect::<Vec<_>>();
        let unique_tables = tables.iter().copied().collect::<HashSet<_>>();
        assert_eq!(tables.len(), unique_tables.len());
        assert_eq!(
            tables.len(),
            MIGRATED_TABLES.len() + OMITTED_TABLES.len() + TARGET_ONLY_OMITTED_TABLES.len()
        );
        assert!(tables.iter().all(|table| safe_identifier(table)));

        let lock_sql = target_application_lock_sql();
        assert!(lock_sql.starts_with("LOCK TABLE "));
        assert!(lock_sql.ends_with(" IN ACCESS EXCLUSIVE MODE"));
        assert!(tables.iter().all(|table| lock_sql.contains(table)));
    }

    #[tokio::test]
    async fn source_preflight_rejects_case_insensitive_plugin_id_collisions() {
        let mut source = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        for table in CASE_INSENSITIVE_PLUGIN_ID_TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {table} (plugin_id TEXT NOT NULL)"
            )))
            .execute(&mut source)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO plugin_manifests (plugin_id) VALUES ('CAFÉ'), ('café')")
            .execute(&mut source)
            .await
            .unwrap();

        let error = validate_case_insensitive_plugin_ids(&mut source)
            .await
            .expect_err("case-insensitive plugin identifiers must not collide");
        let message = error.to_string();
        assert!(message.contains("plugin_manifests"));
        assert!(message.contains("CAFÉ"));
        assert!(message.contains("café"));
    }

    #[tokio::test]
    async fn source_preflight_rejects_plugin_id_casing_inconsistent_across_tables() {
        let mut source = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        for table in CASE_INSENSITIVE_PLUGIN_ID_TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {table} (plugin_id TEXT NOT NULL)"
            )))
            .execute(&mut source)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO installed_plugins (plugin_id) VALUES ('Plugin-ID')")
            .execute(&mut source)
            .await
            .unwrap();
        sqlx::query("INSERT INTO plugin_manifests (plugin_id) VALUES ('plugin-id')")
            .execute(&mut source)
            .await
            .unwrap();

        let error = validate_case_insensitive_plugin_ids(&mut source)
            .await
            .expect_err("plugin identifier casing must agree across tables");
        let message = error.to_string();
        assert!(message.contains("installed_plugins"));
        assert!(message.contains("plugin_manifests"));
        assert!(message.contains("Plugin-ID"));
        assert!(message.contains("plugin-id"));
        assert!(message.contains("normalized id"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT (SELECT count(*) FROM installed_plugins) + \
                        (SELECT count(*) FROM plugin_manifests)",
            )
            .fetch_one(&mut source)
            .await
            .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn source_preflight_validates_provider_secret_references() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-secret-references.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SQLITE_MIGRATOR.run(&pool).await.unwrap();
        let now = "2026-08-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO provider_secrets \
             (secret_id, provider_type, envelope_version, key_id, nonce, ciphertext, \
              revision, created_at, updated_at) \
             VALUES ('fixture-secret', 'xtream', 1, 'fixture-key', ?1, ?2, 7, ?3, ?3)",
        )
        .bind(vec![0x11_u8; 12])
        .bind(vec![0x22_u8; 32])
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        let direct_reference = serde_json::json!({
            "JellyrinProviderSecretRef": {
                "Id": "fixture-secret",
                "Provider": "Xtream",
                "Revision": 7
            }
        })
        .to_string();
        sqlx::query(
            "INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at) \
             VALUES ('plugin-a', ?1, ?2)",
        )
        .bind(&direct_reference)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO live_tv_tuners \
             (tuner_id, provider_type, name, configuration_json, created_at, updated_at) \
             VALUES ('tuner-a', 'xtream', 'Xtream', ?1, ?2, ?2)",
        )
        .bind(&direct_reference)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        let nested_reference = serde_json::json!({
            "TunerHosts": [{
                "JellyrinProviderSecretRef": {
                    "Id": "fixture-secret",
                    "Provider": "xtream",
                    "Revision": 7
                }
            }]
        })
        .to_string();
        sqlx::query(
            "INSERT INTO named_configurations (key, payload_json, updated_at) \
             VALUES ('livetv', ?1, ?2)",
        )
        .bind(nested_reference)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

        let mut connection = pool.acquire().await.unwrap();
        let preflight = preflight_source(&mut connection).await.unwrap();
        assert_eq!(preflight.provider_secret_references_checked, 3);
        drop(connection);

        let missing_reference = serde_json::json!({
            "JellyrinProviderSecretRef": {
                "Id": "missing-secret",
                "Provider": "xtream",
                "Revision": 1
            }
        })
        .to_string();
        sqlx::query(
            "UPDATE plugin_configurations SET configuration_json = ?1 \
             WHERE plugin_id = 'plugin-a'",
        )
        .bind(missing_reference)
        .execute(&pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let error = preflight_source(&mut connection)
            .await
            .expect_err("a missing provider secret envelope must fail preflight");
        assert!(error.to_string().contains("without a matching envelope"));
        drop(connection);
        pool.close().await;
    }

    #[tokio::test]
    async fn embedded_sqlite_schema_is_fully_classified() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("coverage.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SQLITE_MIGRATOR.run(&pool).await.unwrap();
        let actual = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect::<HashSet<_>>();
        let expected = MIGRATED_TABLES
            .iter()
            .map(|table| table.source.to_owned())
            .chain(OMITTED_TABLES.iter().map(|table| table.table.to_owned()))
            .chain(
                SOURCE_INFRASTRUCTURE_TABLES
                    .iter()
                    .map(|table| (*table).to_owned()),
            )
            .collect::<HashSet<_>>();
        assert_eq!(actual, expected);
        for table in MIGRATED_TABLES {
            let actual_columns = sqlx::query_as::<_, (String, String)>(
                "SELECT name, type FROM pragma_table_info(?1)",
            )
            .bind(table.source)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .collect::<HashMap<_, _>>();
            let expected_columns = table
                .columns
                .iter()
                .map(|column| {
                    (
                        column.source.to_owned(),
                        expected_sqlite_type(column.kind).to_owned(),
                    )
                })
                .collect::<HashMap<_, _>>();
            assert_eq!(
                actual_columns, expected_columns,
                "SQLite column classification is incomplete for {}",
                table.source
            );
        }
        pool.close().await;
    }

    #[test]
    fn embedded_postgres_migrator_matches_expected_target_schema() {
        let versions = POSTGRES_MIGRATOR
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(versions.last(), Some(&TARGET_SCHEMA_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            POSTGRES_MIGRATOR
                .iter()
                .all(|migration| !migration.sql.as_ref().is_empty())
        );
        let projection_migration = POSTGRES_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_109)
            .expect("media item facet projection marker migration must be embedded");
        let sql = projection_migration.sql.as_ref().to_ascii_lowercase();
        assert!(sql.contains("create table jellyrin_derived_projection_versions"));
        assert!(!sql.contains("insert into jellyrin_derived_projection_versions"));
    }

    #[test]
    fn postgres_live_tv_source_migration_preflights_and_validates_without_cleanup() {
        let migration = POSTGRES_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_106)
            .expect("live TV source invariant migration must be embedded");
        let sql = migration.sql.as_ref().to_ascii_lowercase();
        assert!(sql.contains("mixed_row_count"));
        assert!(sql.contains("live_tv_channels_opaque_reference_excludes_stream_url"));
        assert!(sql.contains("not valid"));
        assert!(sql.contains("validate constraint"));
        assert!(!sql.contains("delete from live_tv_channels"));
        assert!(!sql.contains("update live_tv_channels set"));
    }

    #[test]
    fn runtime_migration_history_privileges_are_select_only_when_role_exists() {
        let migration = POSTGRES_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_100)
            .expect("runtime privilege migration must be embedded");
        let sql = migration.sql.as_ref().to_ascii_lowercase();
        assert!(sql.contains("pg_catalog.pg_roles"));
        assert!(sql.contains("rolname = 'jellyrin_runtime'"));
        assert!(
            sql.contains("revoke all privileges on table _sqlx_migrations from jellyrin_runtime")
        );
        assert!(sql.contains("grant select on table _sqlx_migrations to jellyrin_runtime"));
    }

    #[test]
    fn query_filter_summary_publication_boundary_is_narrow_and_fail_closed() {
        let migration = POSTGRES_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_120)
            .expect("query-filter summary publication-boundary migration must be embedded");
        let sql = migration.sql.as_ref().to_ascii_lowercase();

        assert!(sql.contains("security definer"));
        assert!(sql.contains("security invoker"));
        assert!(sql.contains("current_user = summary_owner"));
        assert!(sql.contains("set search_path to pg_catalog, %i, pg_temp"));
        assert!(!sql.contains("current_setting"));
        assert!(!sql.contains("jellyrin.query_filter_summary_source_patch"));
        assert!(!sql.contains("jellyrin.query_filter_summary_rebuild"));
        assert!(
            sql.contains("revoke all privileges on table media_item_query_filter_summary_values")
        );
        assert!(sql.contains("grant select on table media_item_query_filter_summary_values"));
        assert!(sql.contains(
            "revoke all on function jellyrin_rebuild_query_filter_summary(uuid) from public"
        ));
        assert!(
            sql.contains("grant execute on function jellyrin_rebuild_query_filter_summary(uuid)")
        );
        assert!(sql.contains("create function jellyrin_reconcile_query_filter_summary_item"));
        assert!(
            !sql.contains("grant insert, update, delete on table media_item_query_filter_summary")
        );
    }

    #[test]
    fn tv_series_publication_serializes_every_invalidation_trigger() {
        let migration = POSTGRES_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_124)
            .expect("TV-series publication-serialization migration must be embedded");
        let sql = migration.sql.as_ref().to_ascii_lowercase();

        for function_name in [
            "jellyrin_invalidate_tv_series_after_insert",
            "jellyrin_invalidate_tv_series_after_delete",
            "jellyrin_invalidate_tv_series_after_update",
        ] {
            assert!(sql.contains(&format!("create or replace function {function_name}")));
            assert!(sql.contains(&format!(
                "revoke all on function {function_name}() from public"
            )));
        }
        assert_eq!(
            sql.matches("jellyrin-tv-series-projection:").count(),
            3,
            "every source trigger must use the rebuild's advisory-lock namespace"
        );
        assert!(sql.contains("security invoker"));
        assert!(sql.contains("order by folder_id"));
        assert_eq!(
            sql.matches("foreach locked_folder_id in array affected_folder_ids")
                .count(),
            3
        );
        assert_eq!(
            sql.matches("if affected_folder_ids is null then").count(),
            3
        );
        assert!(sql.contains("set search_path to pg_catalog, %i, pg_temp"));
    }

    #[test]
    fn embedded_sqlite_migrator_matches_expected_source_schema() {
        let versions = SQLITE_MIGRATOR
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(versions.last(), Some(&SOURCE_SCHEMA_VERSION));
        assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            SQLITE_MIGRATOR
                .iter()
                .all(|migration| !migration.sql.as_ref().is_empty())
        );
    }

    #[tokio::test]
    async fn sqlite_live_tv_source_triggers_enforce_exactly_one_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live-tv-source-invariant.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SQLITE_MIGRATOR.run(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO live_tv_tuners \
             (tuner_id, provider_type, name, created_at, updated_at) \
             VALUES ('tuner-a', 'xtream', 'Xtream', ?1, ?1)",
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO live_tv_channels \
             (channel_id, tuner_id, remote_id, name, sort_name, stream_url, \
              metadata_json, created_at, updated_at) \
             VALUES ('legacy', 'tuner-a', 'legacy', 'Legacy', 'Legacy', \
                     'https://provider.test/live/legacy.ts', '{}', ?1, ?1)",
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(&pool)
        .await
        .expect("a legacy stream URL without ProviderReference must remain valid");

        sqlx::query(
            "INSERT INTO live_tv_channels \
             (channel_id, tuner_id, remote_id, name, sort_name, stream_url, \
              metadata_json, created_at, updated_at) \
             VALUES ('opaque', 'tuner-a', 'opaque', 'Opaque', 'Opaque', '', \
                     '{\"ProviderReference\":\"xtream:v1:opaque\"}', ?1, ?1)",
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(&pool)
        .await
        .expect("an opaque ProviderReference without stream URL must be valid");

        for (channel_id, stream_url, metadata_json) in [
            (
                "mixed",
                "https://provider.test/live/mixed.ts",
                "{\"ProviderReference\":\"xtream:v1:mixed\"}",
            ),
            ("missing", "", "{}"),
        ] {
            let error = sqlx::query(
                "INSERT INTO live_tv_channels \
                 (channel_id, tuner_id, remote_id, name, sort_name, stream_url, \
                  metadata_json, created_at, updated_at) \
                 VALUES (?1, 'tuner-a', ?1, ?1, ?1, ?2, ?3, ?4, ?4)",
            )
            .bind(channel_id)
            .bind(stream_url)
            .bind(metadata_json)
            .bind("2026-08-08T00:00:00Z")
            .execute(&pool)
            .await
            .expect_err("invalid source state must be rejected");
            assert!(error.to_string().contains(
                "live_tv channel must persist exactly one of stream_url or ProviderReference"
            ));
        }

        let mixed_update = sqlx::query(
            "UPDATE live_tv_channels \
             SET metadata_json = '{\"ProviderReference\":\"xtream:v1:mixed\"}' \
             WHERE channel_id = 'legacy'",
        )
        .execute(&pool)
        .await
        .expect_err("updating a legacy row into mixed state must fail");
        assert!(mixed_update.to_string().contains(
            "live_tv channel must persist exactly one of stream_url or ProviderReference"
        ));

        let trigger_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'trigger' \
               AND name IN (\
                   'live_tv_channels_opaque_source_insert', \
                   'live_tv_channels_opaque_source_update'\
               )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trigger_count, 2);
        pool.close().await;
    }

    #[tokio::test]
    async fn sqlite_live_tv_source_migration_aborts_without_cleaning_mixed_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live-tv-source-preflight.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SQLITE_MIGRATOR.run(&pool).await.unwrap();
        sqlx::raw_sql(
            "DROP TRIGGER live_tv_channels_opaque_source_insert; \
             DROP TRIGGER live_tv_channels_opaque_source_update;",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO live_tv_tuners \
             (tuner_id, provider_type, name, created_at, updated_at) \
             VALUES ('tuner-a', 'xtream', 'Xtream', ?1, ?1)",
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO live_tv_channels \
             (channel_id, tuner_id, remote_id, name, sort_name, stream_url, \
              metadata_json, created_at, updated_at) \
             VALUES ('mixed', 'tuner-a', 'mixed', 'Mixed', 'Mixed', \
                     'https://provider.test/live/mixed.ts', \
                     '{\"ProviderReference\":\"xtream:v1:mixed\"}', ?1, ?1)",
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();

        let migration = SQLITE_MIGRATOR
            .iter()
            .find(|migration| migration.version == 202_608_080_106)
            .expect("live TV source invariant migration must be embedded");
        let mut transaction = pool.begin().await.unwrap();
        let error = sqlx::raw_sql(migration.sql.as_ref())
            .execute(&mut *transaction)
            .await
            .expect_err("preflight must reject a pre-existing mixed source row");
        assert!(
            error
                .to_string()
                .contains("jellyrin_livetv_opaque_source_preflight_zero")
        );
        transaction.rollback().await.unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM live_tv_channels WHERE channel_id = 'mixed'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        pool.close().await;
    }

    #[tokio::test]
    #[serial_test::serial(postgres_migrate)]
    async fn postgres_provider_url_audit_reports_counts_without_canaries_when_configured() {
        let Ok(base_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(PgConnectOptions::from_str(&base_url).unwrap())
            .await
            .unwrap();
        let schema = format!("jellyrin_url_audit_test_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin_pool)
            .await
            .unwrap();
        let target_url = scoped_postgres_url(&base_url, &schema);
        let result: anyhow::Result<()> = async {
            apply_schema(&target_url).await?;
            let clean = audit_provider_url_retention(ProviderUrlAuditOptions {
                source: None,
                target_url: target_url.clone(),
            })
            .await?;
            assert!(clean.is_clean());

            let target = open_postgres(&target_url).await?;
            let folder_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO virtual_folders (id, name, collection_type, locations, created_at, updated_at) VALUES ($1, 'Audit', 'movies', '[\"/audit\"]'::jsonb, now(), now())",
            )
            .bind(folder_id)
            .execute(&target)
            .await?;
            for (path, metadata) in [
                ("/audit/remote", serde_json::json!({"remotesourceurl": null})),
                (
                    "/audit/probe",
                    serde_json::json!({"RemoteMediaProbe": {"sourceurl": "https://user-canary:password-canary@provider.invalid/movie"}}),
                ),
                (
                    "/audit/malformed",
                    serde_json::json!({"RemoteMediaProbe": "token-canary"}),
                ),
            ] {
                sqlx::query(
                    "INSERT INTO media_items (id, virtual_folder_id, name, path, media_type, collection_type, media_streams, metadata, created_at, updated_at) VALUES ($1, $2, 'Audit', $3, 'Video', 'movies', '[]'::jsonb, $4, now(), now())",
                )
                .bind(Uuid::new_v4())
                .bind(folder_id)
                .bind(path)
                .bind(metadata)
                .execute(&target)
                .await?;
            }
            for (tuner_id, provider_type, stream_url) in [
                ("m3u", "m3u", "https://legacy.invalid/list.m3u8"),
                ("xtream", "xtream", "https://provider.invalid/live/user-canary/password-canary/1"),
                ("plugin", "plugin:magstv", "https://provider.invalid/token-canary"),
            ] {
                sqlx::query(
                    "INSERT INTO live_tv_tuners (tuner_id, provider_type, name, enabled, configuration, created_at, updated_at) VALUES ($1, $2, $1, true, '{}'::jsonb, now(), now())",
                )
                .bind(tuner_id)
                .bind(provider_type)
                .execute(&target)
                .await?;
                sqlx::query(
                    "INSERT INTO live_tv_channels (channel_id, tuner_id, remote_id, name, sort_name, stream_url, enabled, metadata, created_at, updated_at) VALUES ($1, $1, $1, $1, $1, $2, true, '{}'::jsonb, now(), now())",
                )
                .bind(tuner_id)
                .bind(stream_url)
                .execute(&target)
                .await?;
            }
            let findings = audit_provider_url_retention(ProviderUrlAuditOptions {
                source: None,
                target_url: target_url.clone(),
            })
            .await?;
            assert!(!findings.is_clean());
            assert_eq!(
                findings.postgres,
                ProviderUrlRetentionCounts {
                    remote_source_url_rows: 1,
                    remote_probe_source_url_rows: 1,
                    invalid_remote_probe_rows: 1,
                    live_tv_stream_url_rows: 2,
                }
            );
            let encoded = serde_json::to_string(&findings)?;
            for canary in ["user-canary", "password-canary", "token-canary", "provider.invalid"] {
                assert!(!encoded.contains(canary));
            }
            Ok(())
        }
        .await;
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin_pool)
            .await
            .unwrap();
        admin_pool.close().await;
        result.unwrap();
    }

    #[tokio::test]
    #[serial_test::serial(postgres_migrate)]
    async fn migrates_durable_fixture_and_rolls_back_dry_run_when_postgres_is_configured() {
        let Ok(base_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let admin_options = PgConnectOptions::from_str(&base_url).unwrap();
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(admin_options)
            .await
            .unwrap();
        let schema = format!("jellyrin_migrate_test_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin_pool)
            .await
            .unwrap();
        let target_url = scoped_postgres_url(&base_url, &schema);
        let source_directory = tempfile::tempdir().unwrap();
        let source_path = source_directory.path().join("source.db");

        let test_result = tokio::spawn({
            let target_url = target_url.clone();
            let source_path = source_path.clone();
            async move {
                let schema_report = apply_schema(&target_url).await?;
                assert_eq!(schema_report.schema_version_before, None);
                assert_eq!(schema_report.schema_version_after, TARGET_SCHEMA_VERSION);
                assert_eq!(
                    schema_report.applied_migrations,
                    POSTGRES_MIGRATOR.iter().count() as u64
                );
                let current_schema_report = apply_schema(&target_url).await?;
                assert_eq!(current_schema_report.status, "schema_current");
                assert_eq!(current_schema_report.applied_migrations, 0);
                assert_eq!(
                    current_schema_report.schema_version_before,
                    Some(TARGET_SCHEMA_VERSION)
                );
                let target = open_postgres(&target_url).await?;
                assert_target_schema_coverage(&target).await?;
                assert_eq!(
                    sqlx::query_as::<_, (i32, i64, i64, i64)>(
                        "SELECT extractor_version, source_item_count, \
                         projected_facet_count, projected_alias_count \
                         FROM jellyrin_derived_projection_versions \
                         WHERE projection_name = 'media_item_facets'",
                    )
                    .fetch_one(&target)
                    .await?,
                    (jellyrin_db::MEDIA_ITEM_FACET_PROJECTION_VERSION, 0, 0, 0)
                );

                let mut schema_lock = target.begin().await?;
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                    .bind(SCHEMA_MIGRATION_LOCK_NAME)
                    .execute(&mut *schema_lock)
                    .await?;
                let competing_schema_lock: bool = sqlx::query_scalar(
                    "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
                )
                .bind(SCHEMA_MIGRATION_LOCK_NAME)
                .fetch_one(&target)
                .await?;
                assert!(!competing_schema_lock);
                schema_lock.rollback().await?;

                let mut application_lock = target.begin().await?;
                lock_target_application_tables(&mut application_lock).await?;
                let mut competing_writer = target.acquire().await?;
                sqlx::query("SET lock_timeout = '100ms'")
                    .execute(&mut *competing_writer)
                    .await?;
                let lock_error = sqlx::query(
                    r#"
                    INSERT INTO users (
                        id, name, is_administrator, is_disabled, sync_play_access,
                        created_at, updated_at
                    ) VALUES ($1, 'lock probe', false, false, 'CreateAndJoinGroups', now(), now())
                    "#,
                )
                .bind(Uuid::new_v4())
                .execute(&mut *competing_writer)
                .await
                .expect_err("the exclusive migration lock must block a concurrent writer");
                let lock_error_code = lock_error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .map(|code| code.into_owned());
                assert_eq!(lock_error_code.as_deref(), Some("55P03"));
                drop(competing_writer);
                application_lock.rollback().await?;

                let fixture = seed_sqlite_fixture(&source_path).await?;

                let dry_run = execute(MigrationOptions {
                    source: source_path.clone(),
                    target_url: target_url.clone(),
                    dry_run: true,
                })
                .await?;
                assert_eq!(dry_run.status, "dry_run_validated");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
                        .fetch_one(&target)
                        .await?,
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_items")
                        .fetch_one(&target)
                        .await?,
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_item_facets")
                        .fetch_one(&target)
                        .await?,
                    0
                );
                assert_eq!(
                    sqlx::query_as::<_, (i64, i64, i64)>(
                        "SELECT source_item_count, projected_facet_count, projected_alias_count \
                         FROM jellyrin_derived_projection_versions \
                         WHERE projection_name = 'media_item_facets'",
                    )
                    .fetch_one(&target)
                    .await?,
                    (0, 0, 0),
                    "dry-run must roll the forced projection rebuild back"
                );
                assert!(dry_run.tables.iter().all(|table| {
                    table.source_normalized_digest_sha256 == table.target_normalized_digest_sha256
                }));
                assert_eq!(
                    dry_run
                        .tables
                        .iter()
                        .find(|table| table.table == "media_items")
                        .map(|table| table.source_rows),
                    Some(2)
                );
                assert_eq!(
                    dry_run
                        .tables
                        .iter()
                        .find(|table| table.table == "provider_secrets")
                        .map(|table| table.source_rows),
                    Some(1)
                );
                for omitted in [
                    "live_tv_channels",
                    "live_tv_programs",
                    "active_playback_sessions",
                    "transcode_sessions",
                    "catalog_sync_runs",
                ] {
                    assert!(dry_run.omitted.iter().any(|table| table.table == omitted));
                }
                assert!(dry_run.validation.target_was_empty_for_application_tables);
                assert_eq!(dry_run.validation.provider_secret_references_checked, 1);
                assert_eq!(dry_run.validation.provider_secret_references_missing, 0);

                sqlx::query(
                    r#"
                    INSERT INTO task_runs (id, task_key, status, started_at, completed_at, result, updated_at)
                    VALUES ($1, 'migration-fixture', 'completed', now(), now(), '{}'::jsonb, now())
                    "#,
                )
                .bind(Uuid::new_v4())
                .execute(&target)
                .await?;
                let nonempty_error = execute(MigrationOptions {
                    source: source_path.clone(),
                    target_url: target_url.clone(),
                    dry_run: true,
                })
                .await
                .expect_err("a nonempty omitted application table must block migration");
                assert!(nonempty_error.to_string().contains("task_runs"));
                sqlx::query("DELETE FROM task_runs").execute(&target).await?;

                let committed = execute(MigrationOptions {
                    source: source_path,
                    target_url,
                    dry_run: false,
                })
                .await?;
                assert_eq!(committed.status, "committed");
                assert_eq!(
                    dry_run.overall_digest_sha256,
                    committed.overall_digest_sha256
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
                        .fetch_one(&target)
                        .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM virtual_folders")
                        .fetch_one(&target)
                        .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, Uuid>("SELECT id FROM virtual_folders")
                        .fetch_one(&target)
                        .await?,
                    fixture.folder_id
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_items")
                        .fetch_one(&target)
                        .await?,
                    2
                );
                assert_eq!(
                    sqlx::query_as::<_, (i64, i64, i64)>(
                        "SELECT source_item_count, projected_facet_count, projected_alias_count \
                         FROM jellyrin_derived_projection_versions \
                         WHERE projection_name = 'media_item_facets'",
                    )
                    .fetch_one(&target)
                    .await?,
                    // Stable facet IDs live on `media_item_facets`; the alias projection keeps
                    // only the imported person's UUID in its canonical and compact forms.
                    (2, 2, 2)
                );
                assert_eq!(
                    sqlx::query_scalar::<_, String>(
                        "SELECT display_value FROM media_item_facets \
                         WHERE facet_kind = 'person'",
                    )
                    .fetch_one(&target)
                    .await?,
                    "Fixture Person"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT COUNT(*) FROM media_item_facet_aliases \
                         WHERE facet_kind = 'person' AND entity_id = \
                         'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'",
                    )
                    .fetch_one(&target)
                    .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT COUNT(*) FROM media_item_facet_aliases \
                         WHERE facet_kind = 'person' AND entity_id = \
                         'aaaaaaaabbbbccccddddeeeeeeeeeeee'",
                    )
                    .fetch_one(&target)
                    .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT id FROM media_items WHERE path = 'provider://fixture/item'",
                    )
                    .fetch_one(&target)
                    .await?,
                    fixture.item_id
                );
                assert_eq!(
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT id FROM media_items WHERE path = 'provider://fixture/second'",
                    )
                    .fetch_one(&target)
                    .await?,
                    fixture.second_item_id
                );
                assert_eq!(
                    sqlx::query_as::<_, (Option<i32>, Option<i32>, Option<i64>)>(
                        "SELECT width, height, runtime_ticks FROM media_items \
                         WHERE path = 'provider://fixture/item'",
                    )
                    .fetch_one(&target)
                    .await?,
                    (Some(1920), Some(1080), Some(900_000))
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT position_ticks FROM playback_states")
                        .fetch_one(&target)
                        .await?,
                    123_456
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_list_items")
                        .fetch_one(&target)
                        .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_item_lyrics")
                        .fetch_one(&target)
                        .await?,
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM live_tv_tuners")
                        .fetch_one(&target)
                        .await?,
                    1
                );
                let (nonce, ciphertext) = sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>(
                    "SELECT nonce, ciphertext FROM provider_secrets \
                     WHERE secret_id = 'fixture-secret'",
                )
                .fetch_one(&target)
                .await?;
                assert_eq!(nonce, fixture.provider_secret_nonce);
                assert_eq!(ciphertext, fixture.provider_secret_ciphertext);
                target.close().await;
                anyhow::Ok(())
            }
        })
        .await;

        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin_pool)
            .await
            .unwrap();
        admin_pool.close().await;
        match test_result {
            Ok(result) => result.unwrap(),
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => panic!("migration integration test task was cancelled: {error}"),
        }
    }

    struct FixtureIds {
        folder_id: Uuid,
        item_id: Uuid,
        second_item_id: Uuid,
        provider_secret_nonce: Vec<u8>,
        provider_secret_ciphertext: Vec<u8>,
    }

    async fn seed_sqlite_fixture(path: &Path) -> anyhow::Result<FixtureIds> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        SQLITE_MIGRATOR.run(&pool).await?;
        let user_id = Uuid::new_v4();
        let folder_id = Uuid::new_v4();
        let item_id = Uuid::parse_str("12345678-f000-4000-8000-000000000001")?;
        let second_item_id = Uuid::parse_str("12345678-0000-4000-8000-000000000002")?;
        let list_id = Uuid::new_v4();
        let provider_secret_nonce = vec![
            0x00, 0xff, 0x10, 0x80, 0x7f, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        ];
        let provider_secret_ciphertext = vec![
            0xff, 0x00, 0x80, 0x01, 0x7f, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x81, 0x90,
            0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0,
        ];
        let now = "2026-08-08 00:00:00";
        insert_sqlite_user(&pool, user_id, now).await?;
        sqlx::query(
            "INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at) \
             VALUES (?1, 'argon2id', 'fixture-password-hash', ?2)",
        )
        .bind(user_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations_json, created_at, updated_at
            ) VALUES (?1, 'External', 'movies', '["provider://fixture"]', ?2, ?2)
            "#,
        )
        .bind(folder_id.to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                last_seen_at, file_size, modified_at, runtime_ticks, bitrate, width, height,
                media_streams_json, metadata_json, created_at, updated_at
            ) VALUES (?1, ?2, 'Fixture', 'provider://fixture/item', 'Video', 'movies',
                      ?3, 1048576, ?3, 900000, 4000000, 1920, 1080,
                      '[{"index":0,"codec":"h264"}]',
                      '{"b":1e2,"a":1.2300,"Genres":["Drama"],"People":[{"Name":"Fixture Person","Id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}]}',
                      ?3, ?3)
            "#,
        )
        .bind(item_id.simple().to_string())
        .bind(folder_id.to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                last_seen_at, media_streams_json, metadata_json, created_at, updated_at
            ) VALUES (?1, ?2, 'Second Fixture', 'provider://fixture/second', 'Video', 'movies',
                      ?3, '[]', '{}', ?3, ?3)
            "#,
        )
        .bind(second_item_id.simple().to_string())
        .bind(folder_id.to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, position_ticks, is_paused, played, is_favorite, updated_at
            ) VALUES (?1, ?2, 123456, 0, 0, 1, ?3)
            "#,
        )
        .bind(user_id.simple().to_string())
        .bind(item_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO media_lists \
             (id, kind, name, owner_user_id, metadata_json, created_at, updated_at) \
             VALUES (?1, 'playlist', 'Fixture List', ?2, '{}', ?3, ?3)",
        )
        .bind(list_id.to_string())
        .bind(user_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO media_list_items \
             (list_id, item_id, playlist_item_id, position, added_at) \
             VALUES (?1, ?2, ?3, 0, ?4)",
        )
        .bind(list_id.to_string())
        .bind(item_id.simple().to_string())
        .bind(Uuid::new_v4().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO media_list_user_permissions \
             (list_id, user_id, can_edit, created_at, updated_at) \
             VALUES (?1, ?2, 1, ?3, ?3)",
        )
        .bind(list_id.to_string())
        .bind(user_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO media_item_lyrics (item_id, lyrics_json, created_at, updated_at) \
             VALUES (?1, '{\"Text\":\"fixture\"}', ?2, ?2)",
        )
        .bind(item_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO activity_log_entries \
             (id, name, entry_type, severity, user_id, item_id, created_at) \
             VALUES (7, 'Fixture event', 'MigrationTest', 'Information', ?1, ?2, ?3)",
        )
        .bind(user_id.simple().to_string())
        .bind(item_id.simple().to_string())
        .bind(now)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO provider_secrets \
             (secret_id, provider_type, envelope_version, key_id, nonce, ciphertext, \
              revision, created_at, updated_at) \
             VALUES ('fixture-secret', 'xtream', 1, 'fixture-key', ?1, ?2, 7, ?3, ?3)",
        )
        .bind(&provider_secret_nonce)
        .bind(&provider_secret_ciphertext)
        .bind(now)
        .execute(&pool)
        .await?;
        let tuner_configuration = serde_json::json!({
            "JellyrinProviderSecretRef": {
                "Id": "fixture-secret",
                "Provider": "xtream",
                "Revision": 7
            },
            "CredentialsConfigured": true
        })
        .to_string();
        sqlx::query(
            "INSERT INTO live_tv_tuners \
             (tuner_id, provider_type, name, enabled, configuration_json, created_at, updated_at) \
             VALUES ('fixture-tuner', 'xtream', 'Fixture', 1, ?1, ?2, ?2)",
        )
        .bind(tuner_configuration)
        .bind(now)
        .execute(&pool)
        .await?;
        pool.close().await;
        Ok(FixtureIds {
            folder_id,
            item_id,
            second_item_id,
            provider_secret_nonce,
            provider_secret_ciphertext,
        })
    }

    async fn insert_sqlite_user(pool: &SqlitePool, user_id: Uuid, now: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO users \
             (id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at) \
             VALUES (?1, 'Fixture Admin', 1, 0, 'CreateAndJoinGroups', ?2, ?2)",
        )
        .bind(user_id.simple().to_string())
        .bind(now)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn assert_target_schema_coverage(target: &PgPool) -> anyhow::Result<()> {
        let actual = sqlx::query_scalar::<_, String>(
            r#"
            SELECT table_name
            FROM information_schema.tables
            WHERE table_schema = current_schema()
              AND table_type = 'BASE TABLE'
              AND table_name <> '_sqlx_migrations'
            "#,
        )
        .fetch_all(target)
        .await?
        .into_iter()
        .collect::<HashSet<_>>();
        let expected = MIGRATED_TABLES
            .iter()
            .map(|table| table.target.to_owned())
            .chain(OMITTED_TABLES.iter().map(|table| table.table.to_owned()))
            .chain(
                TARGET_ONLY_OMITTED_TABLES
                    .iter()
                    .map(|table| table.table.to_owned()),
            )
            .chain(
                TARGET_INFRASTRUCTURE_TABLES
                    .iter()
                    .map(|table| (*table).to_owned()),
            )
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            actual == expected,
            "PostgreSQL migration table classification is incomplete"
        );
        for table in MIGRATED_TABLES {
            let actual_columns = sqlx::query_as::<_, (String, String, String)>(
                r#"
                SELECT column_name, data_type, is_nullable
                FROM information_schema.columns
                WHERE table_schema = current_schema() AND table_name = $1
                "#,
            )
            .bind(table.target)
            .fetch_all(target)
            .await?
            .into_iter()
            .map(|(name, data_type, nullable)| (name, (data_type, nullable == "YES")))
            .collect::<HashMap<_, _>>();
            let expected_columns = table
                .columns
                .iter()
                .map(|column| {
                    (
                        column.target.to_owned(),
                        (
                            expected_postgres_type(column.kind).to_owned(),
                            column.nullable,
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            anyhow::ensure!(
                actual_columns == expected_columns,
                "PostgreSQL column classification is incomplete for {}",
                table.target
            );
        }
        Ok(())
    }

    fn scoped_postgres_url(base_url: &str, schema: &str) -> String {
        let separator = if base_url.contains('?') { '&' } else { '?' };
        format!("{base_url}{separator}options=-csearch_path%3D{schema}%2Cpublic")
    }

    fn safe_identifier(value: &str) -> bool {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }
}
