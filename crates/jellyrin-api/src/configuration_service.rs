use jellyrin_db::{Database, NamedConfigurationPayload};
use serde_json::Value;

/// Persistence boundary for named application configuration.
pub(crate) struct ConfigurationService<'a> {
    db: &'a Database,
}

impl<'a> ConfigurationService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn get(&self, key: &str) -> anyhow::Result<Option<Value>> {
        self.db.named_configuration(key).await
    }

    pub(crate) async fn set(&self, key: &str, payload: Value) -> anyhow::Result<()> {
        self.db.update_named_configuration(key, payload).await
    }

    pub(crate) async fn all(&self) -> anyhow::Result<Vec<NamedConfigurationPayload>> {
        self.db.named_configurations().await
    }
}
