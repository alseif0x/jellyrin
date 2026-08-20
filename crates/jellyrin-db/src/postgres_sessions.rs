use anyhow::Context;
use jellyrin_core::{MediaItem, PlaybackState};
use serde_json::Value;
use sqlx::FromRow;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    ActivePlaybackSession, ActiveSessionUser, ActiveViewingSession, DatabasePoolRole,
    ResumeItemsPage, ResumeItemsPageQuery, StaleTranscodeSession, TaskRun,
    TerminalTranscodeSession, TranscodeSession, UpsertActivePlaybackSession,
    UpsertActiveViewingSession, UpsertPlaybackState, UpsertTranscodeSession,
    postgres::{POSTGRES_REPEATABLE_READ_ONLY_BEGIN, PostgresDatabase},
    telemetry::DatabaseOperation,
};

impl PostgresDatabase {
    pub async fn upsert_active_playback_session(
        &self,
        playback: UpsertActivePlaybackSession,
    ) -> anyhow::Result<()> {
        let session_id = playback.session_id.trim();
        anyhow::ensure!(!session_id.is_empty(), "session id must not be empty");
        self.pg_sessions_require_media_item(playback.item_id)
            .await?;

        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO active_playback_sessions (
                session_id, user_id, item_id, media_source_id, audio_stream_index,
                subtitle_stream_index, position_ticks, is_paused, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (session_id) DO UPDATE SET
                user_id = EXCLUDED.user_id,
                item_id = EXCLUDED.item_id,
                media_source_id = EXCLUDED.media_source_id,
                audio_stream_index = CASE
                    WHEN active_playback_sessions.item_id = EXCLUDED.item_id
                        THEN COALESCE(
                            EXCLUDED.audio_stream_index,
                            active_playback_sessions.audio_stream_index
                        )
                    ELSE EXCLUDED.audio_stream_index
                END,
                subtitle_stream_index = CASE
                    WHEN active_playback_sessions.item_id = EXCLUDED.item_id
                        THEN COALESCE(
                            EXCLUDED.subtitle_stream_index,
                            active_playback_sessions.subtitle_stream_index
                        )
                    ELSE EXCLUDED.subtitle_stream_index
                END,
                position_ticks = EXCLUDED.position_ticks,
                is_paused = EXCLUDED.is_paused,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(session_id)
        .bind(playback.user_id)
        .bind(playback.item_id)
        .bind(playback.media_source_id)
        .bind(playback.audio_stream_index)
        .bind(playback.subtitle_stream_index)
        .bind(playback.position_ticks)
        .bind(playback.is_paused)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_active_playback_session(&self, session_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_playback_sessions WHERE session_id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_playback_sessions(&self) -> anyhow::Result<Vec<ActivePlaybackSession>> {
        let rows = sqlx::query_as::<_, PostgresActivePlaybackSessionRow>(
            r#"
            SELECT playback.session_id,
                   playback.user_id,
                   playback.media_source_id,
                   playback.audio_stream_index,
                   playback.subtitle_stream_index,
                   playback.position_ticks,
                   playback.is_paused,
                   playback.updated_at AS playback_updated_at,
                   item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at
            FROM active_playback_sessions AS playback
            INNER JOIN media_items AS item ON item.id = playback.item_id
            WHERE item.missing_since IS NULL
            ORDER BY playback.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_active_viewing_session(
        &self,
        viewing: UpsertActiveViewingSession,
    ) -> anyhow::Result<()> {
        let session_id = viewing.session_id.trim();
        anyhow::ensure!(!session_id.is_empty(), "session id must not be empty");
        self.pg_sessions_require_media_item(viewing.item_id).await?;

        sqlx::query(
            r#"
            INSERT INTO active_viewing_sessions (session_id, user_id, item_id, updated_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (session_id) DO UPDATE SET
                user_id = EXCLUDED.user_id,
                item_id = EXCLUDED.item_id,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(session_id)
        .bind(viewing.user_id)
        .bind(viewing.item_id)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_active_viewing_session(&self, session_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_viewing_sessions WHERE session_id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_viewing_sessions(&self) -> anyhow::Result<Vec<ActiveViewingSession>> {
        let rows = sqlx::query_as::<_, PostgresActiveViewingSessionRow>(
            r#"
            SELECT viewing.session_id,
                   viewing.user_id,
                   viewing.updated_at AS viewing_updated_at,
                   item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at
            FROM active_viewing_sessions AS viewing
            INNER JOIN media_items AS item ON item.id = viewing.item_id
            WHERE item.missing_since IS NULL
            ORDER BY viewing.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn add_session_user(&self, session_id: &str, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO active_session_users (session_id, user_id, added_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (session_id, user_id) DO UPDATE SET
                added_at = EXCLUDED.added_at
            "#,
        )
        .bind(session_id.trim())
        .bind(user_id)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_session_user(&self, session_id: &str, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_session_users WHERE session_id = $1 AND user_id = $2")
            .bind(session_id.trim())
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_session_users(&self) -> anyhow::Result<Vec<ActiveSessionUser>> {
        let rows = sqlx::query_as::<_, PostgresActiveSessionUserRow>(
            r#"
            SELECT additional_user.session_id,
                   additional_user.user_id,
                   users.name AS user_name,
                   additional_user.added_at
            FROM active_session_users AS additional_user
            INNER JOIN users ON users.id = additional_user.user_id
            WHERE NOT users.is_disabled
            ORDER BY additional_user.added_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn upsert_transcode_session(
        &self,
        session: UpsertTranscodeSession,
    ) -> anyhow::Result<TranscodeSession> {
        let play_session_id = session.play_session_id.trim().to_string();
        let output_path = session.output_path.trim().to_string();
        let status = session.status.trim().to_ascii_lowercase();
        anyhow::ensure!(
            !play_session_id.is_empty(),
            "play session id must not be empty"
        );
        anyhow::ensure!(
            !output_path.is_empty(),
            "transcode output path must not be empty"
        );
        anyhow::ensure!(!status.is_empty(), "transcode status must not be empty");
        self.pg_sessions_require_media_item(session.item_id).await?;

        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO transcode_sessions (
                play_session_id, dedupe_key, device_id, user_id, item_id,
                media_source_id, audio_stream_index, subtitle_stream_index,
                video_stream_index, output_path, process_id, status,
                progress_percent, position_ticks, start_position_ticks,
                created_at, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $16
            )
            ON CONFLICT (play_session_id) DO UPDATE SET
                dedupe_key = EXCLUDED.dedupe_key,
                device_id = EXCLUDED.device_id,
                user_id = EXCLUDED.user_id,
                item_id = EXCLUDED.item_id,
                media_source_id = EXCLUDED.media_source_id,
                audio_stream_index = EXCLUDED.audio_stream_index,
                subtitle_stream_index = EXCLUDED.subtitle_stream_index,
                video_stream_index = EXCLUDED.video_stream_index,
                output_path = EXCLUDED.output_path,
                process_id = EXCLUDED.process_id,
                status = EXCLUDED.status,
                progress_percent = EXCLUDED.progress_percent,
                position_ticks = EXCLUDED.position_ticks,
                start_position_ticks = EXCLUDED.start_position_ticks,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(&play_session_id)
        .bind(session.dedupe_key)
        .bind(session.device_id)
        .bind(session.user_id)
        .bind(session.item_id)
        .bind(session.media_source_id)
        .bind(session.audio_stream_index)
        .bind(session.subtitle_stream_index)
        .bind(session.video_stream_index)
        .bind(&output_path)
        .bind(session.process_id)
        .bind(&status)
        .bind(session.progress_percent)
        .bind(session.position_ticks.max(0))
        .bind(session.start_position_ticks.max(0))
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.transcode_session_by_play_session_id(&play_session_id)
            .await?
            .context("transcode session missing after upsert")
    }

    pub async fn claim_transcode_session(
        &self,
        dedupe_key: &str,
        session: UpsertTranscodeSession,
    ) -> anyhow::Result<(TranscodeSession, bool)> {
        let dedupe_key = dedupe_key.trim();
        let play_session_id = session.play_session_id.trim().to_string();
        let output_path = session.output_path.trim().to_string();
        let status = session.status.trim().to_ascii_lowercase();
        anyhow::ensure!(!dedupe_key.is_empty(), "dedupe key must not be empty");
        anyhow::ensure!(
            !play_session_id.is_empty(),
            "play session id must not be empty"
        );
        anyhow::ensure!(
            !output_path.is_empty(),
            "transcode output path must not be empty"
        );
        anyhow::ensure!(!status.is_empty(), "transcode status must not be empty");
        self.pg_sessions_require_media_item(session.item_id).await?;

        let inserted_play_session_id = sqlx::query_scalar::<_, String>(
            r#"
            INSERT INTO transcode_sessions (
                play_session_id, dedupe_key, device_id, user_id, item_id,
                media_source_id, audio_stream_index, subtitle_stream_index,
                video_stream_index, output_path, process_id, status,
                progress_percent, position_ticks, start_position_ticks,
                created_at, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $16
            )
            ON CONFLICT DO NOTHING
            RETURNING play_session_id
            "#,
        )
        .bind(&play_session_id)
        .bind(dedupe_key)
        .bind(session.device_id)
        .bind(session.user_id)
        .bind(session.item_id)
        .bind(session.media_source_id)
        .bind(session.audio_stream_index)
        .bind(session.subtitle_stream_index)
        .bind(session.video_stream_index)
        .bind(&output_path)
        .bind(session.process_id)
        .bind(&status)
        .bind(session.progress_percent)
        .bind(session.position_ticks.max(0))
        .bind(session.start_position_ticks.max(0))
        .bind(OffsetDateTime::now_utc())
        .fetch_optional(&self.pool)
        .await?;

        if let Some(inserted_play_session_id) = inserted_play_session_id {
            let claimed = self
                .transcode_session_by_play_session_id(&inserted_play_session_id)
                .await?
                .context("claimed transcode session missing after insert")?;
            return Ok((claimed, true));
        }

        let existing = self
            .active_transcode_session_by_dedupe_key(dedupe_key)
            .await?
            .context("active transcode claim exists but no reusable session was visible")?;
        Ok((existing, false))
    }

    pub async fn transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.pg_transcode_sessions_with_statuses(&[]).await
    }

    pub async fn transcode_session_output_paths(&self) -> anyhow::Result<Vec<String>> {
        sqlx::query_scalar("SELECT output_path FROM transcode_sessions")
            .fetch_all(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn active_transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.pg_transcode_sessions_with_statuses(&["starting", "running"])
            .await
    }

    pub async fn active_transcode_session_by_dedupe_key(
        &self,
        dedupe_key: &str,
    ) -> anyhow::Result<Option<TranscodeSession>> {
        let row = sqlx::query_as::<_, PostgresTranscodeSessionRow>(
            r#"
            SELECT transcode.play_session_id,
                   transcode.dedupe_key,
                   transcode.device_id,
                   transcode.user_id,
                   transcode.media_source_id,
                   transcode.audio_stream_index,
                   transcode.subtitle_stream_index,
                   transcode.video_stream_index,
                   transcode.output_path,
                   transcode.process_id,
                   transcode.status,
                   transcode.progress_percent,
                   transcode.position_ticks,
                   transcode.start_position_ticks,
                   transcode.created_at AS transcode_created_at,
                   transcode.updated_at AS transcode_updated_at,
                   item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at
            FROM transcode_sessions AS transcode
            INNER JOIN media_items AS item ON item.id = transcode.item_id
            WHERE transcode.dedupe_key = $1
              AND transcode.status IN ('starting', 'running')
              AND item.missing_since IS NULL
            ORDER BY transcode.updated_at DESC
            LIMIT 1
            "#,
        )
        .bind(dedupe_key.trim())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn stale_transcode_sessions_on_startup(
        &self,
    ) -> anyhow::Result<Vec<StaleTranscodeSession>> {
        let rows = sqlx::query_as::<_, PostgresStaleTranscodeSessionRow>(
            r#"
            SELECT play_session_id, output_path, status, process_id
            FROM transcode_sessions
            WHERE status IN ('starting', 'running')
            ORDER BY updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn terminal_transcode_sessions_older_than(
        &self,
        older_than: Duration,
    ) -> anyhow::Result<Vec<TerminalTranscodeSession>> {
        let cutoff = OffsetDateTime::now_utc() - older_than;
        let rows = sqlx::query_as::<_, PostgresTerminalTranscodeSessionRow>(
            r#"
            SELECT play_session_id, output_path, status
            FROM transcode_sessions
            WHERE status IN ('completed', 'failed', 'stopped')
              AND updated_at < $1
            ORDER BY updated_at ASC
            "#,
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn transcode_session_by_play_session_id(
        &self,
        play_session_id: &str,
    ) -> anyhow::Result<Option<TranscodeSession>> {
        let row = sqlx::query_as::<_, PostgresTranscodeSessionRow>(
            r#"
            SELECT transcode.play_session_id,
                   transcode.dedupe_key,
                   transcode.device_id,
                   transcode.user_id,
                   transcode.media_source_id,
                   transcode.audio_stream_index,
                   transcode.subtitle_stream_index,
                   transcode.video_stream_index,
                   transcode.output_path,
                   transcode.process_id,
                   transcode.status,
                   transcode.progress_percent,
                   transcode.position_ticks,
                   transcode.start_position_ticks,
                   transcode.created_at AS transcode_created_at,
                   transcode.updated_at AS transcode_updated_at,
                   item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at
            FROM transcode_sessions AS transcode
            INNER JOIN media_items AS item ON item.id = transcode.item_id
            WHERE transcode.play_session_id = $1
              AND item.missing_since IS NULL
            "#,
        )
        .bind(play_session_id.trim())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn update_transcode_session_status(
        &self,
        play_session_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let status = status.trim().to_ascii_lowercase();
        anyhow::ensure!(!status.is_empty(), "transcode status must not be empty");
        sqlx::query(
            r#"
            UPDATE transcode_sessions
            SET status = $1, updated_at = $2
            WHERE play_session_id = $3
            "#,
        )
        .bind(status)
        .bind(OffsetDateTime::now_utc())
        .bind(play_session_id.trim())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_transcode_session_progress(
        &self,
        play_session_id: &str,
        progress_percent: Option<f64>,
        position_ticks: i64,
    ) -> anyhow::Result<()> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::TranscodeProgressWrite,
            DatabasePoolRole::Api,
        );
        let result = self
            .update_transcode_session_progress_unobserved(
                play_session_id,
                progress_percent,
                position_ticks,
            )
            .await;
        observation.finish_result(&result, |rows| *rows);
        result.map(|_| ())
    }

    async fn update_transcode_session_progress_unobserved(
        &self,
        play_session_id: &str,
        progress_percent: Option<f64>,
        position_ticks: i64,
    ) -> anyhow::Result<u64> {
        let play_session_id = play_session_id.trim();
        anyhow::ensure!(
            !play_session_id.is_empty(),
            "play session id must not be empty"
        );
        let result = sqlx::query(
            r#"
            UPDATE transcode_sessions
            SET progress_percent = COALESCE($1, progress_percent),
                position_ticks = $2,
                updated_at = $3
            WHERE play_session_id = $4
            "#,
        )
        .bind(progress_percent)
        .bind(position_ticks.max(0))
        .bind(OffsetDateTime::now_utc())
        .bind(play_session_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn start_task_run(&self, task_key: &str) -> anyhow::Result<TaskRun> {
        let task_key = task_key.trim();
        anyhow::ensure!(!task_key.is_empty(), "task key must not be empty");

        let id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let result = sqlx::query(
            r#"
            INSERT INTO task_runs (id, task_key, status, started_at, updated_at)
            VALUES ($1, $2, 'running', $3, $3)
            "#,
        )
        .bind(id)
        .bind(task_key)
        .bind(now)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => self.pg_session_task_run_by_id(id).await,
            Err(error) if pg_is_unique_constraint_error(&error) => {
                anyhow::bail!("task is already running")
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn complete_task_run(&self, run_id: Uuid, result: Value) -> anyhow::Result<TaskRun> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'completed',
                completed_at = $1,
                result = $2,
                error_message = NULL,
                updated_at = $1
            WHERE id = $3 AND status = 'running'
            "#,
        )
        .bind(now)
        .bind(result)
        .bind(run_id)
        .execute(&self.pool)
        .await?;

        self.pg_session_task_run_by_id(run_id).await
    }

    pub async fn update_task_run_progress(
        &self,
        run_id: Uuid,
        progress: Value,
    ) -> anyhow::Result<Option<TaskRun>> {
        let row = sqlx::query_as::<_, PostgresTaskRunRow>(
            r#"
            UPDATE task_runs
            SET result = $1,
                updated_at = $2
            WHERE id = $3 AND status = 'running'
            RETURNING id, task_key, status, started_at, completed_at,
                      result, error_message, updated_at
            "#,
        )
        .bind(progress)
        .bind(OffsetDateTime::now_utc())
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn fail_task_run(&self, run_id: Uuid, error: &str) -> anyhow::Result<TaskRun> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'failed',
                completed_at = $1,
                error_message = $2,
                updated_at = $1
            WHERE id = $3 AND status = 'running'
            "#,
        )
        .bind(now)
        .bind(error)
        .bind(run_id)
        .execute(&self.pool)
        .await?;

        self.pg_session_task_run_by_id(run_id).await
    }

    pub async fn fail_current_task_run(
        &self,
        task_key: &str,
        error: &str,
    ) -> anyhow::Result<Option<TaskRun>> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<_, PostgresTaskRunRow>(
            r#"
            WITH candidate AS (
                SELECT id
                FROM task_runs
                WHERE task_key = $1 AND status = 'running'
                ORDER BY started_at DESC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE task_runs AS task
            SET status = 'failed',
                completed_at = $2,
                error_message = $3,
                updated_at = $2
            FROM candidate
            WHERE task.id = candidate.id
            RETURNING task.id, task.task_key, task.status, task.started_at,
                      task.completed_at, task.result, task.error_message, task.updated_at
            "#,
        )
        .bind(task_key)
        .bind(now)
        .bind(error)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn fail_stale_task_runs(
        &self,
        task_key: &str,
        older_than: Duration,
        error: &str,
    ) -> anyhow::Result<usize> {
        let now = OffsetDateTime::now_utc();
        let result = sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'failed',
                completed_at = $1,
                error_message = $2,
                updated_at = $1
            WHERE task_key = $3 AND status = 'running' AND updated_at < $4
            "#,
        )
        .bind(now)
        .bind(error)
        .bind(task_key)
        .bind(now - older_than)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn import_task_run_history(
        &self,
        id: Option<Uuid>,
        task_key: &str,
        status: &str,
        started_at: OffsetDateTime,
        completed_at: OffsetDateTime,
        result: Value,
        error: Option<&str>,
    ) -> anyhow::Result<TaskRun> {
        let task_key = task_key.trim();
        anyhow::ensure!(!task_key.is_empty(), "task key must not be empty");
        anyhow::ensure!(
            matches!(status, "completed" | "failed"),
            "imported task history status must be completed or failed"
        );

        let id = id.unwrap_or_else(Uuid::new_v4);
        sqlx::query(
            r#"
            INSERT INTO task_runs (
                id, task_key, status, started_at, completed_at,
                result, error_message, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $5)
            ON CONFLICT (id) DO UPDATE SET
                task_key = EXCLUDED.task_key,
                status = EXCLUDED.status,
                started_at = EXCLUDED.started_at,
                completed_at = EXCLUDED.completed_at,
                result = EXCLUDED.result,
                error_message = EXCLUDED.error_message,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(id)
        .bind(task_key)
        .bind(status)
        .bind(started_at)
        .bind(completed_at)
        .bind(result)
        .bind(error)
        .execute(&self.pool)
        .await?;

        self.pg_session_task_run_by_id(id).await
    }

    pub async fn current_task_run(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        let row = sqlx::query_as::<_, PostgresTaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at,
                   result, error_message, updated_at
            FROM task_runs
            WHERE task_key = $1 AND status = 'running'
            ORDER BY started_at DESC
            LIMIT 1
            "#,
        )
        .bind(task_key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn last_task_result(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        let row = sqlx::query_as::<_, PostgresTaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at,
                   result, error_message, updated_at
            FROM task_runs
            WHERE task_key = $1 AND status IN ('completed', 'failed')
            ORDER BY completed_at DESC
            LIMIT 1
            "#,
        )
        .bind(task_key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn upsert_playback_state(&self, playback: UpsertPlaybackState) -> anyhow::Result<()> {
        self.pg_sessions_require_media_item(playback.item_id)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, audio_stream_index,
                subtitle_stream_index, position_ticks, is_paused, played,
                is_favorite, rating, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, false, NULL, $9)
            ON CONFLICT (user_id, item_id) DO UPDATE SET
                media_source_id = EXCLUDED.media_source_id,
                audio_stream_index = COALESCE(
                    EXCLUDED.audio_stream_index,
                    playback_states.audio_stream_index
                ),
                subtitle_stream_index = COALESCE(
                    EXCLUDED.subtitle_stream_index,
                    playback_states.subtitle_stream_index
                ),
                position_ticks = EXCLUDED.position_ticks,
                is_paused = EXCLUDED.is_paused,
                played = EXCLUDED.played,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(playback.user_id)
        .bind(playback.item_id)
        .bind(playback.media_source_id)
        .bind(playback.audio_stream_index)
        .bind(playback.subtitle_stream_index)
        .bind(playback.position_ticks.max(0))
        .bind(playback.is_paused)
        .bind(playback.played)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_item_favorite(
        &self,
        user_id: Uuid,
        item_id: Uuid,
        is_favorite: bool,
    ) -> anyhow::Result<()> {
        self.pg_sessions_require_media_item(item_id).await?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, position_ticks, is_paused, played,
                is_favorite, updated_at
            )
            VALUES ($1, $2, 0, false, false, $3, $4)
            ON CONFLICT (user_id, item_id) DO UPDATE SET
                is_favorite = EXCLUDED.is_favorite,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user_id)
        .bind(item_id)
        .bind(is_favorite)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_item_rating(
        &self,
        user_id: Uuid,
        item_id: Uuid,
        rating: Option<f64>,
    ) -> anyhow::Result<()> {
        self.pg_sessions_require_media_item(item_id).await?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, position_ticks, is_paused, played,
                is_favorite, rating, updated_at
            )
            VALUES ($1, $2, 0, false, false, false, $3, $4)
            ON CONFLICT (user_id, item_id) DO UPDATE SET
                rating = EXCLUDED.rating,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user_id)
        .bind(item_id)
        .bind(rating)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn playback_state_for_item(
        &self,
        user_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<Option<PlaybackState>> {
        self.pg_sessions_require_media_item(item_id).await?;
        let row = sqlx::query_as::<_, PostgresPlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, audio_stream_index,
                   subtitle_stream_index, position_ticks, is_paused, played,
                   is_favorite, rating, updated_at
            FROM playback_states
            WHERE user_id = $1 AND item_id = $2
            "#,
        )
        .bind(user_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    /// Fetches user data for a bounded set of items in one indexed query.
    pub async fn playback_states_for_items(
        &self,
        user_id: Uuid,
        item_ids: &[Uuid],
    ) -> anyhow::Result<Vec<PlaybackState>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, PostgresPlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, audio_stream_index,
                   subtitle_stream_index, position_ticks, is_paused, played,
                   is_favorite, rating, updated_at
            FROM playback_states
            WHERE user_id = $1 AND item_id = ANY($2)
            "#,
        )
        .bind(user_id)
        .bind(item_ids.to_vec())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn playback_states_for_user(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<PlaybackState>> {
        let rows = sqlx::query_as::<_, PostgresPlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, audio_stream_index,
                   subtitle_stream_index, position_ticks, is_paused, played,
                   is_favorite, rating, updated_at
            FROM playback_states
            WHERE user_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn resume_items_for_user(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<(MediaItem, PlaybackState)>> {
        let rows = sqlx::query_as::<_, PostgresResumeItemRow>(
            r#"
            SELECT item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at,
                   playback.user_id AS playback_user_id,
                   playback.item_id AS playback_item_id,
                   playback.media_source_id,
                   playback.audio_stream_index,
                   playback.subtitle_stream_index,
                   playback.position_ticks,
                   playback.is_paused,
                   playback.played,
                   playback.is_favorite,
                   playback.rating,
                   playback.updated_at AS playback_updated_at
            FROM playback_states AS playback
            INNER JOIN media_items AS item ON item.id = playback.item_id
            WHERE playback.user_id = $1
              AND item.missing_since IS NULL
              AND playback.position_ticks > 0
              AND NOT playback.played
            ORDER BY playback.updated_at DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Returns an exact, policy-filtered resume page from one repeatable-read snapshot.
    pub async fn resume_items_page_for_user(
        &self,
        user_id: Uuid,
        query: ResumeItemsPageQuery,
    ) -> anyhow::Result<ResumeItemsPage> {
        let mut transaction = self
            .pool
            .begin_with(POSTGRES_REPEATABLE_READ_ONLY_BEGIN)
            .await?;
        let min_pct = query.min_pct.clamp(0, 100);
        let max_pct = query.max_pct.clamp(min_pct, 100);
        let total_record_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM playback_states AS playback
            INNER JOIN media_items AS item ON item.id = playback.item_id
            WHERE playback.user_id = $1
              AND item.missing_since IS NULL
              AND playback.position_ticks > 0
              AND NOT playback.played
              AND (
                    item.runtime_ticks IS NULL
                 OR item.runtime_ticks <= 0
                 OR (
                        item.runtime_ticks >= $2
                    AND playback.position_ticks::double precision * 100.0
                          / item.runtime_ticks::double precision >= $3
                    AND playback.position_ticks::double precision * 100.0
                          / item.runtime_ticks::double precision < $4
                 )
              )
            "#,
        )
        .bind(user_id)
        .bind(query.min_duration_ticks.max(0))
        .bind(min_pct as f64)
        .bind(max_pct as f64)
        .fetch_one(&mut *transaction)
        .await?;

        let rows = sqlx::query_as::<_, PostgresResumeItemRow>(
            r#"
            SELECT item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at,
                   playback.user_id AS playback_user_id,
                   playback.item_id AS playback_item_id,
                   playback.media_source_id,
                   playback.audio_stream_index,
                   playback.subtitle_stream_index,
                   playback.position_ticks,
                   playback.is_paused,
                   playback.played,
                   playback.is_favorite,
                   playback.rating,
                   playback.updated_at AS playback_updated_at
            FROM playback_states AS playback
            INNER JOIN media_items AS item ON item.id = playback.item_id
            WHERE playback.user_id = $1
              AND item.missing_since IS NULL
              AND playback.position_ticks > 0
              AND NOT playback.played
              AND (
                    item.runtime_ticks IS NULL
                 OR item.runtime_ticks <= 0
                 OR (
                        item.runtime_ticks >= $2
                    AND playback.position_ticks::double precision * 100.0
                          / item.runtime_ticks::double precision >= $3
                    AND playback.position_ticks::double precision * 100.0
                          / item.runtime_ticks::double precision < $4
                 )
              )
            ORDER BY playback.updated_at DESC
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(user_id)
        .bind(query.min_duration_ticks.max(0))
        .bind(min_pct as f64)
        .bind(max_pct as f64)
        .bind(i64::try_from(query.limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(query.start_index).unwrap_or(i64::MAX))
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(ResumeItemsPage {
            items: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<_>>()?,
            total_record_count: usize::try_from(total_record_count)
                .context("resume item count does not fit usize")?,
            start_index: query.start_index,
        })
    }

    async fn pg_sessions_require_media_item(&self, item_id: Uuid) -> anyhow::Result<()> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_items WHERE id = $1)")
                .bind(item_id)
                .fetch_one(&self.pool)
                .await?;
        anyhow::ensure!(exists, "media item {item_id} does not exist");
        Ok(())
    }

    async fn pg_transcode_sessions_with_statuses(
        &self,
        statuses: &[&str],
    ) -> anyhow::Result<Vec<TranscodeSession>> {
        let statuses = statuses
            .iter()
            .map(|status| (*status).to_string())
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<_, PostgresTranscodeSessionRow>(
            r#"
            SELECT transcode.play_session_id,
                   transcode.dedupe_key,
                   transcode.device_id,
                   transcode.user_id,
                   transcode.media_source_id,
                   transcode.audio_stream_index,
                   transcode.subtitle_stream_index,
                   transcode.video_stream_index,
                   transcode.output_path,
                   transcode.process_id,
                   transcode.status,
                   transcode.progress_percent,
                   transcode.position_ticks,
                   transcode.start_position_ticks,
                   transcode.created_at AS transcode_created_at,
                   transcode.updated_at AS transcode_updated_at,
                   item.id,
                   item.virtual_folder_id,
                   item.name,
                   item.path,
                   item.media_type,
                   item.collection_type,
                   item.file_size,
                   item.runtime_ticks,
                   item.bitrate,
                   item.width,
                   item.height,
                   item.media_streams,
                   item.created_at,
                   item.updated_at
            FROM transcode_sessions AS transcode
            INNER JOIN media_items AS item ON item.id = transcode.item_id
            WHERE item.missing_since IS NULL
              AND (cardinality($1::text[]) = 0 OR transcode.status = ANY($1))
            ORDER BY transcode.updated_at DESC
            "#,
        )
        .bind(statuses)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn pg_session_task_run_by_id(&self, id: Uuid) -> anyhow::Result<TaskRun> {
        let row = sqlx::query_as::<_, PostgresTaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at,
                   result, error_message, updated_at
            FROM task_runs
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .context("task run not found")?;

        Ok(row.into())
    }
}

#[derive(FromRow)]
struct PostgresMediaItemRow {
    id: Uuid,
    virtual_folder_id: Uuid,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<PostgresMediaItemRow> for MediaItem {
    type Error = anyhow::Error;

    fn try_from(row: PostgresMediaItemRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            virtual_folder_id: row.virtual_folder_id,
            name: row.name,
            path: row.path,
            media_type: row.media_type,
            collection_type: row.collection_type,
            file_size: row.file_size,
            runtime_ticks: row.runtime_ticks,
            bitrate: row.bitrate,
            width: row.width,
            height: row.height,
            media_streams: serde_json::from_value(row.media_streams)
                .context("invalid media streams in PostgreSQL")?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(FromRow)]
struct PostgresPlaybackStateRow {
    user_id: Uuid,
    item_id: Uuid,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    position_ticks: i64,
    is_paused: bool,
    played: bool,
    is_favorite: bool,
    rating: Option<f64>,
    updated_at: OffsetDateTime,
}

impl From<PostgresPlaybackStateRow> for PlaybackState {
    fn from(row: PostgresPlaybackStateRow) -> Self {
        Self {
            user_id: row.user_id,
            item_id: row.item_id,
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            played: row.played,
            is_favorite: row.is_favorite,
            rating: row.rating,
            updated_at: row.updated_at,
        }
    }
}

#[derive(FromRow)]
struct PostgresActivePlaybackSessionRow {
    session_id: String,
    user_id: Uuid,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    position_ticks: i64,
    is_paused: bool,
    playback_updated_at: OffsetDateTime,
    #[sqlx(flatten)]
    item: PostgresMediaItemRow,
}

impl TryFrom<PostgresActivePlaybackSessionRow> for ActivePlaybackSession {
    type Error = anyhow::Error;

    fn try_from(row: PostgresActivePlaybackSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            user_id: row.user_id,
            item: row.item.try_into()?,
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            updated_at: row.playback_updated_at,
        })
    }
}

#[derive(FromRow)]
struct PostgresActiveViewingSessionRow {
    session_id: String,
    user_id: Uuid,
    viewing_updated_at: OffsetDateTime,
    #[sqlx(flatten)]
    item: PostgresMediaItemRow,
}

impl TryFrom<PostgresActiveViewingSessionRow> for ActiveViewingSession {
    type Error = anyhow::Error;

    fn try_from(row: PostgresActiveViewingSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            user_id: row.user_id,
            item: row.item.try_into()?,
            updated_at: row.viewing_updated_at,
        })
    }
}

#[derive(FromRow)]
struct PostgresActiveSessionUserRow {
    session_id: String,
    user_id: Uuid,
    user_name: String,
    added_at: OffsetDateTime,
}

impl From<PostgresActiveSessionUserRow> for ActiveSessionUser {
    fn from(row: PostgresActiveSessionUserRow) -> Self {
        Self {
            session_id: row.session_id,
            user_id: row.user_id,
            user_name: row.user_name,
            added_at: row.added_at,
        }
    }
}

#[derive(FromRow)]
struct PostgresTranscodeSessionRow {
    play_session_id: String,
    dedupe_key: Option<String>,
    device_id: Option<String>,
    user_id: Uuid,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    video_stream_index: Option<i64>,
    output_path: String,
    process_id: Option<i64>,
    status: String,
    progress_percent: Option<f64>,
    position_ticks: i64,
    start_position_ticks: i64,
    transcode_created_at: OffsetDateTime,
    transcode_updated_at: OffsetDateTime,
    #[sqlx(flatten)]
    item: PostgresMediaItemRow,
}

impl TryFrom<PostgresTranscodeSessionRow> for TranscodeSession {
    type Error = anyhow::Error;

    fn try_from(row: PostgresTranscodeSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            play_session_id: row.play_session_id,
            dedupe_key: row.dedupe_key,
            device_id: row.device_id,
            user_id: row.user_id,
            item: row.item.try_into()?,
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            video_stream_index: row.video_stream_index,
            output_path: row.output_path,
            process_id: row.process_id,
            status: row.status,
            progress_percent: row.progress_percent,
            position_ticks: row.position_ticks,
            start_position_ticks: row.start_position_ticks,
            created_at: row.transcode_created_at,
            updated_at: row.transcode_updated_at,
        })
    }
}

#[derive(FromRow)]
struct PostgresStaleTranscodeSessionRow {
    play_session_id: String,
    output_path: String,
    status: String,
    process_id: Option<i64>,
}

impl From<PostgresStaleTranscodeSessionRow> for StaleTranscodeSession {
    fn from(row: PostgresStaleTranscodeSessionRow) -> Self {
        Self {
            play_session_id: row.play_session_id,
            output_path: row.output_path,
            status: row.status,
            process_id: row.process_id,
        }
    }
}

#[derive(FromRow)]
struct PostgresTerminalTranscodeSessionRow {
    play_session_id: String,
    output_path: String,
    status: String,
}

impl From<PostgresTerminalTranscodeSessionRow> for TerminalTranscodeSession {
    fn from(row: PostgresTerminalTranscodeSessionRow) -> Self {
        Self {
            play_session_id: row.play_session_id,
            output_path: row.output_path,
            status: row.status,
        }
    }
}

#[derive(FromRow)]
struct PostgresResumeItemRow {
    #[sqlx(flatten)]
    item: PostgresMediaItemRow,
    playback_user_id: Uuid,
    playback_item_id: Uuid,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    position_ticks: i64,
    is_paused: bool,
    played: bool,
    is_favorite: bool,
    rating: Option<f64>,
    playback_updated_at: OffsetDateTime,
}

impl TryFrom<PostgresResumeItemRow> for (MediaItem, PlaybackState) {
    type Error = anyhow::Error;

    fn try_from(row: PostgresResumeItemRow) -> Result<Self, Self::Error> {
        let item = row.item.try_into()?;
        let playback = PlaybackState {
            user_id: row.playback_user_id,
            item_id: row.playback_item_id,
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            played: row.played,
            is_favorite: row.is_favorite,
            rating: row.rating,
            updated_at: row.playback_updated_at,
        };
        Ok((item, playback))
    }
}

#[derive(FromRow)]
struct PostgresTaskRunRow {
    id: Uuid,
    task_key: String,
    status: String,
    started_at: OffsetDateTime,
    completed_at: Option<OffsetDateTime>,
    result: Option<Value>,
    error_message: Option<String>,
    updated_at: OffsetDateTime,
}

impl From<PostgresTaskRunRow> for TaskRun {
    fn from(row: PostgresTaskRunRow) -> Self {
        Self {
            id: row.id,
            task_key: row.task_key,
            status: row.status,
            started_at: row.started_at,
            completed_at: row.completed_at,
            result_json: row.result,
            error_message: row.error_message,
            updated_at: row.updated_at,
        }
    }
}

fn pg_is_unique_constraint_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::PostgresSettings;

    struct SessionFixture {
        user_id: Uuid,
        folder_id: Uuid,
        item_id: Uuid,
    }

    impl SessionFixture {
        async fn create(database: &PostgresDatabase) -> anyhow::Result<Self> {
            let fixture = Self {
                user_id: Uuid::new_v4(),
                folder_id: Uuid::new_v4(),
                item_id: Uuid::new_v4(),
            };
            let now = OffsetDateTime::now_utc();
            let mut transaction = database.pool.begin().await?;
            sqlx::query(
                r#"
                INSERT INTO users (id, name, created_at, updated_at)
                VALUES ($1, $2, $3, $3)
                "#,
            )
            .bind(fixture.user_id)
            .bind(format!("postgres-session-user-{}", fixture.user_id))
            .bind(now)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO virtual_folders (
                    id, name, collection_type, locations, created_at, updated_at
                )
                VALUES ($1, $2, 'movies', $3, $4, $4)
                "#,
            )
            .bind(fixture.folder_id)
            .bind(format!("postgres-session-folder-{}", fixture.folder_id))
            .bind(json!([]))
            .bind(now)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    media_streams, metadata, created_at, updated_at
                )
                VALUES ($1, $2, $3, $4, 'Movie', 'movies', $5, $6, $7, $7)
                "#,
            )
            .bind(fixture.item_id)
            .bind(fixture.folder_id)
            .bind(format!("PostgreSQL session item {}", fixture.item_id))
            .bind(format!("remote://postgres-session/{}", fixture.item_id))
            .bind(json!([]))
            .bind(json!({}))
            .bind(now)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            Ok(fixture)
        }

        async fn cleanup(&self, database: &PostgresDatabase) -> anyhow::Result<()> {
            let mut transaction = database.pool.begin().await?;
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(self.user_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM virtual_folders WHERE id = $1")
                .bind(self.folder_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok(())
        }
    }

    async fn configured_database() -> Option<PostgresDatabase> {
        let database_url = std::env::var("JELLYRIN_TEST_POSTGRES_URL").ok()?;
        let settings = PostgresSettings::new(database_url).unwrap();
        let database = PostgresDatabase::connect_with_settings(&settings)
            .await
            .unwrap();
        database.migrate().await.unwrap();
        Some(database)
    }

    #[tokio::test]
    async fn postgres_playback_preserves_stream_selection_and_user_item_data() {
        let Some(database) = configured_database().await else {
            return;
        };
        let fixture = SessionFixture::create(&database).await.unwrap();
        let session_id = format!("postgres-playback-{}", Uuid::new_v4());

        let outcome = async {
            database
                .set_item_favorite(fixture.user_id, fixture.item_id, true)
                .await?;
            database
                .set_item_rating(fixture.user_id, fixture.item_id, Some(8.5))
                .await?;
            database
                .upsert_playback_state(UpsertPlaybackState {
                    user_id: fixture.user_id,
                    item_id: fixture.item_id,
                    media_source_id: Some("source-a".to_string()),
                    audio_stream_index: Some(1),
                    subtitle_stream_index: Some(2),
                    position_ticks: 100,
                    is_paused: false,
                    played: false,
                })
                .await?;
            database
                .upsert_playback_state(UpsertPlaybackState {
                    user_id: fixture.user_id,
                    item_id: fixture.item_id,
                    media_source_id: Some("source-a".to_string()),
                    audio_stream_index: None,
                    subtitle_stream_index: Some(-1),
                    position_ticks: 200,
                    is_paused: true,
                    played: false,
                })
                .await?;

            database
                .upsert_active_playback_session(UpsertActivePlaybackSession {
                    session_id: session_id.clone(),
                    user_id: fixture.user_id,
                    item_id: fixture.item_id,
                    media_source_id: Some("source-a".to_string()),
                    audio_stream_index: Some(3),
                    subtitle_stream_index: Some(4),
                    position_ticks: 100,
                    is_paused: false,
                })
                .await?;
            database
                .upsert_active_playback_session(UpsertActivePlaybackSession {
                    session_id: session_id.clone(),
                    user_id: fixture.user_id,
                    item_id: fixture.item_id,
                    media_source_id: Some("source-a".to_string()),
                    audio_stream_index: None,
                    subtitle_stream_index: None,
                    position_ticks: 200,
                    is_paused: true,
                })
                .await?;

            let state = database
                .playback_state_for_item(fixture.user_id, fixture.item_id)
                .await?
                .context("playback state missing after upsert")?;
            let resume_items = database.resume_items_for_user(fixture.user_id, 10).await?;
            let active = database
                .active_playback_sessions()
                .await?
                .into_iter()
                .find(|session| session.session_id == session_id)
                .context("active playback session missing after upsert")?;
            database
                .device_sessions()
                .await
                .context("device session listing failed")?;
            database
                .active_viewing_sessions()
                .await
                .context("active viewing listing failed")?;
            database
                .active_session_users()
                .await
                .context("active session user listing failed")?;
            database
                .server_state()
                .await
                .context("server state lookup failed")?;
            anyhow::Ok((state, resume_items, active))
        }
        .await;

        database
            .clear_active_playback_session(&session_id)
            .await
            .unwrap();
        fixture.cleanup(&database).await.unwrap();
        database.close().await;

        let (state, resume_items, active) = outcome.unwrap();
        assert_eq!(state.audio_stream_index, Some(1));
        assert_eq!(state.subtitle_stream_index, Some(-1));
        assert_eq!(state.position_ticks, 200);
        assert!(state.is_paused);
        assert!(state.is_favorite);
        assert_eq!(state.rating, Some(8.5));
        assert_eq!(resume_items.len(), 1);
        assert_eq!(resume_items[0].0.id, fixture.item_id);
        assert_eq!(active.audio_stream_index, Some(3));
        assert_eq!(active.subtitle_stream_index, Some(4));
        assert_eq!(active.position_ticks, 200);
        assert!(active.is_paused);
    }

    #[tokio::test]
    async fn postgres_resume_page_applies_policy_and_exact_total_in_one_snapshot() {
        let Some(database) = configured_database().await else {
            return;
        };
        let fixture = SessionFixture::create(&database).await.unwrap();
        let outcome = async {
            sqlx::query("UPDATE media_items SET runtime_ticks = $1 WHERE id = $2")
                .bind(10_000_000_000_i64)
                .bind(fixture.item_id)
                .execute(&database.pool)
                .await?;
            database
                .upsert_playback_state(UpsertPlaybackState {
                    user_id: fixture.user_id,
                    item_id: fixture.item_id,
                    media_source_id: None,
                    audio_stream_index: None,
                    subtitle_stream_index: None,
                    position_ticks: 1_000_000_000,
                    is_paused: false,
                    played: false,
                })
                .await?;
            database
                .resume_items_page_for_user(
                    fixture.user_id,
                    ResumeItemsPageQuery {
                        start_index: 0,
                        limit: 1,
                        min_pct: 5,
                        max_pct: 90,
                        min_duration_ticks: 3_000_000_000,
                    },
                )
                .await
        }
        .await;
        fixture.cleanup(&database).await.unwrap();
        database.close().await;

        let page = outcome.unwrap();
        assert_eq!(page.total_record_count, 1);
        assert_eq!(page.start_index, 0);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].0.id, fixture.item_id);
    }

    #[tokio::test]
    async fn postgres_transcode_claim_is_unique_and_progress_is_monotonic_per_update() {
        let Some(database) = configured_database().await else {
            return;
        };
        let fixture = SessionFixture::create(&database).await.unwrap();
        let dedupe_key = format!("postgres-transcode-dedupe-{}", Uuid::new_v4());
        let first_play_session_id = format!("postgres-transcode-a-{}", Uuid::new_v4());
        let second_play_session_id = format!("postgres-transcode-b-{}", Uuid::new_v4());
        let first_database = database.clone();
        let second_database = database.clone();

        let outcome = async {
            let first = UpsertTranscodeSession {
                play_session_id: first_play_session_id,
                dedupe_key: None,
                device_id: Some("device-a".to_string()),
                user_id: fixture.user_id,
                item_id: fixture.item_id,
                media_source_id: Some("source-a".to_string()),
                audio_stream_index: Some(1),
                subtitle_stream_index: Some(-1),
                video_stream_index: Some(0),
                output_path: format!("/tmp/postgres-transcode-a-{}", Uuid::new_v4()),
                process_id: None,
                status: "starting".to_string(),
                progress_percent: None,
                position_ticks: 0,
                start_position_ticks: 0,
            };
            let second = UpsertTranscodeSession {
                play_session_id: second_play_session_id,
                dedupe_key: None,
                device_id: Some("device-b".to_string()),
                user_id: fixture.user_id,
                item_id: fixture.item_id,
                media_source_id: Some("source-a".to_string()),
                audio_stream_index: Some(1),
                subtitle_stream_index: Some(-1),
                video_stream_index: Some(0),
                output_path: format!("/tmp/postgres-transcode-b-{}", Uuid::new_v4()),
                process_id: None,
                status: "starting".to_string(),
                progress_percent: None,
                position_ticks: 0,
                start_position_ticks: 0,
            };

            let (first_result, second_result) = tokio::join!(
                first_database.claim_transcode_session(&dedupe_key, first),
                second_database.claim_transcode_session(&dedupe_key, second),
            );
            let first_result = first_result?;
            let second_result = second_result?;
            anyhow::ensure!(
                first_result.1 ^ second_result.1,
                "exactly one concurrent transcode request must own the claim"
            );
            anyhow::ensure!(
                first_result.0.play_session_id == second_result.0.play_session_id,
                "both requests must resolve to the same transcode session"
            );

            let play_session_id = first_result.0.play_session_id;
            database
                .update_transcode_session_progress(&play_session_id, Some(42.5), 123)
                .await?;
            database
                .update_transcode_session_progress(&play_session_id, None, 456)
                .await?;
            let updated = database
                .transcode_session_by_play_session_id(&play_session_id)
                .await?
                .context("claimed transcode session missing after progress update")?;
            let active_count: i64 = sqlx::query_scalar(
                r#"
                SELECT count(*)
                FROM transcode_sessions
                WHERE dedupe_key = $1 AND status IN ('starting', 'running')
                "#,
            )
            .bind(&dedupe_key)
            .fetch_one(&database.pool)
            .await?;
            anyhow::Ok((updated, active_count))
        }
        .await;

        fixture.cleanup(&database).await.unwrap();
        database.close().await;

        let (updated, active_count) = outcome.unwrap();
        assert_eq!(active_count, 1);
        assert_eq!(updated.progress_percent, Some(42.5));
        assert_eq!(updated.position_ticks, 456);
        assert_eq!(updated.dedupe_key.as_deref(), Some(dedupe_key.as_str()));
    }

    #[tokio::test]
    async fn postgres_task_run_partial_unique_index_serializes_concurrent_starts() {
        let Some(database) = configured_database().await else {
            return;
        };
        let task_key = format!("PostgresConcurrentTask-{}", Uuid::new_v4());
        let first_database = database.clone();
        let second_database = database.clone();

        let outcome = async {
            let (first_result, second_result) = tokio::join!(
                first_database.start_task_run(&task_key),
                second_database.start_task_run(&task_key),
            );
            let successes = usize::from(first_result.is_ok()) + usize::from(second_result.is_ok());
            let failures = usize::from(first_result.is_err()) + usize::from(second_result.is_err());
            anyhow::ensure!(successes == 1, "exactly one task start must succeed");
            anyhow::ensure!(failures == 1, "exactly one task start must be rejected");

            let run = first_result.or(second_result)?;
            let progress = json!({"PercentComplete": 25.0});
            let updated = database
                .update_task_run_progress(run.id, progress.clone())
                .await?
                .context("running task rejected a progress update")?;
            anyhow::ensure!(updated.result_json == Some(progress));
            let failed = database
                .fail_current_task_run(&task_key, "cancelled by test")
                .await?
                .context("current task was not failed atomically")?;
            anyhow::ensure!(failed.id == run.id);
            anyhow::ensure!(failed.status == "failed");
            anyhow::ensure!(database.current_task_run(&task_key).await?.is_none());

            let replacement = database.start_task_run(&task_key).await?;
            database
                .complete_task_run(replacement.id, json!({"ok": true}))
                .await?;
            anyhow::Ok(())
        }
        .await;

        sqlx::query("DELETE FROM task_runs WHERE task_key = $1")
            .bind(&task_key)
            .execute(&database.pool)
            .await
            .unwrap();
        database.close().await;

        outcome.unwrap();
    }
}
