use jellyrin_core::DeviceToken;
use jellyrin_db::{
    ActivePlaybackSession, ActiveSessionUser, ActiveViewingSession, Database, DeviceSession,
    TranscodeSession, UpsertActivePlaybackSession, UpsertPlaybackState,
};
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

    pub(crate) async fn devices(&self) -> anyhow::Result<Vec<DeviceSession>> {
        self.db.device_sessions().await
    }

    pub(crate) async fn devices_for_user(
        &self,
        user_id: uuid::Uuid,
    ) -> anyhow::Result<Vec<DeviceSession>> {
        self.db.device_sessions_for_user(user_id).await
    }

    pub(crate) async fn device_by_id(
        &self,
        device_id: &str,
    ) -> anyhow::Result<Option<DeviceSession>> {
        self.db.device_session_by_id(device_id).await
    }

    pub(crate) async fn active_playback(&self) -> anyhow::Result<Vec<ActivePlaybackSession>> {
        self.db.active_playback_sessions().await
    }

    pub(crate) async fn active_viewing(&self) -> anyhow::Result<Vec<ActiveViewingSession>> {
        self.db.active_viewing_sessions().await
    }

    pub(crate) async fn active_users(&self) -> anyhow::Result<Vec<ActiveSessionUser>> {
        self.db.active_session_users().await
    }

    pub(crate) async fn update_device_name(
        &self,
        device_id: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        self.db.update_device_name(device_id, name).await
    }

    pub(crate) async fn revoke_device(&self, device_id: &str) -> anyhow::Result<()> {
        self.db.revoke_device(device_id).await
    }

    pub(crate) async fn server_id(&self) -> anyhow::Result<uuid::Uuid> {
        Ok(self.db.server_state().await?.server_id)
    }

    pub(crate) async fn active_transcodes(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.db.active_transcode_sessions().await
    }

    pub(crate) async fn transcode_by_play_session(
        &self,
        play_session_id: &str,
    ) -> anyhow::Result<Option<TranscodeSession>> {
        self.db
            .transcode_session_by_play_session_id(play_session_id)
            .await
    }

    pub(crate) async fn update_transcode_status(
        &self,
        play_session_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        self.db
            .update_transcode_session_status(play_session_id, status)
            .await
    }

    pub(crate) async fn upsert_playback_state(
        &self,
        state: UpsertPlaybackState,
    ) -> anyhow::Result<()> {
        self.db.upsert_playback_state(state).await
    }

    pub(crate) async fn upsert_active_playback(
        &self,
        session: UpsertActivePlaybackSession,
    ) -> anyhow::Result<()> {
        self.db.upsert_active_playback_session(session).await
    }

    pub(crate) async fn clear_active_playback(&self, session_id: &str) -> anyhow::Result<()> {
        self.db.clear_active_playback_session(session_id).await
    }
}
