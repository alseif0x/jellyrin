use anyhow::{Context, ensure};
use serde_json::Value;
use sqlx::{Postgres, QueryBuilder};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    ActivityLogEntry, ActivityLogFilter, ActivityLogSortField, BackupManifest, BrandingConfig,
    MediaItemLyrics, PostgresDatabase, QuickConnectSession, SortDirection, TrickplayInfo,
};

impl PostgresDatabase {
    pub async fn add_activity_log_entry(
        &self,
        name: &str,
        overview: Option<&str>,
        short_overview: Option<&str>,
        entry_type: &str,
        user_id: Option<Uuid>,
    ) -> anyhow::Result<ActivityLogEntry> {
        self.add_activity_log_entry_with_item(
            name,
            overview,
            short_overview,
            entry_type,
            user_id,
            None,
        )
        .await
    }

    pub async fn add_activity_log_entry_with_item(
        &self,
        name: &str,
        overview: Option<&str>,
        short_overview: Option<&str>,
        entry_type: &str,
        user_id: Option<Uuid>,
        item_id: Option<Uuid>,
    ) -> anyhow::Result<ActivityLogEntry> {
        let name = name.trim();
        let entry_type = entry_type.trim();
        ensure!(!name.is_empty(), "activity log name must not be empty");
        ensure!(
            !entry_type.is_empty(),
            "activity log type must not be empty"
        );

        let row = sqlx::query_as::<_, PostgresActivityLogEntryRow>(
            r#"
            INSERT INTO activity_log_entries (
                name, overview, short_overview, entry_type, severity,
                user_id, item_id, created_at
            )
            VALUES ($1, $2, $3, $4, 'Information', $5, $6, $7)
            RETURNING id, name, overview, short_overview, entry_type,
                      severity, user_id, item_id, created_at
            "#,
        )
        .bind(name)
        .bind(trimmed_optional_str(overview))
        .bind(trimmed_optional_str(short_overview))
        .bind(entry_type)
        .bind(user_id)
        .bind(item_id)
        .bind(OffsetDateTime::now_utc())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn activity_log_entries(
        &self,
        start_index: i64,
        limit: i64,
        filter: ActivityLogFilter,
    ) -> anyhow::Result<(Vec<ActivityLogEntry>, i64)> {
        let start_index = start_index.max(0);
        let limit = limit.clamp(0, 1000);

        let mut count =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM activity_log_entries");
        push_activity_log_join_and_filters(&mut count, &filter);
        let total = count
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;

        let mut rows = QueryBuilder::<Postgres>::new(
            "SELECT activity_log_entries.id, activity_log_entries.name, \
             activity_log_entries.overview, activity_log_entries.short_overview, \
             activity_log_entries.entry_type, activity_log_entries.severity, \
             activity_log_entries.user_id, activity_log_entries.item_id, \
             activity_log_entries.created_at FROM activity_log_entries",
        );
        push_activity_log_join_and_filters(&mut rows, &filter);
        push_activity_log_order_by(&mut rows, &filter.sort);
        rows.push(" LIMIT ").push_bind(limit);
        rows.push(" OFFSET ").push_bind(start_index);

        let rows = rows
            .build_query_as::<PostgresActivityLogEntryRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok((rows.into_iter().map(Into::into).collect(), total))
    }

    pub async fn branding_config(&self) -> anyhow::Result<BrandingConfig> {
        let row = sqlx::query_as::<_, PostgresBrandingConfigRow>(
            r#"
            SELECT login_disclaimer, custom_css, splashscreen_enabled
            FROM branding_config
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into).unwrap_or_default())
    }

