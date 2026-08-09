use jellyrin_core::StartupConfig;
use jellyrin_db::{BrandingConfig, NamedConfigurationPayload, SystemConfigurationPayloads};
use serde_json::Value;

use crate::Database;

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

    pub(crate) async fn update_startup(&self, config: StartupConfig) -> anyhow::Result<()> {
        self.db.update_startup_config(config).await
    }

    pub(crate) async fn update_system(
        &self,
        payloads: SystemConfigurationPayloads,
    ) -> anyhow::Result<()> {
        self.db.update_system_configuration_payloads(payloads).await
    }

    pub(crate) async fn update_branding(&self, config: BrandingConfig) -> anyhow::Result<()> {
        self.db.update_branding_config(config).await
    }
}
