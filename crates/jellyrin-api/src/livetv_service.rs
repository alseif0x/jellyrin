use jellyrin_db::{
    Database, LiveTvCategoryRecord, LiveTvCategoryUpsert, LiveTvChannelQuery, LiveTvChannelRecord,
    LiveTvChannelUpsert, LiveTvPage, LiveTvTunerUpsert,
};

/// Persistence boundary for Live TV catalogue and tuner state.
pub(crate) struct LiveTvService<'a> {
    db: &'a Database,
}

impl<'a> LiveTvService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn channel_page(
        &self,
        query: LiveTvChannelQuery,
    ) -> anyhow::Result<LiveTvPage<LiveTvChannelRecord>> {
        self.db.live_tv_channel_page(query).await
    }

    pub(crate) async fn channel_count(&self, query: &LiveTvChannelQuery) -> anyhow::Result<usize> {
        self.db.live_tv_channel_count(query).await
    }

    pub(crate) async fn channel_by_id(
        &self,
        channel_id: &str,
    ) -> anyhow::Result<Option<LiveTvChannelRecord>> {
        self.db.live_tv_channel_by_id(channel_id).await
    }

    pub(crate) async fn categories(&self) -> anyhow::Result<Vec<LiveTvCategoryRecord>> {
        self.db.live_tv_categories().await
    }

    pub(crate) async fn delete_tuner_state(&self, tuner_id: &str) -> anyhow::Result<()> {
        self.db.delete_live_tv_tuner_state(tuner_id).await
    }

    pub(crate) async fn replace_tuner_snapshot(
        &self,
        tuner: LiveTvTunerUpsert,
        categories: Vec<LiveTvCategoryUpsert>,
        channels: Vec<LiveTvChannelUpsert>,
    ) -> anyhow::Result<()> {
        self.db
            .replace_live_tv_tuner_snapshot(tuner, categories, channels)
            .await
    }
}
