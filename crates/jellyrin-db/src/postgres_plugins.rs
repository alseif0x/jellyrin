use std::collections::HashMap;

use anyhow::{Context, ensure};
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    DiscoveredPluginPackage, InstallPluginPackage, PluginRuntimeInstanceUpsert, PostgresDatabase,
    format_time, parse_time,
};

const PLUGIN_PLATFORM_LOCK: &str = "jellyrin:postgres:plugin-platform";

#[derive(Debug, Clone)]
struct PluginRepositoryModel {
    name: String,
    url: String,
    enabled: bool,
    payload: Value,
}

#[derive(Debug, Clone)]
struct PackageCatalogModel {
    repository_url: String,
    package_guid: Option<String>,
    package_name: String,
    package_version: String,
    runtime: String,
    target_abi: String,
    payload: Value,
}

impl PostgresDatabase {
    pub async fn sync_plugin_platform_from_system_configuration(&self) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.worker_pool.begin().await?;
        lock_platform_exclusive(&mut transaction).await?;
        let plugin_repositories = sqlx::query_scalar::<_, Value>(
            "SELECT plugin_repositories FROM system_configuration_payloads WHERE id = 1",
        )
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or_else(|| Value::Array(Vec::new()));
        replace_catalog_from_configuration(&mut transaction, &plugin_repositories, now).await?;

