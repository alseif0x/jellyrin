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
use sqlx::{QueryBuilder, Sqlite, SqlitePool, sqlite::SqlitePoolOptions};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::process::Command;
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
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub access_token: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_activity_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct BackupManifest {
    pub path: String,
    pub server_version: String,
    pub backup_engine_version: String,
    pub options: Value,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default)]
struct MediaInfo {
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ActivePlaybackSession {
    pub session_id: String,
    pub user_id: Uuid,
    pub item: MediaItem,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub position_ticks: i64,
    pub is_paused: bool,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct UpsertActivePlaybackSession {
    pub session_id: String,
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub position_ticks: i64,
    pub is_paused: bool,
}

#[derive(Debug, Clone)]
pub struct UpsertPlaybackState {
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub position_ticks: i64,
    pub is_paused: bool,
    pub played: bool,
}

#[derive(Debug, Clone)]
pub struct TranscodeSession {
    pub play_session_id: String,
    pub user_id: Uuid,
    pub item: MediaItem,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub video_stream_index: Option<i64>,
    pub output_path: String,
    pub process_id: Option<i64>,
    pub status: String,
    pub progress_percent: Option<f64>,
    pub position_ticks: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct UpsertTranscodeSession {
    pub play_session_id: String,
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub video_stream_index: Option<i64>,
    pub output_path: String,
    pub process_id: Option<i64>,
    pub status: String,
    pub progress_percent: Option<f64>,
    pub position_ticks: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTranscodeSession {
    pub play_session_id: String,
    pub output_path: String,
    pub status: String,
    pub process_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTranscodeSession {
    pub play_session_id: String,
    pub output_path: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct ActivityLogEntry {
    pub id: i64,
    pub name: String,
    pub overview: Option<String>,
    pub short_overview: Option<String>,
    pub entry_type: String,
    pub severity: String,
    pub user_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLogSortField {
    Name,
    Overview,
    ShortOverview,
    Type,
    DateCreated,
    Username,
    LogSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone)]
pub struct ActivityLogFilter {
    pub has_user_id: Option<bool>,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub short_overview: Option<String>,
    pub entry_type: Option<String>,
    pub username: Option<String>,
    pub severity: Option<String>,
    pub min_date: Option<OffsetDateTime>,
    pub max_date: Option<OffsetDateTime>,
    pub sort: Vec<(ActivityLogSortField, SortDirection)>,
}

impl Default for ActivityLogFilter {
    fn default() -> Self {
        Self {
            has_user_id: None,
            name: None,
            overview: None,
            short_overview: None,
            entry_type: None,
            username: None,
            severity: None,
            min_date: None,
            max_date: None,
            sort: vec![(ActivityLogSortField::DateCreated, SortDirection::Descending)],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandingConfig {
    pub login_disclaimer: Option<String>,
    pub custom_css: Option<String>,
    pub splashscreen_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemConfigurationPayloads {
    pub content_types: Value,
    pub metadata_options: Value,
    pub path_substitutions: Value,
    pub plugin_repositories: Value,
    pub server_options: Value,
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
            SELECT ui_culture, metadata_country_code, preferred_metadata_language, dummy_chapter_duration, chapter_image_resolution, enable_remote_access
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
                dummy_chapter_duration: row.dummy_chapter_duration,
                chapter_image_resolution: row.chapter_image_resolution,
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
                id, ui_culture, metadata_country_code, preferred_metadata_language, dummy_chapter_duration, chapter_image_resolution, enable_remote_access, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                ui_culture = excluded.ui_culture,
                metadata_country_code = excluded.metadata_country_code,
                preferred_metadata_language = excluded.preferred_metadata_language,
                dummy_chapter_duration = excluded.dummy_chapter_duration,
                chapter_image_resolution = excluded.chapter_image_resolution,
                enable_remote_access = excluded.enable_remote_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.ui_culture)
        .bind(config.metadata_country_code)
        .bind(config.preferred_metadata_language)
        .bind(config.dummy_chapter_duration)
        .bind(config.chapter_image_resolution)
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

    pub async fn system_configuration_payloads(
        &self,
    ) -> anyhow::Result<SystemConfigurationPayloads> {
        let row = sqlx::query_as::<_, SystemConfigurationPayloadsRow>(
            r#"
            SELECT content_types_json, metadata_options_json, path_substitutions_json, plugin_repositories_json, server_options_json
            FROM system_configuration_payloads
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => Ok(SystemConfigurationPayloads::default()),
        }
    }

    pub async fn update_system_configuration_payloads(
        &self,
        payloads: SystemConfigurationPayloads,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO system_configuration_payloads (
                id, content_types_json, metadata_options_json, path_substitutions_json, plugin_repositories_json, server_options_json, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
                content_types_json = excluded.content_types_json,
                metadata_options_json = excluded.metadata_options_json,
                path_substitutions_json = excluded.path_substitutions_json,
                plugin_repositories_json = excluded.plugin_repositories_json,
                server_options_json = excluded.server_options_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(serde_json::to_string(&payloads.content_types)?)
        .bind(serde_json::to_string(&payloads.metadata_options)?)
        .bind(serde_json::to_string(&payloads.path_substitutions)?)
        .bind(serde_json::to_string(&payloads.plugin_repositories)?)
        .bind(serde_json::to_string(&payloads.server_options)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn named_configuration(&self, key: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query_as::<_, NamedConfigurationRow>(
            r#"
            SELECT payload_json
            FROM named_configurations
            WHERE key = ?1
            "#,
        )
        .bind(normalize_configuration_key(key))
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            serde_json::from_str(&row.payload_json).context("invalid named configuration")
        })
        .transpose()
    }

    pub async fn update_named_configuration(
        &self,
        key: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        let key = normalize_configuration_key(key);
        anyhow::ensure!(!key.is_empty(), "configuration key must not be empty");

        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO named_configurations (key, payload_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(serde_json::to_string(&payload)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn add_activity_log_entry(
        &self,
        name: &str,
        overview: Option<&str>,
        short_overview: Option<&str>,
        entry_type: &str,
        user_id: Option<Uuid>,
    ) -> anyhow::Result<ActivityLogEntry> {
        let trimmed_name = name.trim();
        let trimmed_type = entry_type.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "activity log name must not be empty"
        );
        anyhow::ensure!(
            !trimmed_type.is_empty(),
            "activity log type must not be empty"
        );

        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            INSERT INTO activity_log_entries (
                name, overview, short_overview, entry_type, severity, user_id, created_at
            )
            VALUES (?1, ?2, ?3, ?4, 'Information', ?5, ?6)
            "#,
        )
        .bind(trimmed_name)
        .bind(trimmed_optional_str(overview))
        .bind(trimmed_optional_str(short_overview))
        .bind(trimmed_type)
        .bind(user_id.map(|id| id.to_string()))
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.activity_log_entry_by_rowid(result.last_insert_rowid())
            .await
    }

    pub async fn activity_log_entries(
        &self,
        start_index: i64,
        limit: i64,
        filter: ActivityLogFilter,
    ) -> anyhow::Result<(Vec<ActivityLogEntry>, i64)> {
        let start_index = start_index.max(0);
        let limit = limit.clamp(0, 1000);
        let mut total_query =
            QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM activity_log_entries");
        push_activity_log_join_and_filters(&mut total_query, &filter)?;
        let total = total_query
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;

        let mut rows_query = QueryBuilder::<Sqlite>::new(
            "SELECT activity_log_entries.id, activity_log_entries.name, \
             activity_log_entries.overview, activity_log_entries.short_overview, \
             activity_log_entries.entry_type, activity_log_entries.severity, \
             activity_log_entries.user_id, activity_log_entries.created_at \
             FROM activity_log_entries",
        );
        push_activity_log_join_and_filters(&mut rows_query, &filter)?;
        push_activity_log_order_by(&mut rows_query, &filter.sort);
        rows_query.push(" LIMIT ");
        rows_query.push_bind(limit);
        rows_query.push(" OFFSET ");
        rows_query.push_bind(start_index);

        let rows = rows_query
            .build_query_as::<ActivityLogEntryRow>()
            .fetch_all(&self.pool)
            .await?;

        Ok((
            rows.into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total,
        ))
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

    pub async fn display_preferences(
        &self,
        user_id: Uuid,
        client: &str,
        id: &str,
    ) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query_as::<_, DisplayPreferencesRow>(
            r#"
            SELECT payload_json
            FROM display_preferences
            WHERE user_id = ?1 AND client = ?2 AND id = ?3
            "#,
        )
        .bind(user_id.to_string())
        .bind(client)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            serde_json::from_str(&row.payload_json).context("invalid display preferences")
        })
        .transpose()
    }

    pub async fn update_display_preferences(
        &self,
        user_id: Uuid,
        client: &str,
        id: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let payload_json = serde_json::to_string(&payload)?;
        sqlx::query(
            r#"
            INSERT INTO display_preferences (
                id, user_id, client, payload_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(id, user_id, client) DO UPDATE SET
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id)
        .bind(user_id.to_string())
        .bind(client)
        .bind(payload_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn user_configuration(&self, user_id: Uuid) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query_as::<_, UserConfigurationRow>(
            r#"
            SELECT payload_json
            FROM user_configurations
            WHERE user_id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| serde_json::from_str(&row.payload_json).context("invalid user configuration"))
            .transpose()
    }

    pub async fn update_user_configuration(
        &self,
        user_id: Uuid,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let payload_json = serde_json::to_string(&payload)?;
        sqlx::query(
            r#"
            INSERT INTO user_configurations (
                user_id, payload_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                payload_json = excluded.payload_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user_id.to_string())
        .bind(payload_json)
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

    pub async fn set_user_password(&self, user_id: Uuid, password: &str) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
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
        .bind(user_id.to_string())
        .bind(password_hash)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reset_user_password(&self, user_id: Uuid) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        sqlx::query("DELETE FROM user_passwords WHERE user_id = ?1")
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn user_has_password(&self, user_id: Uuid) -> anyhow::Result<bool> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_passwords WHERE user_id = ?1")
                .bind(user_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        Ok(count > 0)
    }

    pub async fn update_user_profile(
        &self,
        user_id: Uuid,
        name: &str,
        is_administrator: bool,
        is_disabled: bool,
    ) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        anyhow::ensure!(!trimmed_name.is_empty(), "user name must not be empty");
        self.user_by_id(user_id).await?;

        sqlx::query(
            r#"
            UPDATE users
            SET name = ?1, is_administrator = ?2, is_disabled = ?3, updated_at = ?4
            WHERE id = ?5
            "#,
        )
        .bind(trimmed_name)
        .bind(is_administrator)
        .bind(is_disabled)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .bind(user_id.to_string())
        .execute(&self.pool)
        .await?;

        self.user_by_id(user_id).await
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

    pub async fn verify_user_password(&self, user_id: Uuid, password: &str) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        let password_hash: String = sqlx::query_scalar(
            r#"
            SELECT password_hash
            FROM user_passwords
            WHERE user_id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("password is not configured")?;
        verify_password(password, &password_hash)
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

    pub async fn api_keys(&self) -> anyhow::Result<Vec<ApiKey>> {
        let rows = sqlx::query_as::<_, ApiKeyListRow>(
            r#"
            SELECT
                api_keys.access_token,
                api_keys.user_id,
                users.name AS user_name,
                api_keys.name,
                api_keys.created_at,
                api_keys.last_activity_at
            FROM api_keys
            INNER JOIN users ON users.id = api_keys.user_id
            ORDER BY api_keys.created_at DESC, api_keys.name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn revoke_api_key(&self, api_key: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM api_keys WHERE access_token = ?1")
            .bind(api_key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn backup_manifests(&self) -> anyhow::Result<Vec<BackupManifest>> {
        let rows = sqlx::query_as::<_, BackupManifestRow>(
            r#"
            SELECT path, server_version, backup_engine_version, options_json, created_at
            FROM backup_manifests
            ORDER BY created_at DESC, path
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn backup_manifest(&self, path: &str) -> anyhow::Result<Option<BackupManifest>> {
        let row = sqlx::query_as::<_, BackupManifestRow>(
            r#"
            SELECT path, server_version, backup_engine_version, options_json, created_at
            FROM backup_manifests
            WHERE path = ?1
            "#,
        )
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn create_backup_manifest(
        &self,
        server_version: &str,
        backup_engine_version: &str,
        options: Value,
    ) -> anyhow::Result<BackupManifest> {
        let now = OffsetDateTime::now_utc();
        let created_at = format_time(now)?;
        let path = format!("jellyrin-backup-{}.zip", Uuid::new_v4().simple());
        sqlx::query(
            r#"
            INSERT INTO backup_manifests (
                path, server_version, backup_engine_version, options_json, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(&path)
        .bind(server_version)
        .bind(backup_engine_version)
        .bind(serde_json::to_string(&options)?)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(BackupManifest {
            path,
            server_version: server_version.to_string(),
            backup_engine_version: backup_engine_version.to_string(),
            options,
            created_at: now,
        })
    }

    pub async fn revoke_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM devices WHERE access_token = ?1")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn revoke_user_tokens_except(
        &self,
        user_id: Uuid,
        keep_token: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM active_playback_sessions
            WHERE session_id IN (
                SELECT access_token FROM devices
                WHERE user_id = ?1 AND access_token != ?2
            )
            "#,
        )
        .bind(user_id.to_string())
        .bind(keep_token)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM devices WHERE user_id = ?1 AND access_token != ?2")
            .bind(user_id.to_string())
            .bind(keep_token)
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
                   devices.last_activity_at, devices.capabilities_json
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
                   devices.last_activity_at, devices.capabilities_json
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
                   devices.last_activity_at, devices.capabilities_json
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

    pub async fn update_device_capabilities(
        &self,
        access_token: &str,
        capabilities: Value,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let capabilities_json = serde_json::to_string(&capabilities)?;
        let result = sqlx::query(
            r#"
            UPDATE devices
            SET capabilities_json = ?1, last_activity_at = ?2
            WHERE access_token = ?3
            "#,
        )
        .bind(capabilities_json)
        .bind(now)
        .bind(access_token)
        .execute(&self.pool)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "device not found");
        Ok(())
    }

    pub async fn upsert_active_playback_session(
        &self,
        playback: UpsertActivePlaybackSession,
    ) -> anyhow::Result<()> {
        let trimmed_session_id = playback.session_id.trim();
        anyhow::ensure!(
            !trimmed_session_id.is_empty(),
            "session id must not be empty"
        );
        let existing_stream_indexes = if playback.audio_stream_index.is_none()
            || playback.subtitle_stream_index.is_none()
        {
            sqlx::query_as::<_, (String, Option<i64>, Option<i64>)>(
                r#"
                    SELECT item_id, audio_stream_index, subtitle_stream_index
                    FROM active_playback_sessions
                    WHERE session_id = ?1
                    "#,
            )
            .bind(trimmed_session_id)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|(item_id, audio_stream_index, subtitle_stream_index)| {
                (item_id == playback.item_id.to_string())
                    .then_some((audio_stream_index, subtitle_stream_index))
            })
        } else {
            None
        };
        let audio_stream_index = playback
            .audio_stream_index
            .or_else(|| existing_stream_indexes.and_then(|indexes| indexes.0));
        let subtitle_stream_index = playback
            .subtitle_stream_index
            .or_else(|| existing_stream_indexes.and_then(|indexes| indexes.1));
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO active_playback_sessions (
                session_id, user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                position_ticks, is_paused, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(session_id) DO UPDATE SET
                user_id = excluded.user_id,
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                audio_stream_index = excluded.audio_stream_index,
                subtitle_stream_index = excluded.subtitle_stream_index,
                position_ticks = excluded.position_ticks,
                is_paused = excluded.is_paused,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(trimmed_session_id)
        .bind(playback.user_id.to_string())
        .bind(playback.item_id.to_string())
        .bind(playback.media_source_id)
        .bind(audio_stream_index)
        .bind(subtitle_stream_index)
        .bind(playback.position_ticks)
        .bind(playback.is_paused)
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
                   active_playback_sessions.audio_stream_index,
                   active_playback_sessions.subtitle_stream_index,
                   active_playback_sessions.position_ticks,
                   active_playback_sessions.is_paused,
                   active_playback_sessions.updated_at AS playback_updated_at,
                   media_items.id,
                   media_items.virtual_folder_id,
                   media_items.name,
                   media_items.path,
                   media_items.media_type,
                   media_items.collection_type,
                   media_items.file_size,
                   media_items.runtime_ticks,
                   media_items.bitrate,
                   media_items.width,
                   media_items.height,
                   media_items.media_streams_json,
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

        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO transcode_sessions (
                play_session_id, user_id, item_id, media_source_id, audio_stream_index,
                subtitle_stream_index, video_stream_index, output_path, process_id, status,
                progress_percent, position_ticks, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
            ON CONFLICT(play_session_id) DO UPDATE SET
                user_id = excluded.user_id,
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                audio_stream_index = excluded.audio_stream_index,
                subtitle_stream_index = excluded.subtitle_stream_index,
                video_stream_index = excluded.video_stream_index,
                output_path = excluded.output_path,
                process_id = excluded.process_id,
                status = excluded.status,
                progress_percent = excluded.progress_percent,
                position_ticks = excluded.position_ticks,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&play_session_id)
        .bind(session.user_id.to_string())
        .bind(session.item_id.to_string())
        .bind(session.media_source_id)
        .bind(session.audio_stream_index)
        .bind(session.subtitle_stream_index)
        .bind(session.video_stream_index)
        .bind(&output_path)
        .bind(session.process_id)
        .bind(&status)
        .bind(session.progress_percent)
        .bind(session.position_ticks.max(0))
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.transcode_session_by_play_session_id(&play_session_id)
            .await?
            .context("transcode session missing after upsert")
    }

    pub async fn transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.transcode_sessions_with_statuses(&[]).await
    }

    pub async fn active_transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.transcode_sessions_with_statuses(&["starting", "running"])
            .await
    }

    pub async fn stale_transcode_sessions_on_startup(
        &self,
    ) -> anyhow::Result<Vec<StaleTranscodeSession>> {
        sqlx::query_as::<_, StaleTranscodeSessionRow>(
            r#"
            SELECT play_session_id, output_path, status, process_id
            FROM transcode_sessions
            WHERE status IN ('starting', 'running')
            ORDER BY updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
    }

    pub async fn terminal_transcode_sessions_older_than(
        &self,
        older_than: Duration,
    ) -> anyhow::Result<Vec<TerminalTranscodeSession>> {
        let cutoff = format_time(OffsetDateTime::now_utc() - older_than)?;
        sqlx::query_as::<_, TerminalTranscodeSessionRow>(
            r#"
            SELECT play_session_id, output_path, status
            FROM transcode_sessions
            WHERE status IN ('completed', 'failed', 'stopped')
              AND updated_at < ?1
            ORDER BY updated_at ASC
            "#,
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
    }

    async fn transcode_sessions_with_statuses(
        &self,
        statuses: &[&str],
    ) -> anyhow::Result<Vec<TranscodeSession>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT transcode_sessions.play_session_id,
                   transcode_sessions.user_id,
                   transcode_sessions.media_source_id,
                   transcode_sessions.audio_stream_index,
                   transcode_sessions.subtitle_stream_index,
                   transcode_sessions.video_stream_index,
                   transcode_sessions.output_path,
                   transcode_sessions.process_id,
                   transcode_sessions.status,
                   transcode_sessions.progress_percent,
                   transcode_sessions.position_ticks,
                   transcode_sessions.created_at AS transcode_created_at,
                   transcode_sessions.updated_at AS transcode_updated_at,
                   media_items.id,
                   media_items.virtual_folder_id,
                   media_items.name,
                   media_items.path,
                   media_items.media_type,
                   media_items.collection_type,
                   media_items.file_size,
                   media_items.runtime_ticks,
                   media_items.bitrate,
                   media_items.width,
                   media_items.height,
                   media_items.media_streams_json,
                   media_items.created_at,
                   media_items.updated_at
            FROM transcode_sessions
            INNER JOIN media_items ON media_items.id = transcode_sessions.item_id
            WHERE media_items.missing_since IS NULL
            "#,
        );
        if !statuses.is_empty() {
            builder.push(" AND transcode_sessions.status IN (");
            let mut separated = builder.separated(", ");
            for status in statuses {
                separated.push_bind(status);
            }
            separated.push_unseparated(")");
        }
        builder.push(" ORDER BY transcode_sessions.updated_at DESC");

        let rows = builder
            .build_query_as::<TranscodeSessionRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn transcode_session_by_play_session_id(
        &self,
        play_session_id: &str,
    ) -> anyhow::Result<Option<TranscodeSession>> {
        let row = sqlx::query_as::<_, TranscodeSessionRow>(
            r#"
            SELECT transcode_sessions.play_session_id,
                   transcode_sessions.user_id,
                   transcode_sessions.media_source_id,
                   transcode_sessions.audio_stream_index,
                   transcode_sessions.subtitle_stream_index,
                   transcode_sessions.video_stream_index,
                   transcode_sessions.output_path,
                   transcode_sessions.process_id,
                   transcode_sessions.status,
                   transcode_sessions.progress_percent,
                   transcode_sessions.position_ticks,
                   transcode_sessions.created_at AS transcode_created_at,
                   transcode_sessions.updated_at AS transcode_updated_at,
                   media_items.id,
                   media_items.virtual_folder_id,
                   media_items.name,
                   media_items.path,
                   media_items.media_type,
                   media_items.collection_type,
                   media_items.file_size,
                   media_items.runtime_ticks,
                   media_items.bitrate,
                   media_items.width,
                   media_items.height,
                   media_items.media_streams_json,
                   media_items.created_at,
                   media_items.updated_at
            FROM transcode_sessions
            INNER JOIN media_items ON media_items.id = transcode_sessions.item_id
            WHERE transcode_sessions.play_session_id = ?1
              AND media_items.missing_since IS NULL
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
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE transcode_sessions
            SET status = ?1, updated_at = ?2
            WHERE play_session_id = ?3
            "#,
        )
        .bind(status)
        .bind(now)
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
        let play_session_id = play_session_id.trim();
        anyhow::ensure!(
            !play_session_id.is_empty(),
            "play session id must not be empty"
        );
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE transcode_sessions
            SET progress_percent = COALESCE(?1, progress_percent),
                position_ticks = ?2,
                updated_at = ?3
            WHERE play_session_id = ?4
            "#,
        )
        .bind(progress_percent)
        .bind(position_ticks.max(0))
        .bind(now)
        .bind(play_session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
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
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams_json,
                   created_at, updated_at
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
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams_json,
                   created_at, updated_at
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

    pub async fn update_media_item_media_info(
        &self,
        item_id: Uuid,
        runtime_ticks: Option<i64>,
        bitrate: Option<i64>,
        width: Option<i32>,
        height: Option<i32>,
        media_streams: Vec<Value>,
    ) -> anyhow::Result<()> {
        let media_streams_json = serde_json::to_string(&media_streams)?;
        sqlx::query(
            r#"
            UPDATE media_items
            SET runtime_ticks = ?2, bitrate = ?3, width = ?4, height = ?5, media_streams_json = ?6
            WHERE id = ?1
            "#,
        )
        .bind(item_id.to_string())
        .bind(runtime_ticks)
        .bind(bitrate)
        .bind(width)
        .bind(height)
        .bind(media_streams_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_playback_state(&self, playback: UpsertPlaybackState) -> anyhow::Result<()> {
        let existing_stream_indexes =
            if playback.audio_stream_index.is_none() || playback.subtitle_stream_index.is_none() {
                sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
                    r#"
                    SELECT audio_stream_index, subtitle_stream_index
                    FROM playback_states
                    WHERE user_id = ?1 AND item_id = ?2
                    "#,
                )
                .bind(playback.user_id.to_string())
                .bind(playback.item_id.to_string())
                .fetch_optional(&self.pool)
                .await?
            } else {
                None
            };
        let audio_stream_index = playback
            .audio_stream_index
            .or_else(|| existing_stream_indexes.and_then(|indexes| indexes.0));
        let subtitle_stream_index = playback
            .subtitle_stream_index
            .or_else(|| existing_stream_indexes.and_then(|indexes| indexes.1));
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                position_ticks, is_paused, played, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                media_source_id = excluded.media_source_id,
                audio_stream_index = excluded.audio_stream_index,
                subtitle_stream_index = excluded.subtitle_stream_index,
                position_ticks = excluded.position_ticks,
                is_paused = excluded.is_paused,
                played = excluded.played,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(playback.user_id.to_string())
        .bind(playback.item_id.to_string())
        .bind(playback.media_source_id)
        .bind(audio_stream_index)
        .bind(subtitle_stream_index)
        .bind(playback.position_ticks.max(0))
        .bind(playback.is_paused)
        .bind(playback.played)
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
            SELECT user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                   position_ticks, is_paused, played, updated_at
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
                media_items.media_type, media_items.collection_type, media_items.file_size,
                media_items.runtime_ticks, media_items.bitrate, media_items.width, media_items.height,
                media_items.media_streams_json, media_items.created_at, media_items.updated_at, playback_states.user_id, playback_states.item_id,
                playback_states.media_source_id, playback_states.audio_stream_index,
                playback_states.subtitle_stream_index, playback_states.position_ticks,
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
        let media_info = probe_media_info(Path::new(&path), media_type).await;
        let media_streams_json = serde_json::to_string(&media_info.media_streams)?;
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
                    file_size = ?6, modified_at = ?7,
                    runtime_ticks = ?8, bitrate = ?9, width = ?10, height = ?11,
                    media_streams_json = ?12
                WHERE id = ?13
                "#,
            )
            .bind(name)
            .bind(path)
            .bind(media_type)
            .bind(&folder.collection_type)
            .bind(&now)
            .bind(file_size)
            .bind(modified_at)
            .bind(media_info.runtime_ticks)
            .bind(media_info.bitrate)
            .bind(media_info.width)
            .bind(media_info.height)
            .bind(&media_streams_json)
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
                created_at, updated_at, last_seen_at, missing_since, file_size, modified_at,
                runtime_ticks, bitrate, width, height, media_streams_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7, NULL, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(path) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                name = excluded.name,
                media_type = excluded.media_type,
                collection_type = excluded.collection_type,
                updated_at = excluded.updated_at,
                last_seen_at = excluded.last_seen_at,
                missing_since = NULL,
                file_size = excluded.file_size,
                modified_at = excluded.modified_at,
                runtime_ticks = excluded.runtime_ticks,
                bitrate = excluded.bitrate,
                width = excluded.width,
                height = excluded.height,
                media_streams_json = excluded.media_streams_json
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
        .bind(media_info.runtime_ticks)
        .bind(media_info.bitrate)
        .bind(media_info.width)
        .bind(media_info.height)
        .bind(media_streams_json)
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
            dummy_chapter_duration: 0,
            chapter_image_resolution: "MatchSource".to_string(),
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

    async fn activity_log_entry_by_rowid(&self, rowid: i64) -> anyhow::Result<ActivityLogEntry> {
        let row = sqlx::query_as::<_, ActivityLogEntryRow>(
            r#"
            SELECT id, name, overview, short_overview, entry_type, severity, user_id, created_at
            FROM activity_log_entries
            WHERE id = ?1
            "#,
        )
        .bind(rowid)
        .fetch_one(&self.pool)
        .await?;

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

fn push_activity_log_join_and_filters(
    query: &mut QueryBuilder<'_, Sqlite>,
    filter: &ActivityLogFilter,
) -> anyhow::Result<()> {
    if filter
        .username
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || filter
            .sort
            .iter()
            .any(|(field, _)| *field == ActivityLogSortField::Username)
    {
        query.push(" LEFT JOIN users ON users.id = activity_log_entries.user_id");
    }

    let mut first_filter = true;
    push_activity_log_filter_clause(
        query,
        &mut first_filter,
        "activity_log_entries.name",
        &filter.name,
    );
    push_activity_log_filter_clause(
        query,
        &mut first_filter,
        "activity_log_entries.overview",
        &filter.overview,
    );
    push_activity_log_filter_clause(
        query,
        &mut first_filter,
        "activity_log_entries.short_overview",
        &filter.short_overview,
    );
    push_activity_log_filter_clause(
        query,
        &mut first_filter,
        "activity_log_entries.entry_type",
        &filter.entry_type,
    );
    push_activity_log_filter_clause(query, &mut first_filter, "users.name", &filter.username);
    push_activity_log_exact_clause(
        query,
        &mut first_filter,
        "activity_log_entries.severity",
        &filter.severity,
    );

    if let Some(has_user_id) = filter.has_user_id {
        push_activity_log_where(query, &mut first_filter);
        if has_user_id {
            query.push("activity_log_entries.user_id IS NOT NULL");
        } else {
            query.push("activity_log_entries.user_id IS NULL");
        }
    }

    if let Some(min_date) = filter.min_date {
        push_activity_log_where(query, &mut first_filter);
        query.push("activity_log_entries.created_at >= ");
        query.push_bind(format_time(min_date)?);
    }

    if let Some(max_date) = filter.max_date {
        push_activity_log_where(query, &mut first_filter);
        query.push("activity_log_entries.created_at <= ");
        query.push_bind(format_time(max_date)?);
    }

    Ok(())
}

fn push_activity_log_filter_clause(
    query: &mut QueryBuilder<'_, Sqlite>,
    first_filter: &mut bool,
    column: &'static str,
    value: &Option<String>,
) {
    let Some(value) = trimmed_filter_value(value) else {
        return;
    };
    push_activity_log_where(query, first_filter);
    query.push(column);
    query.push(" LIKE ");
    query.push_bind(format!("%{value}%"));
}

fn push_activity_log_exact_clause(
    query: &mut QueryBuilder<'_, Sqlite>,
    first_filter: &mut bool,
    column: &'static str,
    value: &Option<String>,
) {
    let Some(value) = trimmed_filter_value(value) else {
        return;
    };
    push_activity_log_where(query, first_filter);
    query.push(column);
    query.push(" = ");
    query.push_bind(value);
}

fn push_activity_log_where(query: &mut QueryBuilder<'_, Sqlite>, first_filter: &mut bool) {
    if *first_filter {
        query.push(" WHERE ");
        *first_filter = false;
    } else {
        query.push(" AND ");
    }
}

fn push_activity_log_order_by(
    query: &mut QueryBuilder<'_, Sqlite>,
    sort: &[(ActivityLogSortField, SortDirection)],
) {
    query.push(" ORDER BY ");
    let fallback = [(ActivityLogSortField::DateCreated, SortDirection::Descending)];
    let requested_sort = if sort.is_empty() { &fallback[..] } else { sort };
    let order_parts = requested_sort
        .iter()
        .copied()
        .take(4)
        .map(|(field, direction)| {
            let direction = match direction {
                SortDirection::Ascending => "ASC",
                SortDirection::Descending => "DESC",
            };
            format!("{} {}", activity_log_sort_column(field), direction)
        })
        .chain(std::iter::once("activity_log_entries.id DESC".to_string()))
        .collect::<Vec<_>>();

    query.push(order_parts.join(", "));
}

fn activity_log_sort_column(field: ActivityLogSortField) -> &'static str {
    match field {
        ActivityLogSortField::Name => "activity_log_entries.name COLLATE NOCASE",
        ActivityLogSortField::Overview => "activity_log_entries.overview COLLATE NOCASE",
        ActivityLogSortField::ShortOverview => "activity_log_entries.short_overview COLLATE NOCASE",
        ActivityLogSortField::Type => "activity_log_entries.entry_type COLLATE NOCASE",
        ActivityLogSortField::DateCreated => "activity_log_entries.created_at",
        ActivityLogSortField::Username => "users.name COLLATE NOCASE",
        ActivityLogSortField::LogSeverity => "activity_log_entries.severity COLLATE NOCASE",
    }
}

fn trimmed_filter_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(sqlx::FromRow)]
struct StartupConfigRow {
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
    dummy_chapter_duration: i64,
    chapter_image_resolution: String,
    enable_remote_access: bool,
}

#[derive(sqlx::FromRow)]
struct BrandingConfigRow {
    login_disclaimer: Option<String>,
    custom_css: Option<String>,
    splashscreen_enabled: bool,
}

#[derive(sqlx::FromRow)]
struct DisplayPreferencesRow {
    payload_json: String,
}

#[derive(sqlx::FromRow)]
struct UserConfigurationRow {
    payload_json: String,
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

impl Default for SystemConfigurationPayloads {
    fn default() -> Self {
        Self {
            content_types: Value::Array(Vec::new()),
            metadata_options: Value::Array(Vec::new()),
            path_substitutions: Value::Array(Vec::new()),
            plugin_repositories: Value::Array(Vec::new()),
            server_options: Value::Object(Default::default()),
        }
    }
}

impl TryFrom<SystemConfigurationPayloadsRow> for SystemConfigurationPayloads {
    type Error = anyhow::Error;

    fn try_from(row: SystemConfigurationPayloadsRow) -> Result<Self, Self::Error> {
        Ok(Self {
            content_types: array_payload(&row.content_types_json)?,
            metadata_options: array_payload(&row.metadata_options_json)?,
            path_substitutions: array_payload(&row.path_substitutions_json)?,
            plugin_repositories: array_payload(&row.plugin_repositories_json)?,
            server_options: object_payload(&row.server_options_json)?,
        })
    }
}

fn array_payload(raw: &str) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw).context("invalid system configuration payload")?;
    match value {
        Value::Array(_) => Ok(value),
        _ => Ok(Value::Array(Vec::new())),
    }
}

fn object_payload(raw: &str) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw).context("invalid system configuration payload")?;
    match value {
        Value::Object(_) => Ok(value),
        _ => Ok(Value::Object(Default::default())),
    }
}

fn normalize_configuration_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
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
    capabilities_json: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SystemConfigurationPayloadsRow {
    content_types_json: String,
    metadata_options_json: String,
    path_substitutions_json: String,
    plugin_repositories_json: String,
    server_options_json: String,
}

#[derive(sqlx::FromRow)]
struct NamedConfigurationRow {
    payload_json: String,
}

#[derive(sqlx::FromRow)]
struct ApiKeyRow {
    access_token: String,
    user_id: String,
    name: String,
}

#[derive(sqlx::FromRow)]
struct ApiKeyListRow {
    access_token: String,
    user_id: String,
    user_name: String,
    name: String,
    created_at: String,
    last_activity_at: String,
}

#[derive(sqlx::FromRow)]
struct BackupManifestRow {
    path: String,
    server_version: String,
    backup_engine_version: String,
    options_json: String,
    created_at: String,
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
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams_json: String,
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
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams_json: String,
    created_at: String,
    updated_at: String,
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
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
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
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
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    position_ticks: i64,
    is_paused: bool,
    playback_updated_at: String,
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams_json: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct TranscodeSessionRow {
    play_session_id: String,
    user_id: String,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    video_stream_index: Option<i64>,
    output_path: String,
    process_id: Option<i64>,
    status: String,
    progress_percent: Option<f64>,
    position_ticks: i64,
    transcode_created_at: String,
    transcode_updated_at: String,
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams_json: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct StaleTranscodeSessionRow {
    play_session_id: String,
    output_path: String,
    status: String,
    process_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct TerminalTranscodeSessionRow {
    play_session_id: String,
    output_path: String,
    status: String,
}

#[derive(sqlx::FromRow)]
struct ActivityLogEntryRow {
    id: i64,
    name: String,
    overview: Option<String>,
    short_overview: Option<String>,
    entry_type: String,
    severity: String,
    user_id: Option<String>,
    created_at: String,
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
            file_size: row.file_size,
            runtime_ticks: row.runtime_ticks,
            bitrate: row.bitrate,
            width: row.width,
            height: row.height,
            media_streams: parse_media_streams_json(&row.media_streams_json)?,
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
            file_size: row.file_size,
            runtime_ticks: row.runtime_ticks,
            bitrate: row.bitrate,
            width: row.width,
            height: row.height,
            media_streams: parse_media_streams_json(&row.media_streams_json)?,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        };
        let playback = PlaybackState {
            user_id: Uuid::parse_str(&row.user_id)
                .context("invalid playback user id in database")?,
            item_id: Uuid::parse_str(&row.item_id)
                .context("invalid playback item id in database")?,
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
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
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
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
                file_size: row.file_size,
                runtime_ticks: row.runtime_ticks,
                bitrate: row.bitrate,
                width: row.width,
                height: row.height,
                media_streams: parse_media_streams_json(&row.media_streams_json)?,
                created_at: parse_time(&row.created_at)?,
                updated_at: parse_time(&row.updated_at)?,
            },
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            position_ticks: row.position_ticks,
            is_paused: row.is_paused,
            updated_at: parse_time(&row.playback_updated_at)?,
        })
    }
}

impl TryFrom<TranscodeSessionRow> for TranscodeSession {
    type Error = anyhow::Error;

    fn try_from(row: TranscodeSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            play_session_id: row.play_session_id,
            user_id: Uuid::parse_str(&row.user_id).context("invalid transcode session user id")?,
            item: MediaItem {
                id: Uuid::parse_str(&row.id).context("invalid transcode session item id")?,
                virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                    .context("invalid transcode session virtual folder id")?,
                name: row.name,
                path: row.path,
                media_type: row.media_type,
                collection_type: row.collection_type,
                file_size: row.file_size,
                runtime_ticks: row.runtime_ticks,
                bitrate: row.bitrate,
                width: row.width,
                height: row.height,
                media_streams: parse_media_streams_json(&row.media_streams_json)?,
                created_at: parse_time(&row.created_at)?,
                updated_at: parse_time(&row.updated_at)?,
            },
            media_source_id: row.media_source_id,
            audio_stream_index: row.audio_stream_index,
            subtitle_stream_index: row.subtitle_stream_index,
            video_stream_index: row.video_stream_index,
            output_path: row.output_path,
            process_id: row.process_id,
            status: row.status,
            progress_percent: row.progress_percent,
            position_ticks: row.position_ticks,
            created_at: parse_time(&row.transcode_created_at)?,
            updated_at: parse_time(&row.transcode_updated_at)?,
        })
    }
}

impl TryFrom<StaleTranscodeSessionRow> for StaleTranscodeSession {
    type Error = anyhow::Error;

    fn try_from(row: StaleTranscodeSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            play_session_id: row.play_session_id,
            output_path: row.output_path,
            status: row.status,
            process_id: row.process_id,
        })
    }
}

impl TryFrom<TerminalTranscodeSessionRow> for TerminalTranscodeSession {
    type Error = anyhow::Error;

    fn try_from(row: TerminalTranscodeSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            play_session_id: row.play_session_id,
            output_path: row.output_path,
            status: row.status,
        })
    }
}

impl TryFrom<ActivityLogEntryRow> for ActivityLogEntry {
    type Error = anyhow::Error;

    fn try_from(row: ActivityLogEntryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            overview: row.overview,
            short_overview: row.short_overview,
            entry_type: row.entry_type,
            severity: row.severity,
            user_id: row.user_id.as_deref().map(Uuid::parse_str).transpose()?,
            created_at: parse_time(&row.created_at)?,
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
            capabilities: row
                .capabilities_json
                .map(|value| serde_json::from_str(&value).context("invalid device capabilities"))
                .transpose()?,
        })
    }
}

impl TryFrom<ApiKeyListRow> for ApiKey {
    type Error = anyhow::Error;

    fn try_from(row: ApiKeyListRow) -> Result<Self, Self::Error> {
        Ok(Self {
            access_token: row.access_token,
            user_id: Uuid::parse_str(&row.user_id).context("invalid api key user id")?,
            user_name: row.user_name,
            name: row.name,
            created_at: parse_time(&row.created_at)?,
            last_activity_at: parse_time(&row.last_activity_at)?,
        })
    }
}

impl TryFrom<BackupManifestRow> for BackupManifest {
    type Error = anyhow::Error;

    fn try_from(row: BackupManifestRow) -> Result<Self, Self::Error> {
        Ok(Self {
            path: row.path,
            server_version: row.server_version,
            backup_engine_version: row.backup_engine_version,
            options: serde_json::from_str(&row.options_json).context("invalid backup options")?,
            created_at: parse_time(&row.created_at)?,
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

fn parse_media_streams_json(value: &str) -> anyhow::Result<Vec<Value>> {
    serde_json::from_str(value).context("invalid media streams json in database")
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

async fn probe_media_info(path: &Path, media_type: &str) -> MediaInfo {
    if !matches!(media_type, "Video" | "Audio") {
        return MediaInfo::default();
    }

    let Ok(output) = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .output()
        .await
    else {
        return MediaInfo::default();
    };
    if !output.status.success() {
        return MediaInfo::default();
    }

    serde_json::from_slice::<Value>(&output.stdout)
        .map(|value| parse_ffprobe_media_info(&value))
        .unwrap_or_default()
}

fn parse_ffprobe_media_info(value: &Value) -> MediaInfo {
    let format = value.get("format");
    let runtime_ticks = format
        .and_then(|format| format.get("duration"))
        .and_then(json_number_or_string_f64)
        .map(seconds_to_ticks);
    let format_bitrate = format
        .and_then(|format| format.get("bit_rate"))
        .and_then(json_number_or_string_i64);

    let streams = value
        .get("streams")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let video_stream = streams.iter().find(|stream| {
        stream
            .get("codec_type")
            .and_then(Value::as_str)
            .is_some_and(|codec_type| codec_type.eq_ignore_ascii_case("video"))
    });
    let stream_bitrate = streams
        .iter()
        .filter_map(|stream| stream.get("bit_rate").and_then(json_number_or_string_i64))
        .max();
    let media_streams = streams
        .iter()
        .filter_map(ffprobe_stream_to_media_stream)
        .collect::<Vec<_>>();

    MediaInfo {
        runtime_ticks,
        bitrate: format_bitrate.or(stream_bitrate),
        width: video_stream
            .and_then(|stream| stream.get("width"))
            .and_then(json_number_or_string_i64)
            .and_then(|value| i32::try_from(value).ok()),
        height: video_stream
            .and_then(|stream| stream.get("height"))
            .and_then(json_number_or_string_i64)
            .and_then(|value| i32::try_from(value).ok()),
        media_streams,
    }
}

fn ffprobe_stream_to_media_stream(stream: &Value) -> Option<Value> {
    let codec_type = stream.get("codec_type")?.as_str()?;
    let index = stream.get("index").and_then(json_number_or_string_i64)?;
    let codec = stream
        .get("codec_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let language = stream
        .get("tags")
        .and_then(|tags| tags.get("language"))
        .and_then(Value::as_str);
    let bit_rate = stream.get("bit_rate").and_then(json_number_or_string_i64);
    let is_default = stream
        .get("disposition")
        .and_then(|disposition| disposition.get("default"))
        .and_then(json_number_or_string_i64)
        .is_some_and(|value| value != 0);

    match codec_type {
        "video" => Some(serde_json::json!({
            "Codec": codec,
            "Language": language,
            "DisplayTitle": "Video",
            "IsInterlaced": false,
            "BitRate": bit_rate,
            "BitDepth": stream.get("bits_per_raw_sample").and_then(json_number_or_string_i64),
            "RefFrames": null,
            "IsDefault": is_default,
            "IsForced": false,
            "Height": stream.get("height").and_then(json_number_or_string_i64),
            "Width": stream.get("width").and_then(json_number_or_string_i64),
            "AverageFrameRate": parse_rational(stream.get("avg_frame_rate").and_then(Value::as_str)),
            "RealFrameRate": parse_rational(stream.get("r_frame_rate").and_then(Value::as_str)),
            "Profile": stream.get("profile").and_then(Value::as_str),
            "Type": "Video",
            "AspectRatio": display_aspect_ratio(stream),
            "Index": index,
            "IsExternal": false,
            "IsTextSubtitleStream": false,
            "SupportsExternalStream": false,
            "Path": null,
            "PixelFormat": stream.get("pix_fmt").and_then(Value::as_str),
            "Level": stream.get("level").and_then(json_number_or_string_i64),
            "IsAnamorphic": null
        })),
        "audio" => Some(serde_json::json!({
            "Codec": codec,
            "Language": language,
            "DisplayTitle": "Audio",
            "IsInterlaced": false,
            "BitRate": bit_rate,
            "BitDepth": stream.get("bits_per_sample").and_then(json_number_or_string_i64),
            "Channels": stream.get("channels").and_then(json_number_or_string_i64),
            "SampleRate": stream.get("sample_rate").and_then(json_number_or_string_i64),
            "IsDefault": is_default,
            "IsForced": false,
            "Type": "Audio",
            "Index": index,
            "IsExternal": false,
            "Path": null
        })),
        "subtitle" => Some(serde_json::json!({
            "Codec": codec,
            "Language": language,
            "DisplayTitle": "Subtitle",
            "IsDefault": is_default,
            "IsForced": false,
            "Type": "Subtitle",
            "Index": index,
            "IsExternal": false,
            "Path": null,
            "IsTextSubtitleStream": true,
            "SupportsExternalStream": false
        })),
        _ => None,
    }
}

fn parse_rational(value: Option<&str>) -> Option<f64> {
    let value = value?;
    if let Some((left, right)) = value.split_once('/') {
        let numerator = left.parse::<f64>().ok()?;
        let denominator = right.parse::<f64>().ok()?;
        if denominator == 0.0 {
            None
        } else {
            Some(numerator / denominator)
        }
    } else {
        value.parse::<f64>().ok()
    }
}

fn display_aspect_ratio(stream: &Value) -> Option<String> {
    stream
        .get("display_aspect_ratio")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            let width = stream.get("width").and_then(json_number_or_string_i64)?;
            let height = stream.get("height").and_then(json_number_or_string_i64)?;
            if width > 0 && height > 0 {
                Some(format!("{width}:{height}"))
            } else {
                None
            }
        })
}

fn json_number_or_string_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
}

fn json_number_or_string_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
}

fn seconds_to_ticks(seconds: f64) -> i64 {
    (seconds.max(0.0) * 10_000_000.0).round() as i64
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

fn trimmed_optional_str(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        ActivityLogFilter, ActivityLogSortField, Database, SortDirection,
        SystemConfigurationPayloads, UpsertActivePlaybackSession, UpsertPlaybackState,
        UpsertTranscodeSession, parse_ffprobe_media_info,
    };
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
    async fn system_configuration_payloads_round_trip_arrays() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let defaults = db.system_configuration_payloads().await.unwrap();
        assert_eq!(defaults.content_types, json!([]));
        assert_eq!(defaults.metadata_options, json!([]));
        assert_eq!(defaults.path_substitutions, json!([]));
        assert_eq!(defaults.plugin_repositories, json!([]));
        assert_eq!(defaults.server_options, json!({}));

        db.update_system_configuration_payloads(SystemConfigurationPayloads {
            content_types: json!([{ "Name": "Movies", "Value": "movies" }]),
            metadata_options: json!([{ "ItemType": "Movie" }]),
            path_substitutions: json!([{ "From": "/mnt/a", "To": "/mnt/b" }]),
            plugin_repositories: json!([{ "Name": "Example", "Url": "https://example.invalid" }]),
            server_options: json!({ "RemoteClientBitrateLimit": 1234 }),
        })
        .await
        .unwrap();
        let stored = db.system_configuration_payloads().await.unwrap();
        assert_eq!(
            stored.content_types,
            json!([{ "Name": "Movies", "Value": "movies" }])
        );
        assert_eq!(stored.metadata_options, json!([{ "ItemType": "Movie" }]));
        assert_eq!(
            stored.path_substitutions,
            json!([{ "From": "/mnt/a", "To": "/mnt/b" }])
        );
        assert_eq!(
            stored.plugin_repositories,
            json!([{ "Name": "Example", "Url": "https://example.invalid" }])
        );
        assert_eq!(
            stored.server_options,
            json!({ "RemoteClientBitrateLimit": 1234 })
        );

        db.update_system_configuration_payloads(SystemConfigurationPayloads {
            content_types: json!({ "Name": "Movies" }),
            metadata_options: json!("invalid"),
            path_substitutions: json!(null),
            plugin_repositories: json!([{ "Name": "Kept" }]),
            server_options: json!("invalid"),
        })
        .await
        .unwrap();
        let sanitized = db.system_configuration_payloads().await.unwrap();
        assert_eq!(sanitized.content_types, json!([]));
        assert_eq!(sanitized.metadata_options, json!([]));
        assert_eq!(sanitized.path_substitutions, json!([]));
        assert_eq!(sanitized.plugin_repositories, json!([{ "Name": "Kept" }]));
        assert_eq!(sanitized.server_options, json!({}));
    }

    #[tokio::test]
    async fn named_configurations_round_trip_json_by_key() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        assert!(db.named_configuration("network").await.unwrap().is_none());

        db.update_named_configuration(
            " Network ",
            json!({
                "InternalHttpPort": 8097,
                "EnableIPv4": true,
                "LocalNetworkSubnets": ["192.168.1.0/24"]
            }),
        )
        .await
        .unwrap();

        let stored = db.named_configuration("network").await.unwrap().unwrap();
        assert_eq!(stored["InternalHttpPort"], 8097);
        assert_eq!(stored["EnableIPv4"], true);
        assert_eq!(stored["LocalNetworkSubnets"], json!(["192.168.1.0/24"]));

        db.update_named_configuration("network", json!({ "InternalHttpPort": 8098 }))
            .await
            .unwrap();
        let updated = db.named_configuration("NETWORK").await.unwrap().unwrap();
        assert_eq!(updated, json!({ "InternalHttpPort": 8098 }));
    }

    #[tokio::test]
    async fn activity_log_entries_page_newest_first() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();

        let first = db
            .add_activity_log_entry(
                "First event",
                Some("First overview"),
                None,
                "System",
                Some(user.id),
            )
            .await
            .unwrap();
        let second = db
            .add_activity_log_entry(
                "Second event",
                Some("Second overview"),
                Some("Second short overview"),
                "Library",
                None,
            )
            .await
            .unwrap();

        assert!(second.id > first.id);
        let (entries, total) = db
            .activity_log_entries(0, 1, ActivityLogFilter::default())
            .await
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Second event");
        assert_eq!(entries[0].entry_type, "Library");
        assert_eq!(entries[0].severity, "Information");
        assert_eq!(
            entries[0].short_overview.as_deref(),
            Some("Second short overview")
        );
        assert_eq!(entries[0].user_id, None);

        let (entries, total) = db
            .activity_log_entries(1, 10, ActivityLogFilter::default())
            .await
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "First event");
        assert_eq!(entries[0].user_id, Some(user.id));
    }

    #[tokio::test]
    async fn activity_log_entries_filter_and_sort() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();

        db.add_activity_log_entry(
            "Alpha event",
            Some("First overview"),
            Some("Alpha short"),
            "System",
            Some(user.id),
        )
        .await
        .unwrap();
        db.add_activity_log_entry(
            "Beta event",
            Some("Second overview"),
            Some("Beta short"),
            "Library",
            None,
        )
        .await
        .unwrap();

        let (entries, total) = db
            .activity_log_entries(
                0,
                10,
                ActivityLogFilter {
                    has_user_id: Some(true),
                    username: Some("roo".to_string()),
                    sort: vec![(ActivityLogSortField::Name, SortDirection::Ascending)],
                    ..ActivityLogFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(entries[0].name, "Alpha event");
        assert_eq!(entries[0].user_id, Some(user.id));

        let (entries, total) = db
            .activity_log_entries(
                0,
                10,
                ActivityLogFilter {
                    has_user_id: Some(false),
                    entry_type: Some("lib".to_string()),
                    sort: vec![(ActivityLogSortField::Name, SortDirection::Descending)],
                    ..ActivityLogFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(entries[0].name, "Beta event");
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
        let keys = db.api_keys().await.unwrap();

        assert_eq!(api_user.id, user.id);
        assert_eq!(token.access_token, api_key);
        assert_eq!(token.client, "API Key");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].access_token, api_key);
        assert_eq!(keys[0].user_id, user.id);
        assert_eq!(keys[0].user_name, "root");
        assert_eq!(keys[0].name, "qa");

        assert!(db.revoke_api_key(&api_key).await.unwrap());
        assert!(!db.revoke_api_key(&api_key).await.unwrap());
        assert!(db.user_by_api_key(&api_key).await.is_err());
    }

    #[tokio::test]
    async fn backup_manifests_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let defaults = db.backup_manifests().await.unwrap();
        assert!(defaults.is_empty());

        let created = db
            .create_backup_manifest(
                "12.0.0",
                "1",
                json!({
                    "Metadata": true,
                    "Subtitles": false,
                    "Trickplay": true,
                    "Database": true
                }),
            )
            .await
            .unwrap();
        assert!(created.path.starts_with("jellyrin-backup-"));
        assert_eq!(created.server_version, "12.0.0");
        assert_eq!(created.backup_engine_version, "1");
        assert_eq!(created.options["Database"], true);

        let manifests = db.backup_manifests().await.unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].path, created.path);

        let manifest = db.backup_manifest(&created.path).await.unwrap().unwrap();
        assert_eq!(manifest.path, created.path);
        assert_eq!(manifest.options["Metadata"], true);
        assert!(db.backup_manifest("missing.zip").await.unwrap().is_none());
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

        db.upsert_active_playback_session(UpsertActivePlaybackSession {
            session_id: token.access_token.clone(),
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.to_string()),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(-1),
            position_ticks: 42,
            is_paused: false,
        })
        .await
        .unwrap();
        let sessions = db.active_playback_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, token.access_token);
        assert_eq!(sessions[0].item.id, item.id);
        assert_eq!(sessions[0].audio_stream_index, Some(1));
        assert_eq!(sessions[0].subtitle_stream_index, Some(-1));
        assert_eq!(sessions[0].position_ticks, 42);

        db.upsert_active_playback_session(UpsertActivePlaybackSession {
            session_id: token.access_token.clone(),
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.to_string()),
            audio_stream_index: None,
            subtitle_stream_index: None,
            position_ticks: 84,
            is_paused: true,
        })
        .await
        .unwrap();
        let sessions = db.active_playback_sessions().await.unwrap();
        assert_eq!(sessions[0].audio_stream_index, Some(1));
        assert_eq!(sessions[0].subtitle_stream_index, Some(-1));
        assert_eq!(sessions[0].position_ticks, 84);
        assert!(sessions[0].is_paused);

        db.clear_active_playback_session(&token.access_token)
            .await
            .unwrap();
        assert!(db.active_playback_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn transcode_sessions_track_active_status_and_media_item() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Transcoded Movie.mkv");
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

        let session = db
            .upsert_transcode_session(UpsertTranscodeSession {
                play_session_id: "play-session-1".to_string(),
                user_id: user.id,
                item_id: item.id,
                media_source_id: Some(item.id.simple().to_string()),
                audio_stream_index: Some(1),
                subtitle_stream_index: Some(-1),
                video_stream_index: Some(0),
                output_path: "/tmp/jellyrin-transcodes/play-session-1/main.m3u8".to_string(),
                process_id: Some(123),
                status: "RUNNING".to_string(),
                progress_percent: Some(12.5),
                position_ticks: 456,
            })
            .await
            .unwrap();

        assert_eq!(session.play_session_id, "play-session-1");
        assert_eq!(session.user_id, user.id);
        assert_eq!(session.item.id, item.id);
        assert_eq!(session.status, "running");
        assert_eq!(session.process_id, Some(123));
        assert_eq!(session.audio_stream_index, Some(1));
        assert_eq!(session.subtitle_stream_index, Some(-1));
        assert_eq!(session.video_stream_index, Some(0));
        assert_eq!(session.progress_percent, Some(12.5));
        assert_eq!(session.position_ticks, 456);

        db.update_transcode_session_progress("play-session-1", Some(25.0), 789)
            .await
            .unwrap();
        let progressed = db
            .transcode_session_by_play_session_id("play-session-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progressed.progress_percent, Some(25.0));
        assert_eq!(progressed.position_ticks, 789);

        db.update_transcode_session_progress("play-session-1", None, 1000)
            .await
            .unwrap();
        let progressed = db
            .transcode_session_by_play_session_id("play-session-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progressed.progress_percent, Some(25.0));
        assert_eq!(progressed.position_ticks, 1000);

        let fetched = db
            .transcode_session_by_play_session_id("play-session-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.output_path, session.output_path);

        let sessions = db.transcode_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        let active_sessions = db.active_transcode_sessions().await.unwrap();
        assert_eq!(active_sessions.len(), 1);
        assert_eq!(active_sessions[0].play_session_id, "play-session-1");

        let stale_sessions = db.stale_transcode_sessions_on_startup().await.unwrap();
        assert_eq!(stale_sessions.len(), 1);
        assert_eq!(stale_sessions[0].play_session_id, "play-session-1");
        assert_eq!(stale_sessions[0].status, "running");
        assert_eq!(stale_sessions[0].process_id, Some(123));

        tokio::fs::remove_file(&movie).await.unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        assert!(db.active_transcode_sessions().await.unwrap().is_empty());
        let stale_sessions = db.stale_transcode_sessions_on_startup().await.unwrap();
        assert_eq!(stale_sessions.len(), 1);
        assert_eq!(stale_sessions[0].play_session_id, "play-session-1");

        db.update_transcode_session_status("play-session-1", "Stopped")
            .await
            .unwrap();
        assert!(db.active_transcode_sessions().await.unwrap().is_empty());
        assert!(
            db.stale_transcode_sessions_on_startup()
                .await
                .unwrap()
                .is_empty()
        );
        let terminal_sessions = db
            .terminal_transcode_sessions_older_than(Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(terminal_sessions.len(), 1);
        assert_eq!(terminal_sessions[0].play_session_id, "play-session-1");
        assert_eq!(terminal_sessions[0].status, "stopped");
        let stopped_status: String =
            sqlx::query_scalar("SELECT status FROM transcode_sessions WHERE play_session_id = ?1")
                .bind("play-session-1")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(stopped_status, "stopped");
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
        assert_eq!(items[0].file_size, Some(10));
        assert_eq!(items[0].runtime_ticks, None);

        db.update_media_item_media_info(
            items[0].id,
            Some(12_345_000_000),
            Some(3_000_000),
            Some(1920),
            Some(1080),
            vec![serde_json::json!({
                "Type": "Video",
                "Index": 0,
                "Codec": "h264",
                "Width": 1920,
                "Height": 1080
            })],
        )
        .await
        .unwrap();
        let updated = db.media_items().await.unwrap().remove(0);
        assert_eq!(updated.runtime_ticks, Some(12_345_000_000));
        assert_eq!(updated.bitrate, Some(3_000_000));
        assert_eq!(updated.width, Some(1920));
        assert_eq!(updated.height, Some(1080));
        assert_eq!(updated.media_streams[0]["Codec"], "h264");
    }

    #[test]
    fn parses_ffprobe_media_info_json() {
        let value = json!({
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "width": 1920,
                    "height": 1080,
                    "bit_rate": "2500000"
                },
                {
                    "index": 1,
                    "codec_type": "audio",
                    "bit_rate": "128000"
                }
            ],
            "format": {
                "duration": "123.456",
                "bit_rate": "3000000"
            }
        });
        let info = parse_ffprobe_media_info(&value);
        assert_eq!(info.runtime_ticks, Some(1_234_560_000));
        assert_eq!(info.bitrate, Some(3_000_000));
        assert_eq!(info.width, Some(1920));
        assert_eq!(info.height, Some(1080));
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
        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some("source".to_string()),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(-1),
            position_ticks: 42,
            is_paused: false,
            played: false,
        })
        .await
        .unwrap();
        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some("source".to_string()),
            audio_stream_index: None,
            subtitle_stream_index: None,
            position_ticks: 84,
            is_paused: true,
            played: false,
        })
        .await
        .unwrap();
        let resume_items = db.resume_items_for_user(user.id, 10).await.unwrap();
        assert_eq!(resume_items.len(), 1);
        assert_eq!(resume_items[0].1.audio_stream_index, Some(1));
        assert_eq!(resume_items[0].1.subtitle_stream_index, Some(-1));

        tokio::fs::remove_file(&movie).await.unwrap();
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 0);

        assert!(db.media_items().await.unwrap().is_empty());
        let playback = db
            .playback_state_for_item(user.id, item.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playback.audio_stream_index, Some(1));
        assert_eq!(playback.subtitle_stream_index, Some(-1));
        assert_eq!(playback.position_ticks, 84);
        assert!(playback.is_paused);
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
        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some("source".to_string()),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(-1),
            position_ticks: 42,
            is_paused: false,
            played: false,
        })
        .await
        .unwrap();

        tokio::fs::rename(&movie, &renamed_movie).await.unwrap();
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);

        let items = db.media_items().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, item.id);
        assert_eq!(items[0].name, "Renamed Movie");
        assert_eq!(items[0].path, renamed_movie.to_string_lossy());
        let playback = db
            .playback_state_for_item(user.id, item.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playback.position_ticks, 42);
        assert_eq!(playback.audio_stream_index, Some(1));
        assert_eq!(playback.subtitle_stream_index, Some(-1));
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
