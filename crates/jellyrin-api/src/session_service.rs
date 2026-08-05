use jellyrin_core::DeviceToken;
use jellyrin_db::Database;
use serde_json::Value;

/// Persistence boundary for device/session state used by route handlers.
pub(crate) struct SessionService<'a> {
    db: &'a Database,
}

impl<'a> SessionService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn update_capabilities(
        &self,
        access_token: &str,
        capabilities: Value,
    ) -> anyhow::Result<()> {
        self.db
            .update_device_capabilities(access_token, capabilities)
            .await
    }

    pub(crate) async fn ensure_device(&self, token: &DeviceToken) -> anyhow::Result<()> {
        self.db.ensure_device_session(token).await
    }

    pub(crate) async fn revoke_token(&self, access_token: &str) -> anyhow::Result<()> {
        self.db.revoke_token(access_token).await
    }
}
