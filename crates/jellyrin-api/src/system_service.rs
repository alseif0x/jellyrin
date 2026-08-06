use jellyrin_db::Database;

/// Persistence boundary for system-level runtime settings.
pub(crate) struct SystemService<'a> {
    db: &'a Database,
}

impl<'a> SystemService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn set_remote_access(&self, enabled: bool) -> anyhow::Result<()> {
        self.db.set_remote_access(enabled).await
    }
}
