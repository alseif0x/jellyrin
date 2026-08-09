use jellyrin_db::TrickplayInfo;
use serde_json::Value;
use uuid::Uuid;

use crate::Database;

/// Persistence boundary for media-item metadata and derived media data.
pub(crate) struct MediaService<'a> {
    db: &'a Database,
}

impl<'a> MediaService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn update_metadata(
        &self,
        item_id: Uuid,
        metadata: Value,
    ) -> anyhow::Result<()> {
        self.db.update_media_item_metadata(item_id, metadata).await
    }

    pub(crate) async fn update_metadata_json(
        &self,
        item_id: &str,
        metadata: &Value,
    ) -> anyhow::Result<()> {
        self.db
            .update_media_item_metadata_json(item_id, metadata)
            .await
    }

    pub(crate) async fn update_media_info(
        &self,
        item_id: Uuid,
        runtime_ticks: Option<i64>,
        bitrate: Option<i64>,
        width: Option<i32>,
        height: Option<i32>,
        media_streams: Vec<Value>,
    ) -> anyhow::Result<()> {
        self.db
            .update_media_item_media_info(
                item_id,
                runtime_ticks,
                bitrate,
                width,
                height,
                media_streams,
            )
            .await
    }

    pub(crate) async fn delete_lyrics(&self, item_id: Uuid) -> anyhow::Result<bool> {
        self.db.delete_media_item_lyrics(item_id).await
    }

    pub(crate) async fn update_lyrics(&self, item_id: Uuid, lyrics: Value) -> anyhow::Result<()> {
        self.db.update_media_item_lyrics(item_id, lyrics).await
    }

    pub(crate) async fn delete_items(
        &self,
        item_ids: Vec<Uuid>,
        deleted_by_user_id: Option<Uuid>,
    ) -> anyhow::Result<u64> {
        self.db
            .delete_media_items(item_ids, deleted_by_user_id)
            .await
    }

    pub(crate) async fn upsert_trickplay(
        &self,
        info: TrickplayInfo,
    ) -> anyhow::Result<TrickplayInfo> {
        self.db.upsert_trickplay_info(info).await
    }
}