        transaction.commit().await?;
        Ok(())
    }

    pub async fn plugin_platform_snapshot(&self) -> anyhow::Result<Value> {
        self.sync_plugin_platform_from_system_configuration()
            .await?;

        let mut transaction = self.worker_pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        lock_platform_shared(&mut transaction).await?;
        let connection = &mut *transaction;

        let repositories = plugin_repositories_snapshot(connection).await?;
        let package_catalog = package_catalog_snapshot(connection).await?;
        let package_installations = package_installations_snapshot(connection).await?;
        let installed_plugins = installed_plugins_backup_snapshot(connection).await?;
        let plugin_manifests = plugin_manifests_snapshot(connection).await?;
        let plugin_configurations = plugin_configurations_snapshot(connection).await?;
        let plugin_permissions = plugin_permissions_snapshot(connection).await?;
        let plugin_runtime_instances = plugin_runtime_instances_snapshot(connection).await?;
        let plugin_host_events = plugin_host_events_snapshot(connection).await?;
        let plugin_audit_log = plugin_audit_log_snapshot(connection).await?;

        transaction.commit().await?;
        Ok(json!({
            "ModelVersion": 1,
            "Mode": "metadata-only",
            "Supported": true,
            "PackageBinaries": {
                "Mode": "not-restored",
                "Supported": false,
                "Reason": "Backup restores plugin state and metadata; package binary directories are intentionally not copied."
            },
            "Repositories": {
                "Count": repositories.len(),
                "Items": repositories
            },
            "PackageCatalogCache": {
                "Count": package_catalog.len(),
                "Items": package_catalog
            },
            "PackageInstallations": {
                "Count": package_installations.len(),
                "Items": package_installations
            },
            "InstalledPlugins": {
                "Count": installed_plugins.len(),
                "Items": installed_plugins
            },
            "PluginManifests": {
                "Count": plugin_manifests.len(),
                "Items": plugin_manifests
            },
            "PluginConfigurations": {
                "Count": plugin_configurations.len(),
                "Items": plugin_configurations
            },
            "PluginPermissions": {
                "Count": plugin_permissions.len(),
                "Items": plugin_permissions
            },
            "PluginRuntimeInstances": {
                "Count": plugin_runtime_instances.len(),
                "Items": plugin_runtime_instances
            },
            "PluginHostEvents": {
                "Count": plugin_host_events.len(),
                "Items": plugin_host_events
            },
            "PluginAuditLog": {
                "Count": plugin_audit_log.len(),
                "Items": plugin_audit_log
            }
        }))
    }

    pub async fn restore_plugin_platform_snapshot(&self, snapshot: &Value) -> anyhow::Result<()> {
        let version = snapshot
            .get("ModelVersion")
            .and_then(Value::as_i64)
            .context("plugin snapshot ModelVersion is missing")?;
        ensure!(
            version == 1,
            "unsupported plugin snapshot ModelVersion {version}"
        );

        let now = OffsetDateTime::now_utc();
        let mut transaction = self.worker_pool.begin().await?;
        lock_platform_exclusive(&mut transaction).await?;
        super::postgres_provider_secrets::lock_provider_configuration_mutation(
            &mut transaction,
            "plugin",
            "jellyrin-xtream-provider",
        )
        .await?;
        let mut restored_plugin_configurations = Vec::new();
        for item in plugin_snapshot_items(snapshot, "PluginConfigurations")? {
            let plugin_id = plugin_snapshot_string(item, "PluginId")?;
            let mut configuration = plugin_snapshot_json(item, "Configuration", json!({}));
            if plugin_id.eq_ignore_ascii_case("jellyrin-xtream-provider") {
                configuration = self
                    .protect_provider_configuration_in_connection(
                        &mut transaction,
                        "xtream",
                        configuration,
                    )
                    .await?;
            }
            restored_plugin_configurations.push((
                plugin_id,
                configuration,
                plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?,
            ));
        }
        for table in [
            "plugin_audit_log",
            "plugin_host_events",
            "plugin_runtime_instances",
            "plugin_permissions",
            "plugin_configurations",
            "plugin_manifests",
            "installed_plugins",
            "package_installations",
            "package_catalog_cache",
            "plugin_repositories",
        ] {
            let statement = format!("DELETE FROM {table}");
            sqlx::query(&statement).execute(&mut *transaction).await?;
        }

        for item in plugin_snapshot_items(snapshot, "Repositories")? {
            let name = plugin_snapshot_string(item, "Name")?;
            let url = plugin_snapshot_string(item, "Url")?;
            let enabled = plugin_snapshot_bool(item, "Enabled").unwrap_or(true);
            let payload = plugin_snapshot_value(item, "Payload")
                .cloned()
                .unwrap_or_else(|| json!({ "Name": name, "Url": url, "Enabled": enabled }));
            sqlx::query(
                r#"
                INSERT INTO plugin_repositories (id, name, url, enabled, payload, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(name)
            .bind(url)
            .bind(enabled)
            .bind(payload)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PackageCatalogCache")? {
            let repository_url = plugin_snapshot_string(item, "RepositoryUrl")?;
            let name = plugin_snapshot_string(item, "Name")?;
            let version = plugin_snapshot_string(item, "Version")?;
            sqlx::query(
                r#"
                INSERT INTO package_catalog_cache (
                    id, repository_url, package_guid, package_name, package_version,
                    runtime, target_abi, payload, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(repository_url)
            .bind(plugin_snapshot_optional_string(item, "Guid"))
            .bind(name)
            .bind(version)
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "TargetAbi").unwrap_or_default())
            .bind(
                plugin_snapshot_value(item, "Payload")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PackageInstallations")? {
            sqlx::query(
                r#"
                INSERT INTO package_installations (
                    id, package_name, package_guid, version, runtime, status, source_url,
                    payload, installed_at, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(plugin_snapshot_string(item, "Name")?)
            .bind(plugin_snapshot_optional_string(item, "Guid"))
            .bind(plugin_snapshot_string(item, "Version")?)
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Installed".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "SourceUrl"))
            .bind(
                plugin_snapshot_value(item, "Payload")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )
            .bind(plugin_snapshot_optional_timestamp(item, "InstalledAt")?)
            .bind(plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "InstalledPlugins")? {
            let plugin_id = plugin_snapshot_string(item, "Id")
                .or_else(|_| plugin_snapshot_string(item, "Guid"))?;
            sqlx::query(
                r#"
                INSERT INTO installed_plugins (
                    plugin_id, name, version, runtime, runtime_version, target_abi,
                    server_compatibility, status, capabilities, permissions,
                    configuration_state, last_error, health, manifest, installed_at, updated_at
                )
                VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16
                )
                "#,
            )
            .bind(plugin_id)
            .bind(plugin_snapshot_string(item, "Name")?)
            .bind(plugin_snapshot_string(item, "Version")?)
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "RuntimeVersion").unwrap_or_default())
            .bind(plugin_snapshot_optional_string(item, "TargetAbi").unwrap_or_default())
            .bind(plugin_snapshot_json(item, "ServerCompatibility", json!({})))
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "NotSupported".to_string()),
            )
            .bind(plugin_snapshot_json(item, "Capabilities", json!([])))
            .bind(plugin_snapshot_json(item, "Permissions", json!([])))
            .bind(
                plugin_snapshot_optional_string(item, "ConfigurationState")
                    .unwrap_or_else(|| "Default".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "LastError"))
            .bind(plugin_snapshot_json(item, "Health", json!({})))
            .bind(plugin_snapshot_json(item, "Manifest", json!({})))
            .bind(plugin_snapshot_optional_timestamp(item, "InstalledAt")?)
            .bind(plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginManifests")? {
            sqlx::query(
                "INSERT INTO plugin_manifests (plugin_id, manifest, updated_at) VALUES ($1, $2, $3)",
            )
            .bind(plugin_snapshot_string(item, "PluginId")?)
            .bind(plugin_snapshot_json(item, "Manifest", json!({})))
            .bind(plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for (plugin_id, configuration, updated_at) in restored_plugin_configurations {
            sqlx::query(
                "INSERT INTO plugin_configurations (plugin_id, configuration, updated_at) VALUES ($1, $2, $3)",
            )
            .bind(plugin_id)
            .bind(configuration)
            .bind(updated_at)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginPermissions")? {
            sqlx::query(
                "INSERT INTO plugin_permissions (plugin_id, permissions, updated_at) VALUES ($1, $2, $3)",
            )
            .bind(plugin_snapshot_string(item, "PluginId")?)
            .bind(plugin_snapshot_json(item, "Permissions", json!([])))
            .bind(plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginRuntimeInstances")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_runtime_instances (
                    instance_id, plugin_id, runtime, runtime_version, status, process_id,
                    endpoint, health, last_error, started_at, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                "#,
            )
            .bind(plugin_snapshot_internal_uuid(item, "InstanceId"))
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "RuntimeVersion").unwrap_or_default())
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Stopped".to_string()),
            )
            .bind(plugin_snapshot_value(item, "ProcessId").and_then(Value::as_i64))
            .bind(plugin_snapshot_optional_string(item, "Endpoint"))
            .bind(plugin_snapshot_json(item, "Health", json!({})))
            .bind(plugin_snapshot_optional_string(item, "LastError"))
            .bind(plugin_snapshot_optional_timestamp(item, "StartedAt")?)
            .bind(plugin_snapshot_timestamp_or(item, "UpdatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginHostEvents")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_host_events (
                    id, plugin_id, runtime, event_type, severity, message, payload, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
            )
            .bind(plugin_snapshot_internal_uuid(item, "Id"))
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(plugin_snapshot_optional_string(item, "Runtime"))
            .bind(plugin_snapshot_string(item, "EventType")?)
            .bind(
                plugin_snapshot_optional_string(item, "Severity")
                    .unwrap_or_else(|| "Information".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "Message").unwrap_or_default())
            .bind(plugin_snapshot_json(item, "Payload", json!({})))
            .bind(plugin_snapshot_timestamp_or(item, "CreatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginAuditLog")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_audit_log (
                    id, plugin_id, action, actor_user_id, status, payload, created_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(plugin_snapshot_internal_uuid(item, "Id"))
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(plugin_snapshot_string(item, "Action")?)
            .bind(plugin_snapshot_optional_uuid(item, "ActorUserId")?)
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_json(item, "Payload", json!({})))
            .bind(plugin_snapshot_timestamp_or(item, "CreatedAt", now)?)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, str::FromStr};

    use serde_json::json;
    use sqlx::{
        PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    };

    use super::*;
    use crate::{SystemConfigurationPayloads, postgres::POSTGRES_MIGRATOR};

    const PRE_INTEGRITY_SCHEMA_VERSION: i64 = 202_608_070_004;

    #[test]
    fn unicode_plugin_ids_share_grouping_and_mutation_lock_keys() {
        let uppercase = normalize_plugin_id("CAFÉ");
        let lowercase = normalize_plugin_id("café");
        assert_eq!(uppercase, lowercase);

        let mut grouped = HashMap::new();
        grouped.insert(uppercase, "runtime-event");
        assert_eq!(grouped.remove(&lowercase), Some("runtime-event"));
        assert_eq!(
            plugin_mutation_lock_key("  CAFÉ  "),
            plugin_mutation_lock_key("café")
        );
    }

    struct IsolatedPostgres {
        database: PostgresDatabase,
        administration_pool: PgPool,
        schema: String,
    }

    impl IsolatedPostgres {
        async fn configured() -> Option<Self> {
            Self::configured_through(None).await
        }

        async fn configured_before_integrity_migration() -> Option<Self> {
            Self::configured_through(Some(PRE_INTEGRITY_SCHEMA_VERSION)).await
        }

        async fn configured_through(latest_version: Option<i64>) -> Option<Self> {
            let database_url = std::env::var("JELLYRIN_TEST_POSTGRES_URL").ok()?;
            let connect_options = PgConnectOptions::from_str(&database_url)
                .expect("JELLYRIN_TEST_POSTGRES_URL must be a valid PostgreSQL URL");
            let administration_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(connect_options.clone())
                .await
                .expect("failed to connect to the PostgreSQL plugin-test database");
            let mut extension_lock = administration_pool
                .begin()
                .await
                .expect("failed to start pg_trgm preparation transaction");
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtextextended('jellyrin:schema:migration', 0))",
            )
            .execute(&mut *extension_lock)
            .await
            .expect("failed to lock pg_trgm preparation");
            sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
                .execute(&mut *extension_lock)
                .await
                .expect("failed to prepare pg_trgm for PostgreSQL plugin tests");
            extension_lock
                .commit()
                .await
                .expect("failed to commit pg_trgm preparation");

            let schema = format!("jellyrin_plugin_test_{}", Uuid::new_v4().simple());
            sqlx::query(&format!("CREATE SCHEMA {schema}"))
                .execute(&administration_pool)
                .await
                .expect("failed to create isolated PostgreSQL plugin-test schema");
            let search_path = format!("{schema}, public");
            let pool = connect_isolated_pool(
                connect_options.clone(),
                search_path.clone(),
                4,
                "jellyrin-plugin-test-api",
            )
            .await;
            let worker_pool = connect_isolated_pool(
                connect_options,
                search_path,
                2,
                "jellyrin-plugin-test-worker",
            )
            .await;
            let database = PostgresDatabase {
                pool,
                worker_pool,
                provider_secret_vault: None,
                telemetry: std::sync::Arc::new(crate::telemetry::DatabaseTelemetry::default()),
            };
            if let Some(latest_version) = latest_version {
                let migrator = sqlx::migrate::Migrator {
                    migrations: Cow::Owned(
                        POSTGRES_MIGRATOR
                            .iter()
                            .filter(|migration| migration.version <= latest_version)
                            .cloned()
                            .collect(),
                    ),
                    ..sqlx::migrate::Migrator::DEFAULT
                };
                migrator
                    .run(&database.worker_pool)
                    .await
                    .expect("failed to migrate isolated PostgreSQL plugin-test legacy schema");
            } else {
                database
                    .migrate()
                    .await
                    .expect("failed to migrate isolated PostgreSQL plugin-test schema");
            }
            Some(Self {
                database,
                administration_pool,
                schema,
            })
        }

        async fn cleanup(self) {
            self.database.close().await;
            sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
                .execute(&self.administration_pool)
                .await
                .expect("failed to remove isolated PostgreSQL plugin-test schema");
            self.administration_pool.close().await;
        }
    }

    async fn connect_isolated_pool(
        connect_options: PgConnectOptions,
        search_path: String,
        max_connections: u32,
        application_name: &'static str,
    ) -> PgPool {
        PgPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(move |connection, _metadata| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(
                        r#"
                        SELECT set_config('search_path', $1, false),
                               set_config('TimeZone', 'UTC', false)
                        "#,
                    )
                    .bind(search_path)
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(connect_options.application_name(application_name))
            .await
            .expect("failed to connect an isolated PostgreSQL plugin-test pool")
    }

    fn repository_configuration() -> SystemConfigurationPayloads {
        SystemConfigurationPayloads {
            plugin_repositories: json!([{
                "Name": "Stable",
                "Url": "https://plugins.invalid/stable.json",
                "Enabled": true,
                "Packages": [{
                    "Name": "Fixture",
                    "Guid": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "Runtime": "RustWasi",
                    "Versions": [{
                        "Version": "1.0.0",
                        "TargetAbi": "jellyrin-wasi-0.1",
                        "SourceUrl": "https://plugins.invalid/fixture.wasm"
                    }]
                }]
            }]),
            ..SystemConfigurationPayloads::default()
        }
    }

    fn install_package(plugin_id: &str, version: &str) -> InstallPluginPackage {
        InstallPluginPackage {
            plugin_id: plugin_id.to_string(),
            name: "Plugin Fixture".to_string(),
            version: version.to_string(),
            runtime: "RustWasi".to_string(),
            target_abi: "jellyrin-wasi-0.1".to_string(),
            package: json!({
                "Guid": plugin_id,
                "Name": "Plugin Fixture",
                "Runtime": "RustWasi",
                "Versions": [{
                    "Version": version,
                    "SourceUrl": format!("https://plugins.invalid/{version}.wasm")
                }]
            }),
            manifest: json!({
                "Guid": plugin_id,
                "Name": "Plugin Fixture",
                "Version": version,
                "Runtime": "RustWasi"
            }),
        }
    }

    async fn insert_plugin_identity_row(
        database: &PostgresDatabase,
        table: &str,
        plugin_id: &str,
        marker: &str,
    ) -> Result<(), sqlx::Error> {
        let now = OffsetDateTime::now_utc();
        match table {
            "installed_plugins" => {
                sqlx::query(
                    r#"
                    INSERT INTO installed_plugins (
                        plugin_id, name, version, runtime, status, updated_at
                    )
                    VALUES ($1, $2, '1.0.0', 'RustWasi', 'NotSupported', $3)
                    "#,
                )
                .bind(plugin_id)
                .bind(format!("Plugin {marker}"))
                .bind(now)
                .execute(&database.pool)
                .await?;
            }
            "plugin_manifests" => {
                sqlx::query(
                    "INSERT INTO plugin_manifests (plugin_id, manifest, updated_at) VALUES ($1, $2, $3)",
                )
                .bind(plugin_id)
                .bind(json!({ "Marker": marker }))
                .bind(now)
                .execute(&database.pool)
                .await?;
            }
            "plugin_configurations" => {
                sqlx::query(
                    "INSERT INTO plugin_configurations (plugin_id, configuration, updated_at) VALUES ($1, $2, $3)",
                )
                .bind(plugin_id)
                .bind(json!({ "Marker": marker }))
                .bind(now)
                .execute(&database.pool)
                .await?;
            }
            "plugin_permissions" => {
                sqlx::query(
                    "INSERT INTO plugin_permissions (plugin_id, permissions, updated_at) VALUES ($1, $2, $3)",
                )
                .bind(plugin_id)
                .bind(json!([marker]))
                .bind(now)
                .execute(&database.pool)
                .await?;
            }
            other => panic!("unsupported plugin identity table {other}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn postgres_plugin_ci_uniqueness_migration_diagnoses_every_collision_without_deleting() {
        let collision_cases = [
            (
                "installed_plugins",
                "Installed-Case-Collision",
                "installed-case-collision",
            ),
            (
                "plugin_manifests",
                "Manifest-Case-Collision",
                "manifest-case-collision",
            ),
            (
                "plugin_configurations",
                "Configuration-Case-Collision",
                "configuration-case-collision",
            ),
            (
                "plugin_permissions",
                "Permission-Case-Collision",
                "permission-case-collision",
            ),
        ];

        for (table, first_id, second_id) in collision_cases {
            let Some(test) = IsolatedPostgres::configured_before_integrity_migration().await else {
                return;
            };
            let database = &test.database;
            insert_plugin_identity_row(database, table, first_id, "first")
                .await
                .unwrap();
            insert_plugin_identity_row(database, table, second_id, "second")
                .await
                .unwrap();

            let diagnostic = format!("{:#}", database.migrate().await.unwrap_err());
            assert!(diagnostic.contains("case-insensitive plugin_id collision"));
            assert!(diagnostic.contains(table));
            let normalized_plugin_id = normalize_plugin_id(first_id);
            assert!(diagnostic.contains(normalized_plugin_id.as_str()));
            assert!(diagnostic.contains(first_id));
            assert!(diagnostic.contains(second_id));
            assert!(diagnostic.contains("no rows were discarded automatically"));

            let collision_count: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE lower(plugin_id) = lower($1)"
            ))
            .bind(first_id)
            .fetch_one(&database.pool)
            .await
            .unwrap();
            assert_eq!(collision_count, 2);
            test.cleanup().await;
        }

        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;

        let unique_indexes = [
            "installed_plugins_plugin_id_ci_uniq",
            "plugin_manifests_plugin_id_ci_uniq",
            "plugin_configurations_plugin_id_ci_uniq",
            "plugin_permissions_plugin_id_ci_uniq",
        ];
        for index_name in unique_indexes {
            let definition: String = sqlx::query_scalar(
                r#"
                SELECT indexdef
                FROM pg_indexes
                WHERE schemaname = current_schema() AND indexname = $1
                "#,
            )
            .bind(index_name)
            .fetch_one(&database.pool)
            .await
            .unwrap();
            assert!(definition.contains("CREATE UNIQUE INDEX"));
            assert!(definition.contains("lower(plugin_id)"));
        }

        for (table, first_id, second_id) in collision_cases {
            insert_plugin_identity_row(database, table, first_id, "allowed")
                .await
                .unwrap();
            assert!(
                insert_plugin_identity_row(database, table, second_id, "blocked")
                    .await
                    .is_err()
            );
            let remaining: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE lower(plugin_id) = lower($1)"
            ))
            .bind(first_id)
            .fetch_one(&database.pool)
            .await
            .unwrap();
            assert_eq!(remaining, 1);
        }

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_plugin_lifecycle_round_trip() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;
        let plugin_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let actor = Uuid::new_v4();

        database
            .update_system_configuration_payloads(repository_configuration())
            .await
            .unwrap();
        let platform = database.plugin_platform_snapshot().await.unwrap();
        assert_eq!(platform["Repositories"]["Count"], 1);
        assert_eq!(platform["PackageCatalogCache"]["Count"], 1);

        database
            .install_plugin_package(install_package(plugin_id, "1.0.0"), Some(actor))
            .await
            .unwrap();
        let installed = database
            .installed_plugin_json(&plugin_id.to_ascii_uppercase())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(installed["Version"], "1.0.0");
        assert_eq!(installed["Status"], "NotSupported");
        assert_eq!(
            database
                .installed_plugin_manifest(plugin_id)
                .await
                .unwrap()
                .unwrap()["Runtime"],
            "RustWasi"
        );
        assert_eq!(
            database.plugin_configuration_json(plugin_id).await.unwrap(),
            Some(json!({}))
        );
        assert_eq!(
            database.plugin_permissions_json(plugin_id).await.unwrap(),
            Some(json!([]))
        );

        assert!(
            database
                .update_plugin_configuration_json(
                    plugin_id,
                    json!({ "RefreshMinutes": 30, "Mode": "index-only" }),
                )
                .await
                .unwrap()
        );
        assert!(
            database
                .update_plugin_permissions_json(
                    plugin_id,
                    json!(["MetadataProvider", "ScheduledTask"]),
                    Some(actor),
                )
                .await
                .unwrap()
        );
        assert!(
            database
                .set_installed_plugin_status(
                    plugin_id,
                    "Disabled",
                    Some("maintenance"),
                    Some(actor)
                )
                .await
                .unwrap()
        );
        assert!(
            database
                .upsert_plugin_runtime_instance(
                    PluginRuntimeInstanceUpsert {
                        plugin_id: plugin_id.to_string(),
                        runtime: "RustWasi".to_string(),
                        runtime_version: "0.1.0".to_string(),
                        status: "Active".to_string(),
                        process_id: Some(4242),
                        endpoint: Some("stdio".to_string()),
                        health: json!({ "Status": "Healthy" }),
                        capabilities: vec![
                            "MetadataProvider".to_string(),
                            "ScheduledTask".to_string(),
                        ],
                        last_error: None,
                    },
                    Some(actor),
                )
                .await
                .unwrap()
        );

        let health = database
            .plugin_health_json(plugin_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(health["Status"], "Active");
        assert_eq!(health["Health"]["Status"], "Healthy");
        assert_eq!(health["RuntimeInstances"][0]["ProcessId"], 4242);
        assert!(
            database
                .plugin_host_events_json(plugin_id, 250)
                .await
                .unwrap()
                .unwrap()
                .iter()
                .any(|event| event["EventType"] == "RuntimeStatus")
        );

        database
            .install_plugin_package(install_package(plugin_id, "2.0.0"), Some(actor))
            .await
            .unwrap();
        let installations = database
            .package_installations_json(plugin_id)
            .await
            .unwrap();
        assert_eq!(installations.len(), 2);
        assert!(
            installations
                .iter()
                .any(|installation| installation["Version"] == "1.0.0"
                    && installation["Status"] == "Superseded")
        );
        assert!(
            installations
                .iter()
                .any(|installation| installation["Version"] == "2.0.0"
                    && installation["Status"] == "Installed")
        );

        let discovered_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let discovered = DiscoveredPluginPackage {
            plugin_id: discovered_id.to_string(),
            name: "Discovered Fixture".to_string(),
            version: "0.2.0".to_string(),
            runtime: "NativeProcess".to_string(),
            target_abi: "linux-x86_64".to_string(),
            manifest: Value::Null,
            install_path: "/srv/jellyrin/plugins/discovered".to_string(),
        };
        assert!(
            database
                .upsert_discovered_plugin_package(discovered.clone())
                .await
                .unwrap()
        );
        assert!(
            !database
                .upsert_discovered_plugin_package(discovered)
                .await
                .unwrap()
        );
        assert_eq!(database.installed_plugins_json().await.unwrap().len(), 2);

        assert!(
            database
                .uninstall_plugin_state(plugin_id, Some(actor))
                .await
                .unwrap()
        );
        assert!(
            database
                .installed_plugin_json(plugin_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            database
                .package_installations_json(plugin_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !database
                .uninstall_plugin_state(plugin_id, Some(actor))
                .await
                .unwrap()
        );

        let builtin_id = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
        assert!(
            database
                .ensure_builtin_plugin(
                    builtin_id,
                    "Built-in Fixture",
                    "1.0.0",
                    &json!({ "Guid": builtin_id, "Runtime": "Builtin" }),
                    &["ChannelProvider"],
                )
                .await
                .unwrap()
        );
        database
            .set_installed_plugin_status(builtin_id, "Disabled", None, Some(actor))
            .await
            .unwrap();
        database
            .ensure_builtin_plugin(
                &builtin_id.to_ascii_uppercase(),
                "Built-in Fixture",
                "1.1.0",
                &json!({ "Guid": builtin_id, "Runtime": "Builtin", "Revision": 2 }),
                &["ChannelProvider", "ScheduledTask"],
            )
            .await
            .unwrap();
        let builtin = database
            .installed_plugin_json(builtin_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(builtin["Version"], "1.1.0");
        assert_eq!(builtin["Status"], "Disabled");
        assert_eq!(builtin["Runtime"], "Builtin");
        assert_eq!(builtin["Capabilities"][1], "ScheduledTask");
        assert_eq!(builtin["Manifest"]["Revision"], 2);

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_configuration_and_plugin_catalog_update_atomically() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;
        let original = repository_configuration();
        database
            .update_system_configuration_payloads(original.clone())
            .await
            .unwrap();

        let duplicate_url = "https://plugins.invalid/duplicate.json";
        let invalid = SystemConfigurationPayloads {
            content_types: json!({"wrong": "shape"}),
            plugin_repositories: json!([
                {"Name": "First", "Url": duplicate_url},
                {"Name": "Second", "Url": duplicate_url}
            ]),
            server_options: json!([]),
            ..SystemConfigurationPayloads::default()
        };
        assert!(
            database
                .update_system_configuration_payloads(invalid)
                .await
                .is_err()
        );

        assert_eq!(
            database.system_configuration_payloads().await.unwrap(),
            original
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugin_repositories")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT name FROM plugin_repositories")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            "Stable"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM package_catalog_cache")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            1
        );

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_plugin_snapshot_restores_complete_metadata_state() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;
        let plugin_id = "cccccccc-cccc-cccc-cccc-cccccccccccc";

        database
            .update_system_configuration_payloads(repository_configuration())
            .await
            .unwrap();
        database
            .install_plugin_package(install_package(plugin_id, "1.5.0"), None)
            .await
            .unwrap();
        database
            .update_plugin_configuration_json(plugin_id, json!({ "BatchSize": 250 }))
            .await
            .unwrap();
        database
            .update_plugin_permissions_json(plugin_id, json!(["MetadataProvider"]), None)
            .await
            .unwrap();
        database
            .upsert_plugin_runtime_instance(
                PluginRuntimeInstanceUpsert {
                    plugin_id: plugin_id.to_string(),
                    runtime: "RustWasi".to_string(),
                    runtime_version: "0.1.0".to_string(),
                    status: "Active".to_string(),
                    process_id: None,
                    endpoint: Some("stdio".to_string()),
                    health: json!({ "Status": "Healthy", "Lag": 0 }),
                    capabilities: vec!["MetadataProvider".to_string()],
                    last_error: None,
                },
                None,
            )
            .await
            .unwrap();

        let snapshot = database.plugin_platform_snapshot().await.unwrap();
        assert_eq!(snapshot["InstalledPlugins"]["Count"], 1);
        assert_eq!(snapshot["PluginRuntimeInstances"]["Count"], 1);
        assert!(
            database
                .uninstall_plugin_state(plugin_id, None)
                .await
                .unwrap()
        );
        assert!(
            database
                .installed_plugin_json(plugin_id)
                .await
                .unwrap()
                .is_none()
        );

        database
            .restore_plugin_platform_snapshot(&snapshot)
            .await
            .unwrap();
        let restored = database
            .installed_plugin_json(plugin_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored["Status"], "Active");
        assert_eq!(restored["RuntimeInstances"][0]["Status"], "Active");
        assert_eq!(
            database.plugin_configuration_json(plugin_id).await.unwrap(),
            Some(json!({ "BatchSize": 250 }))
        );
        assert_eq!(
            database.plugin_permissions_json(plugin_id).await.unwrap(),
            Some(json!(["MetadataProvider"]))
        );
        assert_eq!(database.plugin_platform_snapshot().await.unwrap(), snapshot);

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_plugin_restore_rolls_back_all_tables_on_constraint_error() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;
        let plugin_id = "dddddddd-dddd-dddd-dddd-dddddddddddd";

        database
            .update_system_configuration_payloads(repository_configuration())
            .await
            .unwrap();
        database
            .install_plugin_package(install_package(plugin_id, "3.0.0"), None)
            .await
            .unwrap();
        database
            .update_plugin_configuration_json(plugin_id, json!({ "Keep": true }))
            .await
            .unwrap();
        let before = database.plugin_platform_snapshot().await.unwrap();

        let mut invalid = before.clone();
        let duplicate_repository = invalid["Repositories"]["Items"][0].clone();
        invalid["Repositories"]["Items"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_repository);
        invalid["Repositories"]["Count"] = json!(2);

        assert!(
            database
                .restore_plugin_platform_snapshot(&invalid)
                .await
                .is_err()
        );
        assert_eq!(
            database.plugin_configuration_json(plugin_id).await.unwrap(),
            Some(json!({ "Keep": true }))
        );
        assert_eq!(database.plugin_platform_snapshot().await.unwrap(), before);

        test.cleanup().await;
    }
}

fn plugin_repository_models_from_config(value: &Value) -> Vec<PluginRepositoryModel> {
    let Some(repositories) = value.as_array() else {
        return Vec::new();
    };
    repositories
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            let name = json_string_case_insensitive(value, "Name")?;
            let url = json_string_case_insensitive(value, "Url")?;
            let enabled = object
                .get("Enabled")
                .or_else(|| object.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Some(PluginRepositoryModel {
                name,
                url,
                enabled,
                payload: value.clone(),
            })
        })
        .collect()
}

fn package_catalog_models_from_repositories(
    repositories: &[PluginRepositoryModel],
) -> Vec<PackageCatalogModel> {
    let mut packages = Vec::new();
    for repository in repositories.iter().filter(|repository| repository.enabled) {
        let Some(repository_packages) =
            json_array_case_insensitive(&repository.payload, "Packages")
        else {
            continue;
        };
        for package in repository_packages {
            let Some(package_name) = json_string_case_insensitive(package, "Name") else {
                continue;
            };
            let package_guid = json_string_case_insensitive(package, "Guid")
                .or_else(|| json_string_case_insensitive(package, "Id"))
                .or_else(|| json_string_case_insensitive(package, "AssemblyGuid"));
            let package_runtime = json_string_case_insensitive(package, "Runtime")
                .unwrap_or_else(|| "DotNetJellyfin".to_string());
            let versions = json_array_case_insensitive(package, "Versions")
                .cloned()
                .unwrap_or_else(|| vec![package.clone()]);
            for version in versions {
                let package_version = json_string_case_insensitive(&version, "Version")
                    .unwrap_or_else(|| "0.0.0.0".to_string());
                let runtime = json_string_case_insensitive(&version, "Runtime")
                    .unwrap_or_else(|| package_runtime.clone());
                let target_abi = json_string_case_insensitive(&version, "TargetAbi")
                    .or_else(|| json_string_case_insensitive(package, "TargetAbi"))
                    .unwrap_or_default();
                let payload = json!({
                    "RepositoryName": repository.name,
                    "RepositoryUrl": repository.url,
                    "Package": package,
                    "Version": version
                });
                packages.push(PackageCatalogModel {
                    repository_url: repository.url.clone(),
                    package_guid: package_guid.clone(),
                    package_name: package_name.clone(),
                    package_version,
                    runtime,
                    target_abi,
                    payload,
                });
            }
        }
    }
    packages
}

/// Rebuilds the derived repository/package cache inside a caller-owned transaction. The caller
/// must hold the plugin-platform advisory lock so configuration and derived rows never diverge.
pub(super) async fn replace_catalog_from_configuration(
    transaction: &mut Transaction<'_, Postgres>,
    configuration: &Value,
    now: OffsetDateTime,
) -> anyhow::Result<()> {
    let repositories = plugin_repository_models_from_config(configuration);
    let packages = package_catalog_models_from_repositories(&repositories);

    sqlx::query("DELETE FROM package_catalog_cache")
        .execute(&mut **transaction)
        .await?;
    sqlx::query("DELETE FROM plugin_repositories")
        .execute(&mut **transaction)
        .await?;

    for repository in repositories {
        sqlx::query(
            r#"
            INSERT INTO plugin_repositories (id, name, url, enabled, payload, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(repository.name)
        .bind(repository.url)
        .bind(repository.enabled)
        .bind(repository.payload)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .context("failed to synchronize PostgreSQL plugin repository")?;
    }

    for package in packages {
        sqlx::query(
            r#"
            INSERT INTO package_catalog_cache (
                id, repository_url, package_guid, package_name, package_version,
                runtime, target_abi, payload, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(package.repository_url)
        .bind(package.package_guid)
        .bind(package.package_name)
        .bind(package.package_version)
        .bind(package.runtime)
        .bind(package.target_abi)
        .bind(package.payload)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .context("failed to synchronize PostgreSQL plugin catalog package")?;
    }

    Ok(())
}

async fn plugin_repositories_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT name, url, enabled, payload
        FROM plugin_repositories
        ORDER BY lower(name), lower(url), id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "Name": row.try_get::<String, _>("name")?,
                "Url": row.try_get::<String, _>("url")?,
                "Enabled": row.try_get::<bool, _>("enabled")?,
                "Payload": row.try_get::<Value, _>("payload")?
            }))
        })
        .collect()
}

