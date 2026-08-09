use jellyrin_core::VirtualFolder;
use jellyrin_db::MediaList;
use uuid::Uuid;

use crate::Database;

/// Persistence boundary for library folders and media-list lifecycle.
pub(crate) struct LibraryService<'a> {
    db: &'a Database,
}

impl<'a> LibraryService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn virtual_folders(&self) -> anyhow::Result<Vec<VirtualFolder>> {
        self.db.virtual_folders().await
    }

    pub(crate) async fn upsert_virtual_folder(
        &self,
        name: &str,
        collection_type: Option<&str>,
        locations: Vec<String>,
    ) -> anyhow::Result<VirtualFolder> {
        self.db
            .upsert_virtual_folder(name, collection_type, locations)
            .await
    }

    pub(crate) async fn delete_virtual_folder(&self, name: &str) -> anyhow::Result<bool> {
        self.db.delete_virtual_folder(name).await
    }

    pub(crate) async fn rename_virtual_folder(
        &self,
        name: &str,
        new_name: &str,
    ) -> anyhow::Result<bool> {
        self.db.rename_virtual_folder(name, new_name).await
    }

    pub(crate) async fn scan_virtual_folder(&self, folder_id: Uuid) -> anyhow::Result<usize> {
        self.db.scan_virtual_folder_items(folder_id).await
    }

    pub(crate) async fn rename_media_list(
        &self,
        list_id: Uuid,
        name: &str,
    ) -> anyhow::Result<MediaList> {
        self.db.update_media_list_name(list_id, name).await
    }

    pub(crate) async fn create_media_list(
        &self,
        kind: &str,
        name: &str,
        collection_type: Option<&str>,
        owner_user_id: Option<Uuid>,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<MediaList> {
        self.db
            .create_media_list(kind, name, collection_type, owner_user_id, item_ids)
            .await
    }

    pub(crate) async fn add_media_list_items(
        &self,
        list_id: Uuid,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        self.db.add_media_list_items(list_id, item_ids).await
    }
}
