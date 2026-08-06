use std::path::Path;

use jellyrin_db::Database;

/// Persistence boundary for filesystem watcher updates.
pub(crate) struct FileService<'a> {
    db: &'a Database,
}

impl<'a> FileService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn scan_single_file(&self, path: &Path) -> anyhow::Result<bool> {
        self.db.scan_single_file(path).await
    }

    pub(crate) async fn mark_missing(&self, path: &str) -> anyhow::Result<bool> {
        self.db.mark_media_item_missing_by_path(path).await
    }
}