async fn package_catalog_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT repository_url, package_guid, package_name, package_version,
               runtime, target_abi, payload
        FROM package_catalog_cache
        ORDER BY lower(package_name), lower(package_version), id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "RepositoryUrl": row.try_get::<String, _>("repository_url")?,
                "Guid": row.try_get::<Option<String>, _>("package_guid")?,
                "Name": row.try_get::<String, _>("package_name")?,
                "Version": row.try_get::<String, _>("package_version")?,
                "Runtime": row.try_get::<String, _>("runtime")?,
                "TargetAbi": row.try_get::<String, _>("target_abi")?,
                "Payload": row.try_get::<Value, _>("payload")?
            }))
        })
        .collect()
}

async fn package_installations_snapshot(
    connection: &mut PgConnection,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT package_name, package_guid, version, runtime, status, source_url,
               payload, installed_at, updated_at
        FROM package_installations
        ORDER BY lower(package_name), lower(version), id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(package_installation_row_json)
        .collect()
}

async fn installed_plugins_backup_snapshot(
    connection: &mut PgConnection,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query_as::<_, PostgresPluginRow>(
        r#"
        SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
               server_compatibility, status, capabilities, permissions,
               configuration_state, last_error, health, manifest, installed_at, updated_at
        FROM installed_plugins
        ORDER BY lower(name), lower(version), plugin_id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            let installed_at = optional_time_string(row.installed_at)?;
            let updated_at = format_time(row.updated_at)?;
            let mut value = plugin_row_to_json(&row);
            value["InstalledAt"] = json!(installed_at);
            value["UpdatedAt"] = json!(updated_at);
            Ok(value)
        })
        .collect()
}

