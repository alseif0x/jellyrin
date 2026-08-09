use uuid::Uuid;

use crate::Database;

/// Persistence boundary for activity/audit entries emitted by the API.
pub(crate) struct ActivityService<'a> {
    db: &'a Database,
}

impl<'a> ActivityService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn record(
        &self,
        name: &str,
        overview: Option<&str>,
        entry_type: &str,
        user_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        self.db
            .add_activity_log_entry(name, overview, overview, entry_type, user_id)
            .await
            .map(|_| ())
    }
}