    pub async fn update_branding_config(&self, config: BrandingConfig) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO branding_config (
                id, login_disclaimer, custom_css, splashscreen_enabled, updated_at
            )
            VALUES (1, $1, $2, $3, $4)
            ON CONFLICT (id) DO UPDATE SET
                login_disclaimer = excluded.login_disclaimer,
                custom_css = excluded.custom_css,
                splashscreen_enabled = excluded.splashscreen_enabled,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.login_disclaimer)
        .bind(config.custom_css)
        .bind(config.splashscreen_enabled)
        .bind(OffsetDateTime::now_utc())
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
        sqlx::query_scalar(
            r#"
            SELECT payload
            FROM display_preferences
            WHERE user_id = $1 AND client = $2 AND id = $3
            "#,
        )
        .bind(user_id)
        .bind(client)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL display preferences")
    }

    pub async fn update_display_preferences(
        &self,
        user_id: Uuid,
        client: &str,
        id: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        ensure_user_exists(&self.pool, user_id).await?;
        sqlx::query(
            r#"
            INSERT INTO display_preferences (
                id, user_id, client, payload, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $5)
            ON CONFLICT (id, user_id, client) DO UPDATE SET
                payload = excluded.payload,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(client)
        .bind(payload)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn media_item_lyrics(
        &self,
        item_id: Uuid,
    ) -> anyhow::Result<Option<MediaItemLyrics>> {
        let row = sqlx::query_as::<_, PostgresMediaItemLyricsRow>(
            r#"
            SELECT item_id, lyrics, updated_at
            FROM media_item_lyrics
            WHERE item_id = $1
            "#,
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn update_media_item_lyrics(
        &self,
        item_id: Uuid,
        payload: Value,
    ) -> anyhow::Result<()> {
        ensure_media_item_exists(&self.pool, item_id).await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_lyrics (item_id, lyrics, created_at, updated_at)
            VALUES ($1, $2, $3, $3)
            ON CONFLICT (item_id) DO UPDATE SET
                lyrics = excluded.lyrics,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(item_id)
        .bind(payload)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_media_item_lyrics(&self, item_id: Uuid) -> anyhow::Result<bool> {
        ensure_media_item_exists(&self.pool, item_id).await?;
        let result = sqlx::query("DELETE FROM media_item_lyrics WHERE item_id = $1")
            .bind(item_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn initiate_quick_connect(
        &self,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<QuickConnectSession> {
        let now = OffsetDateTime::now_utc();
        let session = QuickConnectSession {
            secret: Uuid::new_v4().simple().to_string(),
            code: Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(6)
                .collect::<String>()
                .to_ascii_uppercase(),
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            client: client.to_string(),
            version: version.to_string(),
            user_id: None,
            authorized: false,
            created_at: now,
            updated_at: now,
            expires_at: now + Duration::minutes(10),
        };
        sqlx::query(
            r#"
            INSERT INTO quick_connect_sessions (
                secret, code, device_id, device_name, client, version,
                user_id, authorized, created_at, updated_at, expires_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULL, false, $7, $7, $8)
            "#,
        )
        .bind(&session.secret)
        .bind(&session.code)
        .bind(&session.device_id)
        .bind(&session.device_name)
        .bind(&session.client)
        .bind(&session.version)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(session)
    }

    pub async fn quick_connect_by_secret(
        &self,
        secret: &str,
    ) -> anyhow::Result<QuickConnectSession> {
        let row = sqlx::query_as::<_, PostgresQuickConnectSessionRow>(
            r#"
            SELECT secret, code, device_id, device_name, client, version,
                   user_id, authorized, created_at, updated_at, expires_at
            FROM quick_connect_sessions
            WHERE secret = $1
            "#,
        )
        .bind(secret)
        .fetch_optional(&self.pool)
        .await?
        .context("quick connect session not found")?;
        Ok(row.into())
    }

    pub async fn authorize_quick_connect(
        &self,
        code: &str,
        user_id: Uuid,
    ) -> anyhow::Result<QuickConnectSession> {
        ensure_user_exists(&self.pool, user_id).await?;
        let row = sqlx::query_as::<_, PostgresQuickConnectSessionRow>(
            r#"
            UPDATE quick_connect_sessions
            SET user_id = $1, authorized = true, updated_at = $2
            WHERE code = $3 AND expires_at > $2
            RETURNING secret, code, device_id, device_name, client, version,
                      user_id, authorized, created_at, updated_at, expires_at
            "#,
        )
        .bind(user_id)
        .bind(OffsetDateTime::now_utc())
        .bind(code.trim().to_ascii_uppercase())
        .fetch_optional(&self.pool)
        .await?
        .context("quick connect code not found")?;
        Ok(row.into())
    }

    pub async fn delete_quick_connect_session(&self, secret: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM quick_connect_sessions WHERE secret = $1")
            .bind(secret)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn backup_manifests(&self) -> anyhow::Result<Vec<BackupManifest>> {
        let rows = sqlx::query_as::<_, PostgresBackupManifestRow>(
            r#"
            SELECT path, server_version, backup_engine_version,
                   options, restore_snapshot, created_at
            FROM backup_manifests
            ORDER BY created_at DESC, path
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn backup_manifest(&self, path: &str) -> anyhow::Result<Option<BackupManifest>> {
        let row = sqlx::query_as::<_, PostgresBackupManifestRow>(
            r#"
            SELECT path, server_version, backup_engine_version,
                   options, restore_snapshot, created_at
            FROM backup_manifests
            WHERE path = $1
            "#,
        )
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn create_backup_manifest(
        &self,
        server_version: &str,
        backup_engine_version: &str,
        options: Value,
        restore_snapshot: Option<Value>,
    ) -> anyhow::Result<BackupManifest> {
        let manifest = BackupManifest {
            path: format!("jellyrin-backup-{}.zip", Uuid::new_v4().simple()),
            server_version: server_version.to_string(),
            backup_engine_version: backup_engine_version.to_string(),
            options,
            restore_snapshot,
            created_at: OffsetDateTime::now_utc(),
        };
        sqlx::query(
            r#"
            INSERT INTO backup_manifests (
                path, server_version, backup_engine_version,
                options, restore_snapshot, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&manifest.path)
        .bind(&manifest.server_version)
        .bind(&manifest.backup_engine_version)
        .bind(&manifest.options)
        .bind(&manifest.restore_snapshot)
        .bind(manifest.created_at)
        .execute(&self.pool)
        .await?;
        Ok(manifest)
    }

    pub async fn trickplay_info(
        &self,
        item_id: Uuid,
        width: i64,
    ) -> anyhow::Result<Option<TrickplayInfo>> {
        let width = i32::try_from(width).context("trickplay width is outside PostgreSQL range")?;
        let row = sqlx::query_as::<_, PostgresTrickplayInfoRow>(
            r#"
            SELECT item_id, width, height, tile_width, tile_height,
                   thumbnail_count, interval_ms, bandwidth, created_at, updated_at
            FROM trickplay_infos
            WHERE item_id = $1 AND width = $2
            "#,
        )
        .bind(item_id)
        .bind(width)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn upsert_trickplay_info(
        &self,
        info: TrickplayInfo,
    ) -> anyhow::Result<TrickplayInfo> {
        ensure!(info.width > 0, "trickplay width must be positive");
        ensure!(info.height > 0, "trickplay height must be positive");
        ensure!(info.tile_width > 0, "trickplay tile width must be positive");
        ensure!(
            info.tile_height > 0,
            "trickplay tile height must be positive"
        );
        ensure!(
            info.thumbnail_count > 0,
            "trickplay thumbnail count must be positive"
        );
        ensure!(info.interval_ms > 0, "trickplay interval must be positive");

        let width = positive_i32(info.width, "trickplay width")?;
        let height = positive_i32(info.height, "trickplay height")?;
        let tile_width = positive_i32(info.tile_width, "trickplay tile width")?;
        let tile_height = positive_i32(info.tile_height, "trickplay tile height")?;
        let thumbnail_count = positive_i32(info.thumbnail_count, "trickplay thumbnail count")?;
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<_, PostgresTrickplayInfoRow>(
            r#"
            INSERT INTO trickplay_infos (
                item_id, width, height, tile_width, tile_height, thumbnail_count,
                interval_ms, bandwidth, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
            ON CONFLICT (item_id, width) DO UPDATE SET
                height = excluded.height,
                tile_width = excluded.tile_width,
                tile_height = excluded.tile_height,
                thumbnail_count = excluded.thumbnail_count,
                interval_ms = excluded.interval_ms,
                bandwidth = excluded.bandwidth,
                updated_at = excluded.updated_at
            RETURNING item_id, width, height, tile_width, tile_height,
                      thumbnail_count, interval_ms, bandwidth, created_at, updated_at
            "#,
        )
        .bind(info.item_id)
        .bind(width)
        .bind(height)
        .bind(tile_width)
        .bind(tile_height)
        .bind(thumbnail_count)
        .bind(info.interval_ms)
        .bind(info.bandwidth.max(0))
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }
}

#[derive(sqlx::FromRow)]
struct PostgresActivityLogEntryRow {
    id: i64,
    name: String,
    overview: Option<String>,
    short_overview: Option<String>,
    entry_type: String,
    severity: String,
    user_id: Option<Uuid>,
    item_id: Option<Uuid>,
    created_at: OffsetDateTime,
}

impl From<PostgresActivityLogEntryRow> for ActivityLogEntry {
    fn from(row: PostgresActivityLogEntryRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            overview: row.overview,
            short_overview: row.short_overview,
            entry_type: row.entry_type,
            severity: row.severity,
            user_id: row.user_id,
            item_id: row.item_id,
            created_at: row.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresBrandingConfigRow {
    login_disclaimer: Option<String>,
    custom_css: Option<String>,
    splashscreen_enabled: bool,
}

impl From<PostgresBrandingConfigRow> for BrandingConfig {
    fn from(row: PostgresBrandingConfigRow) -> Self {
        Self {
            login_disclaimer: row.login_disclaimer,
            custom_css: row.custom_css,
            splashscreen_enabled: row.splashscreen_enabled,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemLyricsRow {
    item_id: Uuid,
    lyrics: Value,
    updated_at: OffsetDateTime,
}

impl From<PostgresMediaItemLyricsRow> for MediaItemLyrics {
    fn from(row: PostgresMediaItemLyricsRow) -> Self {
        Self {
            item_id: row.item_id,
            payload: row.lyrics,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresQuickConnectSessionRow {
    secret: String,
    code: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
    user_id: Option<Uuid>,
    authorized: bool,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    expires_at: OffsetDateTime,
}

impl From<PostgresQuickConnectSessionRow> for QuickConnectSession {
    fn from(row: PostgresQuickConnectSessionRow) -> Self {
        Self {
            secret: row.secret,
            code: row.code,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
            user_id: row.user_id,
            authorized: row.authorized,
            created_at: row.created_at,
            updated_at: row.updated_at,
            expires_at: row.expires_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresBackupManifestRow {
    path: String,
    server_version: String,
    backup_engine_version: String,
    options: Value,
    restore_snapshot: Option<Value>,
    created_at: OffsetDateTime,
}

impl From<PostgresBackupManifestRow> for BackupManifest {
    fn from(row: PostgresBackupManifestRow) -> Self {
        Self {
            path: row.path,
            server_version: row.server_version,
            backup_engine_version: row.backup_engine_version,
            options: row.options,
            restore_snapshot: row.restore_snapshot,
            created_at: row.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresTrickplayInfoRow {
    item_id: Uuid,
    width: i32,
    height: i32,
    tile_width: i32,
    tile_height: i32,
    thumbnail_count: i32,
    interval_ms: i64,
    bandwidth: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresTrickplayInfoRow> for TrickplayInfo {
    fn from(row: PostgresTrickplayInfoRow) -> Self {
        Self {
            item_id: row.item_id,
            width: i64::from(row.width),
            height: i64::from(row.height),
            tile_width: i64::from(row.tile_width),
            tile_height: i64::from(row.tile_height),
            thumbnail_count: i64::from(row.thumbnail_count),
            interval_ms: row.interval_ms,
            bandwidth: row.bandwidth,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn trimmed_optional_str(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn trimmed_filter_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn push_activity_log_join_and_filters(
    query: &mut QueryBuilder<Postgres>,
    filter: &ActivityLogFilter,
) {
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

    let mut first = true;
    push_activity_log_text_filter(query, &mut first, "activity_log_entries.name", &filter.name);
    push_activity_log_text_filter(
        query,
        &mut first,
        "activity_log_entries.overview",
        &filter.overview,
    );
    push_activity_log_text_filter(
        query,
        &mut first,
        "activity_log_entries.short_overview",
        &filter.short_overview,
    );
    push_activity_log_text_filter(
        query,
        &mut first,
        "activity_log_entries.entry_type",
        &filter.entry_type,
    );
    push_activity_log_text_filter(query, &mut first, "users.name", &filter.username);

    if let Some(value) = trimmed_filter_value(&filter.severity) {
        push_activity_log_where(query, &mut first);
        query
            .push("activity_log_entries.severity = ")
            .push_bind(value);
    }
    if let Some(item_id) = filter.item_id {
        push_activity_log_where(query, &mut first);
        query
            .push("activity_log_entries.item_id = ")
            .push_bind(item_id);
    }
    if let Some(has_user_id) = filter.has_user_id {
        push_activity_log_where(query, &mut first);
        query.push(if has_user_id {
            "activity_log_entries.user_id IS NOT NULL"
        } else {
            "activity_log_entries.user_id IS NULL"
        });
    }
    if let Some(min_date) = filter.min_date {
        push_activity_log_where(query, &mut first);
        query
            .push("activity_log_entries.created_at >= ")
            .push_bind(min_date);
    }
    if let Some(max_date) = filter.max_date {
        push_activity_log_where(query, &mut first);
        query
            .push("activity_log_entries.created_at <= ")
            .push_bind(max_date);
    }
}

fn push_activity_log_text_filter(
    query: &mut QueryBuilder<Postgres>,
    first: &mut bool,
    column: &'static str,
    value: &Option<String>,
) {
    let Some(value) = trimmed_filter_value(value) else {
        return;
    };
    push_activity_log_where(query, first);
    query
        .push(column)
        .push(" ILIKE ")
        .push_bind(format!("%{value}%"));
}

fn push_activity_log_where(query: &mut QueryBuilder<Postgres>, first: &mut bool) {
    if *first {
        query.push(" WHERE ");
        *first = false;
    } else {
        query.push(" AND ");
    }
}

fn push_activity_log_order_by(
    query: &mut QueryBuilder<Postgres>,
    sort: &[(ActivityLogSortField, SortDirection)],
) {
    let fallback = [(ActivityLogSortField::DateCreated, SortDirection::Descending)];
    let sort = if sort.is_empty() { &fallback[..] } else { sort };
    let clauses = sort
        .iter()
        .copied()
        .take(4)
        .map(|(field, direction)| {
            let direction = match direction {
                SortDirection::Ascending => "ASC",
                SortDirection::Descending => "DESC",
            };
            format!("{} {direction}", activity_log_sort_column(field))
        })
        .chain(std::iter::once("activity_log_entries.id DESC".to_string()))
        .collect::<Vec<_>>();
    query.push(" ORDER BY ").push(clauses.join(", "));
}

fn activity_log_sort_column(field: ActivityLogSortField) -> &'static str {
    match field {
        ActivityLogSortField::Name => "lower(activity_log_entries.name)",
        ActivityLogSortField::Overview => "lower(activity_log_entries.overview)",
        ActivityLogSortField::ShortOverview => "lower(activity_log_entries.short_overview)",
        ActivityLogSortField::Type => "lower(activity_log_entries.entry_type)",
        ActivityLogSortField::DateCreated => "activity_log_entries.created_at",
        ActivityLogSortField::Username => "lower(users.name)",
        ActivityLogSortField::LogSeverity => "lower(activity_log_entries.severity)",
    }
}

async fn ensure_user_exists(pool: &sqlx::PgPool, user_id: Uuid) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    ensure!(exists, "user not found");
    Ok(())
}

async fn ensure_media_item_exists(pool: &sqlx::PgPool, item_id: Uuid) -> anyhow::Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM media_items WHERE id = $1)")
            .bind(item_id)
            .fetch_one(pool)
            .await?;
    ensure!(exists, "media item not found");
    Ok(())
}

fn positive_i32(value: i64, label: &str) -> anyhow::Result<i32> {
    let value =
        i32::try_from(value).with_context(|| format!("{label} is outside PostgreSQL range"))?;
    ensure!(value > 0, "{label} must be positive");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{
        super::PostgresSettings, ActivityLogFilter, PostgresDatabase, SortDirection, TrickplayInfo,
    };

    #[tokio::test]
    async fn postgres_misc_contracts_round_trip_when_configured() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();

        let user_id = Uuid::new_v4();
        let folder_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)")
            .bind(user_id)
            .bind(format!("misc-user-{user_id}"))
            .bind(now)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations, created_at, updated_at
            ) VALUES ($1, $2, 'movies', '[]'::jsonb, $3, $3)
            "#,
        )
        .bind(folder_id)
        .bind(format!("misc-folder-{folder_id}"))
        .bind(now)
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type,
                media_streams, metadata, created_at, updated_at
            ) VALUES ($1, $2, 'Misc movie', $3, 'Movie', '[]'::jsonb, '{}'::jsonb, $4, $4)
            "#,
        )
        .bind(item_id)
        .bind(folder_id)
        .bind(format!("xtream://misc/{item_id}"))
        .bind(now)
        .execute(&database.pool)
        .await
        .unwrap();

        database
            .update_display_preferences(user_id, "test-client", "home", json!({"View": "list"}))
            .await
            .unwrap();
        assert_eq!(
            database
                .display_preferences(user_id, "test-client", "home")
                .await
                .unwrap(),
            Some(json!({"View": "list"}))
        );

        database
            .update_media_item_lyrics(item_id, json!({"Lyrics": "test"}))
            .await
            .unwrap();
        assert_eq!(
            database
                .media_item_lyrics(item_id)
                .await
                .unwrap()
                .unwrap()
                .payload,
            json!({"Lyrics": "test"})
        );

        let activity = database
            .add_activity_log_entry_with_item(
                "Played",
                Some("misc test"),
                None,
                "PlaybackStart",
                Some(user_id),
                Some(item_id),
            )
            .await
            .unwrap();
        let (entries, total) = database
            .activity_log_entries(
                0,
                10,
                ActivityLogFilter {
                    item_id: Some(item_id),
                    sort: vec![(
                        super::ActivityLogSortField::DateCreated,
                        SortDirection::Descending,
                    )],
                    ..ActivityLogFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(entries[0].id, activity.id);

        let quick_connect = database
            .initiate_quick_connect("misc-device", "Misc device", "tests", "1")
            .await
            .unwrap();
        let authorized = database
            .authorize_quick_connect(&quick_connect.code, user_id)
            .await
            .unwrap();
        assert!(authorized.authorized);
        assert_eq!(authorized.user_id, Some(user_id));

        let manifest = database
            .create_backup_manifest("test", "1", json!({"Database": true}), None)
            .await
            .unwrap();
        assert_eq!(
            database
                .backup_manifest(&manifest.path)
                .await
                .unwrap()
                .unwrap()
                .options,
            json!({"Database": true})
        );

        let trickplay = database
            .upsert_trickplay_info(TrickplayInfo {
                item_id,
                width: 320,
                height: 180,
                tile_width: 5,
                tile_height: 5,
                thumbnail_count: 25,
                interval_ms: 10_000,
                bandwidth: 1_024,
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
        assert_eq!(trickplay.width, 320);
        assert_eq!(
            database
                .trickplay_info(item_id, 320)
                .await
                .unwrap()
                .unwrap()
                .thumbnail_count,
            25
        );

        database
            .delete_quick_connect_session(&quick_connect.secret)
            .await
            .unwrap();
        sqlx::query("DELETE FROM activity_log_entries WHERE id = $1")
            .bind(activity.id)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM backup_manifests WHERE path = $1")
            .bind(&manifest.path)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM virtual_folders WHERE id = $1")
            .bind(folder_id)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&database.pool)
            .await
            .unwrap();
        database.close().await;
    }
}