async fn plugin_manifests_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, manifest, updated_at FROM plugin_manifests ORDER BY lower(plugin_id)",
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "PluginId": row.try_get::<String, _>("plugin_id")?,
                "Manifest": row.try_get::<Value, _>("manifest")?,
                "UpdatedAt": format_time(row.try_get("updated_at")?)?
            }))
        })
        .collect()
}

async fn plugin_configurations_snapshot(
    connection: &mut PgConnection,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, configuration, updated_at FROM plugin_configurations ORDER BY lower(plugin_id)",
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "PluginId": row.try_get::<String, _>("plugin_id")?,
                "Configuration": row.try_get::<Value, _>("configuration")?,
                "UpdatedAt": format_time(row.try_get("updated_at")?)?
            }))
        })
        .collect()
}

async fn plugin_permissions_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, permissions, updated_at FROM plugin_permissions ORDER BY lower(plugin_id)",
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "PluginId": row.try_get::<String, _>("plugin_id")?,
                "Permissions": row.try_get::<Value, _>("permissions")?,
                "UpdatedAt": format_time(row.try_get("updated_at")?)?
            }))
        })
        .collect()
}

async fn plugin_runtime_instances_snapshot(
    connection: &mut PgConnection,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT instance_id, plugin_id, runtime, runtime_version, status, process_id,
               endpoint, health, last_error, started_at, updated_at
        FROM plugin_runtime_instances
        ORDER BY lower(plugin_id), instance_id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter().map(plugin_runtime_row_json).collect()
}

