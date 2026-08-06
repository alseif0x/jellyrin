use jellyrin_db::{Database, TaskRun};
use serde_json::Value;
use uuid::Uuid;

/// Persistence boundary for scheduled-task lifecycle and progress.
pub(crate) struct TaskService<'a> {
    db: &'a Database,
}

impl<'a> TaskService<'a> {
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub(crate) async fn update_progress(
        &self,
        run_id: Uuid,
        result: Value,
    ) -> anyhow::Result<Option<TaskRun>> {
        self.db.update_task_run_progress(run_id, result).await
    }

    pub(crate) async fn complete(&self, run_id: Uuid, result: Value) -> anyhow::Result<TaskRun> {
        self.db.complete_task_run(run_id, result).await
    }

    pub(crate) async fn fail(&self, run_id: Uuid, error: &str) -> anyhow::Result<TaskRun> {
        self.db.fail_task_run(run_id, error).await
    }

    pub(crate) async fn fail_current(
        &self,
        task_key: &str,
        error: &str,
    ) -> anyhow::Result<Option<TaskRun>> {
        self.db.fail_current_task_run(task_key, error).await
    }

    pub(crate) async fn current(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        self.db.current_task_run(task_key).await
    }

    pub(crate) async fn last_result(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        self.db.last_task_result(task_key).await
    }

    pub(crate) async fn start(&self, task_key: &str) -> anyhow::Result<TaskRun> {
        self.db.start_task_run(task_key).await
    }

    pub(crate) async fn fail_stale(
        &self,
        task_key: &str,
        older_than: time::Duration,
        error: &str,
    ) -> anyhow::Result<usize> {
        self.db
            .fail_stale_task_runs(task_key, older_than, error)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn import_history(
        &self,
        id: Option<Uuid>,
        task_key: &str,
        status: &str,
        started_at: time::OffsetDateTime,
        completed_at: time::OffsetDateTime,
        result: Value,
        error: Option<&str>,
    ) -> anyhow::Result<TaskRun> {
        self.db
            .import_task_run_history(
                id,
                task_key,
                status,
                started_at,
                completed_at,
                result,
                error,
            )
            .await
    }
}
