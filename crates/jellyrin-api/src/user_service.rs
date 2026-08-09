use jellyrin_core::{DeviceToken, User};
use serde_json::Value;
use uuid::Uuid;

use crate::Database;

/// User-domain persistence boundary used by HTTP handlers.
///
/// Keeping database details behind this small façade lets the API migrate to
/// repository-backed persistence without changing route behavior.
pub(crate) struct UserService<'a> {
    db: &'a Database,
}

impl<'a> UserService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn list(&self) -> anyhow::Result<Vec<User>> {
        self.db.users().await
    }

    pub(crate) async fn by_id(&self, user_id: Uuid) -> anyhow::Result<User> {
        self.db.user_by_id(user_id).await
    }

    pub(crate) async fn create(&self, name: &str, password: Option<&str>) -> anyhow::Result<User> {
        self.db.create_user(name, password).await
    }

    pub(crate) async fn delete(&self, user_id: Uuid) -> anyhow::Result<()> {
        self.db.delete_user(user_id).await
    }

    pub(crate) async fn update_profile(
        &self,
        user_id: Uuid,
        name: &str,
        is_administrator: bool,
        is_disabled: bool,
        sync_play_access: &str,
    ) -> anyhow::Result<User> {
        self.db
            .update_user_profile(
                user_id,
                name,
                is_administrator,
                is_disabled,
                sync_play_access,
            )
            .await
    }

    pub(crate) async fn configuration(&self, user_id: Uuid) -> anyhow::Result<Option<Value>> {
        self.db.user_configuration(user_id).await
    }

    pub(crate) async fn update_configuration(
        &self,
        user_id: Uuid,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.db.update_user_configuration(user_id, payload).await
    }

    pub(crate) async fn set_password(&self, user_id: Uuid, password: &str) -> anyhow::Result<()> {
        self.db.set_user_password(user_id, password).await
    }

    pub(crate) async fn reset_password(&self, user_id: Uuid) -> anyhow::Result<()> {
        self.db.reset_user_password(user_id).await
    }

    pub(crate) async fn verify_password(
        &self,
        user_id: Uuid,
        password: &str,
    ) -> anyhow::Result<()> {
        self.db.verify_user_password(user_id, password).await
    }

    pub(crate) async fn revoke_tokens_except(
        &self,
        user_id: Uuid,
        keep_token: &str,
    ) -> anyhow::Result<()> {
        self.db.revoke_user_tokens_except(user_id, keep_token).await
    }

    pub(crate) async fn authenticate_by_name(
        &self,
        username: &str,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        self.db
            .authenticate_user_by_name(username, password, device_id, device_name, client, version)
            .await
    }

    pub(crate) async fn authenticate_by_id(
        &self,
        user_id: Uuid,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        self.db
            .authenticate_user_by_id(user_id, password, device_id, device_name, client, version)
            .await
    }
}