async fn plugin_host_events_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, runtime, event_type, severity, message, payload, created_at
        FROM plugin_host_events
        ORDER BY created_at, id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter().map(plugin_host_event_row_json).collect()
}

async fn plugin_audit_log_snapshot(connection: &mut PgConnection) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, action, actor_user_id, status, payload, created_at
        FROM plugin_audit_log
        ORDER BY created_at, id
        "#,
    )
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "Id": row.try_get::<Uuid, _>("id")?,
                "PluginId": row.try_get::<Option<String>, _>("plugin_id")?,
                "Action": row.try_get::<String, _>("action")?,
                "ActorUserId": row.try_get::<Option<Uuid>, _>("actor_user_id")?,
                "Status": row.try_get::<String, _>("status")?,
                "Payload": row.try_get::<Value, _>("payload")?,
                "CreatedAt": format_time(row.try_get("created_at")?)?
            }))
        })
        .collect()
}

fn package_installation_row_json(row: PgRow) -> anyhow::Result<Value> {
    let installed_at = optional_time_string(row.try_get("installed_at")?)?;
    Ok(json!({
        "Name": row.try_get::<String, _>("package_name")?,
        "Guid": row.try_get::<Option<String>, _>("package_guid")?,
        "Version": row.try_get::<String, _>("version")?,
        "Runtime": row.try_get::<String, _>("runtime")?,
        "Status": row.try_get::<String, _>("status")?,
        "SourceUrl": row.try_get::<Option<String>, _>("source_url")?,
        "Payload": row.try_get::<Value, _>("payload")?,
        "InstalledAt": installed_at,
        "UpdatedAt": format_time(row.try_get("updated_at")?)?
    }))
}

#[derive(sqlx::FromRow)]
struct PostgresPluginRow {
    plugin_id: String,
    name: String,
    version: String,
    runtime: String,
    runtime_version: String,
    target_abi: String,
    server_compatibility: Value,
    status: String,
    capabilities: Value,
    permissions: Value,
    configuration_state: String,
    last_error: Option<String>,
    health: Value,
    manifest: Value,
    installed_at: Option<OffsetDateTime>,
    updated_at: OffsetDateTime,
}

fn plugin_row_to_json(row: &PostgresPluginRow) -> Value {
    json!({
        "Id": row.plugin_id,
        "Guid": row.plugin_id,
        "Name": row.name,
        "Version": row.version,
        "Runtime": row.runtime,
        "RuntimeVersion": row.runtime_version,
        "TargetAbi": row.target_abi,
        "ServerCompatibility": row.server_compatibility,
        "Status": row.status,
        "Capabilities": row.capabilities,
        "Permissions": row.permissions,
        "ConfigurationState": row.configuration_state,
        "LastError": row.last_error,
        "Health": row.health,
        "Manifest": row.manifest
    })
}

async fn enrich_plugin_runtime_state(
    pool: &sqlx::PgPool,
    plugin: &mut Value,
) -> anyhow::Result<()> {
    let Some(plugin_id) = plugin.get("Id").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(());
    };
    plugin["RuntimeInstances"] =
        Value::Array(plugin_runtime_instances_for_plugin(pool, &plugin_id).await?);
    plugin["RecentEvents"] =
        Value::Array(plugin_host_events_for_plugin(pool, &plugin_id, 25).await?);
    Ok(())
}

async fn enrich_plugins_runtime_state(
    pool: &sqlx::PgPool,
    plugins: &mut [Value],
) -> anyhow::Result<()> {
    if plugins.is_empty() {
        return Ok(());
    }

    let runtime_rows = sqlx::query(
        r#"
        SELECT instance_id, plugin_id, runtime, runtime_version, status, process_id,
               endpoint, health, last_error, started_at, updated_at
        FROM plugin_runtime_instances
        WHERE plugin_id IS NOT NULL
        ORDER BY lower(plugin_id), updated_at DESC, instance_id
        "#,
    )
    .fetch_all(pool)
    .await?;
    let mut runtime_by_plugin: HashMap<String, Vec<Value>> = HashMap::new();
    for row in runtime_rows {
        let plugin_id = row
            .try_get::<Option<String>, _>("plugin_id")?
            .unwrap_or_default();
        let plugin_id = normalize_plugin_id(&plugin_id);
        runtime_by_plugin
            .entry(plugin_id)
            .or_default()
            .push(plugin_runtime_row_json(row)?);
    }

    let event_rows = sqlx::query(
        r#"
        SELECT id, plugin_id, runtime, event_type, severity, message, payload, created_at
        FROM (
            SELECT id, plugin_id, runtime, event_type, severity, message, payload, created_at,
                   row_number() OVER (
                       PARTITION BY lower(plugin_id)
                       ORDER BY created_at DESC, id DESC
                   ) AS event_rank
            FROM plugin_host_events
            WHERE plugin_id IS NOT NULL
        ) ranked_events
        WHERE event_rank <= 25
        ORDER BY lower(plugin_id), created_at DESC, id DESC
        "#,
    )
    .fetch_all(pool)
    .await?;
    let mut events_by_plugin: HashMap<String, Vec<Value>> = HashMap::new();
    for row in event_rows {
        let plugin_id = row
            .try_get::<Option<String>, _>("plugin_id")?
            .unwrap_or_default();
        let plugin_id = normalize_plugin_id(&plugin_id);
        events_by_plugin
            .entry(plugin_id)
            .or_default()
            .push(plugin_host_event_row_json(row)?);
    }

    for plugin in plugins {
        let id = normalize_plugin_id(plugin.get("Id").and_then(Value::as_str).unwrap_or_default());
        plugin["RuntimeInstances"] =
            Value::Array(runtime_by_plugin.remove(&id).unwrap_or_default());
        plugin["RecentEvents"] = Value::Array(events_by_plugin.remove(&id).unwrap_or_default());
    }
    Ok(())
}

