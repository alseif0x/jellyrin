use jellyrin_db::{DiscoveredPluginPackage, PluginRuntimeInstanceUpsert};
use serde_json::Value;
use uuid::Uuid;

use crate::Database;

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
        self.db
            .ensure_builtin_plugin(plugin_id, name, version, manifest, capabilities)
            .await
    }
}
