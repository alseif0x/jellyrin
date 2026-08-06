use jellyrin_core::VirtualFolder;
use jellyrin_db::{Database, MediaList};
use uuid::Uuid;

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

    pub(crate) async fn rename_media_list(
        &self,
        list_id: Uuid,
        name: &str,
    ) -> anyhow::Result<MediaList> {
        self.db.update_media_list_name(list_id, name).await
    }
}