async fn plugin_runtime_instances_for_plugin(
    pool: &sqlx::PgPool,
    plugin_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT instance_id, plugin_id, runtime, runtime_version, status, process_id,
               endpoint, health, last_error, started_at, updated_at
        FROM plugin_runtime_instances
        WHERE lower(plugin_id) = lower($1)
        ORDER BY updated_at DESC, instance_id
        "#,
    )
    .bind(plugin_id.trim())
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(plugin_runtime_row_json).collect()
}

fn plugin_runtime_row_json(row: PgRow) -> anyhow::Result<Value> {
    Ok(json!({
        "InstanceId": row.try_get::<Uuid, _>("instance_id")?,
        "PluginId": row.try_get::<Option<String>, _>("plugin_id")?,
        "Runtime": row.try_get::<String, _>("runtime")?,
        "RuntimeVersion": row.try_get::<String, _>("runtime_version")?,
        "Status": row.try_get::<String, _>("status")?,
        "ProcessId": row.try_get::<Option<i64>, _>("process_id")?,
        "Endpoint": row.try_get::<Option<String>, _>("endpoint")?,
        "Health": row.try_get::<Value, _>("health")?,
        "LastError": row.try_get::<Option<String>, _>("last_error")?,
        "StartedAt": optional_time_string(row.try_get("started_at")?)?,
        "UpdatedAt": format_time(row.try_get("updated_at")?)?
    }))
}

async fn plugin_host_events_for_plugin(
    pool: &sqlx::PgPool,
    plugin_id: &str,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, runtime, event_type, severity, message, payload, created_at
        FROM plugin_host_events
        WHERE lower(plugin_id) = lower($1)
        ORDER BY created_at DESC, id DESC
        LIMIT $2
        "#,
    )
    .bind(plugin_id.trim())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(plugin_host_event_row_json).collect()
}

fn plugin_host_event_row_json(row: PgRow) -> anyhow::Result<Value> {
    Ok(json!({
        "Id": row.try_get::<Uuid, _>("id")?,
        "PluginId": row.try_get::<Option<String>, _>("plugin_id")?,
        "Runtime": row.try_get::<Option<String>, _>("runtime")?,
        "EventType": row.try_get::<String, _>("event_type")?,
        "Severity": row.try_get::<String, _>("severity")?,
        "Message": row.try_get::<String, _>("message")?,
        "Payload": row.try_get::<Value, _>("payload")?,
        "CreatedAt": format_time(row.try_get("created_at")?)?
    }))
}

pub(super) async fn lock_platform_exclusive(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(PLUGIN_PLATFORM_LOCK)
        .execute(&mut **transaction)
        .await
        .context("failed to lock PostgreSQL plugin platform")?;
    Ok(())
}

async fn lock_platform_shared(transaction: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 0))")
        .bind(PLUGIN_PLATFORM_LOCK)
        .execute(&mut **transaction)
        .await
        .context("failed to share-lock PostgreSQL plugin platform")?;
    Ok(())
}

async fn lock_plugin_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
) -> anyhow::Result<()> {
    lock_platform_shared(transaction).await?;
    let key = plugin_mutation_lock_key(plugin_id);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut **transaction)
        .await
        .context("failed to lock PostgreSQL plugin mutation")?;
    Ok(())
}

fn normalize_plugin_id(plugin_id: &str) -> String {
    plugin_id.to_lowercase()
}

fn plugin_mutation_lock_key(plugin_id: &str) -> String {
    format!(
        "jellyrin:postgres:plugin:{}",
        normalize_plugin_id(plugin_id.trim())
    )
}

async fn canonical_plugin_id(
    transaction: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
) -> anyhow::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT plugin_id FROM installed_plugins WHERE lower(plugin_id) = lower($1) LIMIT 1",
    )
    .bind(plugin_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to resolve PostgreSQL plugin id")
}

async fn canonical_manifest_plugin_id(
    transaction: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
) -> anyhow::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT plugin_id FROM plugin_manifests WHERE lower(plugin_id) = lower($1) LIMIT 1",
    )
    .bind(plugin_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to resolve PostgreSQL plugin manifest id")
}

async fn canonical_plugin_reference(
    transaction: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
) -> anyhow::Result<Option<String>> {
    sqlx::query_scalar(
        r#"
        SELECT plugin_id
        FROM (
            SELECT plugin_id, 0 AS priority
            FROM plugin_manifests
            WHERE lower(plugin_id) = lower($1)
            UNION ALL
            SELECT plugin_id, 1 AS priority
            FROM installed_plugins
            WHERE lower(plugin_id) = lower($1)
        ) plugin_references
        ORDER BY priority
        LIMIT 1
        "#,
    )
    .bind(plugin_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to resolve PostgreSQL plugin reference")
}

fn plugin_snapshot_items<'a>(snapshot: &'a Value, section: &str) -> anyhow::Result<&'a Vec<Value>> {
    snapshot
        .get(section)
        .and_then(|section| section.get("Items"))
        .and_then(Value::as_array)
        .with_context(|| format!("plugin snapshot section {section}.Items must be an array"))
}

fn plugin_snapshot_value<'a>(item: &'a Value, field: &str) -> Option<&'a Value> {
    item.as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .map(|(_, value)| value)
}

fn plugin_snapshot_string(item: &Value, field: &str) -> anyhow::Result<String> {
    plugin_snapshot_optional_string(item, field)
        .with_context(|| format!("plugin snapshot item is missing {field}"))
}

