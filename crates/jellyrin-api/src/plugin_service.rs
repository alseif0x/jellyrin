use jellyrin_db::{Database, DiscoveredPluginPackage, PluginRuntimeInstanceUpsert};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// Persistence boundary for plugin discovery and runtime health.
pub(crate) struct PluginService<'a> {
    db: &'a Database,
}

impl<'a> PluginService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn upsert_discovered_package(
        &self,
        package: DiscoveredPluginPackage,
    ) -> anyhow::Result<bool> {
        self.db.upsert_discovered_plugin_package(package).await
    }

    pub(crate) async fn upsert_runtime_instance(
        &self,
        instance: PluginRuntimeInstanceUpsert,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        self.db
            .upsert_plugin_runtime_instance(instance, actor_user_id)
            .await
    }

    pub(crate) async fn ensure_builtin(
        &self,
        plugin_id: &str,
        name: &str,
        version: &str,
        manifest: &Value,
        capabilities: &[&str],
    ) -> anyhow::Result<bool> {
        let existing_status = self
            .db
            .installed_plugin_json(plugin_id)
            .await?
            .and_then(|plugin| {
                plugin
                    .get("Status")
                    .and_then(Value::as_str)
                    .filter(|status| !status.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "Active".to_string());
        let now = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        let result = sqlx::query(
            r#"INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility_json,
                status, capabilities_json, permissions_json, configuration_state,
                last_error, health_json, manifest_json, installed_at, updated_at
            ) VALUES (?1, ?2, ?3, 'Builtin', '', '{}', ?4, ?5, '[]', 'Default', NULL, '{}', ?6, ?7, ?7)
            ON CONFLICT(plugin_id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                runtime = excluded.runtime,
                capabilities_json = excluded.capabilities_json,
                manifest_json = excluded.manifest_json,
                status = excluded.status,
                updated_at = excluded.updated_at"#,
        )
        .bind(plugin_id)
        .bind(name)
        .bind(version)
        .bind(existing_status)
        .bind(serde_json::to_string(capabilities)?)
        .bind(serde_json::to_string(manifest)?)
        .bind(now)
        .execute(self.db.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
