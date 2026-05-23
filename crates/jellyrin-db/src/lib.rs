use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Context;
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use jellyrin_core::{
    DeviceToken, MediaItem, PlaybackState, ServerState, StartupConfig, User, VirtualFolder,
};
use serde_json::Value;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

#[derive(Debug, Clone)]
pub struct TaskRun {
    pub id: Uuid,
    pub task_key: String,
    pub status: String,
    pub started_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub result_json: Option<Value>,
    pub error_message: Option<String>,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct DeviceSession {
    pub access_token: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub device_id: String,
    pub device_name: String,
    pub client: String,
    pub version: String,
    pub last_activity_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ActivePlaybackSession {
    pub session_id: String,
    pub user_id: Uuid,
    pub item: MediaItem,
    pub media_source_id: Option<String>,
    pub position_ticks: i64,
    pub is_paused: bool,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandingConfig {
    pub login_disclaimer: Option<String>,
    pub custom_css: Option<String>,
    pub splashscreen_enabled: bool,
}

impl Database {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .with_context(|| format!("failed to connect SQLite database at {database_url}"))?;

        MIGRATOR
            .run(&pool)
            .await
            .context("failed to run migrations")?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn server_state(&self) -> anyhow::Result<ServerState> {
        let row = sqlx::query_as::<_, ServerStateRow>(
            r#"
            SELECT server_id, server_name, startup_wizard_completed, created_at, updated_at
            FROM server_state
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => self.create_initial_server_state().await,
        }
    }

    pub async fn startup_config(&self) -> anyhow::Result<StartupConfig> {
        let state = self.server_state().await?;
        let row = sqlx::query_as::<_, StartupConfigRow>(
            r#"
            SELECT ui_culture, metadata_country_code, preferred_metadata_language, enable_remote_access
            FROM startup_config
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(StartupConfig {
                server_name: state.server_name,
                ui_culture: row.ui_culture,
                metadata_country_code: row.metadata_country_code,
                preferred_metadata_language: row.preferred_metadata_language,
                enable_remote_access: row.enable_remote_access,
            }),
            None => self.create_initial_startup_config(state.server_name).await,
        }
    }

    pub async fn update_startup_config(&self, config: StartupConfig) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE server_state
            SET server_name = ?1, updated_at = ?2
            WHERE id = 1
            "#,
        )
        .bind(&config.server_name)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO startup_config (
                id, ui_culture, metadata_country_code, preferred_metadata_language, enable_remote_access, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                ui_culture = excluded.ui_culture,
                metadata_country_code = excluded.metadata_country_code,
                preferred_metadata_language = excluded.preferred_metadata_language,
                enable_remote_access = excluded.enable_remote_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.ui_culture)
        .bind(config.metadata_country_code)
        .bind(config.preferred_metadata_language)
        .bind(config.enable_remote_access)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn set_remote_access(&self, enabled: bool) -> anyhow::Result<()> {
        let mut config = self.startup_config().await?;
        config.enable_remote_access = enabled;
        self.update_startup_config(config).await
    }

    pub async fn branding_config(&self) -> anyhow::Result<BrandingConfig> {
        let row = sqlx::query_as::<_, BrandingConfigRow>(
            r#"
            SELECT login_disclaimer, custom_css, splashscreen_enabled
            FROM branding_config
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => Ok(BrandingConfig::default()),
        }
    }

    pub async fn update_branding_config(&self, config: BrandingConfig) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO branding_config (
                id, login_disclaimer, custom_css, splashscreen_enabled, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET
                login_disclaimer = excluded.login_disclaimer,
                custom_css = excluded.custom_css,
                splashscreen_enabled = excluded.splashscreen_enabled,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.login_disclaimer)
        .bind(config.custom_css)
        .bind(config.splashscreen_enabled)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_startup_wizard(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE server_state
            SET startup_wizard_completed = 1, updated_at = ?1
            WHERE id = 1
            "#,
        )
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn first_user(&self) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            ORDER BY created_at
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => self.create_placeholder_admin_user().await,
        }
    }

    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_admin_user(&self, name: &str, password: &str) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "admin user name must not be empty"
        );
        anyhow::ensure!(!password.is_empty(), "admin password must not be empty");

        let now = format_time(OffsetDateTime::now_utc())?;
        let existing = self.optional_user_by_name(trimmed_name).await?;
        let id = existing.as_ref().map_or_else(Uuid::new_v4, |user| user.id);

        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, created_at, updated_at)
            VALUES (?1, ?2, 1, 0, ?3, ?3)
            ON CONFLICT(name) DO UPDATE SET
                is_administrator = 1,
                is_disabled = 0,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_name)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let password_hash = hash_password(password)?;
        sqlx::query(
            r#"
            INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
            VALUES (?1, 'argon2id', ?2, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                algorithm = excluded.algorithm,
                password_hash = excluded.password_hash,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(password_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.user_by_id(id).await
    }

    pub async fn update_first_user(&self, name: String, password: &str) -> anyhow::Result<User> {
        let user = self.first_user().await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE users
            SET name = ?1, is_administrator = 1, is_disabled = 0, updated_at = ?2
            WHERE id = ?3
            "#,
        )
        .bind(name.trim())
        .bind(&now)
        .bind(user.id.to_string())
        .execute(&self.pool)
        .await?;

        let password_hash = hash_password(password)?;
        sqlx::query(
            r#"
            INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
            VALUES (?1, 'argon2id', ?2, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                algorithm = excluded.algorithm,
                password_hash = excluded.password_hash,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user.id.to_string())
        .bind(password_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.user_by_id(user.id).await
    }

    pub async fn authenticate_user_by_name(
        &self,
        username: &str,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_name(username).await?;
        anyhow::ensure!(!user.is_disabled, "user is disabled");

        let password_row = sqlx::query_as::<_, PasswordRow>(
            r#"
            SELECT password_hash
            FROM user_passwords
            WHERE user_id = ?1
            "#,
        )
        .bind(user.id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("password is not configured")?;

        verify_password(password, &password_row.password_hash)?;
        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn user_by_token(&self, token: &str) -> anyhow::Result<(User, DeviceToken)> {
        let token_row = sqlx::query_as::<_, DeviceTokenRow>(
            r#"
            SELECT access_token, user_id, device_id, device_name, client, version
            FROM devices
            WHERE access_token = ?1
            "#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?
        .context("invalid token")?;

        let token: DeviceToken = token_row.try_into()?;
        self.touch_device_token(&token.access_token).await?;
        let user = self.user_by_id(token.user_id).await?;
        Ok((user, token))
    }

    pub async fn user_by_api_key(&self, api_key: &str) -> anyhow::Result<(User, DeviceToken)> {
        let row = sqlx::query_as::<_, ApiKeyRow>(
            r#"
            SELECT access_token, user_id, name
            FROM api_keys
            WHERE access_token = ?1
            "#,
        )
        .bind(api_key)
        .fetch_optional(&self.pool)
        .await?
        .context("invalid api key")?;

        sqlx::query("UPDATE api_keys SET last_activity_at = ?1 WHERE access_token = ?2")
            .bind(format_time(OffsetDateTime::now_utc())?)
            .bind(api_key)
            .execute(&self.pool)
            .await?;

        let user = self.user_by_id(Uuid::parse_str(&row.user_id)?).await?;
        Ok((
            user,
            DeviceToken {
                access_token: row.access_token,
                user_id: Uuid::parse_str(&row.user_id)?,
                device_id: format!("api-key:{}", row.name),
                device_name: row.name,
                client: "API Key".to_string(),
                version: "dev".to_string(),
            },
        ))
    }

    pub async fn issue_api_key_for_user(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> anyhow::Result<String> {
        let trimmed_name = name.trim();
        anyhow::ensure!(!trimmed_name.is_empty(), "api key name must not be empty");

        let now = format_time(OffsetDateTime::now_utc())?;
        let access_token = Uuid::new_v4().simple().to_string();
        sqlx::query(
            r#"
            INSERT INTO api_keys (access_token, user_id, name, created_at, last_activity_at)
            VALUES (?1, ?2, ?3, ?4, ?4)
            "#,
        )
        .bind(&access_token)
        .bind(user_id.to_string())
        .bind(trimmed_name)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(access_token)
    }

    pub async fn revoke_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM devices WHERE access_token = ?1")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn revoke_device(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM active_playback_sessions
            WHERE session_id IN (
                SELECT access_token FROM devices WHERE access_token = ?1 OR device_id = ?1
            )
            "#,
        )
        .bind(id)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM devices WHERE access_token = ?1 OR device_id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn device_sessions(&self) -> anyhow::Result<Vec<DeviceSession>> {
        let rows = sqlx::query_as::<_, DeviceSessionRow>(
            r#"
            SELECT devices.access_token, devices.user_id, users.name AS user_name,
                   devices.device_id, devices.device_name, devices.client, devices.version,
                   devices.last_activity_at
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE users.is_disabled = 0
            ORDER BY devices.last_activity_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn device_sessions_for_user(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<DeviceSession>> {
        let rows = sqlx::query_as::<_, DeviceSessionRow>(
            r#"
            SELECT devices.access_token, devices.user_id, users.name AS user_name,
                   devices.device_id, devices.device_name, devices.client, devices.version,
                   devices.last_activity_at
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE users.is_disabled = 0 AND devices.user_id = ?1
            ORDER BY devices.last_activity_at DESC
            "#,
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn device_session_by_id(&self, id: &str) -> anyhow::Result<Option<DeviceSession>> {
        let row = sqlx::query_as::<_, DeviceSessionRow>(
            r#"
            SELECT devices.access_token, devices.user_id, users.name AS user_name,
                   devices.device_id, devices.device_name, devices.client, devices.version,
                   devices.last_activity_at
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE users.is_disabled = 0 AND (devices.access_token = ?1 OR devices.device_id = ?1)
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn update_device_name(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE devices
            SET device_name = ?1, last_activity_at = ?2
            WHERE access_token = ?3 OR device_id = ?3
            "#,
        )
        .bind(name)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_active_playback_session(
        &self,
        session_id: &str,
        user_id: Uuid,
        item_id: Uuid,
        media_source_id: Option<&str>,
        position_ticks: i64,
        is_paused: bool,
    ) -> anyhow::Result<()> {
        let trimmed_session_id = session_id.trim();
        anyhow::ensure!(
            !trimmed_session_id.is_empty(),
            "session id must not be empty"
        );
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO active_playback_sessions (
                session_id, user_id, item_id, media_source_id, position_ticks, is_paused, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(session_id) DO UPDATE SET
                user_id = excluded.user_id,
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                position_ticks = excluded.position_ticks,
                is_paused = excluded.is_paused,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(trimmed_session_id)
        .bind(user_id.to_string())
        .bind(item_id.to_string())
        .bind(media_source_id)
        .bind(position_ticks)
        .bind(is_paused)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_active_playback_session(&self, session_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_playback_sessions WHERE session_id = ?1")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_playback_sessions(&self) -> anyhow::Result<Vec<ActivePlaybackSession>> {
        let rows = sqlx::query_as::<_, ActivePlaybackSessionRow>(
            r#"
            SELECT active_playback_sessions.session_id,
                   active_playback_sessions.user_id,
                   active_playback_sessions.media_source_id,
                   active_playback_sessions.position_ticks,
                   active_playback_sessions.is_paused,
                   active_playback_sessions.updated_at AS playback_updated_at,
                   media_items.id,
                   media_items.virtual_folder_id,
                   media_items.name,
                   media_items.path,
                   media_items.media_type,
                   media_items.collection_type,
                   media_items.created_at,
                   media_items.updated_at
            FROM active_playback_sessions
            INNER JOIN media_items ON media_items.id = active_playback_sessions.item_id
            WHERE media_items.missing_since IS NULL
            ORDER BY active_playback_sessions.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn start_task_run(&self, task_key: &str) -> anyhow::Result<TaskRun> {
        let trimmed_key = task_key.trim();
        anyhow::ensure!(!trimmed_key.is_empty(), "task key must not be empty");

        let id = Uuid::new_v4();
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            INSERT INTO task_runs (id, task_key, status, started_at, updated_at)
            VALUES (?1, ?2, 'running', ?3, ?3)
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_key)
        .bind(now)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => self.task_run_by_id(id).await,
            Err(error) if is_unique_constraint_error(&error) => {
                anyhow::bail!("task is already running")
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn complete_task_run(&self, run_id: Uuid, result: Value) -> anyhow::Result<TaskRun> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let result_json = serde_json::to_string(&result)?;
        sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'completed',
                completed_at = ?1,
                result_json = ?2,
                error_message = NULL,
                updated_at = ?1
            WHERE id = ?3 AND status = 'running'
            "#,
        )
        .bind(now)
        .bind(result_json)
        .bind(run_id.to_string())
        .execute(&self.pool)
        .await?;

        self.task_run_by_id(run_id).await
    }

    pub async fn fail_task_run(&self, run_id: Uuid, error: &str) -> anyhow::Result<TaskRun> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'failed',
                completed_at = ?1,
                error_message = ?2,
                updated_at = ?1
            WHERE id = ?3 AND status = 'running'
            "#,
        )
        .bind(now)
        .bind(error)
        .bind(run_id.to_string())
        .execute(&self.pool)
        .await?;

        self.task_run_by_id(run_id).await
    }

    pub async fn fail_current_task_run(
        &self,
        task_key: &str,
        error: &str,
    ) -> anyhow::Result<Option<TaskRun>> {
        let Some(run) = self.current_task_run(task_key).await? else {
            return Ok(None);
        };
        self.fail_task_run(run.id, error).await.map(Some)
    }

    pub async fn fail_stale_task_runs(
        &self,
        task_key: &str,
        older_than: Duration,
        error: &str,
    ) -> anyhow::Result<usize> {
        let cutoff = format_time(OffsetDateTime::now_utc() - older_than)?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            UPDATE task_runs
            SET status = 'failed',
                completed_at = ?1,
                error_message = ?2,
                updated_at = ?1
            WHERE task_key = ?3 AND status = 'running' AND updated_at < ?4
            "#,
        )
        .bind(now)
        .bind(error)
        .bind(task_key)
        .bind(cutoff)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    pub async fn current_task_run(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        let row = sqlx::query_as::<_, TaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at, result_json, error_message, updated_at
            FROM task_runs
            WHERE task_key = ?1 AND status = 'running'
            ORDER BY started_at DESC
            LIMIT 1
            "#,
        )
        .bind(task_key)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn last_task_result(&self, task_key: &str) -> anyhow::Result<Option<TaskRun>> {
        let row = sqlx::query_as::<_, TaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at, result_json, error_message, updated_at
            FROM task_runs
            WHERE task_key = ?1 AND status IN ('completed', 'failed')
            ORDER BY completed_at DESC
            LIMIT 1
            "#,
        )
        .bind(task_key)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn virtual_folders(&self) -> anyhow::Result<Vec<VirtualFolder>> {
        let rows = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_virtual_folder(
        &self,
        name: &str,
        collection_type: Option<&str>,
        locations: Vec<String>,
    ) -> anyhow::Result<VirtualFolder> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );

        let now = format_time(OffsetDateTime::now_utc())?;
        let existing = self.virtual_folder_by_name(trimmed_name).await?;
        let id = existing
            .as_ref()
            .map_or_else(Uuid::new_v4, |folder| folder.id);
        let locations_json = serde_json::to_string(&normalized_locations(locations))?;

        sqlx::query(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(name) DO UPDATE SET
                collection_type = excluded.collection_type,
                locations_json = excluded.locations_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_name)
        .bind(
            collection_type
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        )
        .bind(locations_json)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.virtual_folder_by_name(trimmed_name)
            .await?
            .context("virtual folder was not persisted")
    }

    pub async fn add_virtual_folder_path(&self, name: &str, path: &str) -> anyhow::Result<()> {
        let mut folder = self
            .virtual_folder_by_name(name)
            .await?
            .context("virtual folder not found")?;
        let trimmed_path = path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );

        if !folder
            .locations
            .iter()
            .any(|location| location == trimmed_path)
        {
            folder.locations.push(trimmed_path.to_string());
            self.upsert_virtual_folder(
                &folder.name,
                folder.collection_type.as_deref(),
                folder.locations,
            )
            .await?;
        }

        Ok(())
    }

    pub async fn remove_virtual_folder_path(&self, name: &str, path: &str) -> anyhow::Result<bool> {
        let Some(mut folder) = self.virtual_folder_by_name(name).await? else {
            return Ok(false);
        };
        let trimmed_path = path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );

        let original_len = folder.locations.len();
        folder.locations.retain(|location| location != trimmed_path);
        if folder.locations.len() == original_len {
            return Ok(false);
        }

        let folder_id = folder.id;
        self.upsert_virtual_folder(
            &folder.name,
            folder.collection_type.as_deref(),
            folder.locations,
        )
        .await?;
        self.delete_media_items_under_path(folder_id, trimmed_path)
            .await?;
        Ok(true)
    }

    pub async fn delete_virtual_folder(&self, name: &str) -> anyhow::Result<bool> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );
        let Some(folder) = self.virtual_folder_by_name(trimmed_name).await? else {
            return Ok(false);
        };

        self.delete_media_items_for_folder(folder.id).await?;
        let result = sqlx::query("DELETE FROM virtual_folders WHERE id = ?1")
            .bind(folder.id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn media_items(&self) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type, created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn latest_media_items(&self, limit: i64) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type, created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL
            ORDER BY created_at DESC, name COLLATE NOCASE
            LIMIT ?1
            "#,
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_playback_state(
        &self,
        user_id: Uuid,
        item_id: Uuid,
        media_source_id: Option<&str>,
        position_ticks: i64,
        is_paused: bool,
        played: bool,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, position_ticks, is_paused, played, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                media_source_id = excluded.media_source_id,
                position_ticks = excluded.position_ticks,
                is_paused = excluded.is_paused,
                played = excluded.played,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user_id.to_string())
        .bind(item_id.to_string())
        .bind(media_source_id)
        .bind(position_ticks.max(0))
        .bind(is_paused)
        .bind(played)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn playback_state_for_item(
        &self,
        user_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<Option<PlaybackState>> {
        let row = sqlx::query_as::<_, PlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, position_ticks, is_paused, played, updated_at
            FROM playback_states
            WHERE user_id = ?1 AND item_id = ?2
            "#,
        )
        .bind(user_id.to_string())
        .bind(item_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn resume_items_for_user(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<(MediaItem, PlaybackState)>> {
        let rows = sqlx::query_as::<_, ResumeItemRow>(
            r#"
            SELECT
                media_items.id, media_items.virtual_folder_id, media_items.name, media_items.path,
                media_items.media_type, media_items.collection_type, media_items.created_at,
                media_items.updated_at, playback_states.user_id, playback_states.item_id,
                playback_states.media_source_id, playback_states.position_ticks,
                playback_states.is_paused, playback_states.played,
                playback_states.updated_at AS playback_updated_at
            FROM playback_states
            INNER JOIN media_items ON media_items.id = playback_states.item_id
            WHERE playback_states.user_id = ?1
              AND media_items.missing_since IS NULL
              AND playback_states.position_ticks > 0
              AND playback_states.played = 0
            ORDER BY playback_states.updated_at DESC
            LIMIT ?2
            "#,
        )
        .bind(user_id.to_string())
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn scan_virtual_folder_items(&self, folder_id: Uuid) -> anyhow::Result<usize> {
        let folder = self
            .virtual_folder_by_id(folder_id)
            .await?
            .context("virtual folder not found")?;
        let mut scanned = 0usize;
        let mut found_paths = HashSet::new();
        let mut can_reconcile_stale = true;

        for location in &folder.locations {
            let location = Path::new(location);
            let Some(media_files) = collect_media_files_if_root_available(location).await? else {
                can_reconcile_stale = false;
                continue;
            };
            for path in media_files {
                let Some(name) = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
                else {
                    continue;
                };
                let Some(media_type) = media_type_for_path(&path) else {
                    continue;
                };

                found_paths.insert(path.to_string_lossy().to_string());
                self.upsert_media_item(&folder, &name, &path, media_type)
                    .await?;
                scanned += 1;
            }
        }

        if can_reconcile_stale {
            self.mark_stale_media_items_for_folder(folder.id, &found_paths)
                .await?;
        }

        Ok(scanned)
    }

    async fn upsert_media_item(
        &self,
        folder: &VirtualFolder,
        name: &str,
        path: &Path,
        media_type: &str,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let path = path.to_string_lossy().to_string();
        let metadata = tokio::fs::metadata(path.as_str()).await.ok();
        let file_size = metadata.as_ref().map(|metadata| metadata.len() as i64);
        let modified_at = metadata
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| format_time(OffsetDateTime::from(modified)).ok());
        let exact_id =
            sqlx::query_as::<_, MediaItemIdRow>("SELECT id FROM media_items WHERE path = ?1")
                .bind(&path)
                .fetch_optional(&self.pool)
                .await?
                .map(|row| row.id);

        if exact_id.is_none()
            && let Some(missing_id) = self
                .missing_media_item_id_for_identity(
                    folder.id,
                    media_type,
                    &path,
                    file_size,
                    modified_at.as_deref(),
                )
                .await?
        {
            sqlx::query(
                r#"
                UPDATE media_items
                SET name = ?1, path = ?2, media_type = ?3, collection_type = ?4,
                    updated_at = ?5, last_seen_at = ?5, missing_since = NULL,
                    file_size = ?6, modified_at = ?7
                WHERE id = ?8
                "#,
            )
            .bind(name)
            .bind(path)
            .bind(media_type)
            .bind(&folder.collection_type)
            .bind(&now)
            .bind(file_size)
            .bind(modified_at)
            .bind(missing_id)
            .execute(&self.pool)
            .await?;
            return Ok(());
        }

        let existing_id = exact_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                created_at, updated_at, last_seen_at, missing_since, file_size, modified_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7, NULL, ?8, ?9)
            ON CONFLICT(path) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                name = excluded.name,
                media_type = excluded.media_type,
                collection_type = excluded.collection_type,
                updated_at = excluded.updated_at,
                last_seen_at = excluded.last_seen_at,
                missing_since = NULL,
                file_size = excluded.file_size,
                modified_at = excluded.modified_at
            "#,
        )
        .bind(existing_id)
        .bind(folder.id.to_string())
        .bind(name)
        .bind(path)
        .bind(media_type)
        .bind(&folder.collection_type)
        .bind(&now)
        .bind(file_size)
        .bind(modified_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn create_initial_server_state(&self) -> anyhow::Result<ServerState> {
        let now = OffsetDateTime::now_utc();
        let state = ServerState {
            server_id: Uuid::new_v4(),
            server_name: "Jellyrin".to_string(),
            startup_wizard_completed: false,
            created_at: now,
            updated_at: now,
        };

        sqlx::query(
            r#"
            INSERT INTO server_state (
                id, server_id, server_name, startup_wizard_completed, created_at, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(state.server_id.to_string())
        .bind(&state.server_name)
        .bind(state.startup_wizard_completed)
        .bind(format_time(state.created_at)?)
        .bind(format_time(state.updated_at)?)
        .execute(&self.pool)
        .await?;

        Ok(state)
    }

    async fn create_initial_startup_config(
        &self,
        server_name: String,
    ) -> anyhow::Result<StartupConfig> {
        let config = StartupConfig {
            server_name,
            ui_culture: "en-US".to_string(),
            metadata_country_code: "US".to_string(),
            preferred_metadata_language: "en".to_string(),
            enable_remote_access: false,
        };
        self.update_startup_config(config.clone()).await?;
        Ok(config)
    }

    async fn create_placeholder_admin_user(&self) -> anyhow::Result<User> {
        let now = OffsetDateTime::now_utc();
        let user = User {
            id: Uuid::new_v4(),
            name: "admin".to_string(),
            is_administrator: true,
            is_disabled: false,
            created_at: now,
            updated_at: now,
        };

        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(user.id.to_string())
        .bind(&user.name)
        .bind(user.is_administrator)
        .bind(user.is_disabled)
        .bind(format_time(user.created_at)?)
        .bind(format_time(user.updated_at)?)
        .execute(&self.pool)
        .await?;

        Ok(user)
    }

    async fn user_by_name(&self, username: &str) -> anyhow::Result<User> {
        self.optional_user_by_name(username)
            .await?
            .context("user not found")
    }

    async fn optional_user_by_name(&self, username: &str) -> anyhow::Result<Option<User>> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            WHERE name = ?1
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    async fn user_by_id(&self, user_id: Uuid) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            WHERE id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("user not found")?;

        row.try_into()
    }

    async fn touch_device_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE devices SET last_activity_at = ?1 WHERE access_token = ?2")
            .bind(format_time(OffsetDateTime::now_utc())?)
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn issue_device_token(
        &self,
        user: &User,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<DeviceToken> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let access_token = Uuid::new_v4().simple().to_string();
        sqlx::query("DELETE FROM devices WHERE user_id = ?1 AND device_id = ?2")
            .bind(user.id.to_string())
            .bind(device_id)
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                access_token, user_id, device_id, device_name, client, version, created_at, last_activity_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            "#,
        )
        .bind(&access_token)
        .bind(user.id.to_string())
        .bind(device_id)
        .bind(device_name)
        .bind(client)
        .bind(version)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(DeviceToken {
            access_token,
            user_id: user.id,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            client: client.to_string(),
            version: version.to_string(),
        })
    }

    async fn virtual_folder_by_name(&self, name: &str) -> anyhow::Result<Option<VirtualFolder>> {
        let row = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            WHERE name = ?1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    async fn virtual_folder_by_id(&self, id: Uuid) -> anyhow::Result<Option<VirtualFolder>> {
        let row = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            WHERE id = ?1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    async fn task_run_by_id(&self, id: Uuid) -> anyhow::Result<TaskRun> {
        let row = sqlx::query_as::<_, TaskRunRow>(
            r#"
            SELECT id, task_key, status, started_at, completed_at, result_json, error_message, updated_at
            FROM task_runs
            WHERE id = ?1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("task run not found")?;

        row.try_into()
    }

    async fn delete_media_items_for_folder(&self, folder_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM playback_states
            WHERE item_id IN (SELECT id FROM media_items WHERE virtual_folder_id = ?1)
            "#,
        )
        .bind(folder_id.to_string())
        .execute(&self.pool)
        .await?;
        sqlx::query("DELETE FROM media_items WHERE virtual_folder_id = ?1")
            .bind(folder_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_media_items_under_path(
        &self,
        folder_id: Uuid,
        path: &str,
    ) -> anyhow::Result<()> {
        let nested_prefix = format!("{}/%", path.trim_end_matches('/'));
        sqlx::query(
            r#"
            DELETE FROM playback_states
            WHERE item_id IN (
                SELECT id FROM media_items
                WHERE virtual_folder_id = ?1 AND (path = ?2 OR path LIKE ?3)
            )
            "#,
        )
        .bind(folder_id.to_string())
        .bind(path)
        .bind(&nested_prefix)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            DELETE FROM media_items
            WHERE virtual_folder_id = ?1 AND (path = ?2 OR path LIKE ?3)
            "#,
        )
        .bind(folder_id.to_string())
        .bind(path)
        .bind(nested_prefix)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn missing_media_item_id_for_identity(
        &self,
        folder_id: Uuid,
        media_type: &str,
        current_path: &str,
        file_size: Option<i64>,
        modified_at: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let Some(file_size) = file_size else {
            return Ok(None);
        };
        let Some(modified_at) = modified_at else {
            return Ok(None);
        };

        let row = sqlx::query_as::<_, MediaItemIdRow>(
            r#"
            SELECT id
            FROM media_items
            WHERE virtual_folder_id = ?1
              AND media_type = ?2
              AND file_size = ?3
              AND modified_at = ?4
              AND path <> ?5
            ORDER BY missing_since IS NULL, missing_since DESC
            LIMIT 1
            "#,
        )
        .bind(folder_id.to_string())
        .bind(media_type)
        .bind(file_size)
        .bind(modified_at)
        .bind(current_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| row.id))
    }

    async fn mark_stale_media_items_for_folder(
        &self,
        folder_id: Uuid,
        found_paths: &HashSet<String>,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let rows = sqlx::query_as::<_, MediaItemPathRow>(
            "SELECT id, path FROM media_items WHERE virtual_folder_id = ?1 AND missing_since IS NULL",
        )
        .bind(folder_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        for row in rows
            .into_iter()
            .filter(|row| !found_paths.contains(&row.path))
        {
            sqlx::query(
                r#"
                UPDATE media_items
                SET missing_since = ?1, updated_at = ?1
                WHERE id = ?2
                "#,
            )
            .bind(&now)
            .bind(&row.id)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct StartupConfigRow {
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
    enable_remote_access: bool,
}

#[derive(sqlx::FromRow)]
struct BrandingConfigRow {
    login_disclaimer: Option<String>,
    custom_css: Option<String>,
    splashscreen_enabled: bool,
}

impl Default for BrandingConfig {
    fn default() -> Self {
        Self {
            login_disclaimer: None,
            custom_css: None,
            splashscreen_enabled: true,
        }
    }
}

impl TryFrom<BrandingConfigRow> for BrandingConfig {
    type Error = anyhow::Error;

    fn try_from(row: BrandingConfigRow) -> Result<Self, Self::Error> {
        Ok(Self {
            login_disclaimer: row.login_disclaimer,
            custom_css: row.custom_css,
            splashscreen_enabled: row.splashscreen_enabled,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ServerStateRow {
    server_id: String,
    server_name: String,
    startup_wizard_completed: bool,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    created_at: String,
    updated_at: String,
}

impl TryFrom<UserRow> for User {
    type Error = anyhow::Error;

    fn try_from(row: UserRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid user id in database")?,
            name: row.name,
            is_administrator: row.is_administrator,
            is_disabled: row.is_disabled,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PasswordRow {
    password_hash: String,
}

#[derive(sqlx::FromRow)]
struct DeviceTokenRow {
    access_token: String,
    user_id: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
}

#[derive(sqlx::FromRow)]
struct DeviceSessionRow {
    access_token: String,
    user_id: String,
    user_name: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
    last_activity_at: String,
}

#[derive(sqlx::FromRow)]
struct ApiKeyRow {
    access_token: String,
    user_id: String,
    name: String,
}

#[derive(sqlx::FromRow)]
struct VirtualFolderRow {
    id: String,
    name: String,
    collection_type: Option<String>,
    locations_json: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemRow {
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemIdRow {
    id: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemPathRow {
    id: String,
    path: String,
}

#[derive(sqlx::FromRow)]
struct ResumeItemRow {
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    created_at: String,
    updated_at: String,
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    position_ticks: i64,
    is_paused: bool,
    played: bool,
    playback_updated_at: String,
}

#[derive(sqlx::FromRow)]
struct PlaybackStateRow {
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    position_ticks: i64,
    is_paused: bool,
    played: bool,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct ActivePlaybackSessionRow {
    session_id: String,
    user_id: String,
    media_source_id: Option<String>,
    position_ticks: i64,
    is_paused: bool,
    playback_updated_at: String,
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct TaskRunRow {
    id: String,
    task_key: String,
    status: String,
    started_at: String,
    completed_at: Option<String>,
    result_json: Option<String>,
    error_message: Option<String>,
    updated_at: String,
}

impl TryFrom<VirtualFolderRow> for VirtualFolder {
    type Error = anyhow::Error;

    fn try_from(row: VirtualFolderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid virtual folder id in database")?,
            name: row.name,
            collection_type: row.collection_type,
            locations: serde_json::from_str(&row.locations_json)
                .context("invalid virtual folder locations in database")?,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<MediaItemRow> for MediaItem {
    type Error = anyhow::Error;

    fn try_from(row: MediaItemRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid media item id in database")?,
            virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                .context("invalid media item virtual folder id in database")?,
            name: row.name,
            path: row.path,
            media_type: row.media_type,
            collection_type: row.collection_type,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<ResumeItemRow> for (MediaItem, PlaybackState) {
    type Error = anyhow::Error;

    fn try_from(row: ResumeItemRow) -> Result<Self, Self::Error> {
        let item = MediaItem {
            id: Uuid::parse_str(&row.id).context("invalid media item id in database")?,
            virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                .context("invalid media item virtual folder id in database")?,
            name: row.name,
            path: row.path,
            media_type: row.media_type,
            collection_type: row.collection_type,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        };
        let playback = PlaybackState {
            user_id: Uuid::parse_str(&row.user_id)
                .context("invalid playback user id in database")?,
            item_id: Uuid::parse_str(&row.item_id)
                .context("invalid playback item id in database")?,
            media_source_id: row.media_source_id,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            played: row.played,
            updated_at: parse_time(&row.playback_updated_at)?,
        };
        Ok((item, playback))
    }
}

impl TryFrom<PlaybackStateRow> for PlaybackState {
    type Error = anyhow::Error;

    fn try_from(row: PlaybackStateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            user_id: Uuid::parse_str(&row.user_id)
                .context("invalid playback user id in database")?,
            item_id: Uuid::parse_str(&row.item_id)
                .context("invalid playback item id in database")?,
            media_source_id: row.media_source_id,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            played: row.played,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<ActivePlaybackSessionRow> for ActivePlaybackSession {
    type Error = anyhow::Error;

    fn try_from(row: ActivePlaybackSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            user_id: Uuid::parse_str(&row.user_id).context("invalid active playback user id")?,
            item: MediaItem {
                id: Uuid::parse_str(&row.id).context("invalid active playback item id")?,
                virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                    .context("invalid active playback virtual folder id")?,
                name: row.name,
                path: row.path,
                media_type: row.media_type,
                collection_type: row.collection_type,
                created_at: parse_time(&row.created_at)?,
                updated_at: parse_time(&row.updated_at)?,
            },
            media_source_id: row.media_source_id,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            updated_at: parse_time(&row.playback_updated_at)?,
        })
    }
}

impl TryFrom<TaskRunRow> for TaskRun {
    type Error = anyhow::Error;

    fn try_from(row: TaskRunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid task run id in database")?,
            task_key: row.task_key,
            status: row.status,
            started_at: parse_time(&row.started_at)?,
            completed_at: row.completed_at.as_deref().map(parse_time).transpose()?,
            result_json: row
                .result_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            error_message: row.error_message,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<DeviceTokenRow> for DeviceToken {
    type Error = anyhow::Error;

    fn try_from(row: DeviceTokenRow) -> Result<Self, Self::Error> {
        Ok(Self {
            access_token: row.access_token,
            user_id: Uuid::parse_str(&row.user_id).context("invalid token user id in database")?,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
        })
    }
}

impl TryFrom<DeviceSessionRow> for DeviceSession {
    type Error = anyhow::Error;

    fn try_from(row: DeviceSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            access_token: row.access_token,
            user_id: Uuid::parse_str(&row.user_id).context("invalid session user id")?,
            user_name: row.user_name,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
            last_activity_at: parse_time(&row.last_activity_at)?,
        })
    }
}

impl TryFrom<ServerStateRow> for ServerState {
    type Error = anyhow::Error;

    fn try_from(row: ServerStateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            server_id: Uuid::parse_str(&row.server_id).context("invalid server_id in database")?,
            server_name: row.server_name,
            startup_wizard_completed: row.startup_wizard_completed,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

fn format_time(value: OffsetDateTime) -> anyhow::Result<String> {
    value.format(&Rfc3339).context("failed to format timestamp")
}

fn parse_time(value: &str) -> anyhow::Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).context("failed to parse timestamp")
}

fn is_unique_constraint_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(anyhow::Error::msg)
}

fn verify_password(password: &str, password_hash: &str) -> anyhow::Result<()> {
    let parsed_hash = PasswordHash::new(password_hash).map_err(anyhow::Error::msg)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| anyhow::anyhow!("invalid username or password"))
}

async fn collect_media_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut media_files = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(path) = pending.pop() {
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };

        if metadata.is_file() {
            if media_type_for_path(&path).is_some() {
                media_files.push(path);
            }
            continue;
        }

        if !metadata.is_dir() {
            continue;
        }

        let Ok(mut entries) = tokio::fs::read_dir(&path).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            pending.push(entry.path());
        }
    }

    media_files.sort();
    Ok(media_files)
}

async fn collect_media_files_if_root_available(
    root: &Path,
) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let Ok(metadata) = tokio::fs::symlink_metadata(root).await else {
        return Ok(None);
    };

    if metadata.is_dir() && tokio::fs::read_dir(root).await.is_err() {
        return Ok(None);
    }

    collect_media_files(root).await.map(Some)
}

fn media_type_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "mkv" | "mp4" | "avi" | "mov" | "wmv" | "m4v" | "webm" => Some("Video"),
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav" => Some("Audio"),
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" => Some("Photo"),
        "epub" | "pdf" | "cbz" | "cbr" => Some("Book"),
        _ => None,
    }
}

fn normalized_locations(locations: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for location in locations {
        let location = location.trim();
        if !location.is_empty() && !normalized.iter().any(|value| value == location) {
            normalized.push(location.to_string());
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::Database;
    use serde_json::json;
    use time::Duration;

    #[tokio::test]
    async fn creates_initial_server_state_once() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let first = db.server_state().await.unwrap();
        let second = db.server_state().await.unwrap();

        assert_eq!(first.server_id, second.server_id);
        assert_eq!(first.server_name, "Jellyrin");
        assert!(!first.startup_wizard_completed);
    }

    #[tokio::test]
    async fn config_user_and_token_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let mut config = db.startup_config().await.unwrap();
        config.server_name = "Casa".to_string();
        config.ui_culture = "es-ES".to_string();
        db.update_startup_config(config).await.unwrap();

        let first = db.first_user().await.unwrap();
        assert_eq!(first.name, "admin");

        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();
        let (logged_in, token) = db
            .authenticate_user_by_name(
                "root",
                "secret",
                "device-1",
                "Browser",
                "Jellyfin Web",
                "dev",
            )
            .await
            .unwrap();

        assert_eq!(user.id, logged_in.id);
        assert!(!token.access_token.is_empty());

        let (token_user, _) = db.user_by_token(&token.access_token).await.unwrap();
        assert_eq!(token_user.id, user.id);

        db.revoke_token(&token.access_token).await.unwrap();
        assert!(db.user_by_token(&token.access_token).await.is_err());
    }

    #[tokio::test]
    async fn api_key_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();

        let api_key = db.issue_api_key_for_user(user.id, "qa").await.unwrap();
        let (api_user, token) = db.user_by_api_key(&api_key).await.unwrap();

        assert_eq!(api_user.id, user.id);
        assert_eq!(token.access_token, api_key);
        assert_eq!(token.client, "API Key");
    }

    #[tokio::test]
    async fn device_sessions_are_created_by_login_and_revoked_with_token() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();

        let (_, token) = db
            .authenticate_user_by_name(
                "root",
                "secret",
                "device-1",
                "Firefox",
                "Jellyfin Web",
                "dev",
            )
            .await
            .unwrap();
        let sessions = db.device_sessions_for_user(user.id).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].access_token, token.access_token);
        assert_eq!(sessions[0].user_name, "root");
        assert_eq!(sessions[0].device_id, "device-1");
        assert_eq!(sessions[0].client, "Jellyfin Web");

        db.revoke_token(&token.access_token).await.unwrap();
        assert!(
            db.device_sessions_for_user(user.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn active_playback_sessions_track_and_clear_now_playing() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Example Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        let (_, token) = db
            .authenticate_user_by_name(
                "root",
                "secret",
                "device-1",
                "Firefox",
                "Jellyfin Web",
                "dev",
            )
            .await
            .unwrap();

        db.upsert_active_playback_session(
            &token.access_token,
            user.id,
            item.id,
            Some(&item.id.to_string()),
            42,
            false,
        )
        .await
        .unwrap();
        let sessions = db.active_playback_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, token.access_token);
        assert_eq!(sessions[0].item.id, item.id);
        assert_eq!(sessions[0].position_ticks, 42);

        db.clear_active_playback_session(&token.access_token)
            .await
            .unwrap();
        assert!(db.active_playback_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn task_runs_track_current_and_last_result() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let run = db.start_task_run("RefreshLibrary").await.unwrap();
        assert_eq!(run.task_key, "RefreshLibrary");
        assert_eq!(run.status, "running");
        assert!(db.start_task_run("RefreshLibrary").await.is_err());
        assert!(
            db.current_task_run("RefreshLibrary")
                .await
                .unwrap()
                .is_some()
        );

        let completed = db
            .complete_task_run(run.id, json!({ "ItemsScanned": 7 }))
            .await
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.result_json.unwrap()["ItemsScanned"], 7);
        assert!(
            db.current_task_run("RefreshLibrary")
                .await
                .unwrap()
                .is_none()
        );

        let last = db
            .last_task_result("RefreshLibrary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last.id, run.id);
        assert_eq!(last.status, "completed");
    }

    #[tokio::test]
    async fn task_runs_can_be_cancelled_and_stale_runs_expire() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let run = db.start_task_run("RefreshLibrary").await.unwrap();
        let failed = db
            .fail_current_task_run("RefreshLibrary", "cancelled")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.id, run.id);
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.error_message.as_deref(), Some("cancelled"));

        let stale = db.start_task_run("RefreshLibrary").await.unwrap();
        let expired = db
            .fail_stale_task_runs("RefreshLibrary", Duration::ZERO, "expired")
            .await
            .unwrap();
        assert_eq!(expired, 1);
        let last = db
            .last_task_result("RefreshLibrary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last.id, stale.id);
        assert_eq!(last.status, "failed");
        assert_eq!(last.error_message.as_deref(), Some("expired"));
    }

    #[tokio::test]
    async fn virtual_folders_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec!["/media/movies".to_string(), "/media/movies".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(folder.name, "Movies");
        assert_eq!(folder.collection_type.as_deref(), Some("movies"));
        assert_eq!(folder.locations, vec!["/media/movies"]);

        db.add_virtual_folder_path("Movies", "/media/more-movies")
            .await
            .unwrap();
        let folders = db.virtual_folders().await.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(
            folders[0].locations,
            vec!["/media/movies", "/media/more-movies"]
        );

        assert!(
            db.remove_virtual_folder_path("Movies", "/media/more-movies")
                .await
                .unwrap()
        );
        let folders = db.virtual_folders().await.unwrap();
        assert_eq!(folders[0].locations, vec!["/media/movies"]);
        assert!(db.delete_virtual_folder("Movies").await.unwrap());
        assert!(db.virtual_folders().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn scans_media_items_from_virtual_folder_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Movies").join("Example Movie.mkv");
        tokio::fs::create_dir_all(movie.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&movie, b"fake video").await.unwrap();
        tokio::fs::write(tmp.path().join("ignore.txt"), b"not media")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        let scanned = db.scan_virtual_folder_items(folder.id).await.unwrap();
        assert_eq!(scanned, 1);

        let items = db.media_items().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Example Movie");
        assert_eq!(items[0].path, movie.to_string_lossy());
        assert_eq!(items[0].media_type, "Video");
        assert_eq!(items[0].collection_type.as_deref(), Some("movies"));
    }

    #[tokio::test]
    async fn rescan_marks_stale_media_items_without_deleting_playback_state() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Example Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);
        let item = db.media_items().await.unwrap().remove(0);
        db.upsert_playback_state(user.id, item.id, Some("source"), 42, false, false)
            .await
            .unwrap();

        tokio::fs::remove_file(&movie).await.unwrap();
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 0);

        assert!(db.media_items().await.unwrap().is_empty());
        assert!(
            db.playback_state_for_item(user.id, item.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn rescan_renamed_file_preserves_item_id_and_playback_state() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Example Movie.mp4");
        let renamed_movie = tmp.path().join("Renamed Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);
        let item = db.media_items().await.unwrap().remove(0);
        db.upsert_playback_state(user.id, item.id, Some("source"), 42, false, false)
            .await
            .unwrap();

        tokio::fs::rename(&movie, &renamed_movie).await.unwrap();
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);

        let items = db.media_items().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, item.id);
        assert_eq!(items[0].name, "Renamed Movie");
        assert_eq!(items[0].path, renamed_movie.to_string_lossy());
        assert_eq!(
            db.playback_state_for_item(user.id, item.id)
                .await
                .unwrap()
                .unwrap()
                .position_ticks,
            42
        );
    }

    #[tokio::test]
    async fn rescan_skips_missing_reconciliation_when_library_root_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Movies");
        tokio::fs::create_dir_all(&root).await.unwrap();
        let movie = root.join("Example Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![root.to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);
        tokio::fs::remove_dir_all(&root).await.unwrap();
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 0);

        let items = db.media_items().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, movie.to_string_lossy());
    }

    #[tokio::test]
    async fn upsert_admin_user_creates_separate_login_account() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.update_first_user("admin".to_string(), "admin-secret")
            .await
            .unwrap();

        let user = db
            .upsert_admin_user("jellyrin-e2e-admin", "e2e-secret")
            .await
            .unwrap();
        assert_eq!(user.name, "jellyrin-e2e-admin");
        assert!(user.is_administrator);
        assert!(!user.is_disabled);

        let (auth_user, _) = db
            .authenticate_user_by_name(
                "jellyrin-e2e-admin",
                "e2e-secret",
                "e2e-device",
                "E2E Device",
                "Jellyrin E2E",
                "dev",
            )
            .await
            .unwrap();
        assert_eq!(auth_user.id, user.id);

        let users = db.users().await.unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|user| user.name == "admin"));
        assert!(users.iter().any(|user| user.name == "jellyrin-e2e-admin"));
    }
}