fn plugin_snapshot_optional_string(item: &Value, field: &str) -> Option<String> {
    plugin_snapshot_value(item, field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn plugin_snapshot_bool(item: &Value, field: &str) -> Option<bool> {
    plugin_snapshot_value(item, field).and_then(Value::as_bool)
}

fn plugin_snapshot_json(item: &Value, field: &str, default: Value) -> Value {
    plugin_snapshot_value(item, field)
        .cloned()
        .unwrap_or(default)
}

fn plugin_snapshot_optional_uuid(item: &Value, field: &str) -> anyhow::Result<Option<Uuid>> {
    plugin_snapshot_optional_string(item, field)
        .map(|value| {
            Uuid::parse_str(&value)
                .with_context(|| format!("plugin snapshot {field} is not a valid UUID"))
        })
        .transpose()
}

fn plugin_snapshot_internal_uuid(item: &Value, field: &str) -> Uuid {
    // SQLite snapshots may contain deterministic textual internal IDs (for example
    // "plugin-id:runtime"). They have no external identity contract, so PostgreSQL safely
    // remaps legacy/non-UUID values while preserving UUIDs produced by this adapter.
    plugin_snapshot_optional_string(item, field)
        .and_then(|value| Uuid::parse_str(&value).ok())
        .unwrap_or_else(Uuid::new_v4)
}

fn plugin_snapshot_optional_timestamp(
    item: &Value,
    field: &str,
) -> anyhow::Result<Option<OffsetDateTime>> {
    plugin_snapshot_optional_string(item, field)
        .map(|value| {
            parse_time(&value)
                .with_context(|| format!("plugin snapshot {field} is not a valid timestamp"))
        })
        .transpose()
}

fn plugin_snapshot_timestamp_or(
    item: &Value,
    field: &str,
    default: OffsetDateTime,
) -> anyhow::Result<OffsetDateTime> {
    Ok(plugin_snapshot_optional_timestamp(item, field)?.unwrap_or(default))
}

fn optional_time_string(value: Option<OffsetDateTime>) -> anyhow::Result<Option<String>> {
    value.map(format_time).transpose()
}

fn json_string_case_insensitive(value: &Value, field: &str) -> Option<String> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .and_then(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_array_case_insensitive<'a>(value: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .and_then(|(_, value)| value.as_array())
}

impl PostgresDatabase {
    pub async fn ensure_builtin_plugin(
        &self,
        plugin_id: &str,
        name: &str,
        version: &str,
        manifest: &Value,
        capabilities: &[&str],
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        ensure!(!normalized.is_empty(), "plugin id must not be empty");
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;

        let existing = sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT plugin_id, status
            FROM installed_plugins
            WHERE lower(plugin_id) = lower($1)
            LIMIT 1
            "#,
        )
        .bind(normalized)
        .fetch_optional(&mut *transaction)
        .await?;
        let (canonical, status) = existing
            .map(|(canonical, status)| {
                let status = if status.is_empty() {
                    "Active".to_string()
                } else {
                    status
                };
                (canonical, status)
            })
            .unwrap_or_else(|| (normalized.to_string(), "Active".to_string()));

        let result = sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility,
                status, capabilities, permissions, configuration_state,
                last_error, health, manifest, installed_at, updated_at
            )
            VALUES (
                $1, $2, $3, 'Builtin', '', '{}'::jsonb,
                $4, $5, '[]'::jsonb, 'Default',
                NULL, '{}'::jsonb, $6, $7, $7
            )
            ON CONFLICT (plugin_id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                runtime = excluded.runtime,
                capabilities = excluded.capabilities,
                manifest = excluded.manifest,
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(canonical)
        .bind(name)
        .bind(version)
        .bind(status)
        .bind(json!(capabilities))
        .bind(manifest)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .context("failed to ensure PostgreSQL built-in plugin")?;

        transaction.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn install_plugin_package(
        &self,
        package: InstallPluginPackage,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let plugin_id = package.plugin_id.trim();
        ensure!(!plugin_id.is_empty(), "plugin id must not be empty");
        let now = OffsetDateTime::now_utc();
        let source_url = json_array_case_insensitive(&package.package, "Versions")
            .and_then(|versions| {
                versions.iter().find(|version| {
                    json_string_case_insensitive(version, "Version")
                        .is_some_and(|version| version.eq_ignore_ascii_case(&package.version))
                })
            })
            .and_then(|version| json_string_case_insensitive(version, "SourceUrl"))
            .or_else(|| json_string_case_insensitive(&package.package, "SourceUrl"));
        let runtime_missing = format!("{} runtime host is not implemented yet.", package.runtime);
        let health = json!({
            "Status": "NotSupported",
            "Message": runtime_missing
        });

        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, plugin_id).await?;
        let canonical_plugin_id = canonical_plugin_id(&mut transaction, plugin_id)
            .await?
            .unwrap_or_else(|| plugin_id.to_string());

        sqlx::query(
            r#"
            UPDATE package_installations
            SET status = 'Superseded', updated_at = $1
            WHERE lower(package_guid) = lower($2)
              AND lower(version) <> lower($3)
              AND status = 'Installed'
            "#,
        )
        .bind(now)
        .bind(&canonical_plugin_id)
        .bind(&package.version)
        .execute(&mut *transaction)
        .await?;

        let install_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM package_installations
            WHERE lower(package_guid) = lower($1) AND lower(version) = lower($2)
            ORDER BY updated_at DESC, id
            LIMIT 1
            FOR UPDATE
            "#,
        )
        .bind(&canonical_plugin_id)
        .bind(&package.version)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or_else(Uuid::new_v4);

        sqlx::query(
            r#"
            INSERT INTO package_installations (
                id, package_name, package_guid, version, runtime, status, source_url,
                payload, installed_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, 'Installed', $6, $7, $8, $8)
            ON CONFLICT (id) DO UPDATE SET
                package_name = excluded.package_name,
                package_guid = excluded.package_guid,
                version = excluded.version,
                runtime = excluded.runtime,
                status = excluded.status,
                source_url = excluded.source_url,
                payload = excluded.payload,
                installed_at = COALESCE(package_installations.installed_at, excluded.installed_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(install_id)
        .bind(&package.name)
        .bind(&canonical_plugin_id)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(source_url)
        .bind(&package.package)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility,
                status, capabilities, permissions, configuration_state, last_error,
                health, manifest, installed_at, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, '{}'::jsonb,
                'NotSupported', '[]'::jsonb, '[]'::jsonb, 'Default', $6,
                $7, $8, $9, $9
            )
            ON CONFLICT (plugin_id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                runtime = excluded.runtime,
                target_abi = excluded.target_abi,
                status = excluded.status,
                last_error = excluded.last_error,
                health = excluded.health,
                manifest = excluded.manifest,
                installed_at = COALESCE(installed_plugins.installed_at, excluded.installed_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&canonical_plugin_id)
        .bind(&package.name)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(&package.target_abi)
        .bind(&runtime_missing)
        .bind(&health)
        .bind(&package.manifest)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_manifests (plugin_id, manifest, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (plugin_id) DO UPDATE SET
                manifest = excluded.manifest,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&canonical_plugin_id)
        .bind(&package.manifest)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration, updated_at)
            VALUES ($1, '{}'::jsonb, $2)
            ON CONFLICT (plugin_id) DO NOTHING
            "#,
        )
        .bind(&canonical_plugin_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions, updated_at)
            VALUES ($1, '[]'::jsonb, $2)
            ON CONFLICT (plugin_id) DO NOTHING
            "#,
        )
        .bind(&canonical_plugin_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'Install', $3, 'NotSupported', $4, $5)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&canonical_plugin_id)
        .bind(actor_user_id)
        .bind(json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "Reason": runtime_missing
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (
                id, plugin_id, runtime, event_type, severity, message, payload, created_at
            )
            VALUES ($1, $2, $3, 'RuntimeUnavailable', 'Warning', $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&canonical_plugin_id)
        .bind(&package.runtime)
        .bind(&runtime_missing)
        .bind(json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "Status": "NotSupported"
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(())
    }

    pub async fn installed_plugin_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query_as::<_, PostgresPluginRow>(
            r#"
            SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
                   server_compatibility, status, capabilities, permissions,
                   configuration_state, last_error, health, manifest,
                   installed_at, updated_at
            FROM installed_plugins
            WHERE lower(plugin_id) = lower($1)
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut plugin = plugin_row_to_json(&row);
        enrich_plugin_runtime_state(&self.pool, &mut plugin).await?;
        Ok(Some(plugin))
    }

    pub async fn installed_plugins_json(&self) -> anyhow::Result<Vec<Value>> {
        let rows = sqlx::query_as::<_, PostgresPluginRow>(
            r#"
            SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
                   server_compatibility, status, capabilities, permissions,
                   configuration_state, last_error, health, manifest,
                   installed_at, updated_at
            FROM installed_plugins
            ORDER BY lower(name), lower(version), plugin_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut plugins = rows.iter().map(plugin_row_to_json).collect::<Vec<_>>();
        enrich_plugins_runtime_state(&self.pool, &mut plugins).await?;
        Ok(plugins)
    }

    pub async fn plugin_health_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let Some(plugin) = self.installed_plugin_json(plugin_id).await? else {
            return Ok(None);
        };
        Ok(Some(json!({
            "PluginId": plugin["Id"].clone(),
            "Guid": plugin["Guid"].clone(),
            "Name": plugin["Name"].clone(),
            "Version": plugin["Version"].clone(),
            "Runtime": plugin["Runtime"].clone(),
            "Status": plugin["Status"].clone(),
            "LastError": plugin["LastError"].clone(),
            "Health": plugin["Health"].clone(),
            "RuntimeInstances": plugin["RuntimeInstances"].clone(),
            "RecentEvents": plugin["RecentEvents"].clone()
        })))
    }

    pub async fn plugin_host_events_json(
        &self,
        plugin_id: &str,
        limit: i64,
    ) -> anyhow::Result<Option<Vec<Value>>> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM installed_plugins WHERE lower(plugin_id) = lower($1))",
        )
        .bind(plugin_id.trim())
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Ok(None);
        }
        plugin_host_events_for_plugin(&self.pool, plugin_id, limit.clamp(1, 250))
            .await
            .map(Some)
    }

    pub async fn upsert_discovered_plugin_package(
        &self,
        package: DiscoveredPluginPackage,
    ) -> anyhow::Result<bool> {
        let plugin_id = package.plugin_id.trim();
        ensure!(!plugin_id.is_empty(), "plugin id must not be empty");
        let now = OffsetDateTime::now_utc();
        let runtime_missing = format!("{} runtime host is not implemented yet.", package.runtime);
        let mut manifest = package.manifest;
        if !manifest.is_object() {
            manifest = json!({});
        }
        manifest["Guid"] = json!(plugin_id);
        manifest["Name"] = json!(package.name);
        manifest["Version"] = json!(package.version);
        manifest["Runtime"] = json!(package.runtime);
        manifest["TargetAbi"] = json!(package.target_abi);
        manifest["Installation"] = json!({
            "Mode": "filesystem-discovered",
            "InstallPath": package.install_path
        });
        let health = json!({ "Status": "NotSupported", "Message": runtime_missing });

        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, plugin_id).await?;
        if canonical_plugin_id(&mut transaction, plugin_id)
            .await?
            .is_some()
        {
            transaction.rollback().await?;
            return Ok(false);
        }

        sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility,
                status, capabilities, permissions, configuration_state, last_error,
                health, manifest, installed_at, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, '{}'::jsonb,
                'NotSupported', '[]'::jsonb, '[]'::jsonb, 'Default', $6,
                $7, $8, $9, $9
            )
            "#,
        )
        .bind(plugin_id)
        .bind(&package.name)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(&package.target_abi)
        .bind(&runtime_missing)
        .bind(&health)
        .bind(&manifest)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_manifests (plugin_id, manifest, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (plugin_id) DO NOTHING
            "#,
        )
        .bind(plugin_id)
        .bind(&manifest)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration, updated_at)
            VALUES ($1, '{}'::jsonb, $2)
            ON CONFLICT (plugin_id) DO NOTHING
            "#,
        )
        .bind(plugin_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions, updated_at)
            VALUES ($1, '[]'::jsonb, $2)
            ON CONFLICT (plugin_id) DO NOTHING
            "#,
        )
        .bind(plugin_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (
                id, plugin_id, runtime, event_type, severity, message, payload, created_at
            )
            VALUES ($1, $2, $3, 'Discovery', 'Information', $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(plugin_id)
        .bind(&package.runtime)
        .bind(format!(
            "{} {} discovered from filesystem.",
            package.name, package.version
        ))
        .bind(json!({
            "InstallPath": package.install_path,
            "Runtime": package.runtime
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'Discover', NULL, 'NotSupported', $3, $4)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(plugin_id)
        .bind(json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "InstallPath": package.install_path
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(true)
    }

    pub async fn package_installations_json(&self, plugin_id: &str) -> anyhow::Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT package_name, package_guid, version, runtime, status, source_url,
                   payload, installed_at, updated_at
            FROM package_installations
            WHERE lower(package_guid) = lower($1)
            ORDER BY lower(version), id
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(package_installation_row_json)
            .collect()
    }

    pub async fn installed_plugin_manifest(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar::<_, Value>(
            "SELECT manifest FROM plugin_manifests WHERE lower(plugin_id) = lower($1)",
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL plugin manifest")
    }

    pub async fn plugin_configuration_json(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar::<_, Value>(
            "SELECT configuration FROM plugin_configurations WHERE lower(plugin_id) = lower($1)",
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL plugin configuration")
    }

    pub async fn plugin_permissions_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar::<_, Value>(
            "SELECT permissions FROM plugin_permissions WHERE lower(plugin_id) = lower($1)",
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL plugin permissions")
    }

    pub async fn update_plugin_configuration_json(
        &self,
        plugin_id: &str,
        mut configuration: Value,
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;
        let Some(canonical) = canonical_plugin_reference(&mut transaction, normalized).await?
        else {
            transaction.rollback().await?;
            return Ok(false);
        };
        if normalized.eq_ignore_ascii_case("jellyrin-xtream-provider") {
            super::postgres_provider_secrets::lock_provider_configuration_mutation(
                &mut transaction,
                "plugin",
                normalized,
            )
            .await?;
            let existing = sqlx::query_scalar::<_, Value>(
                "SELECT configuration FROM plugin_configurations WHERE lower(plugin_id) = lower($1) FOR UPDATE",
            )
            .bind(normalized)
            .fetch_optional(&mut *transaction)
            .await?;
            super::inherit_provider_secret_reference(&mut configuration, existing.as_ref());
            configuration = self
                .protect_provider_configuration_in_connection(
                    &mut transaction,
                    "xtream",
                    configuration,
                )
                .await?;
        }

        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (plugin_id) DO UPDATE SET
                configuration = excluded.configuration,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(canonical)
        .bind(configuration)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn update_plugin_permissions_json(
        &self,
        plugin_id: &str,
        permissions: Value,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;
        let Some(canonical) = canonical_manifest_plugin_id(&mut transaction, normalized).await?
        else {
            transaction.rollback().await?;
            return Ok(false);
        };

        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (plugin_id) DO UPDATE SET
                permissions = excluded.permissions,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&canonical)
        .bind(&permissions)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE installed_plugins
            SET permissions = $1, updated_at = $2
            WHERE lower(plugin_id) = lower($3)
            "#,
        )
        .bind(&permissions)
        .bind(now)
        .bind(&canonical)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'UpdatePermissions', $3, 'Updated', $4, $5)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&canonical)
        .bind(actor_user_id)
        .bind(permissions)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn set_installed_plugin_status(
        &self,
        plugin_id: &str,
        status: &str,
        last_error: Option<&str>,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;
        let row = sqlx::query_scalar::<_, String>(
            r#"
            UPDATE installed_plugins
            SET status = $1, last_error = $2, updated_at = $3
            WHERE lower(plugin_id) = lower($4)
            RETURNING plugin_id
            "#,
        )
        .bind(status)
        .bind(last_error)
        .bind(now)
        .bind(normalized)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(canonical) = row else {
            transaction.rollback().await?;
            return Ok(false);
        };

        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'SetStatus', $3, $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(canonical)
        .bind(actor_user_id)
        .bind(status)
        .bind(json!({ "LastError": last_error }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn upsert_plugin_runtime_instance(
        &self,
        instance: PluginRuntimeInstanceUpsert,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = instance.plugin_id.trim();
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;

        let canonical = sqlx::query_scalar::<_, String>(
            r#"
            UPDATE installed_plugins
            SET runtime_version = $1,
                status = $2,
                capabilities = $3,
                last_error = $4,
                health = $5,
                updated_at = $6
            WHERE lower(plugin_id) = lower($7)
            RETURNING plugin_id
            "#,
        )
        .bind(&instance.runtime_version)
        .bind(&instance.status)
        .bind(json!(instance.capabilities))
        .bind(instance.last_error.as_deref())
        .bind(&instance.health)
        .bind(now)
        .bind(normalized)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(canonical) = canonical else {
            transaction.rollback().await?;
            return Ok(false);
        };

        let instance_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT instance_id
            FROM plugin_runtime_instances
            WHERE lower(plugin_id) = lower($1) AND lower(runtime) = lower($2)
            ORDER BY updated_at DESC, instance_id
            LIMIT 1
            FOR UPDATE
            "#,
        )
        .bind(&canonical)
        .bind(&instance.runtime)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or_else(Uuid::new_v4);

        sqlx::query(
            r#"
            INSERT INTO plugin_runtime_instances (
                instance_id, plugin_id, runtime, runtime_version, status, process_id,
                endpoint, health, last_error, started_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
            ON CONFLICT (instance_id) DO UPDATE SET
                runtime_version = excluded.runtime_version,
                status = excluded.status,
                process_id = excluded.process_id,
                endpoint = excluded.endpoint,
                health = excluded.health,
                last_error = excluded.last_error,
                started_at = COALESCE(plugin_runtime_instances.started_at, excluded.started_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(instance_id)
        .bind(&canonical)
        .bind(&instance.runtime)
        .bind(&instance.runtime_version)
        .bind(&instance.status)
        .bind(instance.process_id)
        .bind(instance.endpoint.as_deref())
        .bind(&instance.health)
        .bind(instance.last_error.as_deref())
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (
                id, plugin_id, runtime, event_type, severity, message, payload, created_at
            )
            VALUES ($1, $2, $3, 'RuntimeStatus', $4, $5, $6, $7)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&canonical)
        .bind(&instance.runtime)
        .bind(if instance.status.eq_ignore_ascii_case("Active") {
            "Information"
        } else {
            "Warning"
        })
        .bind(format!(
            "{} runtime status changed to {}.",
            instance.runtime, instance.status
        ))
        .bind(json!({
            "InstanceId": instance_id,
            "RuntimeVersion": instance.runtime_version,
            "ProcessId": instance.process_id,
            "Endpoint": instance.endpoint,
            "Health": instance.health,
            "LastError": instance.last_error
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'RuntimeStatus', $3, $4, $5, $6)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&canonical)
        .bind(actor_user_id)
        .bind(&instance.status)
        .bind(json!({
            "Runtime": instance.runtime,
            "RuntimeVersion": instance.runtime_version,
            "Capabilities": instance.capabilities
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(true)
    }

    pub async fn uninstall_plugin_state(
        &self,
        plugin_id: &str,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_plugin_mutation(&mut transaction, normalized).await?;
        let canonical = sqlx::query_scalar::<_, String>(
            r#"
            DELETE FROM installed_plugins
            WHERE lower(plugin_id) = lower($1)
            RETURNING plugin_id
            "#,
        )
        .bind(normalized)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(canonical) = canonical else {
            transaction.rollback().await?;
            return Ok(false);
        };

        sqlx::query("DELETE FROM plugin_manifests WHERE lower(plugin_id) = lower($1)")
            .bind(&canonical)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM plugin_configurations WHERE lower(plugin_id) = lower($1)")
            .bind(&canonical)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM plugin_permissions WHERE lower(plugin_id) = lower($1)")
            .bind(&canonical)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM plugin_runtime_instances WHERE lower(plugin_id) = lower($1)")
            .bind(&canonical)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM package_installations WHERE lower(package_guid) = lower($1)")
            .bind(&canonical)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (
                id, plugin_id, action, actor_user_id, status, payload, created_at
            )
            VALUES ($1, $2, 'Uninstall', $3, 'Deleted', '{}'::jsonb, $4)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(canonical)
        .bind(actor_user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}
