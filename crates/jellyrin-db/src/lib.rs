#[cfg(any(test, feature = "sqlite"))]
use futures_util::TryStreamExt;
#[cfg(any(test, feature = "sqlite"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "sqlite"))]
use std::sync::Arc;
use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration as StdDuration,
};

use anyhow::Context;
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use jellyrin_core::{
    DeviceToken, MediaItem, PlaybackState, ServerState, User, VirtualFolder,
    effective_media_item_type,
};
#[cfg(any(test, feature = "sqlite"))]
use jellyrin_core::{StartupConfig, tv_episode_path_info};
use jellyrin_transcode::{
    BoundedCommandOutputError, BoundedCommandOutputOptions, TranscodeJobPermit,
    acquire_multimedia_probe, run_bounded_command_output,
};
use serde_json::{Value, json};
#[cfg(any(test, feature = "sqlite"))]
use sqlx::{
    QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
#[cfg(any(test, feature = "sqlite"))]
use time::Duration;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::process::Command;
use uuid::Uuid;

mod driver;
mod facets;
mod ffprobe_telemetry;
mod manager;
mod postgres;
mod postgres_auth;
mod postgres_catalog;
mod postgres_devices;
mod postgres_lists;
mod postgres_livetv;
mod postgres_misc;
mod postgres_plugins;
mod postgres_provider_secrets;
mod postgres_scan;
mod postgres_sessions;
mod provider_secrets;
mod query_filter_projection;
mod telemetry;
pub use driver::{DatabaseBackend, DatabaseDriver};
pub use facets::{
    ExtractedMediaItemFacet, MediaItemFacetCandidateQuery, MediaItemFacetKind, MediaItemFacetValue,
    MediaItemFilterSelectorKind, extract_media_item_facets, extract_media_item_filter_selectors,
    extract_media_item_genre_selectors,
};
use ffprobe_telemetry::{FfprobeOutcome, ffprobe_telemetry};
pub use ffprobe_telemetry::{FfprobeTelemetrySnapshot, ffprobe_telemetry_snapshot};
pub use manager::{DatabaseConfig, DatabaseManager};
pub use postgres::{PostgresDatabase, PostgresSettings};
pub use postgres_catalog::{
    MEDIA_ITEM_FACET_PROJECTION_NAME, MEDIA_ITEM_FACET_PROJECTION_VERSION,
    MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME, MediaItemFacetProjectionMode,
    MediaItemFacetProjectionReport, MediaItemQueryFilterProjectionReport,
    ensure_media_item_facet_projection, ensure_media_item_query_filter_projection,
};
pub use provider_secrets::{
    PROVIDER_SECRET_REFERENCE_FIELD, ProviderCredentials, ProviderSecretEnvelope,
    ProviderSecretReference, ProviderSecretVault, provider_secret_namespace_for_configuration,
};
use provider_secrets::{
    collect_provider_secret_reference_identities, configuration_has_provider_secret_input_field,
    configuration_has_provider_secret_material, configuration_has_provider_secret_reference_field,
    configuration_references_provider_secret, inherit_provider_secret_reference,
    inherit_provider_secret_reference_for_configuration, new_provider_secret_id,
    normalize_provider_type, provider_credentials_from_configuration,
    redacted_provider_configuration, resolved_provider_configuration,
    set_provider_secret_reference,
};
pub use telemetry::{
    DATABASE_DURATION_BUCKET_COUNT, DATABASE_DURATION_BUCKET_UPPER_MICROSECONDS,
    DatabaseAcquireDiagnostics, DatabaseDurationHistogramDiagnostics,
    DatabaseErrorClassDiagnostics, DatabaseOperationDiagnostics, DatabasePoolRole,
    DatabaseRowDiagnostics, DatabaseTelemetryCoverage, DatabaseTelemetryDiagnostics,
};
#[cfg(any(test, feature = "sqlite"))]
use telemetry::{DatabaseOperation, DatabaseTelemetry};

/// The supported production adapter. Repository traits provide extension seams; this alias stays
/// concrete until another backend has a complete native implementation and conformance suite.
pub type ProductionDatabase = PostgresDatabase;

const TV_SERIES_CATALOG_PROJECTION_VERSION: i32 = 3;
const CATALOG_LOCK_RETRY_ATTEMPTS: usize = 6;
const CATALOG_LOCK_RETRY_BASE_DELAY_MS: u64 = 25;
const CATALOG_LOCK_RETRY_MAX_DELAY_MS: u64 = 400;
use query_filter_projection::{
    MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION, MediaItemQueryFilterProjection,
    MediaItemQueryFilterProjectionSource, encode_media_item_query_filter_position,
    extract_media_item_query_filter_projection,
};

fn transient_catalog_lock_error(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        let Some(sqlx::Error::Database(database_error)) = source.downcast_ref::<sqlx::Error>()
        else {
            return false;
        };
        let Some(code) = database_error.code() else {
            return false;
        };
        if matches!(code.as_ref(), "55P03" | "40P01" | "40001") {
            return true;
        }
        code.parse::<i32>()
            .is_ok_and(|sqlite_code| matches!(sqlite_code & 0xff, 5 | 6))
    })
}

fn catalog_lock_retry_delay(attempt: usize, jitter_seed: u64) -> StdDuration {
    let exponent = u32::try_from(attempt.min(8)).unwrap_or(8);
    let base = CATALOG_LOCK_RETRY_BASE_DELAY_MS
        .saturating_mul(1_u64 << exponent)
        .min(CATALOG_LOCK_RETRY_MAX_DELAY_MS);
    // SplitMix64 gives each catalogue/folder a stable but different retry cadence. The retry stays
    // deterministic in tests while concurrent publishers avoid waking on the same millisecond.
    let mut mixed = jitter_seed
        ^ u64::try_from(attempt)
            .unwrap_or(u64::MAX)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    let jitter = mixed % (base / 2 + 1);
    StdDuration::from_millis(base.saturating_add(jitter))
}

fn catalog_lock_jitter_seed(id: Uuid) -> u64 {
    let value = id.as_u128();
    (value as u64) ^ u64::try_from(value >> 64).unwrap_or_default()
}

async fn retry_transient_catalog_lock<T, F, Fut>(
    jitter_seed: u64,
    mut attempt_operation: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    for attempt in 0..CATALOG_LOCK_RETRY_ATTEMPTS {
        match attempt_operation().await {
            Ok(value) => return Ok(value),
            Err(error)
                if attempt + 1 < CATALOG_LOCK_RETRY_ATTEMPTS
                    && transient_catalog_lock_error(&error) =>
            {
                let delay = catalog_lock_retry_delay(attempt, jitter_seed);
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("catalogue lock retry loop always returns")
}

#[cfg(test)]
pub type Database = SqliteDatabase;
#[cfg(not(test))]
pub type Database = ProductionDatabase;

#[cfg(any(test, feature = "sqlite"))]
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
#[cfg(any(test, feature = "sqlite"))]
const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
#[cfg(any(test, feature = "sqlite"))]
const SQLITE_MAX_CONNECTIONS: u32 = 5;
#[cfg(any(test, feature = "sqlite"))]
const FACET_REBUILD_BATCH_SIZE: i64 = 500;
const DEFAULT_SYNC_PLAY_ACCESS: &str = "CreateAndJoinGroups";
const DEFAULT_FFPROBE_TIMEOUT_SECONDS: u64 = 15;
const MAX_FFPROBE_TIMEOUT_SECONDS: u64 = 120;
const FFPROBE_STDOUT_MAX_BYTES: usize = 8 * 1024 * 1024;
const FFPROBE_STDERR_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
#[cfg(any(test, feature = "sqlite"))]
pub struct SqliteDatabase {
    pool: SqlitePool,
    provider_secret_vault: Option<ProviderSecretVault>,
    telemetry: Arc<DatabaseTelemetry>,
}

impl DatabaseBackend for PostgresDatabase {
    const DRIVER: DatabaseDriver = DatabaseDriver::PostgreSql;

    fn telemetry_diagnostics(&self) -> DatabaseTelemetryDiagnostics {
        PostgresDatabase::telemetry_diagnostics(self)
    }
}

#[cfg(any(test, feature = "sqlite"))]
impl DatabaseBackend for SqliteDatabase {
    const DRIVER: DatabaseDriver = DatabaseDriver::Sqlite;

    fn telemetry_diagnostics(&self) -> DatabaseTelemetryDiagnostics {
        SqliteDatabase::telemetry_diagnostics(self)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MediaItemFilterSummary {
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub containers: Vec<String>,
    pub media_types: Vec<String>,
}

/// Dialect-neutral distinct values exposed by Jellyfin's item-filter endpoints.
///
/// Values are de-duplicated case-insensitively and returned in normalized-value order while
/// preserving a deterministic display spelling. `staff_names` deliberately contains names only;
/// the API owns the synthetic Person response shape and identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaItemQueryFilterValues {
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub official_ratings: Vec<String>,
    pub years: Vec<String>,
    pub containers: Vec<String>,
    pub media_types: Vec<String>,
    pub video_types: Vec<String>,
    pub series_statuses: Vec<String>,
    pub staff_names: Vec<String>,
    pub artists: Vec<String>,
    pub albums: Vec<String>,
    pub studios: Vec<String>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub has_subtitles: bool,
    pub has_trailer: bool,
}

/// Exact subset of filter families requested by a Jellyfin filter endpoint.
///
/// Keeping this in the repository contract lets each SQL adapter avoid producing and sorting
/// values that the caller will discard, without changing the selected item set or applying a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaItemQueryFilterSelection(u16);

impl MediaItemQueryFilterSelection {
    const ALBUMS: u16 = 1 << 0;
    const ARTISTS: u16 = 1 << 1;
    const AUDIO_LANGUAGES: u16 = 1 << 2;
    const GENRES: u16 = 1 << 3;
    const OFFICIAL_RATINGS: u16 = 1 << 4;
    const SERIES_STATUSES: u16 = 1 << 5;
    const STAFF_NAMES: u16 = 1 << 6;
    const STUDIOS: u16 = 1 << 7;
    const SUBTITLE_LANGUAGES: u16 = 1 << 8;
    const TAGS: u16 = 1 << 9;
    const YEARS: u16 = 1 << 10;
    const SCALARS: u16 = 1 << 11;

    pub const ITEMS_FILTERS: Self = Self(
        Self::ALBUMS
            | Self::ARTISTS
            | Self::GENRES
            | Self::OFFICIAL_RATINGS
            | Self::SERIES_STATUSES
            | Self::STAFF_NAMES
            | Self::STUDIOS
            | Self::TAGS
            | Self::YEARS
            | Self::SCALARS,
    );

    pub const FILTERS2: Self =
        Self(Self::AUDIO_LANGUAGES | Self::GENRES | Self::SUBTITLE_LANGUAGES | Self::TAGS);

    pub const ALL: Self = Self(Self::ITEMS_FILTERS.0 | Self::FILTERS2.0);

    pub(crate) const fn includes_scalars(self) -> bool {
        self.0 & Self::SCALARS != 0
    }

    pub(crate) fn includes_field(self, field: &str) -> bool {
        let flag = match field {
            "albums" => Self::ALBUMS,
            "artists" => Self::ARTISTS,
            "audio_languages" => Self::AUDIO_LANGUAGES,
            "genres" => Self::GENRES,
            "official_ratings" => Self::OFFICIAL_RATINGS,
            "series_statuses" => Self::SERIES_STATUSES,
            "staff_names" => Self::STAFF_NAMES,
            "studios" => Self::STUDIOS,
            "subtitle_languages" => Self::SUBTITLE_LANGUAGES,
            "tags" => Self::TAGS,
            "years" => Self::YEARS,
            _ => 0,
        };
        self.0 & flag != 0
    }

    pub(crate) fn projected_fields(self) -> Vec<&'static str> {
        [
            "albums",
            "artists",
            "audio_languages",
            "genres",
            "official_ratings",
            "series_statuses",
            "staff_names",
            "studios",
            "subtitle_languages",
            "tags",
            "years",
        ]
        .into_iter()
        .filter(|field| self.includes_field(field))
        .collect()
    }

    pub(crate) fn summary_fields(self) -> Vec<&'static str> {
        let mut fields = self.projected_fields();
        if self.includes_scalars() {
            fields.extend([
                "containers",
                "media_types",
                "video_types",
                "has_subtitles",
                "has_trailer",
            ]);
        }
        fields
    }
}

impl MediaItemQueryFilterValues {
    pub(crate) fn retain_selection(&mut self, selection: MediaItemQueryFilterSelection) {
        if !selection.includes_field("genres") {
            self.genres.clear();
        }
        if !selection.includes_field("tags") {
            self.tags.clear();
        }
        if !selection.includes_field("official_ratings") {
            self.official_ratings.clear();
        }
        if !selection.includes_field("years") {
            self.years.clear();
        }
        if !selection.includes_field("series_statuses") {
            self.series_statuses.clear();
        }
        if !selection.includes_field("staff_names") {
            self.staff_names.clear();
        }
        if !selection.includes_field("artists") {
            self.artists.clear();
        }
        if !selection.includes_field("albums") {
            self.albums.clear();
        }
        if !selection.includes_field("studios") {
            self.studios.clear();
        }
        if !selection.includes_field("audio_languages") {
            self.audio_languages.clear();
        }
        if !selection.includes_field("subtitle_languages") {
            self.subtitle_languages.clear();
        }
        if !selection.includes_scalars() {
            self.containers.clear();
            self.media_types.clear();
            self.video_types.clear();
            self.has_subtitles = false;
            self.has_trailer = false;
        }
    }
}

/// Bounded resume-list request shared by the native database adapters.
///
/// The policy values are supplied by the API because they live in Jellyfin's server
/// configuration.  Applying them before `LIMIT/OFFSET` is important: filtering a small raw
/// playback page afterwards can return short or empty pages even when later resumable rows exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeItemsPageQuery {
    pub start_index: usize,
    pub limit: usize,
    pub min_pct: i64,
    pub max_pct: i64,
    pub min_duration_ticks: i64,
}

#[derive(Debug, Clone)]
pub struct ResumeItemsPage {
    pub items: Vec<(MediaItem, PlaybackState)>,
    pub total_record_count: usize,
    pub start_index: usize,
}

#[cfg(any(test, feature = "sqlite"))]
pub(crate) fn push_media_item_query_filter_value(
    values: &mut MediaItemQueryFilterValues,
    field: &str,
    display_value: String,
) {
    match field {
        "genres" => values.genres.push(display_value),
        "tags" => values.tags.push(display_value),
        "official_ratings" => values.official_ratings.push(display_value),
        "years" => values.years.push(display_value),
        "containers" => values.containers.push(display_value),
        "media_types" => values.media_types.push(display_value),
        "video_types" => values.video_types.push(display_value),
        "series_statuses" => values.series_statuses.push(display_value),
        "staff_names" => values.staff_names.push(display_value),
        "artists" => values.artists.push(display_value),
        "albums" => values.albums.push(display_value),
        "studios" => values.studios.push(display_value),
        "audio_languages" => values.audio_languages.push(display_value),
        "subtitle_languages" => values.subtitle_languages.push(display_value),
        "__has_subtitles" => values.has_subtitles = true,
        "__has_trailer" => values.has_trailer = true,
        _ => {}
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_media_item_query_filter_value_count(values: &MediaItemQueryFilterValues) -> u64 {
    let count = values.genres.len()
        + values.tags.len()
        + values.official_ratings.len()
        + values.years.len()
        + values.containers.len()
        + values.media_types.len()
        + values.video_types.len()
        + values.series_statuses.len()
        + values.staff_names.len()
        + values.artists.len()
        + values.albums.len()
        + values.studios.len()
        + values.audio_languages.len()
        + values.subtitle_languages.len();
    u64::try_from(count).unwrap_or(u64::MAX)
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

/// A credential-free snapshot of one SQLx connection pool.
///
/// These values are intentionally limited to bounded resource counts. Connection strings,
/// statements and per-request identifiers never cross the database boundary through this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabasePoolDiagnostics {
    pub max_connections: u32,
    pub size: u32,
    pub idle: u32,
    pub in_use: u32,
}

/// Driver-neutral runtime diagnostics for the database adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseRuntimeDiagnostics {
    pub driver: DatabaseDriver,
    pub api_pool: DatabasePoolDiagnostics,
    pub worker_pool: Option<DatabasePoolDiagnostics>,
}

/// Safe details about the most recently started catalogue synchronization.
///
/// Provider, folder and generation identifiers are deliberately omitted, as is the raw error
/// message: provider failures can contain upstream URLs or credential-shaped values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSyncRunDiagnostics {
    pub status: String,
    pub item_count: u64,
    pub started_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub duration_millis: Option<u64>,
}

/// Aggregate catalogue synchronization state suitable for administrative diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSyncDiagnostics {
    pub total: u64,
    pub running: u64,
    pub completed: u64,
    pub failed: u64,
    pub last_run: Option<CatalogSyncRunDiagnostics>,
}

#[derive(Debug, sqlx::FromRow)]
struct CatalogSyncCountsRow {
    total: i64,
    running: i64,
    completed: i64,
    failed: i64,
}

fn nonnegative_count(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(u64::MAX)
}

fn database_pool_diagnostics<DB: sqlx::Database>(pool: &sqlx::Pool<DB>) -> DatabasePoolDiagnostics {
    let size = pool.size();
    let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX).min(size);
    DatabasePoolDiagnostics {
        max_connections: pool.options().get_max_connections(),
        size,
        idle,
        in_use: size.saturating_sub(idle),
    }
}

fn catalog_sync_duration_millis(
    started_at: OffsetDateTime,
    completed_at: Option<OffsetDateTime>,
) -> Option<u64> {
    let duration = completed_at? - started_at;
    Some(u64::try_from(duration.whole_milliseconds().max(0)).unwrap_or(u64::MAX))
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
    pub restore_snapshot: Option<Value>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaItemMetadata {
    pub item_id: Uuid,
    pub payload: Value,
}

/// Maximum number of catalog rows returned by one database round trip.
///
/// Callers may request a larger page, but both database adapters clamp it to this value. The
/// exact count is still computed over the complete filtered result set.
pub const MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE: usize = 500;
/// Bound repeated facet selectors so SQLite stays below its bind limit and hostile queries cannot
/// create unbounded SQL. API requests above this limit are rejected instead of invoking the more
/// expensive application fallback; repository callers receive an error as a second line of defense.
pub const MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS: usize = 64;

fn validate_media_item_catalog_query(query: &MediaItemCatalogQuery) -> anyhow::Result<()> {
    for (kind, values) in [
        ("genre", &query.genre_ids),
        ("person", &query.person_ids),
        ("studio", &query.studio_ids),
        ("tag", &query.tags),
        ("official rating", &query.official_ratings),
        ("series status", &query.series_statuses),
        ("year", &query.years),
    ] {
        let selector_count = values
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<BTreeSet<_>>()
            .len();
        anyhow::ensure!(
            selector_count <= MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS,
            "media catalog {kind} selector count exceeds {}",
            MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaItemCatalogSortField {
    SortName,
    DateCreated,
    DateLastMediaAdded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaItemFavoriteFilter {
    /// Match the persisted `is_favorite` flag. A missing playback row behaves as `false`, matching
    /// Jellyfin's user-data semantics.
    Favorite(bool),
    /// Match either a favorite flag or a positive user rating.
    FavoriteOrLiked,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MediaItemCatalogSearchScope {
    /// Search only the persisted item name.
    #[default]
    Name,
    /// Search every scalar metadata value, but never JSON object keys.
    AllMetadataScalars,
    /// Match Jellyfin search hints exactly: album, album artist, series and artist values.
    SearchHintFields,
}

/// SQL-pushdown surface for the `/Items` catalog hot path.
///
/// String lists are matched case-insensitively and empty lists do not filter. `item_types` use the
/// public Jellyfin names (`Movie`, `Episode`, `Video`, `Audio`, `Photo`, `Book`, `MusicVideo`, and
/// `BaseItem`). Search is always applied to the item name; `search_scope` controls the additional
/// metadata values inspected by the dialect-native query.
#[derive(Debug, Clone)]
pub struct MediaItemCatalogQuery {
    pub start_index: usize,
    pub limit: usize,
    pub ids: Vec<Uuid>,
    pub virtual_folder_ids: Vec<Uuid>,
    pub include_item_types: Vec<String>,
    pub exclude_item_types: Vec<String>,
    pub collection_types: Vec<String>,
    pub media_types: Vec<String>,
    pub containers: Vec<String>,
    pub video_types: Vec<String>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    /// Genre names, stable IDs or imported entity IDs. Any selector may match.
    pub genre_ids: Vec<String>,
    /// Person names, stable IDs or imported entity IDs. Any selector may match.
    pub person_ids: Vec<String>,
    /// Studio names, stable IDs or imported entity IDs. Any selector may match.
    pub studio_ids: Vec<String>,
    /// Raw tag values. Any selector may match.
    pub tags: Vec<String>,
    /// Official rating values projected from item and inherited series metadata.
    pub official_ratings: Vec<String>,
    /// Series status values projected from item metadata.
    pub series_statuses: Vec<String>,
    /// Production years projected from item metadata.
    pub years: Vec<String>,
    pub location_types: Vec<String>,
    pub exclude_location_types: Vec<String>,
    pub search_term: Option<String>,
    pub search_scope: MediaItemCatalogSearchScope,
    pub has_subtitles: Option<bool>,
    pub has_trailer: Option<bool>,
    pub has_overview: Option<bool>,
    pub has_imdb_id: Option<bool>,
    pub has_tmdb_id: Option<bool>,
    pub has_tvdb_id: Option<bool>,
    pub has_official_rating: Option<bool>,
    pub is_locked: Option<bool>,
    pub is_hd: Option<bool>,
    pub is_4k: Option<bool>,
    pub min_width: Option<i64>,
    pub max_width: Option<i64>,
    pub min_height: Option<i64>,
    pub max_height: Option<i64>,
    pub min_community_rating: Option<f64>,
    pub max_community_rating: Option<f64>,
    pub min_critic_rating: Option<f64>,
    pub max_critic_rating: Option<f64>,
    pub min_premiere_date: Option<OffsetDateTime>,
    pub max_premiere_date: Option<OffsetDateTime>,
    pub is_missing: Option<bool>,
    pub is_unaired: Option<bool>,
    pub is_folder: Option<bool>,
    pub min_date_created: Option<OffsetDateTime>,
    pub max_date_created: Option<OffsetDateTime>,
    pub min_date_last_saved: Option<OffsetDateTime>,
    pub max_date_last_saved: Option<OffsetDateTime>,
    pub name_starts_with: Option<String>,
    pub name_starts_with_or_greater: Option<String>,
    pub name_less_than: Option<String>,
    pub user_id: Option<Uuid>,
    pub is_played: Option<bool>,
    pub favorite: Option<MediaItemFavoriteFilter>,
    pub is_resumable: bool,
    pub sort: Vec<(MediaItemCatalogSortField, SortDirection)>,
}

impl Default for MediaItemCatalogQuery {
    fn default() -> Self {
        Self {
            start_index: 0,
            limit: 100,
            ids: Vec::new(),
            virtual_folder_ids: Vec::new(),
            include_item_types: Vec::new(),
            exclude_item_types: Vec::new(),
            collection_types: Vec::new(),
            media_types: Vec::new(),
            containers: Vec::new(),
            video_types: Vec::new(),
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            genre_ids: Vec::new(),
            person_ids: Vec::new(),
            studio_ids: Vec::new(),
            tags: Vec::new(),
            official_ratings: Vec::new(),
            series_statuses: Vec::new(),
            years: Vec::new(),
            location_types: Vec::new(),
            exclude_location_types: Vec::new(),
            search_term: None,
            search_scope: MediaItemCatalogSearchScope::Name,
            has_subtitles: None,
            has_trailer: None,
            has_overview: None,
            has_imdb_id: None,
            has_tmdb_id: None,
            has_tvdb_id: None,
            has_official_rating: None,
            is_locked: None,
            is_hd: None,
            is_4k: None,
            min_width: None,
            max_width: None,
            min_height: None,
            max_height: None,
            min_community_rating: None,
            max_community_rating: None,
            min_critic_rating: None,
            max_critic_rating: None,
            min_premiere_date: None,
            max_premiere_date: None,
            is_missing: None,
            is_unaired: None,
            is_folder: None,
            min_date_created: None,
            max_date_created: None,
            min_date_last_saved: None,
            max_date_last_saved: None,
            name_starts_with: None,
            name_starts_with_or_greater: None,
            name_less_than: None,
            user_id: None,
            is_played: None,
            favorite: None,
            is_resumable: false,
            sort: vec![(
                MediaItemCatalogSortField::SortName,
                SortDirection::Ascending,
            )],
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaItemCatalogEntry {
    pub item: MediaItem,
    pub metadata: Value,
    pub playback_state: Option<PlaybackState>,
}

/// Resolve the date used by Jellyfin's Upcoming view without accepting looser SQL date coercions.
///
/// Key precedence is significant: once an earlier key exists, an invalid/non-string value does
/// not fall through to a later key. Keeping this parser shared by the repositories and API avoids
/// adapter-specific behaviour for legacy metadata.
pub fn upcoming_media_item_premiere_date(metadata: &Value) -> Option<OffsetDateTime> {
    metadata
        .get("PremiereDate")
        .or_else(|| metadata.get("AirDate"))
        .or_else(|| metadata.get("DateCreated"))
        .and_then(Value::as_str)
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
}

pub(crate) fn upcoming_media_item_premiere_parts(metadata: &Value) -> Option<(i64, i32)> {
    let date = upcoming_media_item_premiere_date(metadata)?;
    Some((
        date.unix_timestamp(),
        i32::try_from(date.nanosecond()).ok()?,
    ))
}

fn is_upcoming_media_item_entry(entry: &MediaItemCatalogEntry, now: OffsetDateTime) -> bool {
    effective_media_item_type(&entry.item) == "Episode"
        && upcoming_media_item_premiere_date(&entry.metadata).is_some_and(|date| date > now)
}

#[derive(Debug, Default)]
pub(crate) struct EffectiveTypeCandidateScope {
    pub(crate) all_raw_media_types: bool,
    pub(crate) all_video: bool,
    pub(crate) video_collection_types: BTreeSet<&'static str>,
    pub(crate) raw_media_types: BTreeSet<&'static str>,
}

impl EffectiveTypeCandidateScope {
    pub(crate) fn from_effective_types(item_types: &[String]) -> Self {
        let mut scope = Self::default();
        for item_type in item_types {
            match item_type.trim().to_ascii_lowercase().as_str() {
                "movie" => {
                    scope.video_collection_types.insert("movies");
                }
                "musicvideo" => {
                    scope.video_collection_types.insert("musicvideos");
                    scope.video_collection_types.insert("musicvideo");
                }
                "episode" => {
                    scope.video_collection_types.insert("tvshows");
                    scope.video_collection_types.insert("tvshow");
                    scope.video_collection_types.insert("series");
                }
                "video" => scope.all_video = true,
                "audio" => {
                    scope.raw_media_types.insert("Audio");
                }
                "photo" => {
                    scope.raw_media_types.insert("Photo");
                }
                "book" => {
                    scope.raw_media_types.insert("Book");
                }
                "baseitem" => scope.all_raw_media_types = true,
                _ => {}
            }
        }
        scope
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.all_raw_media_types
            && !self.all_video
            && self.video_collection_types.is_empty()
            && self.raw_media_types.is_empty()
    }
}

pub(crate) fn retain_entries_with_effective_types(
    entries: Vec<MediaItemCatalogEntry>,
    item_types: &[String],
) -> Vec<MediaItemCatalogEntry> {
    let requested = item_types
        .iter()
        .map(|item_type| item_type.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    entries
        .into_iter()
        .filter(|entry| {
            requested.contains(&effective_media_item_type(&entry.item).to_ascii_lowercase())
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct MediaItemCatalogPage {
    pub items: Vec<MediaItemCatalogEntry>,
    pub total_record_count: usize,
    pub start_index: usize,
}

/// A bounded page of TV series keys and the episodes belonging to the requested page.
/// Explicit provider series anchors may produce a key with no episode rows. `None` means at least
/// one visible source row lacks a canonical persisted SeriesId/SeriesName, so callers must preserve
/// the legacy path-derived grouping semantics.
#[derive(Debug, Clone)]
pub struct TvSeriesCatalogPage {
    pub series: Vec<TvSeriesCatalogKey>,
    pub episodes: Vec<MediaItemCatalogEntry>,
    pub total_record_count: usize,
    pub start_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvSeriesCatalogKey {
    pub id: String,
    pub name: String,
}

/// Name predicates applied before counting and paging the synthetic TV-Series projection.
#[derive(Debug, Clone, Default)]
pub struct TvSeriesCatalogNameFilter {
    pub search_term: Option<String>,
    pub starts_with: Option<String>,
    pub starts_with_or_greater: Option<String>,
    pub less_than: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct TvSeriesCatalogNamePatterns<'a> {
    search: Option<&'a str>,
    starts_with: Option<&'a str>,
    lower_bound: Option<&'a str>,
    upper_bound: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaItemCatalogCounts {
    pub movie_count: u64,
    pub series_count: u64,
    pub episode_count: u64,
    pub artist_count: u64,
    pub trailer_count: u64,
    pub song_count: u64,
    pub album_count: u64,
    pub music_video_count: u64,
    pub book_count: u64,
    pub item_count: u64,
}

#[derive(Default)]
struct CatalogMetadataCountAccumulator {
    albums: BTreeSet<String>,
    artists: BTreeSet<String>,
    trailers: u64,
}

impl CatalogMetadataCountAccumulator {
    fn add_album(&mut self, raw: Option<&str>) -> anyhow::Result<()> {
        if let Some(value) = parse_catalog_count_json(raw)? {
            collect_catalog_count_metadata_value(&value, &mut self.albums);
        }
        Ok(())
    }

    fn add_artist(&mut self, raw: Option<&str>) -> anyhow::Result<()> {
        if let Some(value) = parse_catalog_count_json(raw)? {
            collect_catalog_count_metadata_value(&value, &mut self.artists);
        }
        Ok(())
    }

    fn add_trailers(&mut self, raw: Option<&str>) -> anyhow::Result<()> {
        if let Some(value) = parse_catalog_count_json(raw)? {
            self.trailers = self
                .trailers
                .checked_add(count_catalog_trailer_values(&value)?)
                .context("trailer count overflow")?;
        }
        Ok(())
    }
}

fn parse_catalog_count_json(raw: Option<&str>) -> anyhow::Result<Option<Value>> {
    raw.map(serde_json::from_str)
        .transpose()
        .context("invalid projected catalog metadata JSON")
}

fn collect_catalog_count_metadata_value(value: &Value, values: &mut BTreeSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_catalog_count_metadata_value(item, values);
            }
        }
        Value::String(value) => insert_catalog_count_metadata_value(value, values),
        Value::Number(value) => {
            insert_catalog_count_metadata_value(&value.to_string(), values);
        }
        Value::Object(object) => {
            if let Some(name) = object.get("Name").and_then(Value::as_str) {
                insert_catalog_count_metadata_value(name, values);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn insert_catalog_count_metadata_value(value: &str, values: &mut BTreeSet<String>) {
    let value = value.trim();
    if !value.is_empty() {
        values.insert(value.to_ascii_lowercase());
    }
}

fn count_catalog_trailer_values(value: &Value) -> anyhow::Result<u64> {
    match value {
        Value::Array(values) => values.iter().try_fold(0u64, |count, value| {
            count
                .checked_add(count_catalog_trailer_values(value)?)
                .context("trailer count overflow")
        }),
        Value::String(url) => Ok(u64::from(!url.trim().is_empty())),
        Value::Object(object) => Ok(u64::from(
            object
                .get("Url")
                .or_else(|| object.get("url"))
                .or_else(|| object.get("Path"))
                .or_else(|| object.get("path"))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()),
        )),
        Value::Number(_) | Value::Bool(_) | Value::Null => Ok(0),
    }
}

#[derive(Debug, Clone)]
pub struct RemoteMediaItemUpsert {
    pub id: String,
    pub name: String,
    pub path: String,
    pub media_type: String,
    pub collection_type: String,
    pub runtime_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub media_streams: Vec<Value>,
    pub metadata: Value,
}

/// One complete provider-owned media library ready to be published.
///
/// A provider that exposes related libraries (for example Xtream movies and series) submits them
/// together through [`XtreamCatalogStore`]. The database adapter is then responsible for making
/// the complete batch visible in one transaction, including intentionally empty libraries.
#[derive(Debug, Clone)]
pub struct RemoteMediaLibrarySnapshot {
    pub library_name: String,
    pub collection_type: String,
    pub source_location: String,
    pub items: Vec<RemoteMediaItemUpsert>,
}

/// Maximum number of provider-owned media items accepted by one durable stage append.
///
/// Keeping this public lets producers bound their buffers to the same contract enforced by both
/// database adapters. An empty append is rejected as a caller error.
pub const REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS: usize = 1_000;
/// Maximum retained item count for each movies/series library in one durable stage.
pub const REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS: usize = 1_000_000;

/// Opaque handle for one durable, not-yet-visible remote media catalogue generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMediaCatalogStage {
    id: String,
}

impl RemoteMediaCatalogStage {
    fn new(id: Uuid) -> Self {
        Self { id: id.to_string() }
    }

    /// Reconstruct an opaque handle received from a trusted scheduling boundary.
    pub fn try_from_id(id: impl AsRef<str>) -> anyhow::Result<Self> {
        let id =
            Uuid::parse_str(id.as_ref()).context("invalid remote media catalogue stage handle")?;
        Ok(Self::new(id))
    }

    /// Stable opaque identifier suitable for diagnostics that do not include provider secrets.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn parsed_id(&self) -> anyhow::Result<Uuid> {
        Uuid::parse_str(&self.id).context("invalid remote media catalogue stage handle")
    }
}

/// A complete durable stage that can be published without contacting its provider again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyRemoteMediaCatalogStage {
    pub stage: RemoteMediaCatalogStage,
    pub movie_count: usize,
    pub series_item_count: usize,
}

/// One named library slot belonging to a durable remote media catalogue stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMediaLibraryStageSpec {
    pub key: String,
    pub library_name: String,
    pub collection_type: String,
    pub source_location: String,
}

#[derive(Debug, Clone)]
struct PreparedRemoteMediaLibraryStageSpec {
    key: String,
    position: i16,
    library_name: String,
    collection_type: String,
    source_location: String,
}

fn prepare_remote_media_library_stage_specs(
    specs: Vec<RemoteMediaLibraryStageSpec>,
) -> anyhow::Result<Vec<PreparedRemoteMediaLibraryStageSpec>> {
    anyhow::ensure!(
        specs.len() == 2,
        "remote media catalogue stage requires exactly two libraries"
    );
    let mut prepared = Vec::with_capacity(2);
    let mut keys = HashSet::with_capacity(2);
    let mut names = HashSet::with_capacity(2);
    for spec in specs {
        let position = match spec.key.as_str() {
            "movies" => 0,
            "series" => 1,
            _ => {
                anyhow::bail!("remote media catalogue stage keys must be exactly movies and series")
            }
        };
        anyhow::ensure!(
            keys.insert(spec.key.clone()),
            "remote media catalogue stage contains duplicate library keys"
        );
        let library_name = spec.library_name.trim().to_owned();
        anyhow::ensure!(
            !library_name.is_empty(),
            "remote media catalogue stage library name must not be empty"
        );
        anyhow::ensure!(
            names.insert(library_name.to_ascii_lowercase()),
            "remote media catalogue stage contains duplicate library names"
        );
        prepared.push(PreparedRemoteMediaLibraryStageSpec {
            key: spec.key,
            position,
            library_name,
            collection_type: spec.collection_type.trim().to_owned(),
            source_location: spec.source_location.trim().to_owned(),
        });
    }
    anyhow::ensure!(
        keys.contains("movies") && keys.contains("series"),
        "remote media catalogue stage requires movies and series libraries"
    );
    prepared.sort_unstable_by_key(|spec| spec.position);
    Ok(prepared)
}

fn remote_media_stage_source_revision(value: &str) -> anyhow::Result<&str> {
    let value = value.trim();
    anyhow::ensure!(
        value.len() <= 256 && !value.chars().any(char::is_control),
        "remote media catalogue source revision is invalid"
    );
    Ok(value)
}

fn validate_remote_media_catalog_stage_append(
    items: &[RemoteMediaItemUpsert],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !items.is_empty(),
        "remote media catalogue stage append must not be empty"
    );
    anyhow::ensure!(
        items.len() <= REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS,
        "remote media catalogue stage append exceeds its item limit"
    );
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
struct PreparedSqliteRemoteMediaItem {
    id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: String,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams_json: String,
    metadata_json: String,
}

#[cfg(any(test, feature = "sqlite"))]
struct PreparedSqliteRemoteMediaLibrarySnapshot {
    library_name: String,
    collection_type: String,
    source_location: String,
    items: Vec<PreparedSqliteRemoteMediaItem>,
}

#[cfg(any(test, feature = "sqlite"))]
impl TryFrom<RemoteMediaLibrarySnapshot> for PreparedSqliteRemoteMediaLibrarySnapshot {
    type Error = anyhow::Error;

    fn try_from(snapshot: RemoteMediaLibrarySnapshot) -> Result<Self, Self::Error> {
        let library_name = snapshot.library_name.trim().to_owned();
        anyhow::ensure!(
            !library_name.is_empty(),
            "virtual folder name must not be empty"
        );
        let items = snapshot
            .items
            .into_iter()
            .map(PreparedSqliteRemoteMediaItem::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            library_name,
            collection_type: snapshot.collection_type.trim().to_owned(),
            source_location: snapshot.source_location.trim().to_owned(),
            items,
        })
    }
}

#[cfg(any(test, feature = "sqlite"))]
impl TryFrom<RemoteMediaItemUpsert> for PreparedSqliteRemoteMediaItem {
    type Error = anyhow::Error;

    fn try_from(item: RemoteMediaItemUpsert) -> Result<Self, Self::Error> {
        let raw_id = item.id.trim();
        Ok(Self {
            id: Uuid::parse_str(raw_id)
                .with_context(|| format!("invalid remote media item id: {raw_id}"))?
                .to_string(),
            name: item.name.trim().to_owned(),
            path: item.path.trim().to_owned(),
            media_type: item.media_type.trim().to_owned(),
            collection_type: item.collection_type.trim().to_owned(),
            runtime_ticks: item.runtime_ticks,
            bitrate: item.bitrate,
            width: item.width,
            height: item.height,
            media_streams_json: serde_json::to_string(&item.media_streams)?,
            metadata_json: serde_json::to_string(&item.metadata)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MediaItemLyrics {
    pub item_id: Uuid,
    pub payload: Value,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaList {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub collection_type: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub metadata: Value,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaListItem {
    pub item: MediaItem,
    pub playlist_item_id: Uuid,
    pub position: i64,
    pub added_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MediaListUserPermission {
    pub list_id: Uuid,
    pub user: User,
    pub can_edit: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct QuickConnectSession {
    pub secret: String,
    pub code: String,
    pub device_id: String,
    pub device_name: String,
    pub client: String,
    pub version: String,
    pub user_id: Option<Uuid>,
    pub authorized: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    pub runtime_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub media_streams: Vec<Value>,
    pub metadata: Value,
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
pub struct ActiveViewingSession {
    pub session_id: String,
    pub user_id: Uuid,
    pub item: MediaItem,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ActiveSessionUser {
    pub session_id: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub added_at: OffsetDateTime,
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
pub struct UpsertActiveViewingSession {
    pub session_id: String,
    pub user_id: Uuid,
    pub item_id: Uuid,
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

#[cfg(any(test, feature = "sqlite"))]
#[derive(Debug, Clone, Default)]
struct ExistingUserItemData {
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    is_favorite: bool,
    rating: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct TranscodeSession {
    pub play_session_id: String,
    pub dedupe_key: Option<String>,
    pub device_id: Option<String>,
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
    pub start_position_ticks: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct UpsertTranscodeSession {
    pub play_session_id: String,
    pub dedupe_key: Option<String>,
    pub device_id: Option<String>,
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
    pub start_position_ticks: i64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrickplayInfo {
    pub item_id: Uuid,
    pub width: i64,
    pub height: i64,
    pub tile_width: i64,
    pub tile_height: i64,
    pub thumbnail_count: i64,
    pub interval_ms: i64,
    pub bandwidth: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
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
    pub item_id: Option<Uuid>,
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
    pub item_id: Option<Uuid>,
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
            item_id: None,
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

#[derive(Debug, Clone, PartialEq)]
pub struct NamedConfigurationPayload {
    pub key: String,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct InstallPluginPackage {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub runtime: String,
    pub target_abi: String,
    pub package: Value,
    pub manifest: Value,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPluginPackage {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub runtime: String,
    pub target_abi: String,
    pub manifest: Value,
    pub install_path: String,
}

#[derive(Debug, Clone)]
pub struct PluginRuntimeInstanceUpsert {
    pub plugin_id: String,
    pub runtime: String,
    pub runtime_version: String,
    pub status: String,
    pub process_id: Option<i64>,
    pub endpoint: Option<String>,
    pub health: Value,
    pub capabilities: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LiveTvTunerUpsert {
    pub tuner_id: String,
    pub provider_type: String,
    pub name: String,
    pub source_url: Option<String>,
    pub configuration: Value,
}

#[derive(Debug, Clone)]
pub struct LiveTvCategoryUpsert {
    pub category_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct LiveTvChannelUpsert {
    pub channel_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    pub category_id: Option<String>,
    pub name: String,
    pub sort_name: String,
    pub number: Option<String>,
    pub stream_url: String,
    pub logo_url: Option<String>,
    pub channel_type: String,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct LiveTvChannelRecord {
    pub channel_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub name: String,
    pub sort_name: String,
    pub number: Option<String>,
    pub stream_url: String,
    pub logo_url: Option<String>,
    pub channel_type: String,
    pub metadata: Value,
}

/// Versioned outcome of a bounded Live TV stream probe.
///
/// This intentionally contains no diagnostic text: provider errors and raw ffprobe output can
/// include credential-bearing source URLs and must remain outside durable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveTvStreamProbeOutcome {
    Tracks,
    Empty,
    Failed,
    Unsupported,
}

impl LiveTvStreamProbeOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Empty => "empty",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }

    fn from_stored(value: &str) -> anyhow::Result<Self> {
        match value {
            "tracks" => Ok(Self::Tracks),
            "empty" => Ok(Self::Empty),
            "failed" => Ok(Self::Failed),
            "unsupported" => Ok(Self::Unsupported),
            _ => anyhow::bail!("invalid persisted Live TV stream probe outcome"),
        }
    }
}

/// Sanitized, derived cache entry written after probing one revision of a live stream.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveTvStreamProbeUpsert {
    pub channel_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    /// Credential-free stable digest of provider reference, configuration revision and probe ABI.
    pub source_revision: String,
    pub probe_version: i16,
    pub outcome: LiveTvStreamProbeOutcome,
    /// A bounded array of typed stream descriptors, never raw ffprobe JSON.
    pub streams: Value,
    pub observed_at: OffsetDateTime,
    pub completed_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTvStreamProbeRecord {
    pub channel_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    pub source_revision: String,
    pub probe_version: i16,
    pub outcome: LiveTvStreamProbeOutcome,
    pub streams: Value,
    pub observed_at: OffsetDateTime,
    pub completed_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

const LIVE_TV_STREAM_PROBE_MAX_STREAMS: usize = 32;
const LIVE_TV_STREAM_PROBE_MAX_JSON_BYTES: usize = 64 * 1024;

fn validate_live_tv_stream_probe(probe: &LiveTvStreamProbeUpsert) -> anyhow::Result<()> {
    for (name, value) in [
        ("channel id", probe.channel_id.trim()),
        ("tuner id", probe.tuner_id.trim()),
        ("remote id", probe.remote_id.trim()),
    ] {
        anyhow::ensure!(
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
            "Live TV stream probe {name} is invalid"
        );
    }
    let revision = probe.source_revision.trim();
    anyhow::ensure!(
        (16..=128).contains(&revision.len())
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "Live TV stream probe source revision is invalid"
    );
    anyhow::ensure!(
        probe.probe_version > 0,
        "Live TV stream probe version must be positive"
    );
    let streams = probe
        .streams
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Live TV stream probe streams must be an array"))?;
    anyhow::ensure!(
        streams.len() <= LIVE_TV_STREAM_PROBE_MAX_STREAMS,
        "Live TV stream probe contains too many streams"
    );
    anyhow::ensure!(
        serde_json::to_vec(&probe.streams)?.len() <= LIVE_TV_STREAM_PROBE_MAX_JSON_BYTES,
        "Live TV stream probe streams payload is too large"
    );
    anyhow::ensure!(
        probe.completed_at >= probe.observed_at,
        "Live TV stream probe completed before it was observed"
    );
    anyhow::ensure!(
        probe.expires_at > probe.completed_at,
        "Live TV stream probe expiry must follow completion"
    );
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LiveTvCategoryRecord {
    pub category_id: String,
    pub tuner_id: String,
    pub remote_id: String,
    pub name: String,
    pub sort_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct LiveTvChannelQuery {
    pub start_index: usize,
    pub limit: Option<usize>,
    pub search_term: Option<String>,
    /// Resolved tuner ids to match (OR). Empty = channels from every tuner.
    pub tuner_ids: Vec<String>,
    /// Resolved category ids to match (OR). Empty = no category filter.
    pub category_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LiveTvPage<T> {
    pub items: Vec<T>,
    pub total_record_count: usize,
    pub start_index: usize,
}

/// Dialect-neutral catalog contract with dialect-native implementations.
///
/// This is deliberately a narrow domain repository, not a generic query/connection abstraction.
/// A future driver must implement the paging/count/user-data semantics natively and pass the same
/// conformance tests before it can become a production backend.
pub trait MediaCatalogStore: DatabaseBackend {
    /// Whether an exact catalog id identifies a currently visible media item.
    fn media_item_exists(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_;

    /// Exact visible item lookup that keeps absence distinct from database failure.
    fn media_item_by_id_visible(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItem>>> + Send + '_;

    fn media_item_catalog_page<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogPage>> + Send + 'a;

    fn media_item_catalog_counts<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogCounts>> + Send + 'a;

    fn media_item_query_filter_values<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemQueryFilterValues>> + Send + 'a;

    fn playback_states_for_items<'a>(
        &'a self,
        user_id: Uuid,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<PlaybackState>>> + Send + 'a;

    /// Exact visible catalogue candidates for the requested public Jellyfin item types, with
    /// metadata loaded inline and without an artificial page limit. Type matching uses the same
    /// effective-type rules as `media_item_catalog_page`; an empty type list returns no rows.
    fn media_items_with_metadata_by_effective_types<'a>(
        &'a self,
        item_types: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a;

    /// Visible TV episode candidates with metadata, used to resolve synthetic Jellyfin Series
    /// identifiers without materializing unrelated library domains.
    fn tv_series_lookup_candidates(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_;

    /// The same candidates restricted to one persisted `SeriesId`, so resolving a single series
    /// does
    /// not materialize every episode in the library.
    fn tv_series_lookup_candidates_for_series<'a>(
        &'a self,
        series_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a;

    /// The same candidates restricted to one persisted `SeasonId`.
    fn tv_series_lookup_candidates_for_season<'a>(
        &'a self,
        season_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a;

    /// The same candidates restricted to rows without a canonical persisted `SeriesId`, the exact
    /// fallback scope for name-derived synthetic ids.
    fn tv_series_lookup_candidates_without_canonical_series_id(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_;

    /// Publish the TV series projection for a folder whose coverage row is missing, so the catalogue
    /// stops answering from the bounded live page. `false` means the data is not projectable.
    fn ensure_tv_series_catalog_projection(
        &self,
        virtual_folder_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_;

    fn tv_series_catalog_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_;

    /// The bounded synthetic-Series page with case-insensitive name predicates applied in the
    /// database projection before counting and paging.
    fn tv_series_catalog_search_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
        filter: TvSeriesCatalogNameFilter,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_;

    /// Visible TV candidates whose user-data row is either absent or unplayed.
    ///
    /// This is the SQL-prefiltered input for Jellyfin's common `/Shows/NextUp` request. The API
    /// intentionally retains episode classification and one-per-series selection because those
    /// rules include path-derived compatibility semantics.
    fn tv_next_up_candidates(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_;

    /// The same candidates without their `media_streams` payload.
    ///
    /// Path-derived selection never reads streams or metadata, so fetching those JSONB columns for
    /// every episode only to discard all but one per series is what made the candidate fetch exceed
    /// the API statement timeout. `media_streams` arrives empty, so callers must hydrate the page
    /// they retain with `media_items_by_ids` before serializing it.
    fn tv_next_up_candidate_items(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + '_;

    /// Exact visible items for a bounded id list, preserving the caller's order.
    fn media_items_by_ids<'a>(
        &'a self,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + 'a;

    /// Visible TV video candidates carrying inline metadata for the common `/Shows/Upcoming`
    /// request. A shared Rust predicate retains effective episode classification and strict
    /// RFC3339 date semantics; the API retains exact total calculation and final ordering.
    fn tv_upcoming_candidates(
        &self,
        now: OffsetDateTime,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_;

    fn media_item_facet_values<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemFacetValue>>> + Send + 'a;

    fn media_item_facet_by_entity_id<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        entity_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a;

    fn media_item_facet_by_normalized_value<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        value: &'a str,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a;

    fn media_item_ids_for_facets<'a>(
        &'a self,
        query: &'a MediaItemFacetCandidateQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Uuid>>> + Send + 'a;

    fn rebuild_media_item_facets(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + '_;
}

impl MediaCatalogStore for PostgresDatabase {
    fn media_item_exists(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_ {
        PostgresDatabase::media_item_exists(self, item_id)
    }

    fn media_item_by_id_visible(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItem>>> + Send + '_ {
        PostgresDatabase::media_item_by_id_visible(self, item_id)
    }

    fn media_item_catalog_page<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogPage>> + Send + 'a {
        PostgresDatabase::media_item_catalog_page(self, query)
    }

    fn media_item_catalog_counts<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogCounts>> + Send + 'a {
        PostgresDatabase::media_item_catalog_counts(self, query)
    }

    fn media_item_query_filter_values<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemQueryFilterValues>> + Send + 'a
    {
        PostgresDatabase::media_item_query_filter_values(self, query, selection)
    }

    fn playback_states_for_items<'a>(
        &'a self,
        user_id: Uuid,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<PlaybackState>>> + Send + 'a {
        PostgresDatabase::playback_states_for_items(self, user_id, item_ids)
    }

    fn media_items_with_metadata_by_effective_types<'a>(
        &'a self,
        item_types: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        PostgresDatabase::media_items_with_metadata_by_effective_types(self, item_types)
    }

    fn tv_series_lookup_candidates(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        PostgresDatabase::tv_series_lookup_candidates(self)
    }

    fn tv_series_lookup_candidates_for_series<'a>(
        &'a self,
        series_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        PostgresDatabase::tv_series_lookup_candidates_for_series(self, series_id)
    }

    fn tv_series_lookup_candidates_for_season<'a>(
        &'a self,
        season_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        PostgresDatabase::tv_series_lookup_candidates_for_season(self, season_id)
    }

    fn tv_series_lookup_candidates_without_canonical_series_id(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        PostgresDatabase::tv_series_lookup_candidates_without_canonical_series_id(self)
    }

    fn ensure_tv_series_catalog_projection(
        &self,
        virtual_folder_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_ {
        PostgresDatabase::ensure_tv_series_catalog_projection(self, virtual_folder_id)
    }

    fn tv_series_catalog_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_
    {
        PostgresDatabase::tv_series_catalog_page(self, virtual_folder_id, start_index, limit)
    }

    fn tv_series_catalog_search_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
        filter: TvSeriesCatalogNameFilter,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_
    {
        PostgresDatabase::tv_series_catalog_search_page(
            self,
            virtual_folder_id,
            start_index,
            limit,
            filter,
        )
    }

    fn tv_next_up_candidates(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        PostgresDatabase::tv_next_up_candidates(self, user_id)
    }

    fn tv_next_up_candidate_items(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + '_ {
        PostgresDatabase::tv_next_up_candidate_items(self, user_id)
    }

    fn media_items_by_ids<'a>(
        &'a self,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + 'a {
        PostgresDatabase::media_items_by_ids(self, item_ids)
    }

    fn tv_upcoming_candidates(
        &self,
        now: OffsetDateTime,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        PostgresDatabase::tv_upcoming_candidates(self, now)
    }

    fn media_item_facet_values<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemFacetValue>>> + Send + 'a
    {
        PostgresDatabase::media_item_facet_values(self, kind, virtual_folder_ids)
    }

    fn media_item_facet_by_entity_id<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        entity_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a
    {
        PostgresDatabase::media_item_facet_by_entity_id(self, kind, entity_id)
    }

    fn media_item_facet_by_normalized_value<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        value: &'a str,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a
    {
        PostgresDatabase::media_item_facet_by_normalized_value(
            self,
            kind,
            value,
            virtual_folder_ids,
        )
    }

    fn media_item_ids_for_facets<'a>(
        &'a self,
        query: &'a MediaItemFacetCandidateQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Uuid>>> + Send + 'a {
        PostgresDatabase::media_item_ids_for_facets(self, query)
    }

    fn rebuild_media_item_facets(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + '_ {
        PostgresDatabase::rebuild_media_item_facets(self)
    }
}

#[cfg(any(test, feature = "sqlite"))]
impl MediaCatalogStore for SqliteDatabase {
    fn media_item_exists(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_ {
        SqliteDatabase::media_item_exists(self, item_id)
    }

    fn media_item_by_id_visible(
        &self,
        item_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItem>>> + Send + '_ {
        SqliteDatabase::media_item_by_id_visible(self, item_id)
    }

    fn media_item_catalog_page<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogPage>> + Send + 'a {
        SqliteDatabase::media_item_catalog_page(self, query)
    }

    fn media_item_catalog_counts<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemCatalogCounts>> + Send + 'a {
        SqliteDatabase::media_item_catalog_counts(self, query)
    }

    fn media_item_query_filter_values<'a>(
        &'a self,
        query: &'a MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> impl std::future::Future<Output = anyhow::Result<MediaItemQueryFilterValues>> + Send + 'a
    {
        SqliteDatabase::media_item_query_filter_values(self, query, selection)
    }

    fn playback_states_for_items<'a>(
        &'a self,
        user_id: Uuid,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<PlaybackState>>> + Send + 'a {
        SqliteDatabase::playback_states_for_items(self, user_id, item_ids)
    }

    fn media_items_with_metadata_by_effective_types<'a>(
        &'a self,
        item_types: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        SqliteDatabase::media_items_with_metadata_by_effective_types(self, item_types)
    }

    fn tv_series_lookup_candidates(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        SqliteDatabase::tv_series_lookup_candidates(self)
    }

    fn tv_series_lookup_candidates_for_series<'a>(
        &'a self,
        series_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        SqliteDatabase::tv_series_lookup_candidates_for_series(self, series_id)
    }

    fn tv_series_lookup_candidates_for_season<'a>(
        &'a self,
        season_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + 'a
    {
        SqliteDatabase::tv_series_lookup_candidates_for_season(self, season_id)
    }

    fn tv_series_lookup_candidates_without_canonical_series_id(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        SqliteDatabase::tv_series_lookup_candidates_without_canonical_series_id(self)
    }

    fn ensure_tv_series_catalog_projection(
        &self,
        virtual_folder_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send + '_ {
        SqliteDatabase::ensure_tv_series_catalog_projection(self, virtual_folder_id)
    }

    fn tv_series_catalog_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_
    {
        SqliteDatabase::tv_series_catalog_page(self, virtual_folder_id, start_index, limit)
    }

    fn tv_series_catalog_search_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
        filter: TvSeriesCatalogNameFilter,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TvSeriesCatalogPage>>> + Send + '_
    {
        SqliteDatabase::tv_series_catalog_search_page(
            self,
            virtual_folder_id,
            start_index,
            limit,
            filter,
        )
    }

    fn tv_next_up_candidates(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        SqliteDatabase::tv_next_up_candidates(self, user_id)
    }

    fn tv_next_up_candidate_items(
        &self,
        user_id: Uuid,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + '_ {
        SqliteDatabase::tv_next_up_candidate_items(self, user_id)
    }

    fn media_items_by_ids<'a>(
        &'a self,
        item_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItem>>> + Send + 'a {
        SqliteDatabase::media_items_by_ids(self, item_ids)
    }

    fn tv_upcoming_candidates(
        &self,
        now: OffsetDateTime,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemCatalogEntry>>> + Send + '_
    {
        SqliteDatabase::tv_upcoming_candidates(self, now)
    }

    fn media_item_facet_values<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<MediaItemFacetValue>>> + Send + 'a
    {
        SqliteDatabase::media_item_facet_values(self, kind, virtual_folder_ids)
    }

    fn media_item_facet_by_entity_id<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        entity_id: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a
    {
        SqliteDatabase::media_item_facet_by_entity_id(self, kind, entity_id)
    }

    fn media_item_facet_by_normalized_value<'a>(
        &'a self,
        kind: MediaItemFacetKind,
        value: &'a str,
        virtual_folder_ids: &'a [Uuid],
    ) -> impl std::future::Future<Output = anyhow::Result<Option<MediaItemFacetValue>>> + Send + 'a
    {
        SqliteDatabase::media_item_facet_by_normalized_value(self, kind, value, virtual_folder_ids)
    }

    fn media_item_ids_for_facets<'a>(
        &'a self,
        query: &'a MediaItemFacetCandidateQuery,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Uuid>>> + Send + 'a {
        SqliteDatabase::media_item_ids_for_facets(self, query)
    }

    fn rebuild_media_item_facets(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + '_ {
        SqliteDatabase::rebuild_media_item_facets(self)
    }
}

/// Narrow persistence contract used by the external Xtream indexer. Keeping this trait in the
/// database boundary lets API unit tests exercise the legacy SQLite source while the production
/// runtime remains unconditionally PostgreSQL.
pub trait XtreamCatalogStore: DatabaseBackend {
    fn replace_remote_media_library_snapshots(
        &self,
        snapshots: Vec<RemoteMediaLibrarySnapshot>,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<VirtualFolder>>> + Send;

    fn begin_remote_media_catalog_stage(
        &self,
        _libraries: Vec<RemoteMediaLibraryStageSpec>,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteMediaCatalogStage>> + Send {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn begin_remote_media_catalog_stage_for_revision<'a>(
        &'a self,
        _libraries: Vec<RemoteMediaLibraryStageSpec>,
        _source_revision: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteMediaCatalogStage>> + Send + 'a
    {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn append_remote_media_catalog_stage<'a>(
        &'a self,
        _stage: &'a RemoteMediaCatalogStage,
        _library_key: &'a str,
        _items: Vec<RemoteMediaItemUpsert>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn complete_remote_media_catalog_stage<'a>(
        &'a self,
        _stage: &'a RemoteMediaCatalogStage,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn ready_remote_media_catalog_stage<'a>(
        &'a self,
        _libraries: Vec<RemoteMediaLibraryStageSpec>,
        _source_revision: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<ReadyRemoteMediaCatalogStage>>>
    + Send
    + 'a {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn publish_remote_media_catalog_stage<'a>(
        &'a self,
        _stage: &'a RemoteMediaCatalogStage,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<VirtualFolder>>> + Send + 'a {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn abort_remote_media_catalog_stage<'a>(
        &'a self,
        _stage: &'a RemoteMediaCatalogStage,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn cleanup_abandoned_remote_media_catalog_stages(
        &self,
        _older_than: OffsetDateTime,
    ) -> impl std::future::Future<Output = anyhow::Result<u64>> + Send {
        async { anyhow::bail!("durable remote media catalogue staging is not supported") }
    }

    fn live_tv_tuner_configurations_by_provider(
        &self,
        provider_type: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Value>>> + Send;
}

macro_rules! impl_xtream_catalog_store {
    ($database:ty) => {
        impl XtreamCatalogStore for $database {
            fn replace_remote_media_library_snapshots(
                &self,
                snapshots: Vec<RemoteMediaLibrarySnapshot>,
            ) -> impl std::future::Future<Output = anyhow::Result<Vec<VirtualFolder>>> + Send {
                <$database>::replace_remote_media_library_snapshots(self, snapshots)
            }

            fn begin_remote_media_catalog_stage(
                &self,
                libraries: Vec<RemoteMediaLibraryStageSpec>,
            ) -> impl std::future::Future<Output = anyhow::Result<RemoteMediaCatalogStage>> + Send
            {
                <$database>::begin_remote_media_catalog_stage(self, libraries)
            }

            fn begin_remote_media_catalog_stage_for_revision<'a>(
                &'a self,
                libraries: Vec<RemoteMediaLibraryStageSpec>,
                source_revision: &'a str,
            ) -> impl std::future::Future<Output = anyhow::Result<RemoteMediaCatalogStage>> + Send + 'a
            {
                <$database>::begin_remote_media_catalog_stage_for_revision(
                    self,
                    libraries,
                    source_revision,
                )
            }

            fn append_remote_media_catalog_stage<'a>(
                &'a self,
                stage: &'a RemoteMediaCatalogStage,
                library_key: &'a str,
                items: Vec<RemoteMediaItemUpsert>,
            ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
                <$database>::append_remote_media_catalog_stage(self, stage, library_key, items)
            }

            fn complete_remote_media_catalog_stage<'a>(
                &'a self,
                stage: &'a RemoteMediaCatalogStage,
            ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
                <$database>::complete_remote_media_catalog_stage(self, stage)
            }

            fn ready_remote_media_catalog_stage<'a>(
                &'a self,
                libraries: Vec<RemoteMediaLibraryStageSpec>,
                source_revision: &'a str,
            ) -> impl std::future::Future<Output = anyhow::Result<Option<ReadyRemoteMediaCatalogStage>>> + Send + 'a
            {
                <$database>::ready_remote_media_catalog_stage(self, libraries, source_revision)
            }

            fn publish_remote_media_catalog_stage<'a>(
                &'a self,
                stage: &'a RemoteMediaCatalogStage,
            ) -> impl std::future::Future<Output = anyhow::Result<Vec<VirtualFolder>>> + Send + 'a
            {
                <$database>::publish_remote_media_catalog_stage(self, stage)
            }

            fn abort_remote_media_catalog_stage<'a>(
                &'a self,
                stage: &'a RemoteMediaCatalogStage,
            ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
                <$database>::abort_remote_media_catalog_stage(self, stage)
            }

            fn cleanup_abandoned_remote_media_catalog_stages(
                &self,
                older_than: OffsetDateTime,
            ) -> impl std::future::Future<Output = anyhow::Result<u64>> + Send {
                <$database>::cleanup_abandoned_remote_media_catalog_stages(self, older_than)
            }

            fn live_tv_tuner_configurations_by_provider(
                &self,
                provider_type: &str,
            ) -> impl std::future::Future<Output = anyhow::Result<Vec<Value>>> + Send {
                <$database>::live_tv_tuner_configurations_by_provider(self, provider_type)
            }
        }
    };
}

impl_xtream_catalog_store!(PostgresDatabase);
#[cfg(any(test, feature = "sqlite"))]
impl_xtream_catalog_store!(SqliteDatabase);

#[cfg(any(test, feature = "sqlite"))]
const SQLITE_MEDIA_ITEM_TYPE_SQL: &str = r#"CASE
    WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('musicvideos', 'musicvideo') THEN 'musicvideo'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('tvshows', 'tvshow', 'series')
         AND (('/' || lower(item.path) || '/') LIKE '%/extras/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/featurettes/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/special features/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/behind the scenes/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/deleted scenes/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/interviews/%'
              OR ('/' || lower(item.path) || '/') LIKE '%/trailers/%') THEN 'video'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
    WHEN item.media_type = 'Video' THEN 'video'
    WHEN item.media_type = 'Audio' THEN 'audio'
    WHEN item.media_type = 'Photo' THEN 'photo'
    WHEN item.media_type = 'Book' THEN 'book'
    ELSE 'baseitem'
END"#;

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_catalog_from(builder: &mut QueryBuilder<Sqlite>, query: &MediaItemCatalogQuery) {
    builder.push(
        " FROM media_items AS item \
         LEFT JOIN playback_states AS playback \
           ON playback.item_id = item.id AND ",
    );
    if let Some(user_id) = query.user_id {
        builder
            .push("playback.user_id = ")
            .push_bind(user_id.to_string());
    } else {
        builder.push("0");
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_catalog_filters(
    builder: &mut QueryBuilder<Sqlite>,
    query: &MediaItemCatalogQuery,
) -> anyhow::Result<()> {
    builder.push(" WHERE item.missing_since IS NULL");

    if !query.ids.is_empty() {
        let ids = query
            .ids
            .iter()
            .flat_map(|id| [id.to_string(), id.simple().to_string()])
            .collect::<BTreeSet<_>>();
        push_sqlite_in_strings(builder, "item.id", ids, false);
    }
    if !query.virtual_folder_ids.is_empty() {
        let folder_ids = query
            .virtual_folder_ids
            .iter()
            .flat_map(|id| [id.to_string(), id.simple().to_string()])
            .collect::<BTreeSet<_>>();
        push_sqlite_in_strings(builder, "item.virtual_folder_id", folder_ids, false);
    }

    let include_item_types = sqlite_normalized_catalog_values(&query.include_item_types);
    if !include_item_types.is_empty() {
        push_sqlite_in_strings(
            builder,
            &format!("({SQLITE_MEDIA_ITEM_TYPE_SQL})"),
            include_item_types.into_iter().collect(),
            false,
        );
    }
    let exclude_item_types = sqlite_normalized_catalog_values(&query.exclude_item_types);
    if !exclude_item_types.is_empty() {
        push_sqlite_in_strings(
            builder,
            &format!("({SQLITE_MEDIA_ITEM_TYPE_SQL})"),
            exclude_item_types.into_iter().collect(),
            true,
        );
    }

    push_sqlite_ci_in_filter(builder, "item.collection_type", &query.collection_types);
    push_sqlite_ci_in_filter(builder, "item.media_type", &query.media_types);

    for (kind, values) in [
        ("official_ratings", &query.official_ratings),
        ("series_statuses", &query.series_statuses),
        ("years", &query.years),
    ] {
        push_sqlite_projected_value_filter(builder, kind, values);
    }

    let mut genre_selectors = sqlite_normalized_catalog_values(&query.genre_ids);
    genre_selectors.sort_unstable();
    genre_selectors.dedup();
    if !genre_selectors.is_empty() {
        builder.push(
            " AND EXISTS (\
             SELECT 1 FROM media_item_genre_selectors AS genre \
             WHERE genre.item_id = item.id AND genre.selector IN (",
        );
        let mut separated = builder.separated(", ");
        for selector in &genre_selectors {
            separated.push_bind(selector);
        }
        separated.push_unseparated("))");
    }

    for (kind, values) in [
        ("person", &query.person_ids),
        ("studio", &query.studio_ids),
        ("tag", &query.tags),
    ] {
        let mut selectors = sqlite_normalized_catalog_values(values);
        selectors.sort_unstable();
        selectors.dedup();
        if selectors.is_empty() {
            continue;
        }
        builder
            .push(
                " AND EXISTS (\
                 SELECT 1 FROM media_item_filter_selectors AS filter_selector \
                 WHERE filter_selector.item_id = item.id \
                   AND filter_selector.selector_kind = ",
            )
            .push_bind(kind)
            .push(" AND filter_selector.selector IN (");
        let mut separated = builder.separated(", ");
        for selector in &selectors {
            separated.push_bind(selector);
        }
        separated.push_unseparated("))");
    }

    let containers = sqlite_normalized_catalog_values(&query.containers);
    if !containers.is_empty() {
        builder.push(" AND (");
        for (index, container) in containers.iter().enumerate() {
            if index > 0 {
                builder.push(" OR ");
            }
            builder
                .push("lower(item.path) LIKE ")
                .push_bind(format!("%.{}", sqlite_escape_catalog_like_value(container)))
                .push(" ESCAPE '\\'");
        }
        builder.push(")");
    }

    let video_types = sqlite_normalized_catalog_values(&query.video_types);
    if !video_types.is_empty() {
        push_sqlite_in_strings(
            builder,
            "(CASE WHEN item.media_type = 'Video' THEN 'videofile' ELSE 'unknown' END)",
            video_types.into_iter().collect(),
            false,
        );
    }

    push_sqlite_stream_language_filter(
        builder,
        "Audio",
        &sqlite_normalized_catalog_values(&query.audio_languages),
    );
    push_sqlite_stream_language_filter(
        builder,
        "Subtitle",
        &sqlite_normalized_catalog_values(&query.subtitle_languages),
    );
    if let Some(has_subtitles) = query.has_subtitles {
        builder
            .push(
                " AND EXISTS (SELECT 1 FROM json_each(item.media_streams_json) AS stream \
                   WHERE lower(json_extract(stream.value, '$.Type')) = 'subtitle') = ",
            )
            .push_bind(has_subtitles);
    }
    if let Some(has_trailer) = query.has_trailer {
        builder
            .push(
                " AND COALESCE((SELECT source.has_trailer \
                   FROM media_item_query_filter_sources AS source \
                  WHERE source.item_id = item.id), 0) = ",
            )
            .push_bind(has_trailer);
    }
    push_sqlite_metadata_presence_filter(
        builder,
        &["overview", "seriesoverview"],
        query.has_overview,
    );
    for (provider, expected) in [
        ("imdb", query.has_imdb_id),
        ("tmdb", query.has_tmdb_id),
        ("tvdb", query.has_tvdb_id),
    ] {
        push_sqlite_provider_id_filter(builder, provider, expected);
    }
    if let Some(expected) = query.has_official_rating {
        builder
            .push(
                " AND EXISTS (SELECT 1 FROM media_item_query_filter_values AS rating_value \
                  WHERE rating_value.item_id = item.id \
                    AND rating_value.value_kind = 'official_ratings' \
                    AND trim(rating_value.display_value) <> '') = ",
            )
            .push_bind(expected);
    }
    if let Some(expected) = query.is_locked {
        builder
            .push(
                " AND COALESCE((SELECT CASE locked.type WHEN 'true' THEN 1 ELSE 0 END \
                   FROM json_each(item.metadata_json) AS locked \
                  WHERE lower(locked.key) = 'lockdata' LIMIT 1), 0) = ",
            )
            .push_bind(expected);
    }

    if sqlite_catalog_static_filters_are_impossible(query) {
        builder.push(" AND 0");
    }

    if let Some(search_term) = sqlite_normalized_catalog_scalar(query.search_term.as_deref()) {
        let pattern = format!("%{}%", sqlite_escape_catalog_like_value(&search_term));
        builder
            .push(" AND (lower(item.name) LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\'");
        match query.search_scope {
            MediaItemCatalogSearchScope::Name => {}
            MediaItemCatalogSearchScope::AllMetadataScalars => {
                builder
                    .push(
                        " OR EXISTS (SELECT 1 FROM json_tree(item.metadata_json) AS metadata_scalar \
                           WHERE metadata_scalar.type IN ('text', 'integer', 'real') \
                             AND lower(CAST(metadata_scalar.atom AS TEXT)) LIKE ",
                    )
                    .push_bind(pattern)
                    .push(" ESCAPE '\\')");
            }
            MediaItemCatalogSearchScope::SearchHintFields => {
                push_sqlite_search_hint_metadata_filter(builder, &pattern);
            }
        }
        builder.push(")");
    }

    if let Some(is_hd) = query.is_hd {
        builder
            .push(" AND COALESCE(item.height >= 720, 0) = ")
            .push_bind(is_hd);
    }
    if let Some(is_4k) = query.is_4k {
        builder
            .push(" AND COALESCE(item.width >= 3840 OR item.height >= 2160, 0) = ")
            .push_bind(is_4k);
    }
    push_sqlite_optional_i64_bound(builder, "item.width", query.min_width, ">=");
    push_sqlite_optional_i64_bound(builder, "item.width", query.max_width, "<=");
    push_sqlite_optional_i64_bound(builder, "item.height", query.min_height, ">=");
    push_sqlite_optional_i64_bound(builder, "item.height", query.max_height, "<=");

    let community_rating = sqlite_metadata_number_expression(&["communityrating", "rating"]);
    let critic_rating = sqlite_metadata_number_expression(&["criticrating"]);
    push_sqlite_optional_f64_bound(builder, &community_rating, query.min_community_rating, ">=");
    push_sqlite_optional_f64_bound(builder, &community_rating, query.max_community_rating, "<=");
    push_sqlite_optional_f64_bound(builder, &critic_rating, query.min_critic_rating, ">=");
    push_sqlite_optional_f64_bound(builder, &critic_rating, query.max_critic_rating, "<=");
    push_sqlite_optional_metadata_time_bound(builder, query.min_premiere_date, ">=")?;
    push_sqlite_optional_metadata_time_bound(builder, query.max_premiere_date, "<=")?;

    push_sqlite_optional_time_bound(builder, "item.created_at", query.min_date_created, ">=")?;
    push_sqlite_optional_time_bound(builder, "item.created_at", query.max_date_created, "<=")?;
    push_sqlite_optional_time_bound(builder, "item.updated_at", query.min_date_last_saved, ">=")?;
    push_sqlite_optional_time_bound(builder, "item.updated_at", query.max_date_last_saved, "<=")?;

    if let Some(prefix) = sqlite_normalized_catalog_scalar(query.name_starts_with.as_deref()) {
        builder
            .push(" AND lower(item.name) LIKE ")
            .push_bind(format!("{}%", sqlite_escape_catalog_like_value(&prefix)))
            .push(" ESCAPE '\\'");
    }
    if let Some(lower_bound) =
        sqlite_normalized_catalog_scalar(query.name_starts_with_or_greater.as_deref())
    {
        builder
            .push(" AND lower(item.name) >= ")
            .push_bind(lower_bound);
    }
    if let Some(upper_bound) = sqlite_normalized_catalog_scalar(query.name_less_than.as_deref()) {
        builder
            .push(" AND lower(item.name) < ")
            .push_bind(upper_bound);
    }

    if query.is_played.is_some() || query.favorite.is_some() || query.is_resumable {
        if query.user_id.is_none() {
            builder.push(" AND 0");
        } else {
            if let Some(is_played) = query.is_played {
                builder
                    .push(" AND COALESCE(playback.played, 0) = ")
                    .push_bind(is_played);
            }
            if let Some(favorite) = query.favorite {
                match favorite {
                    MediaItemFavoriteFilter::Favorite(expected) => {
                        builder
                            .push(" AND COALESCE(playback.is_favorite, 0) = ")
                            .push_bind(expected);
                    }
                    MediaItemFavoriteFilter::FavoriteOrLiked => {
                        builder.push(
                            " AND (COALESCE(playback.is_favorite, 0) \
                               OR COALESCE(playback.rating > 0, 0))",
                        );
                    }
                }
            }
            if query.is_resumable {
                builder.push(" AND playback.position_ticks > 0 AND playback.played = 0");
            }
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_search_hint_metadata_filter(builder: &mut QueryBuilder<Sqlite>, pattern: &str) {
    builder
        .push(
            " OR EXISTS (\
           WITH RECURSIVE hint_values(value, value_type) AS (\
             SELECT hint_field.value, hint_field.type \
             FROM json_each(item.metadata_json) AS hint_field \
             WHERE hint_field.key IN (\
               'Album', 'AlbumName', 'AlbumArtist', 'AlbumArtists', \
               'SeriesName', 'Series', 'Artists'\
             ) \
             UNION ALL \
             SELECT array_value.value, array_value.type \
             FROM hint_values \
             JOIN json_each(\
               CASE WHEN hint_values.value_type = 'array' THEN hint_values.value ELSE '[]' END\
             ) AS array_value \
             WHERE hint_values.value_type = 'array'\
           ) \
           SELECT 1 FROM hint_values \
           WHERE (hint_values.value_type IN ('text', 'integer', 'real') \
                  AND lower(CAST(hint_values.value AS TEXT)) LIKE ",
        )
        .push_bind(pattern.to_owned())
        .push(
            " ESCAPE '\\') \
              OR (hint_values.value_type = 'object' \
                  AND json_type(hint_values.value, '$.Name') = 'text' \
                  AND lower(json_extract(hint_values.value, '$.Name')) LIKE ",
        )
        .push_bind(pattern.to_owned())
        .push(
            " ESCAPE '\\')\
         )",
        );
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_catalog_order(builder: &mut QueryBuilder<Sqlite>, query: &MediaItemCatalogQuery) {
    builder.push(" ORDER BY ");
    let sort = if query.sort.is_empty() {
        &[(
            MediaItemCatalogSortField::SortName,
            SortDirection::Ascending,
        )][..]
    } else {
        query.sort.as_slice()
    };
    for (index, (field, direction)) in sort.iter().take(3).enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push(match field {
            MediaItemCatalogSortField::SortName => "lower(item.name)",
            MediaItemCatalogSortField::DateCreated => "item.created_at",
            MediaItemCatalogSortField::DateLastMediaAdded => "item.updated_at",
        });
        builder.push(match direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
    }
    builder.push(match sort.last().map(|(_, direction)| direction) {
        Some(SortDirection::Descending) => ", item.id DESC",
        Some(SortDirection::Ascending) | None => ", item.id ASC",
    });
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_in_strings(
    builder: &mut QueryBuilder<Sqlite>,
    expression: &str,
    values: BTreeSet<String>,
    negate: bool,
) {
    if values.is_empty() {
        return;
    }
    builder.push(if negate { " AND NOT (" } else { " AND " });
    builder.push(expression).push(" IN (");
    let mut separated = builder.separated(", ");
    for value in values {
        separated.push_bind(value);
    }
    separated.push_unseparated(if negate { "))" } else { ")" });
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_ci_in_filter(builder: &mut QueryBuilder<Sqlite>, column: &str, values: &[String]) {
    push_sqlite_in_strings(
        builder,
        &format!("lower({column})"),
        sqlite_normalized_catalog_values(values)
            .into_iter()
            .collect(),
        false,
    );
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_projected_value_filter(
    builder: &mut QueryBuilder<Sqlite>,
    value_kind: &str,
    values: &[String],
) {
    let values = sqlite_normalized_catalog_values(values);
    if values.is_empty() {
        return;
    }
    builder
        .push(
            " AND EXISTS (SELECT 1 FROM media_item_query_filter_values AS projected_value \
              WHERE projected_value.item_id = item.id \
                AND projected_value.value_kind = ",
        )
        .push_bind(value_kind.to_owned())
        .push(" AND lower(trim(projected_value.display_value)) IN (");
    let mut separated = builder.separated(", ");
    for value in values {
        separated.push_bind(value);
    }
    separated.push_unseparated("))");
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_metadata_presence_filter(
    builder: &mut QueryBuilder<Sqlite>,
    keys: &[&str],
    expected: Option<bool>,
) {
    let Some(expected) = expected else {
        return;
    };
    builder.push(
        " AND EXISTS (SELECT 1 FROM json_each(item.metadata_json) AS metadata_value \
          WHERE lower(metadata_value.key) IN (",
    );
    {
        let mut separated = builder.separated(", ");
        for key in keys {
            separated.push_bind((*key).to_owned());
        }
        separated.push_unseparated(
            ") AND metadata_value.type = 'text' \
                           AND trim(CAST(metadata_value.value AS TEXT)) <> '') = ",
        );
    }
    builder.push_bind(expected);
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_provider_id_filter(
    builder: &mut QueryBuilder<Sqlite>,
    provider: &str,
    expected: Option<bool>,
) {
    let Some(expected) = expected else {
        return;
    };
    builder
        .push(
            " AND EXISTS (SELECT 1 \
              FROM json_each(item.metadata_json) AS provider_parent \
              JOIN json_each(CASE WHEN provider_parent.type = 'object' \
                                  THEN provider_parent.value ELSE '{}' END) AS provider_value \
             WHERE lower(provider_parent.key) IN ('providerids', 'seriesproviderids') \
               AND lower(provider_value.key) = ",
        )
        .push_bind(provider.to_owned())
        .push(
            " AND provider_value.type = 'text' \
                AND trim(CAST(provider_value.value AS TEXT)) <> '') = ",
        )
        .push_bind(expected);
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_stream_language_filter(
    builder: &mut QueryBuilder<Sqlite>,
    stream_type: &str,
    languages: &[String],
) {
    if languages.is_empty() {
        return;
    }
    builder
        .push(
            " AND EXISTS (SELECT 1 FROM json_each(item.media_streams_json) AS stream \
               WHERE lower(json_extract(stream.value, '$.Type')) = lower(",
        )
        .push_bind(stream_type.to_owned())
        .push(
            ") AND CASE lower(trim(json_extract(stream.value, '$.Language'))) \
                 WHEN 'fre' THEN 'fra' WHEN 'ger' THEN 'deu' \
                 ELSE lower(trim(json_extract(stream.value, '$.Language'))) END IN (",
        );
    let mut separated = builder.separated(", ");
    for language in languages {
        separated.push_bind(language.to_owned());
    }
    separated
        .push_unseparated(") AND lower(trim(json_extract(stream.value, '$.Language'))) <> 'und')");
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_optional_i64_bound(
    builder: &mut QueryBuilder<Sqlite>,
    column: &str,
    value: Option<i64>,
    operator: &str,
) {
    if let Some(value) = value {
        builder
            .push(" AND ")
            .push(column)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(value);
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_optional_f64_bound(
    builder: &mut QueryBuilder<Sqlite>,
    expression: &str,
    value: Option<f64>,
    operator: &str,
) {
    if let Some(value) = value {
        builder
            .push(" AND ")
            .push(expression)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(value);
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_metadata_number_expression(keys: &[&str]) -> String {
    let fallback_priority = keys.len();
    let order = keys
        .iter()
        .enumerate()
        .map(|(index, key)| format!("WHEN '{key}' THEN {index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let keys = keys
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(SELECT CASE \
                      WHEN metadata_number.type IN ('integer', 'real') \
                      THEN CAST(metadata_number.atom AS REAL) \
                      WHEN metadata_number.type = 'text' \
                       AND json_valid(trim(CAST(metadata_number.atom AS TEXT))) \
                       AND json_type(trim(CAST(metadata_number.atom AS TEXT))) IN ('integer', 'real') \
                      THEN CAST(metadata_number.atom AS REAL) END \
           FROM json_each(item.metadata_json) AS metadata_number \
          WHERE lower(metadata_number.key) IN ({keys}) \
          ORDER BY CASE lower(metadata_number.key) {order} ELSE {} END LIMIT 1)",
        fallback_priority
    )
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_optional_metadata_time_bound(
    builder: &mut QueryBuilder<Sqlite>,
    value: Option<OffsetDateTime>,
    operator: &str,
) -> anyhow::Result<()> {
    if let Some(value) = value {
        builder
            .push(
                " AND julianday((SELECT CASE WHEN premiere.type = 'text' THEN premiere.value END \
                   FROM json_each(item.metadata_json) AS premiere \
                  WHERE lower(premiere.key) IN ('premieredate', 'airdate', 'datecreated') \
                  ORDER BY CASE lower(premiere.key) \
                      WHEN 'premieredate' THEN 0 WHEN 'airdate' THEN 1 ELSE 2 END LIMIT 1)) ",
            )
            .push(operator)
            .push(" julianday(")
            .push_bind(format_time(value)?)
            .push(")");
    }
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
fn push_sqlite_optional_time_bound(
    builder: &mut QueryBuilder<Sqlite>,
    column: &str,
    value: Option<OffsetDateTime>,
    operator: &str,
) -> anyhow::Result<()> {
    if let Some(value) = value {
        builder
            .push(" AND ")
            .push(column)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(format_time(value)?);
    }
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_normalized_catalog_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_normalized_catalog_scalar(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_escape_catalog_like_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_catalog_static_filters_are_impossible(query: &MediaItemCatalogQuery) -> bool {
    let location_types = sqlite_normalized_catalog_values(&query.location_types);
    let exclude_location_types = sqlite_normalized_catalog_values(&query.exclude_location_types);
    (!location_types.is_empty() && !location_types.iter().any(|value| value == "filesystem"))
        || exclude_location_types
            .iter()
            .any(|value| value == "filesystem")
        || query.is_missing == Some(true)
        || query.is_unaired == Some(true)
        || query.is_folder == Some(true)
}

fn normalized_facet_query_values(values: &[String]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

#[cfg(any(test, feature = "sqlite"))]
async fn replace_sqlite_media_item_facets(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    item_id: &str,
    metadata: &Value,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM media_item_filter_selectors WHERE item_id = ?1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_upcoming_dates WHERE item_id = ?1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_genre_selectors WHERE item_id = ?1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_facets WHERE item_id = ?1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    for facet in extract_media_item_facets(metadata) {
        sqlx::query(
            r#"
            INSERT INTO media_item_facets (
                item_id, facet_kind, normalized_value, display_value,
                stable_id, position, payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(item_id)
        .bind(facet.kind.as_str())
        .bind(&facet.normalized_value)
        .bind(&facet.display_value)
        .bind(&facet.stable_id)
        .bind(i64::from(facet.position))
        .bind(serde_json::to_string(&facet.payload)?)
        .execute(&mut **tx)
        .await?;
        for alias in facet.aliases {
            sqlx::query(
                r#"
                INSERT INTO media_item_facet_aliases (
                    item_id, facet_kind, normalized_value, entity_id
                ) VALUES (?1, ?2, ?3, ?4)
                "#,
            )
            .bind(item_id)
            .bind(facet.kind.as_str())
            .bind(&facet.normalized_value)
            .bind(alias)
            .execute(&mut **tx)
            .await?;
        }
    }
    for selector in extract_media_item_genre_selectors(metadata) {
        sqlx::query("INSERT INTO media_item_genre_selectors (item_id, selector) VALUES (?1, ?2)")
            .bind(item_id)
            .bind(selector)
            .execute(&mut **tx)
            .await?;
    }
    for (kind, selector) in extract_media_item_filter_selectors(metadata) {
        sqlx::query(
            "INSERT INTO media_item_filter_selectors \
             (item_id, selector_kind, selector) VALUES (?1, ?2, ?3)",
        )
        .bind(item_id)
        .bind(kind.as_str())
        .bind(selector)
        .execute(&mut **tx)
        .await?;
    }
    if let Some((unix_seconds, nanosecond)) = upcoming_media_item_premiere_parts(metadata) {
        sqlx::query(
            "INSERT INTO media_item_upcoming_dates \
             (item_id, unix_seconds, nanosecond) VALUES (?1, ?2, ?3)",
        )
        .bind(item_id)
        .bind(unix_seconds)
        .bind(nanosecond)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
async fn replace_sqlite_media_item_query_filter_projection(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    item_id: &str,
    folder_id: &str,
    projection: &MediaItemQueryFilterProjection,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM media_item_query_filter_sources WHERE item_id = ?1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        r#"
        INSERT INTO media_item_query_filter_sources (
            item_id, virtual_folder_id, extractor_version, container_present, container_value, media_type,
            is_video, has_subtitles, has_trailer, projected_value_count, completed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, CURRENT_TIMESTAMP)
        "#,
    )
    .bind(item_id)
    .bind(folder_id)
    .bind(projection.extractor_version)
    .bind(projection.features.container_present)
    .bind(&projection.features.container)
    .bind(&projection.features.media_type)
    .bind(projection.features.is_video)
    .bind(projection.features.has_subtitles)
    .bind(projection.features.has_trailer)
    .bind(i64::try_from(projection.values.len()).context("query-filter value count overflow")?)
    .execute(&mut **tx)
    .await?;
    for value in &projection.values {
        sqlx::query(
            r#"
            INSERT INTO media_item_query_filter_values (
                item_id, virtual_folder_id, value_kind, display_value, source_key,
                source_priority, source_position
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(item_id)
        .bind(folder_id)
        .bind(value.kind.as_str())
        .bind(&value.display_value)
        .bind(&value.source_key)
        .bind(i64::from(value.source_priority))
        .bind(encode_media_item_query_filter_position(&value.position))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
async fn replace_sqlite_media_item_query_filter_projection_from_live(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    item_id: &str,
) -> anyhow::Result<()> {
    let (folder_id, path, media_type, media_streams_json, metadata_json) =
        sqlx::query_as::<_, (String, String, String, String, String)>(
            "SELECT virtual_folder_id, path, media_type, media_streams_json, metadata_json \
             FROM media_items WHERE id = ?1",
        )
        .bind(item_id)
        .fetch_one(&mut **tx)
        .await?;
    let media_streams = serde_json::from_str::<Vec<Value>>(&media_streams_json)
        .context("invalid projected media streams JSON")?;
    let metadata = serde_json::from_str::<Value>(&metadata_json)
        .context("invalid projected media metadata JSON")?;
    let projection =
        extract_media_item_query_filter_projection(MediaItemQueryFilterProjectionSource {
            path: &path,
            media_type: &media_type,
            media_streams: &media_streams,
            metadata: &metadata,
        });
    replace_sqlite_media_item_query_filter_projection(tx, item_id, &folder_id, &projection).await
}

#[cfg(any(test, feature = "sqlite"))]
impl SqliteDatabase {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let options = database_url
            .parse::<SqliteConnectOptions>()
            .with_context(|| format!("failed to parse SQLite database URL at {database_url}"))?
            .busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))
            .foreign_keys(true);
        // SQLite is now restricted to legacy migration and tests. Keep its
        // rollback journal until the pinned SQLx line can ship a bundled
        // SQLite containing the upstream WAL-reset fix; production uses
        // PostgreSQL and does not pay this compatibility tradeoff.

        let pool = SqlitePoolOptions::new()
            .max_connections(SQLITE_MAX_CONNECTIONS)
            .after_connect(|connection, _metadata| {
                Box::pin(async move { configure_sqlite_connection(connection).await })
            })
            .connect_with(options)
            .await
            .with_context(|| format!("failed to connect SQLite database at {database_url}"))?;

        MIGRATOR
            .run(&pool)
            .await
            .context("failed to run migrations")?;

        let database = Self {
            pool,
            // In-memory SQLite exists exclusively as the fast conformance/test harness, including
            // when jellyrin-db is compiled as another crate's dev-dependency (where cfg(test) is
            // not propagated). Persistent legacy databases never receive an implicit key.
            provider_secret_vault: database_url
                .trim()
                .eq_ignore_ascii_case("sqlite::memory:")
                .then(ProviderSecretVault::for_legacy_test_harness),
            telemetry: Arc::new(DatabaseTelemetry::default()),
        };
        let projection_version = sqlx::query_scalar::<_, i32>(
            "SELECT extractor_version FROM jellyrin_derived_projection_versions \
             WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .fetch_optional(&database.pool)
        .await
        .context("failed to inspect SQLite media item facet projection")?;
        anyhow::ensure!(
            projection_version.is_none_or(|version| version <= MEDIA_ITEM_FACET_PROJECTION_VERSION),
            "SQLite media item facet projection version {} is newer than supported version {}",
            projection_version.unwrap_or_default(),
            MEDIA_ITEM_FACET_PROJECTION_VERSION
        );
        if projection_version != Some(MEDIA_ITEM_FACET_PROJECTION_VERSION) {
            database.rebuild_media_item_facets().await?;
        }
        let query_filter_marker = sqlx::query_as::<_, (i32, i64, i64)>(
            "SELECT extractor_version, source_item_count, projected_facet_count \
             FROM jellyrin_derived_projection_versions \
             WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
        .fetch_optional(&database.pool)
        .await
        .context("failed to inspect SQLite media item query-filter projection")?;
        anyhow::ensure!(
            query_filter_marker
                .is_none_or(|marker| marker.0 <= MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION),
            "SQLite query-filter projection version {} is newer than supported version {}",
            query_filter_marker.unwrap_or_default().0,
            MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION
        );
        let actual_query_filter_counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "WITH value_counts AS (SELECT item_id, count(*) AS value_count \
                 FROM media_item_query_filter_values GROUP BY item_id) \
             SELECT (SELECT count(*) FROM media_items), \
                    count(*) FILTER (WHERE source.extractor_version = ?1), \
                    coalesce(sum(value_counts.value_count), 0), \
                    count(*) FILTER (WHERE source.projected_value_count \
                        <> coalesce(value_counts.value_count, 0)) \
             FROM media_item_query_filter_sources AS source \
             LEFT JOIN value_counts ON value_counts.item_id = source.item_id",
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .fetch_one(&database.pool)
        .await?;
        let query_filter_current = query_filter_marker.is_some_and(|marker| {
            marker.0 == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION
                && actual_query_filter_counts.0 == actual_query_filter_counts.1
                && actual_query_filter_counts.3 == 0
        });
        if query_filter_current {
            sqlx::query(
                "UPDATE jellyrin_derived_projection_versions \
                 SET source_item_count = ?2, projected_facet_count = ?3, \
                     completed_at = CURRENT_TIMESTAMP \
                 WHERE projection_name = ?1 \
                   AND (source_item_count <> ?2 OR projected_facet_count <> ?3)",
            )
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
            .bind(actual_query_filter_counts.0)
            .bind(actual_query_filter_counts.2)
            .execute(&database.pool)
            .await?;
        } else {
            database
                .rebuild_media_item_query_filter_projection()
                .await?;
        }
        Ok(database)
    }

    pub fn with_provider_secret_vault(mut self, vault: ProviderSecretVault) -> Self {
        self.provider_secret_vault = Some(vault);
        self
    }

    #[cfg(test)]
    // Test-only harness for crypto/idempotence checks. Production callers must use a writer that
    // persists the envelope and its configuration reference in one transaction.
    pub(crate) async fn protect_provider_configuration(
        &self,
        provider_type: &str,
        configuration: Value,
    ) -> anyhow::Result<Value> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let protected = self
            .protect_provider_configuration_with_policy(
                &mut transaction,
                provider_type,
                configuration,
                false,
            )
            .await?;
        transaction.commit().await?;
        Ok(protected)
    }

    async fn protect_provider_configuration_in_connection(
        &self,
        connection: &mut SqliteConnection,
        provider_type: &str,
        configuration: Value,
    ) -> anyhow::Result<Value> {
        self.protect_provider_configuration_with_policy(
            connection,
            provider_type,
            configuration,
            true,
        )
        .await
    }

    async fn protect_provider_configuration_with_policy(
        &self,
        connection: &mut SqliteConnection,
        provider_type: &str,
        configuration: Value,
        reuse_existing_secret: bool,
    ) -> anyhow::Result<Value> {
        let existing_reference = ProviderSecretReference::from_configuration(&configuration);
        let submitted = provider_credentials_from_configuration(&configuration)?;
        let has_reference_field = configuration_has_provider_secret_reference_field(&configuration);
        anyhow::ensure!(
            !has_reference_field || existing_reference.is_some(),
            "provider secret reference is invalid"
        );
        if submitted.is_none() && !has_reference_field {
            return Ok(configuration);
        }
        let provider_type = normalize_provider_type(provider_type)?;
        let reference = match (submitted, existing_reference) {
            (None, None) => return Ok(configuration),
            (None, Some(reference)) => {
                let (current, _) = self
                    .provider_secret_in_connection(connection, &reference)
                    .await?;
                current
            }
            (Some((username, password)), existing_reference) => {
                let previous = match existing_reference.as_ref() {
                    Some(reference) => Some(
                        self.provider_secret_in_connection(connection, reference)
                            .await?,
                    ),
                    None => None,
                };
                let username = username
                    .or_else(|| {
                        previous
                            .as_ref()
                            .map(|(_, value)| value.protected_username_copy())
                    })
                    .context("provider username is required")?;
                let password = password
                    .or_else(|| {
                        previous
                            .as_ref()
                            .map(|(_, value)| value.protected_password_copy())
                    })
                    .context("provider password is required")?;
                let credentials = ProviderCredentials::from_protected_parts(username, password)?;
                match previous.as_ref() {
                    Some((current_reference, previous_credentials))
                        if previous_credentials == &credentials =>
                    {
                        current_reference.clone()
                    }
                    _ => {
                        self.upsert_provider_secret_in_connection(
                            connection,
                            &provider_type,
                            if reuse_existing_secret {
                                existing_reference.as_ref().map(|value| value.id.as_str())
                            } else {
                                None
                            },
                            &credentials,
                        )
                        .await?
                    }
                }
            }
        };
        anyhow::ensure!(
            reference.provider_type.eq_ignore_ascii_case(&provider_type),
            "provider secret reference belongs to a different provider"
        );
        redacted_provider_configuration(configuration, &reference)
    }

    async fn protect_live_tv_named_configuration_in_connection(
        &self,
        connection: &mut SqliteConnection,
        mut configuration: Value,
        existing: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let Some(hosts) = configuration
            .get_mut("TunerHosts")
            .and_then(Value::as_array_mut)
        else {
            return Ok(configuration);
        };
        let existing_hosts = existing
            .and_then(|value| value.get("TunerHosts"))
            .and_then(Value::as_array);
        for host in hosts {
            let provider_type = host
                .get("Type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let host_id = host.get("Id").and_then(Value::as_str);
            let existing_host = host_id.and_then(|host_id| {
                existing_hosts?.iter().find(|candidate| {
                    candidate
                        .get("Id")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.eq_ignore_ascii_case(host_id))
                })
            });
            let is_xtream = provider_type.eq_ignore_ascii_case("xtream");
            let is_plugin = provider_type.eq_ignore_ascii_case("plugin")
                || provider_type
                    .split_once(':')
                    .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case("plugin"));
            anyhow::ensure!(
                !configuration_has_provider_secret_input_field(host) || is_xtream || is_plugin,
                "Live TV core credentials require an explicit xtream or plugin provider type"
            );
            if !is_xtream && !is_plugin {
                continue;
            }
            inherit_provider_secret_reference_for_configuration(
                host,
                existing_host,
                &provider_type,
            )?;
            let has_core_secret = configuration_has_provider_secret_material(host);
            if !has_core_secret {
                continue;
            }
            let secret_namespace =
                provider_secret_namespace_for_configuration(&provider_type, host)?;
            *host = self
                .protect_provider_configuration_in_connection(
                    connection,
                    &secret_namespace,
                    host.clone(),
                )
                .await?;
        }
        Ok(configuration)
    }

    pub async fn resolve_provider_configuration(
        &self,
        configuration: &Value,
    ) -> anyhow::Result<Value> {
        let Some(reference) = ProviderSecretReference::from_configuration(configuration) else {
            return Ok(configuration.clone());
        };
        let (current_reference, credentials) = self.provider_secret(&reference).await?;
        resolved_provider_configuration(configuration.clone(), &current_reference, &credentials)
    }

    /// Resolves a vault reference directly for just-in-time use without constructing a JSON value
    /// containing plaintext credentials.
    pub async fn provider_credentials_for_configuration(
        &self,
        configuration: &Value,
    ) -> anyhow::Result<Option<(ProviderSecretReference, ProviderCredentials)>> {
        let Some(reference) = ProviderSecretReference::from_configuration(configuration) else {
            return Ok(None);
        };
        self.provider_secret(&reference).await.map(Some)
    }

    pub async fn provider_secret_count(&self) -> anyhow::Result<i64> {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_secrets")
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    /// Deletes vault envelopes that are not referenced by any persisted provider configuration.
    ///
    /// `BEGIN IMMEDIATE` serialises this complete scan with SQLite writers. Invalid JSON or an
    /// invalid nested reference aborts the transaction, so reconciliation always fails closed.
    pub async fn reconcile_orphaned_provider_secrets(&self) -> anyhow::Result<usize> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let configurations = sqlx::query_scalar::<_, String>(
            r#"
            SELECT configuration_json FROM live_tv_tuners
            UNION ALL
            SELECT configuration_json FROM plugin_configurations
            UNION ALL
            SELECT payload_json FROM named_configurations
            "#,
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut references = HashSet::new();
        for configuration in configurations {
            let configuration = serde_json::from_str::<Value>(&configuration)
                .context("invalid persisted configuration during provider secret reconciliation")?;
            collect_provider_secret_reference_identities(&configuration, &mut references)?;
        }

        let envelopes = sqlx::query("SELECT secret_id, provider_type FROM provider_secrets")
            .fetch_all(&mut *transaction)
            .await?;
        let mut deleted = 0usize;
        for envelope in envelopes {
            let secret_id = envelope.get::<String, _>("secret_id");
            let provider_type = envelope.get::<String, _>("provider_type");
            if references.contains(&(secret_id.clone(), provider_type.to_ascii_lowercase())) {
                continue;
            }
            deleted += sqlx::query(
                "DELETE FROM provider_secrets WHERE secret_id = ?1 AND provider_type = ?2 COLLATE NOCASE",
            )
            .bind(secret_id)
            .bind(provider_type)
            .execute(&mut *transaction)
            .await?
            .rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(deleted)
    }

    pub async fn validate_provider_secret_readiness(&self) -> anyhow::Result<()> {
        if self.provider_secret_vault.is_none() && self.provider_secret_count().await? > 0 {
            anyhow::bail!(
                "provider secrets exist but no provider secret key was configured; set JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE"
            );
        }
        Ok(())
    }

    /// Fails before a write path invokes an external provider if encryption is unavailable.
    pub fn validate_provider_secret_write_readiness(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.provider_secret_vault.is_some(),
            "provider credentials cannot be stored without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE"
        );
        Ok(())
    }

    pub async fn rotate_provider_secrets_to_active_key(&self) -> anyhow::Result<usize> {
        let Some(vault) = self.provider_secret_vault.as_ref() else {
            self.validate_provider_secret_readiness().await?;
            return Ok(0);
        };
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query(
            "SELECT secret_id, provider_type, revision FROM provider_secrets WHERE key_id <> ?1 ORDER BY secret_id",
        )
        .bind(vault.active_key_id())
        .fetch_all(&mut *transaction)
        .await?;
        let mut rotated = 0usize;
        for row in rows {
            let reference = ProviderSecretReference {
                id: row.get("secret_id"),
                provider_type: row.get("provider_type"),
                revision: row.get("revision"),
            };
            let (_, credentials) = self
                .provider_secret_in_connection(&mut transaction, &reference)
                .await?;
            self.upsert_provider_secret_in_connection(
                &mut transaction,
                &reference.provider_type,
                Some(&reference.id),
                &credentials,
            )
            .await?;
            rotated += 1;
        }
        transaction.commit().await?;
        Ok(rotated)
    }

    pub async fn backfill_legacy_provider_secrets(&self) -> anyhow::Result<usize> {
        // BEGIN IMMEDIATE serializes SQLite writers before any source configuration is read.
        // Envelopes and all redacted references therefore commit or roll back as one unit.
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let plugin_configuration = sqlx::query_scalar::<_, String>(
            "SELECT configuration_json FROM plugin_configurations WHERE plugin_id = ?1 COLLATE NOCASE",
        )
        .bind("jellyrin-xtream-provider")
        .fetch_optional(&mut *transaction)
        .await?
        .map(|configuration| {
            serde_json::from_str::<Value>(&configuration)
                .context("invalid plugin configuration payload")
        })
        .transpose()?;
        let tuner_rows =
            sqlx::query("SELECT tuner_id, provider_type, configuration_json FROM live_tv_tuners")
                .fetch_all(&mut *transaction)
                .await?;
        let tuner_configurations = tuner_rows
            .into_iter()
            .map(|row| {
                let configuration = serde_json::from_str::<Value>(
                    row.get::<String, _>("configuration_json").as_str(),
                )
                .context("invalid live TV tuner configuration json")?;
                Ok((
                    row.get::<String, _>("tuner_id"),
                    row.get::<String, _>("provider_type"),
                    configuration,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let named_configuration = sqlx::query_scalar::<_, String>(
            "SELECT payload_json FROM named_configurations WHERE key = 'livetv'",
        )
        .fetch_optional(&mut *transaction)
        .await?
        .map(|payload| {
            serde_json::from_str::<Value>(&payload).context("invalid named configuration")
        })
        .transpose()?;

        let builtin_tuner_configuration = tuner_configurations
            .iter()
            .find(|(tuner_id, _, _)| tuner_id.eq_ignore_ascii_case("xtream-plugin"))
            .map(|(_, _, configuration)| configuration);
        let named_builtin_configuration = named_configuration
            .as_ref()
            .and_then(|configuration| configuration.get("TunerHosts"))
            .and_then(Value::as_array)
            .and_then(|hosts| {
                hosts.iter().find(|host| {
                    host.get("Id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.eq_ignore_ascii_case("xtream-plugin"))
                })
            });
        let canonical_seed = [
            plugin_configuration.as_ref(),
            builtin_tuner_configuration,
            named_builtin_configuration,
        ]
        .into_iter()
        .flatten()
        .find(|configuration| configuration_has_provider_secret_material(configuration))
        .cloned();
        let canonical = if let Some(seed) = canonical_seed {
            let canonical_credentials = self
                .configuration_credentials_in_connection(&mut transaction, &seed)
                .await?
                .context("builtin Xtream credentials are incomplete")?;
            for configuration in [
                plugin_configuration.as_ref(),
                builtin_tuner_configuration,
                named_builtin_configuration,
            ]
            .into_iter()
            .flatten()
            .filter(|configuration| configuration_has_provider_secret_material(configuration))
            {
                let credentials = self
                    .configuration_credentials_in_connection(&mut transaction, configuration)
                    .await?
                    .context("builtin Xtream credentials are incomplete")?;
                anyhow::ensure!(
                    credentials == canonical_credentials,
                    "conflicting legacy Xtream credentials; provider secret backfill was not applied"
                );
            }
            Some(
                self.protect_provider_configuration_in_connection(&mut transaction, "xtream", seed)
                    .await?,
            )
        } else {
            None
        };
        let canonical_reference = canonical
            .as_ref()
            .and_then(ProviderSecretReference::from_configuration);

        let plugin_rewrite = if let (Some(original), Some(reference)) =
            (plugin_configuration.as_ref(), canonical_reference.as_ref())
        {
            let mut candidate = original.clone();
            set_provider_secret_reference(&mut candidate, reference)?;
            let protected = self
                .protect_provider_configuration_in_connection(&mut transaction, "xtream", candidate)
                .await?;
            (protected != *original).then_some(protected)
        } else {
            None
        };

        let mut tuner_rewrites = Vec::new();
        for (tuner_id, provider_type, configuration) in &tuner_configurations {
            let mut candidate = configuration.clone();
            if tuner_id.eq_ignore_ascii_case("xtream-plugin")
                && let Some(reference) = canonical_reference.as_ref()
            {
                set_provider_secret_reference(&mut candidate, reference)?;
            }
            let secret_namespace = if configuration_has_provider_secret_material(&candidate) {
                provider_secret_namespace_for_configuration(provider_type, &candidate)?
            } else {
                provider_type.clone()
            };
            let protected = self
                .protect_provider_configuration_in_connection(
                    &mut transaction,
                    &secret_namespace,
                    candidate,
                )
                .await?;
            if protected != *configuration {
                tuner_rewrites.push((tuner_id.clone(), protected));
            }
        }

        let named_rewrite = if let Some(original) = named_configuration.as_ref() {
            let mut candidate = original.clone();
            if let Some(reference) = canonical_reference.as_ref()
                && let Some(host) = candidate
                    .get_mut("TunerHosts")
                    .and_then(Value::as_array_mut)
                    .and_then(|hosts| {
                        hosts.iter_mut().find(|host| {
                            host.get("Id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id.eq_ignore_ascii_case("xtream-plugin"))
                        })
                    })
            {
                set_provider_secret_reference(host, reference)?;
            }
            let protected = self
                .protect_live_tv_named_configuration_in_connection(
                    &mut transaction,
                    candidate,
                    Some(original),
                )
                .await?;
            (protected != *original).then_some(protected)
        } else {
            None
        };

        let rewritten = usize::from(plugin_rewrite.is_some())
            + tuner_rewrites.len()
            + usize::from(named_rewrite.is_some());
        if rewritten > 0 {
            let now = format_time(OffsetDateTime::now_utc())?;
            if let Some(protected) = plugin_rewrite {
                sqlx::query(
                    "UPDATE plugin_configurations SET configuration_json = ?1, updated_at = ?2 WHERE plugin_id = ?3 COLLATE NOCASE",
                )
                .bind(serde_json::to_string(&protected)?)
                .bind(&now)
                .bind("jellyrin-xtream-provider")
                .execute(&mut *transaction)
                .await?;
            }
            for (tuner_id, protected) in tuner_rewrites {
                sqlx::query(
                    "UPDATE live_tv_tuners SET configuration_json = ?1, updated_at = ?2 WHERE tuner_id = ?3",
                )
                .bind(serde_json::to_string(&protected)?)
                .bind(&now)
                .bind(tuner_id)
                .execute(&mut *transaction)
                .await?;
            }
            if let Some(protected) = named_rewrite {
                sqlx::query(
                    "UPDATE named_configurations SET payload_json = ?1, updated_at = ?2 WHERE key = 'livetv'",
                )
                .bind(serde_json::to_string(&protected)?)
                .bind(&now)
                .execute(&mut *transaction)
                .await?;
            }
        }

        transaction.commit().await?;

        self.validate_provider_secret_readiness().await?;
        Ok(rewritten)
    }

    async fn configuration_credentials_in_connection(
        &self,
        connection: &mut SqliteConnection,
        configuration: &Value,
    ) -> anyhow::Result<Option<ProviderCredentials>> {
        if let Some(reference) = ProviderSecretReference::from_configuration(configuration) {
            let (_, credentials) = self
                .provider_secret_in_connection(connection, &reference)
                .await?;
            return Ok(Some(credentials));
        }
        let Some((username, password)) = provider_credentials_from_configuration(configuration)?
        else {
            return Ok(None);
        };
        match (username, password) {
            (Some(username), Some(password)) => Ok(Some(
                ProviderCredentials::from_protected_parts(username, password)?,
            )),
            _ => anyhow::bail!("provider credentials are incomplete"),
        }
    }

    pub async fn rotate_provider_secret(
        &self,
        reference: &ProviderSecretReference,
    ) -> anyhow::Result<ProviderSecretReference> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (_, credentials) = self
            .provider_secret_in_connection(&mut transaction, reference)
            .await?;
        let current = self
            .upsert_provider_secret_in_connection(
                &mut transaction,
                &reference.provider_type,
                Some(&reference.id),
                &credentials,
            )
            .await?;
        transaction.commit().await?;
        Ok(current)
    }

    async fn upsert_provider_secret_in_connection(
        &self,
        connection: &mut SqliteConnection,
        provider_type: &str,
        secret_id: Option<&str>,
        credentials: &ProviderCredentials,
    ) -> anyhow::Result<ProviderSecretReference> {
        let vault = self.provider_secret_vault.as_ref().context(
            "provider credentials cannot be stored without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE",
        )?;
        let secret_id = secret_id
            .map(str::to_owned)
            .unwrap_or_else(new_provider_secret_id);
        let envelope = vault.seal(&secret_id, provider_type, credentials)?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let revision = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO provider_secrets (
                secret_id, provider_type, envelope_version, key_id, nonce, ciphertext,
                revision, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)
            ON CONFLICT(secret_id) DO UPDATE SET
                envelope_version = excluded.envelope_version,
                key_id = excluded.key_id,
                nonce = excluded.nonce,
                ciphertext = excluded.ciphertext,
                revision = provider_secrets.revision + 1,
                updated_at = excluded.updated_at
            WHERE lower(provider_secrets.provider_type) = lower(excluded.provider_type)
            RETURNING revision
            "#,
        )
        .bind(&secret_id)
        .bind(provider_type)
        .bind(i64::from(envelope.version))
        .bind(&envelope.key_id)
        .bind(envelope.nonce.as_slice())
        .bind(&envelope.ciphertext)
        .bind(&now)
        .fetch_optional(connection)
        .await?
        .context("provider secret id belongs to a different provider")?;
        Ok(ProviderSecretReference {
            id: secret_id,
            provider_type: provider_type.to_owned(),
            revision,
        })
    }

    async fn provider_secret(
        &self,
        reference: &ProviderSecretReference,
    ) -> anyhow::Result<(ProviderSecretReference, ProviderCredentials)> {
        let mut connection = self.pool.acquire().await?;
        self.provider_secret_in_connection(&mut connection, reference)
            .await
    }

    async fn provider_secret_in_connection(
        &self,
        connection: &mut SqliteConnection,
        reference: &ProviderSecretReference,
    ) -> anyhow::Result<(ProviderSecretReference, ProviderCredentials)> {
        let vault = self.provider_secret_vault.as_ref().context(
            "provider credentials cannot be resolved without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE",
        )?;
        let row = sqlx::query(
            r#"
            SELECT provider_type, envelope_version, key_id, nonce, ciphertext, revision
            FROM provider_secrets
            WHERE secret_id = ?1 AND provider_type = ?2 COLLATE NOCASE
            "#,
        )
        .bind(&reference.id)
        .bind(&reference.provider_type)
        .fetch_optional(connection)
        .await?
        .context("provider secret reference is unavailable")?;
        let nonce = row.get::<Vec<u8>, _>("nonce");
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| anyhow::anyhow!("provider secret envelope is invalid"))?;
        let provider_type = row.get::<String, _>("provider_type");
        let revision = row.get::<i64, _>("revision");
        let envelope = ProviderSecretEnvelope {
            version: u16::try_from(row.get::<i64, _>("envelope_version"))?,
            key_id: row.get("key_id"),
            nonce,
            ciphertext: row.get("ciphertext"),
        };
        let credentials = vault.open(&reference.id, &provider_type, &envelope)?;
        Ok((
            ProviderSecretReference {
                id: reference.id.clone(),
                provider_type,
                revision,
            },
            credentials,
        ))
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn health(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn schema_health(&self) -> anyhow::Result<()> {
        self.health().await
    }

    pub fn runtime_diagnostics(&self) -> DatabaseRuntimeDiagnostics {
        DatabaseRuntimeDiagnostics {
            driver: DatabaseDriver::Sqlite,
            api_pool: database_pool_diagnostics(&self.pool),
            // SQLite is a legacy/test adapter and intentionally has no independent worker pool.
            worker_pool: None,
        }
    }

    pub fn telemetry_diagnostics(&self) -> DatabaseTelemetryDiagnostics {
        self.telemetry.snapshot(false)
    }

    pub async fn catalog_sync_diagnostics(&self) -> anyhow::Result<CatalogSyncDiagnostics> {
        let counts = sqlx::query_as::<_, CatalogSyncCountsRow>(
            r#"
            SELECT COUNT(*) AS total,
                   COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS running,
                   COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0) AS completed,
                   COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failed
            FROM catalog_sync_runs
            "#,
        )
        .fetch_one(&self.pool)
        .await?;
        let last_run = sqlx::query_as::<_, (String, i64, String, Option<String>)>(
            r#"
            SELECT status, item_count, started_at, completed_at
            FROM catalog_sync_runs
            ORDER BY started_at DESC, id DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(|(status, item_count, started_at, completed_at)| {
            let started_at = parse_time(&started_at)?;
            let completed_at = completed_at.as_deref().map(parse_time).transpose()?;
            Ok::<_, anyhow::Error>(CatalogSyncRunDiagnostics {
                status,
                item_count: nonnegative_count(item_count),
                started_at,
                completed_at,
                duration_millis: catalog_sync_duration_millis(started_at, completed_at),
            })
        })
        .transpose()?;
        Ok(CatalogSyncDiagnostics {
            total: nonnegative_count(counts.total),
            running: nonnegative_count(counts.running),
            completed: nonnegative_count(counts.completed),
            failed: nonnegative_count(counts.failed),
            last_run,
        })
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
        let _ = self.server_state().await?;
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
        self.sync_plugin_platform_from_system_configuration()
            .await?;
        Ok(())
    }

    pub async fn sync_plugin_platform_from_system_configuration(&self) -> anyhow::Result<()> {
        let payloads = self.system_configuration_payloads().await?;
        let repositories = plugin_repository_models_from_config(&payloads.plugin_repositories);
        let packages = package_catalog_models_from_repositories(&repositories);
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM package_catalog_cache")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM plugin_repositories")
            .execute(&mut *tx)
            .await?;

        for repository in repositories {
            sqlx::query(
                r#"
                INSERT INTO plugin_repositories (id, name, url, enabled, payload_json, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )
            .bind(&repository.id)
            .bind(&repository.name)
            .bind(&repository.url)
            .bind(repository.enabled)
            .bind(serde_json::to_string(&repository.payload)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        for package in packages {
            sqlx::query(
                r#"
                INSERT INTO package_catalog_cache (
                    id, repository_url, package_guid, package_name, package_version, runtime,
                    target_abi, payload_json, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )
            .bind(&package.id)
            .bind(&package.repository_url)
            .bind(&package.package_guid)
            .bind(&package.package_name)
            .bind(&package.package_version)
            .bind(&package.runtime)
            .bind(&package.target_abi)
            .bind(serde_json::to_string(&package.payload)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn plugin_platform_snapshot(&self) -> anyhow::Result<Value> {
        self.sync_plugin_platform_from_system_configuration()
            .await?;
        let repositories = plugin_repositories_snapshot(&self.pool).await?;
        let package_catalog = package_catalog_snapshot(&self.pool).await?;
        let package_installations = package_installations_snapshot(&self.pool).await?;
        let installed_plugins = installed_plugins_backup_snapshot(&self.pool).await?;
        let plugin_manifests = plugin_manifests_snapshot(&self.pool).await?;
        let plugin_configurations = plugin_configurations_snapshot(&self.pool).await?;
        let plugin_permissions = plugin_permissions_snapshot(&self.pool).await?;
        let plugin_runtime_instances = plugin_runtime_instances_snapshot(&self.pool).await?;
        let plugin_host_events = plugin_host_events_snapshot(&self.pool).await?;
        let plugin_audit_log = plugin_audit_log_snapshot(&self.pool).await?;
        Ok(json!({
            "ModelVersion": 1,
            "Mode": "metadata-only",
            "Supported": true,
            "PackageBinaries": {
                "Mode": "not-restored",
                "Supported": false,
                "Reason": "Backup restores plugin state and metadata; package binary directories are intentionally not copied."
            },
            "Repositories": {
                "Count": repositories.len(),
                "Items": repositories
            },
            "PackageCatalogCache": {
                "Count": package_catalog.len(),
                "Items": package_catalog
            },
            "PackageInstallations": {
                "Count": package_installations.len(),
                "Items": package_installations
            },
            "InstalledPlugins": {
                "Count": installed_plugins.len(),
                "Items": installed_plugins
            },
            "PluginManifests": {
                "Count": plugin_manifests.len(),
                "Items": plugin_manifests
            },
            "PluginConfigurations": {
                "Count": plugin_configurations.len(),
                "Items": plugin_configurations
            },
            "PluginPermissions": {
                "Count": plugin_permissions.len(),
                "Items": plugin_permissions
            },
            "PluginRuntimeInstances": {
                "Count": plugin_runtime_instances.len(),
                "Items": plugin_runtime_instances
            },
            "PluginHostEvents": {
                "Count": plugin_host_events.len(),
                "Items": plugin_host_events
            },
            "PluginAuditLog": {
                "Count": plugin_audit_log.len(),
                "Items": plugin_audit_log
            }
        }))
    }

    pub async fn replace_live_tv_tuner_snapshot(
        &self,
        mut tuner: LiveTvTunerUpsert,
        categories: Vec<LiveTvCategoryUpsert>,
        channels: Vec<LiveTvChannelUpsert>,
    ) -> anyhow::Result<Value> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let existing_configuration = sqlx::query_scalar::<_, String>(
            r#"
            SELECT configuration_json
            FROM live_tv_tuners
            WHERE enabled = 1 AND tuner_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(tuner.tuner_id.trim())
        .fetch_optional(&mut *tx)
        .await?
        .map(|configuration| {
            serde_json::from_str::<Value>(&configuration)
                .context("invalid persisted Live TV tuner configuration")
        })
        .transpose()?;
        inherit_provider_secret_reference_for_configuration(
            &mut tuner.configuration,
            existing_configuration.as_ref(),
            &tuner.provider_type,
        )?;
        let secret_namespace = if configuration_has_provider_secret_material(&tuner.configuration) {
            provider_secret_namespace_for_configuration(&tuner.provider_type, &tuner.configuration)?
        } else {
            tuner.provider_type.clone()
        };
        tuner.configuration = self
            .protect_provider_configuration_in_connection(
                &mut tx,
                &secret_namespace,
                tuner.configuration,
            )
            .await?;
        let now = format_time(OffsetDateTime::now_utc())?;

        sqlx::query(
            r#"
            INSERT INTO live_tv_tuners (
                tuner_id, provider_type, name, source_url, enabled, configuration_json,
                last_sync_at, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6, ?6)
            ON CONFLICT(tuner_id) DO UPDATE SET
                provider_type = excluded.provider_type,
                name = excluded.name,
                source_url = excluded.source_url,
                enabled = 1,
                configuration_json = excluded.configuration_json,
                last_sync_at = excluded.last_sync_at,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(tuner.tuner_id.trim())
        .bind(tuner.provider_type.trim())
        .bind(tuner.name.trim())
        .bind(tuner.source_url.as_deref())
        .bind(serde_json::to_string(&tuner.configuration)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM live_tv_channels WHERE tuner_id = ?1")
            .bind(tuner.tuner_id.trim())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM live_tv_categories WHERE tuner_id = ?1")
            .bind(tuner.tuner_id.trim())
            .execute(&mut *tx)
            .await?;

        for category in categories {
            sqlx::query(
                r#"
                INSERT INTO live_tv_categories (
                    category_id, tuner_id, remote_id, name, sort_name, created_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                "#,
            )
            .bind(category.category_id.trim())
            .bind(category.tuner_id.trim())
            .bind(category.remote_id.trim())
            .bind(category.name.trim())
            .bind(category.name.trim().to_ascii_lowercase())
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        for channel in channels {
            sqlx::query(
                r#"
                INSERT INTO live_tv_channels (
                    channel_id, tuner_id, remote_id, category_id, name, sort_name, number,
                    stream_url, logo_url, enabled, channel_type, metadata_json, created_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?12)
                "#,
            )
            .bind(channel.channel_id.trim())
            .bind(channel.tuner_id.trim())
            .bind(channel.remote_id.trim())
            .bind(channel.category_id.as_deref())
            .bind(channel.name.trim())
            .bind(channel.sort_name.trim())
            .bind(channel.number.as_deref())
            .bind(channel.stream_url.trim())
            .bind(channel.logo_url.as_deref())
            .bind(channel.channel_type.trim())
            .bind(serde_json::to_string(&channel.metadata)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        // Probe rows deliberately do not reference channels because this snapshot replaces those
        // rows. Remove only identities no longer present after the replacement is published.
        sqlx::query(
            r#"
            DELETE FROM live_tv_channel_stream_probes AS probe
            WHERE probe.tuner_id = ?1
              AND NOT EXISTS (
                  SELECT 1 FROM live_tv_channels AS channel
                  WHERE channel.channel_id = probe.channel_id
                    AND channel.tuner_id = probe.tuner_id
                    AND channel.remote_id = probe.remote_id
              )
            "#,
        )
        .bind(tuner.tuner_id.trim())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(tuner.configuration)
    }

    pub async fn live_tv_channel_page(
        &self,
        query: LiveTvChannelQuery,
    ) -> anyhow::Result<LiveTvPage<LiveTvChannelRecord>> {
        let total_record_count = self.live_tv_channel_count(&query).await?;
        let mut builder = live_tv_channel_select_builder();
        append_live_tv_channel_filters(&mut builder, &query);
        builder.push(" ORDER BY c.sort_name COLLATE NOCASE, c.name COLLATE NOCASE");
        if let Some(limit) = query.limit {
            builder.push(" LIMIT ");
            builder.push_bind(limit as i64);
            builder.push(" OFFSET ");
            builder.push_bind(query.start_index as i64);
        }
        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows
            .into_iter()
            .map(live_tv_channel_record_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(LiveTvPage {
            items,
            total_record_count,
            start_index: query.start_index,
        })
    }

    pub async fn live_tv_channel_count(&self, query: &LiveTvChannelQuery) -> anyhow::Result<usize> {
        let mut builder: QueryBuilder<Sqlite> = QueryBuilder::new(
            r#"
            SELECT COUNT(*) AS count
            FROM live_tv_channels c
            LEFT JOIN live_tv_categories cat ON cat.category_id = c.category_id
            WHERE c.enabled = 1
            "#,
        );
        append_live_tv_channel_filters(&mut builder, query);
        let row = builder.build().fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("count").max(0) as usize)
    }

    pub async fn live_tv_channel_by_id(
        &self,
        channel_id: &str,
    ) -> anyhow::Result<Option<LiveTvChannelRecord>> {
        let mut builder = live_tv_channel_select_builder();
        builder.push(" AND c.channel_id = ");
        builder.push_bind(channel_id.trim().to_string());
        let row = builder.build().fetch_optional(&self.pool).await?;
        row.map(live_tv_channel_record_from_row).transpose()
    }

    /// Returns only a current, exact-revision probe belonging to the channel still in the
    /// published catalogue. Expired and orphaned cache rows are indistinguishable from a miss.
    pub async fn live_tv_stream_probe(
        &self,
        channel_id: &str,
        source_revision: &str,
        probe_version: i16,
        now: OffsetDateTime,
    ) -> anyhow::Result<Option<LiveTvStreamProbeRecord>> {
        let row = sqlx::query(
            r#"
            SELECT probe.channel_id, probe.tuner_id, probe.remote_id,
                   probe.source_revision, probe.probe_version, probe.outcome,
                   probe.streams_json, probe.observed_at, probe.completed_at, probe.expires_at
            FROM live_tv_channel_stream_probes AS probe
            JOIN live_tv_channels AS channel
              ON channel.channel_id = probe.channel_id
             AND channel.tuner_id = probe.tuner_id
             AND channel.remote_id = probe.remote_id
            WHERE channel.enabled = 1
              AND probe.channel_id = ?1
              AND probe.source_revision = ?2
              AND probe.probe_version = ?3
              AND probe.expires_at > ?4
            "#,
        )
        .bind(channel_id.trim())
        .bind(source_revision.trim())
        .bind(i64::from(probe_version))
        .bind(format_time(now)?)
        .fetch_optional(&self.pool)
        .await?;
        row.map(live_tv_stream_probe_record_from_sqlite_row)
            .transpose()
    }

    /// Stores a sanitized probe only if its channel identity is currently published.
    pub async fn upsert_live_tv_stream_probe(
        &self,
        probe: LiveTvStreamProbeUpsert,
    ) -> anyhow::Result<()> {
        validate_live_tv_stream_probe(&probe)?;
        let streams_json = serde_json::to_string(&probe.streams)?;
        let result = sqlx::query(
            r#"
            INSERT INTO live_tv_channel_stream_probes (
                channel_id, tuner_id, remote_id, source_revision, probe_version, outcome,
                streams_json, observed_at, completed_at, expires_at
            )
            SELECT channel.channel_id, channel.tuner_id, channel.remote_id, ?4, ?5, ?6,
                   ?7, ?8, ?9, ?10
            FROM live_tv_channels AS channel
            WHERE channel.enabled = 1
              AND channel.channel_id = ?1
              AND channel.tuner_id = ?2
              AND channel.remote_id = ?3
            ON CONFLICT(channel_id, source_revision, probe_version) DO UPDATE SET
                tuner_id = excluded.tuner_id,
                remote_id = excluded.remote_id,
                outcome = CASE
                    WHEN excluded.observed_at >= live_tv_channel_stream_probes.observed_at
                    THEN excluded.outcome ELSE live_tv_channel_stream_probes.outcome END,
                streams_json = CASE
                    WHEN excluded.observed_at >= live_tv_channel_stream_probes.observed_at
                    THEN excluded.streams_json ELSE live_tv_channel_stream_probes.streams_json END,
                observed_at = max(excluded.observed_at, live_tv_channel_stream_probes.observed_at),
                completed_at = CASE
                    WHEN excluded.observed_at >= live_tv_channel_stream_probes.observed_at
                    THEN excluded.completed_at ELSE live_tv_channel_stream_probes.completed_at END,
                expires_at = CASE
                    WHEN excluded.observed_at >= live_tv_channel_stream_probes.observed_at
                    THEN excluded.expires_at ELSE live_tv_channel_stream_probes.expires_at END
            "#,
        )
        .bind(probe.channel_id.trim())
        .bind(probe.tuner_id.trim())
        .bind(probe.remote_id.trim())
        .bind(probe.source_revision.trim())
        .bind(i64::from(probe.probe_version))
        .bind(probe.outcome.as_str())
        .bind(streams_json)
        .bind(format_time(probe.observed_at)?)
        .bind(format_time(probe.completed_at)?)
        .bind(format_time(probe.expires_at)?)
        .execute(&self.pool)
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "Live TV stream probe channel identity is not published"
        );
        Ok(())
    }

    pub async fn delete_live_tv_stream_probes_for_channel(
        &self,
        channel_id: &str,
    ) -> anyhow::Result<u64> {
        Ok(
            sqlx::query("DELETE FROM live_tv_channel_stream_probes WHERE channel_id = ?1")
                .bind(channel_id.trim())
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    /// Removes a bounded batch of expired or orphaned derived rows.
    pub async fn cleanup_live_tv_stream_probes(
        &self,
        now: OffsetDateTime,
        limit: usize,
    ) -> anyhow::Result<u64> {
        let limit = limit.clamp(1, 10_000) as i64;
        Ok(sqlx::query(
            r#"
            DELETE FROM live_tv_channel_stream_probes
            WHERE rowid IN (
                SELECT probe.rowid
                FROM live_tv_channel_stream_probes AS probe
                LEFT JOIN live_tv_channels AS channel
                  ON channel.channel_id = probe.channel_id
                 AND channel.tuner_id = probe.tuner_id
                 AND channel.remote_id = probe.remote_id
                WHERE probe.expires_at <= ?1 OR channel.channel_id IS NULL
                ORDER BY probe.expires_at, probe.channel_id
                LIMIT ?2
            )
            "#,
        )
        .bind(format_time(now)?)
        .bind(limit)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn live_tv_categories(&self) -> anyhow::Result<Vec<LiveTvCategoryRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT category_id, tuner_id, remote_id, name, sort_name
            FROM live_tv_categories
            ORDER BY sort_name COLLATE NOCASE, name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(LiveTvCategoryRecord {
                    category_id: row.get("category_id"),
                    tuner_id: row.get("tuner_id"),
                    remote_id: row.get("remote_id"),
                    name: row.get("name"),
                    sort_name: row.get("sort_name"),
                })
            })
            .collect()
    }

    pub async fn live_tv_tuner_configurations_by_provider(
        &self,
        provider_type: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT configuration_json
            FROM live_tv_tuners
            WHERE enabled = 1 AND provider_type = ?1 COLLATE NOCASE
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .bind(provider_type.trim())
        .fetch_all(&self.pool)
        .await?;

        let configurations = rows
            .into_iter()
            .map(|row| {
                let configuration_json = row.get::<String, _>("configuration_json");
                serde_json::from_str(&configuration_json)
                    .context("invalid live TV tuner configuration json")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut resolved = Vec::with_capacity(configurations.len());
        for configuration in configurations {
            resolved.push(self.resolve_provider_configuration(&configuration).await?);
        }
        Ok(resolved)
    }

    pub async fn live_tv_tuner_configuration_by_id(
        &self,
        tuner_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query_scalar::<_, String>(
            r#"
            SELECT configuration_json
            FROM live_tv_tuners
            WHERE enabled = 1 AND tuner_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(tuner_id.trim())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|configuration| {
            serde_json::from_str(&configuration)
                .context("invalid persisted Live TV tuner configuration")
        })
        .transpose()
    }

    pub async fn delete_live_tv_tuner_state(&self, tuner_id: &str) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let deleted_configuration = sqlx::query_scalar::<_, String>(
            r#"
            SELECT configuration_json
            FROM live_tv_tuners
            WHERE tuner_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(tuner_id.trim())
        .fetch_optional(&mut *transaction)
        .await?
        .map(|configuration| {
            serde_json::from_str::<Value>(&configuration)
                .context("invalid persisted Live TV tuner configuration")
        })
        .transpose()?;

        sqlx::query("DELETE FROM live_tv_tuners WHERE tuner_id = ?1 COLLATE NOCASE")
            .bind(tuner_id.trim())
            .execute(&mut *transaction)
            .await?;

        if let Some(reference) = deleted_configuration
            .as_ref()
            .and_then(ProviderSecretReference::from_configuration)
        {
            let configurations = sqlx::query_scalar::<_, String>(
                r#"
                SELECT configuration_json FROM live_tv_tuners
                UNION ALL
                SELECT configuration_json FROM plugin_configurations
                UNION ALL
                SELECT payload_json FROM named_configurations
                "#,
            )
            .fetch_all(&mut *transaction)
            .await?;
            // Invalid unrelated configuration must fail closed for GC without preventing the
            // requested tuner deletion. A future repair can collect the retained envelope.
            let mut still_referenced = false;
            for configuration in configurations {
                let Ok(configuration) = serde_json::from_str::<Value>(&configuration) else {
                    still_referenced = true;
                    break;
                };
                if configuration_references_provider_secret(&configuration, &reference) {
                    still_referenced = true;
                    break;
                }
            }
            if !still_referenced {
                sqlx::query(
                    r#"
                    DELETE FROM provider_secrets
                    WHERE secret_id = ?1 AND provider_type = ?2 COLLATE NOCASE
                    "#,
                )
                .bind(&reference.id)
                .bind(&reference.provider_type)
                .execute(&mut *transaction)
                .await?;
            }
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn restore_plugin_platform_snapshot(&self, snapshot: &Value) -> anyhow::Result<()> {
        let version = snapshot
            .get("ModelVersion")
            .and_then(Value::as_i64)
            .context("plugin snapshot ModelVersion is missing")?;
        anyhow::ensure!(
            version == 1,
            "unsupported plugin snapshot ModelVersion {version}"
        );

        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut restored_plugin_configurations = Vec::new();
        for item in plugin_snapshot_items(snapshot, "PluginConfigurations")? {
            let plugin_id = plugin_snapshot_string(item, "PluginId")?;
            let mut configuration = plugin_snapshot_value(item, "Configuration")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if plugin_id.eq_ignore_ascii_case("jellyrin-xtream-provider") {
                configuration = self
                    .protect_provider_configuration_in_connection(&mut tx, "xtream", configuration)
                    .await?;
            }
            restored_plugin_configurations.push((
                plugin_id,
                serde_json::to_string(&configuration)?,
                plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()),
            ));
        }
        for table in [
            "plugin_audit_log",
            "plugin_host_events",
            "plugin_runtime_instances",
            "plugin_permissions",
            "plugin_configurations",
            "plugin_manifests",
            "installed_plugins",
            "package_installations",
            "package_catalog_cache",
            "plugin_repositories",
        ] {
            let sql = format!("DELETE FROM {table}");
            // `table` comes exclusively from the static allowlist above; no snapshot value is
            // interpolated into this administrative statement.
            sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .execute(&mut *tx)
                .await?;
        }

        for item in plugin_snapshot_items(snapshot, "Repositories")? {
            let name = plugin_snapshot_string(item, "Name")?;
            let url = plugin_snapshot_string(item, "Url")?;
            let payload = plugin_snapshot_value(item, "Payload")
                .cloned()
                .unwrap_or_else(|| {
                    json!({
                        "Name": name,
                        "Url": url,
                        "Enabled": plugin_snapshot_bool(item, "Enabled").unwrap_or(true)
                    })
                });
            sqlx::query(
                r#"
                INSERT INTO plugin_repositories (id, name, url, enabled, payload_json, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )
            .bind(stable_plugin_model_id("repo", &url))
            .bind(name)
            .bind(url)
            .bind(plugin_snapshot_bool(item, "Enabled").unwrap_or(true))
            .bind(serde_json::to_string(&payload)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PackageCatalogCache")? {
            let repository_url = plugin_snapshot_string(item, "RepositoryUrl")?;
            let name = plugin_snapshot_string(item, "Name")?;
            let version = plugin_snapshot_string(item, "Version")?;
            let runtime = plugin_snapshot_optional_string(item, "Runtime")
                .unwrap_or_else(|| "Unknown".to_string());
            let target_abi = plugin_snapshot_optional_string(item, "TargetAbi").unwrap_or_default();
            let payload = plugin_snapshot_value(item, "Payload")
                .cloned()
                .unwrap_or_else(|| json!({}));
            sqlx::query(
                r#"
                INSERT INTO package_catalog_cache (
                    id, repository_url, package_guid, package_name, package_version, runtime,
                    target_abi, payload_json, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )
            .bind(stable_plugin_model_id(
                "package",
                &format!("{repository_url}:{name}:{version}"),
            ))
            .bind(repository_url)
            .bind(plugin_snapshot_optional_string(item, "Guid"))
            .bind(name)
            .bind(version)
            .bind(runtime)
            .bind(target_abi)
            .bind(serde_json::to_string(&payload)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PackageInstallations")? {
            let name = plugin_snapshot_string(item, "Name")?;
            let version = plugin_snapshot_string(item, "Version")?;
            let guid = plugin_snapshot_optional_string(item, "Guid");
            let runtime = plugin_snapshot_optional_string(item, "Runtime")
                .unwrap_or_else(|| "Unknown".to_string());
            let payload = plugin_snapshot_value(item, "Payload")
                .cloned()
                .unwrap_or_else(|| json!({}));
            sqlx::query(
                r#"
                INSERT INTO package_installations (
                    id, package_name, package_guid, version, runtime, status, source_url,
                    payload_json, installed_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
            )
            .bind(stable_plugin_model_id(
                "install",
                &format!("{}:{}", guid.as_deref().unwrap_or(&name), version),
            ))
            .bind(name)
            .bind(guid)
            .bind(version)
            .bind(runtime)
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Installed".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "SourceUrl"))
            .bind(serde_json::to_string(&payload)?)
            .bind(plugin_snapshot_optional_string(item, "InstalledAt"))
            .bind(plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "InstalledPlugins")? {
            let plugin_id = plugin_snapshot_string(item, "Id")
                .or_else(|_| plugin_snapshot_string(item, "Guid"))?;
            sqlx::query(
                r#"
                INSERT INTO installed_plugins (
                    plugin_id, name, version, runtime, runtime_version, target_abi,
                    server_compatibility_json, status, capabilities_json, permissions_json,
                    configuration_state, last_error, health_json, manifest_json, installed_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                "#,
            )
            .bind(plugin_id)
            .bind(plugin_snapshot_string(item, "Name")?)
            .bind(plugin_snapshot_string(item, "Version")?)
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "RuntimeVersion").unwrap_or_default())
            .bind(plugin_snapshot_optional_string(item, "TargetAbi").unwrap_or_default())
            .bind(plugin_snapshot_json_string(
                item,
                "ServerCompatibility",
                json!({}),
            )?)
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "NotSupported".to_string()),
            )
            .bind(plugin_snapshot_json_string(
                item,
                "Capabilities",
                json!([]),
            )?)
            .bind(plugin_snapshot_json_string(item, "Permissions", json!([]))?)
            .bind(
                plugin_snapshot_optional_string(item, "ConfigurationState")
                    .unwrap_or_else(|| "Default".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "LastError"))
            .bind(plugin_snapshot_json_string(item, "Health", json!({}))?)
            .bind(plugin_snapshot_json_string(item, "Manifest", json!({}))?)
            .bind(plugin_snapshot_optional_string(item, "InstalledAt"))
            .bind(plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginManifests")? {
            sqlx::query(
                "INSERT INTO plugin_manifests (plugin_id, manifest_json, updated_at) VALUES (?1, ?2, ?3)",
            )
            .bind(plugin_snapshot_string(item, "PluginId")?)
            .bind(plugin_snapshot_json_string(item, "Manifest", json!({}))?)
            .bind(plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for (plugin_id, configuration, updated_at) in restored_plugin_configurations {
            sqlx::query(
                "INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at) VALUES (?1, ?2, ?3)",
            )
            .bind(plugin_id)
            .bind(configuration)
            .bind(updated_at)
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginPermissions")? {
            sqlx::query(
                "INSERT INTO plugin_permissions (plugin_id, permissions_json, updated_at) VALUES (?1, ?2, ?3)",
            )
            .bind(plugin_snapshot_string(item, "PluginId")?)
            .bind(plugin_snapshot_json_string(item, "Permissions", json!([]))?)
            .bind(plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginRuntimeInstances")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_runtime_instances (
                    instance_id, plugin_id, runtime, runtime_version, status, process_id,
                    endpoint, health_json, last_error, started_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
            )
            .bind(plugin_snapshot_string(item, "InstanceId")?)
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(
                plugin_snapshot_optional_string(item, "Runtime")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "RuntimeVersion").unwrap_or_default())
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Stopped".to_string()),
            )
            .bind(plugin_snapshot_value(item, "ProcessId").and_then(Value::as_i64))
            .bind(plugin_snapshot_optional_string(item, "Endpoint"))
            .bind(plugin_snapshot_json_string(item, "Health", json!({}))?)
            .bind(plugin_snapshot_optional_string(item, "LastError"))
            .bind(plugin_snapshot_optional_string(item, "StartedAt"))
            .bind(plugin_snapshot_optional_string(item, "UpdatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginHostEvents")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_host_events (
                    id, plugin_id, runtime, event_type, severity, message, payload_json, created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )
            .bind(
                plugin_snapshot_optional_string(item, "Id")
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(plugin_snapshot_optional_string(item, "Runtime"))
            .bind(plugin_snapshot_string(item, "EventType")?)
            .bind(
                plugin_snapshot_optional_string(item, "Severity")
                    .unwrap_or_else(|| "Information".to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "Message").unwrap_or_default())
            .bind(plugin_snapshot_json_string(item, "Payload", json!({}))?)
            .bind(plugin_snapshot_optional_string(item, "CreatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        for item in plugin_snapshot_items(snapshot, "PluginAuditLog")? {
            sqlx::query(
                r#"
                INSERT INTO plugin_audit_log (
                    id, plugin_id, action, actor_user_id, status, payload_json, created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
            )
            .bind(
                plugin_snapshot_optional_string(item, "Id")
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
            )
            .bind(plugin_snapshot_optional_string(item, "PluginId"))
            .bind(plugin_snapshot_string(item, "Action")?)
            .bind(plugin_snapshot_optional_string(item, "ActorUserId"))
            .bind(
                plugin_snapshot_optional_string(item, "Status")
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .bind(plugin_snapshot_json_string(item, "Payload", json!({}))?)
            .bind(plugin_snapshot_optional_string(item, "CreatedAt").unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn install_plugin_package(
        &self,
        package: InstallPluginPackage,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let source_url = json_array_case_insensitive(&package.package, "Versions")
            .and_then(|versions| {
                versions.iter().find(|version| {
                    json_string_case_insensitive(version, "Version")
                        .is_some_and(|version| version.eq_ignore_ascii_case(&package.version))
                })
            })
            .and_then(|version| json_string_case_insensitive(version, "SourceUrl"))
            .or_else(|| json_string_case_insensitive(&package.package, "SourceUrl"));
        let install_id = stable_plugin_model_id(
            "install",
            &format!("{}:{}", package.plugin_id, package.version),
        );
        let runtime_missing = format!("{} runtime host is not implemented yet.", package.runtime);
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            UPDATE package_installations
            SET status = 'Superseded', updated_at = ?1
            WHERE package_guid = ?2 COLLATE NOCASE
              AND version != ?3 COLLATE NOCASE
              AND status = 'Installed'
            "#,
        )
        .bind(&now)
        .bind(&package.plugin_id)
        .bind(&package.version)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO package_installations (
                id, package_name, package_guid, version, runtime, status, source_url,
                payload_json, installed_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'Installed', ?6, ?7, ?8, ?8)
            ON CONFLICT(id) DO UPDATE SET
                package_name = excluded.package_name,
                package_guid = excluded.package_guid,
                version = excluded.version,
                runtime = excluded.runtime,
                status = excluded.status,
                source_url = excluded.source_url,
                payload_json = excluded.payload_json,
                installed_at = COALESCE(package_installations.installed_at, excluded.installed_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&install_id)
        .bind(&package.name)
        .bind(&package.plugin_id)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(&source_url)
        .bind(serde_json::to_string(&package.package)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility_json,
                status, capabilities_json, permissions_json, configuration_state, last_error,
                health_json, manifest_json, installed_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, '{}', 'NotSupported', '[]', '[]', 'Default', ?6, ?7, ?8, ?9, ?9)
            ON CONFLICT(plugin_id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                runtime = excluded.runtime,
                target_abi = excluded.target_abi,
                status = excluded.status,
                last_error = excluded.last_error,
                health_json = excluded.health_json,
                manifest_json = excluded.manifest_json,
                installed_at = COALESCE(installed_plugins.installed_at, excluded.installed_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&package.name)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(&package.target_abi)
        .bind(&runtime_missing)
        .bind(serde_json::to_string(&json!({
            "Status": "NotSupported",
            "Message": runtime_missing
        }))?)
        .bind(serde_json::to_string(&package.manifest)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_manifests (plugin_id, manifest_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(plugin_id) DO UPDATE SET
                manifest_json = excluded.manifest_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&package.plugin_id)
        .bind(serde_json::to_string(&package.manifest)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at)
            VALUES (?1, '{}', ?2)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions_json, updated_at)
            VALUES (?1, '[]', ?2)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'Install', ?3, 'NotSupported', ?4, ?5)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&package.plugin_id)
        .bind(actor_user_id.map(|id| id.to_string()))
        .bind(serde_json::to_string(&json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "Reason": runtime_missing
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (id, plugin_id, runtime, event_type, severity, message, payload_json, created_at)
            VALUES (?1, ?2, ?3, 'RuntimeUnavailable', 'Warning', ?4, ?5, ?6)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&package.plugin_id)
        .bind(&package.runtime)
        .bind(&runtime_missing)
        .bind(serde_json::to_string(&json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "Status": "NotSupported"
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn ensure_builtin_plugin(
        &self,
        plugin_id: &str,
        name: &str,
        version: &str,
        manifest: &Value,
        capabilities: &[&str],
    ) -> anyhow::Result<bool> {
        let existing_status = self
            .installed_plugin_json(plugin_id)
            .await?
            .and_then(|plugin| {
                plugin
                    .get("Status")
                    .and_then(Value::as_str)
                    .filter(|status| !status.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "Active".to_string());
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility_json,
                status, capabilities_json, permissions_json, configuration_state,
                last_error, health_json, manifest_json, installed_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'Builtin', '', '{}', ?4, ?5, '[]',
                    'Default', NULL, '{}', ?6, ?7, ?7)
            ON CONFLICT(plugin_id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                runtime = excluded.runtime,
                capabilities_json = excluded.capabilities_json,
                manifest_json = excluded.manifest_json,
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(plugin_id)
        .bind(name)
        .bind(version)
        .bind(existing_status)
        .bind(serde_json::to_string(capabilities)?)
        .bind(serde_json::to_string(manifest)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn installed_plugin_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            r#"
            SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
                server_compatibility_json, status, capabilities_json, permissions_json,
                configuration_state, last_error, health_json, manifest_json
            FROM installed_plugins
            WHERE plugin_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut plugin = plugin_row_to_json(&row)?;
        enrich_plugin_runtime_state(&self.pool, &mut plugin).await?;
        Ok(Some(plugin))
    }

    pub async fn installed_plugins_json(&self) -> anyhow::Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
                server_compatibility_json, status, capabilities_json, permissions_json,
                configuration_state, last_error, health_json, manifest_json
            FROM installed_plugins
            ORDER BY name COLLATE NOCASE, version COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut plugins = rows
            .into_iter()
            .map(|row| plugin_row_to_json(&row))
            .collect::<anyhow::Result<Vec<_>>>()?;
        for plugin in &mut plugins {
            enrich_plugin_runtime_state(&self.pool, plugin).await?;
        }
        Ok(plugins)
    }

    pub async fn plugin_health_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let Some(plugin) = self.installed_plugin_json(plugin_id).await? else {
            return Ok(None);
        };
        Ok(Some(json!({
            "PluginId": plugin["Id"].clone(),
            "Guid": plugin["Guid"].clone(),
            "Name": plugin["Name"].clone(),
            "Version": plugin["Version"].clone(),
            "Runtime": plugin["Runtime"].clone(),
            "Status": plugin["Status"].clone(),
            "LastError": plugin["LastError"].clone(),
            "Health": plugin["Health"].clone(),
            "RuntimeInstances": plugin["RuntimeInstances"].clone(),
            "RecentEvents": plugin["RecentEvents"].clone()
        })))
    }

    pub async fn plugin_host_events_json(
        &self,
        plugin_id: &str,
        limit: i64,
    ) -> anyhow::Result<Option<Vec<Value>>> {
        if self.installed_plugin_json(plugin_id).await?.is_none() {
            return Ok(None);
        }
        plugin_host_events_for_plugin(&self.pool, plugin_id, limit.clamp(1, 250))
            .await
            .map(Some)
    }

    pub async fn upsert_discovered_plugin_package(
        &self,
        package: DiscoveredPluginPackage,
    ) -> anyhow::Result<bool> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let runtime_missing = format!("{} runtime host is not implemented yet.", package.runtime);
        let mut manifest = package.manifest;
        if !manifest.is_object() {
            manifest = json!({});
        }
        manifest["Guid"] = json!(package.plugin_id);
        manifest["Name"] = json!(package.name);
        manifest["Version"] = json!(package.version);
        manifest["Runtime"] = json!(package.runtime);
        manifest["TargetAbi"] = json!(package.target_abi);
        manifest["Installation"] = json!({
            "Mode": "filesystem-discovered",
            "InstallPath": package.install_path
        });

        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            INSERT INTO installed_plugins (
                plugin_id, name, version, runtime, target_abi, server_compatibility_json,
                status, capabilities_json, permissions_json, configuration_state, last_error,
                health_json, manifest_json, installed_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, '{}', 'NotSupported', '[]', '[]', 'Default', ?6, ?7, ?8, ?9, ?9)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&package.name)
        .bind(&package.version)
        .bind(&package.runtime)
        .bind(&package.target_abi)
        .bind(&runtime_missing)
        .bind(serde_json::to_string(&json!({
            "Status": "NotSupported",
            "Message": runtime_missing
        }))?)
        .bind(serde_json::to_string(&manifest)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        sqlx::query(
            r#"
            INSERT INTO plugin_manifests (plugin_id, manifest_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(serde_json::to_string(&manifest)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at)
            VALUES (?1, '{}', ?2)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions_json, updated_at)
            VALUES (?1, '[]', ?2)
            ON CONFLICT(plugin_id) DO NOTHING
            "#,
        )
        .bind(&package.plugin_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (id, plugin_id, runtime, event_type, severity, message, payload_json, created_at)
            VALUES (?1, ?2, ?3, 'Discovery', 'Information', ?4, ?5, ?6)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&package.plugin_id)
        .bind(&package.runtime)
        .bind(format!(
            "{} {} discovered from filesystem.",
            package.name, package.version
        ))
        .bind(serde_json::to_string(&json!({
            "InstallPath": package.install_path,
            "Runtime": package.runtime
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'Discover', NULL, 'NotSupported', ?3, ?4)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&package.plugin_id)
        .bind(serde_json::to_string(&json!({
            "Name": package.name,
            "Version": package.version,
            "Runtime": package.runtime,
            "InstallPath": package.install_path
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn package_installations_json(&self, plugin_id: &str) -> anyhow::Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT package_name, package_guid, version, runtime, status, source_url,
                payload_json, installed_at, updated_at
            FROM package_installations
            WHERE package_guid = ?1 COLLATE NOCASE
            ORDER BY version COLLATE NOCASE
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                    .context("invalid package installation payload")?;
                Ok(json!({
                    "Name": row.get::<String, _>("package_name"),
                    "Guid": row.get::<Option<String>, _>("package_guid"),
                    "Version": row.get::<String, _>("version"),
                    "Runtime": row.get::<String, _>("runtime"),
                    "Status": row.get::<String, _>("status"),
                    "SourceUrl": row.get::<Option<String>, _>("source_url"),
                    "Payload": payload,
                    "InstalledAt": row.get::<Option<String>, _>("installed_at"),
                    "UpdatedAt": row.get::<String, _>("updated_at")
                }))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }

    pub async fn installed_plugin_manifest(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            r#"
            SELECT manifest_json
            FROM plugin_manifests
            WHERE plugin_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            serde_json::from_str(row.get::<&str, _>("manifest_json"))
                .context("invalid plugin manifest payload")
        })
        .transpose()
    }

    pub async fn plugin_configuration_json(
        &self,
        plugin_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            r#"
            SELECT configuration_json
            FROM plugin_configurations
            WHERE plugin_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            serde_json::from_str(row.get::<&str, _>("configuration_json"))
                .context("invalid plugin configuration payload")
        })
        .transpose()
    }

    pub async fn plugin_permissions_json(&self, plugin_id: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            r#"
            SELECT permissions_json
            FROM plugin_permissions
            WHERE plugin_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            serde_json::from_str(row.get::<&str, _>("permissions_json"))
                .context("invalid plugin permissions payload")
        })
        .transpose()
    }

    pub async fn update_plugin_configuration_json(
        &self,
        plugin_id: &str,
        mut configuration: Value,
    ) -> anyhow::Result<bool> {
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM plugin_manifests WHERE plugin_id = ?1 COLLATE NOCASE
                UNION ALL
                SELECT 1 FROM installed_plugins WHERE plugin_id = ?1 COLLATE NOCASE
            )
            "#,
        )
        .bind(plugin_id.trim())
        .fetch_one(&mut *transaction)
        .await?
            != 0;
        if !exists {
            transaction.rollback().await?;
            return Ok(false);
        }
        if plugin_id
            .trim()
            .eq_ignore_ascii_case("jellyrin-xtream-provider")
        {
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT configuration_json FROM plugin_configurations WHERE plugin_id = ?1 COLLATE NOCASE",
            )
            .bind(plugin_id.trim())
            .fetch_optional(&mut *transaction)
            .await?
            .map(|configuration| {
                serde_json::from_str::<Value>(&configuration)
                    .context("invalid plugin configuration payload")
            })
            .transpose()?;
            inherit_provider_secret_reference(&mut configuration, existing.as_ref());
            configuration = self
                .protect_provider_configuration_in_connection(
                    &mut transaction,
                    "xtream",
                    configuration,
                )
                .await?;
        }
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(plugin_id) DO UPDATE SET
                configuration_json = excluded.configuration_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(plugin_id.trim())
        .bind(serde_json::to_string(&configuration)?)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn update_plugin_permissions_json(
        &self,
        plugin_id: &str,
        permissions: Value,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        if self.installed_plugin_manifest(plugin_id).await?.is_none() {
            return Ok(false);
        }
        let now = format_time(OffsetDateTime::now_utc())?;
        let permissions_json = serde_json::to_string(&permissions)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_permissions (plugin_id, permissions_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(plugin_id) DO UPDATE SET
                permissions_json = excluded.permissions_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(plugin_id.trim())
        .bind(&permissions_json)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            UPDATE installed_plugins
            SET permissions_json = ?1, updated_at = ?2
            WHERE plugin_id = ?3 COLLATE NOCASE
            "#,
        )
        .bind(&permissions_json)
        .bind(&now)
        .bind(plugin_id.trim())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'UpdatePermissions', ?3, 'Updated', ?4, ?5)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(plugin_id.trim())
        .bind(actor_user_id.map(|id| id.to_string()))
        .bind(permissions_json)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn set_installed_plugin_status(
        &self,
        plugin_id: &str,
        status: &str,
        last_error: Option<&str>,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            UPDATE installed_plugins
            SET status = ?1, last_error = ?2, updated_at = ?3
            WHERE plugin_id = ?4 COLLATE NOCASE
            "#,
        )
        .bind(status)
        .bind(last_error)
        .bind(&now)
        .bind(plugin_id.trim())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(false);
        }
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'SetStatus', ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(plugin_id.trim())
        .bind(actor_user_id.map(|id| id.to_string()))
        .bind(status)
        .bind(serde_json::to_string(&json!({ "LastError": last_error }))?)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    pub async fn upsert_plugin_runtime_instance(
        &self,
        instance: PluginRuntimeInstanceUpsert,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = instance.plugin_id.trim();
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        let plugin_result = sqlx::query(
            r#"
            UPDATE installed_plugins
            SET runtime_version = ?1,
                status = ?2,
                capabilities_json = ?3,
                last_error = ?4,
                health_json = ?5,
                updated_at = ?6
            WHERE plugin_id = ?7 COLLATE NOCASE
            "#,
        )
        .bind(&instance.runtime_version)
        .bind(&instance.status)
        .bind(serde_json::to_string(&instance.capabilities)?)
        .bind(instance.last_error.as_deref())
        .bind(serde_json::to_string(&instance.health)?)
        .bind(&now)
        .bind(normalized)
        .execute(&mut *tx)
        .await?;
        if plugin_result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        let instance_id = plugin_runtime_instance_id(normalized, &instance.runtime);
        sqlx::query(
            r#"
            INSERT INTO plugin_runtime_instances (
                instance_id, plugin_id, runtime, runtime_version, status, process_id,
                endpoint, health_json, last_error, started_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
            ON CONFLICT(instance_id) DO UPDATE SET
                runtime_version = excluded.runtime_version,
                status = excluded.status,
                process_id = excluded.process_id,
                endpoint = excluded.endpoint,
                health_json = excluded.health_json,
                last_error = excluded.last_error,
                started_at = COALESCE(plugin_runtime_instances.started_at, excluded.started_at),
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&instance_id)
        .bind(normalized)
        .bind(&instance.runtime)
        .bind(&instance.runtime_version)
        .bind(&instance.status)
        .bind(instance.process_id)
        .bind(instance.endpoint.as_deref())
        .bind(serde_json::to_string(&instance.health)?)
        .bind(instance.last_error.as_deref())
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_host_events (id, plugin_id, runtime, event_type, severity, message, payload_json, created_at)
            VALUES (?1, ?2, ?3, 'RuntimeStatus', ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(normalized)
        .bind(&instance.runtime)
        .bind(if instance.status.eq_ignore_ascii_case("Active") {
            "Information"
        } else {
            "Warning"
        })
        .bind(format!(
            "{} runtime status changed to {}.",
            instance.runtime, instance.status
        ))
        .bind(serde_json::to_string(&json!({
            "InstanceId": instance_id,
            "RuntimeVersion": instance.runtime_version,
            "ProcessId": instance.process_id,
            "Endpoint": instance.endpoint,
            "Health": instance.health,
            "LastError": instance.last_error
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'RuntimeStatus', ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(normalized)
        .bind(actor_user_id.map(|id| id.to_string()))
        .bind(&instance.status)
        .bind(serde_json::to_string(&json!({
            "Runtime": instance.runtime,
            "RuntimeVersion": instance.runtime_version,
            "Capabilities": instance.capabilities
        }))?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(true)
    }

    pub async fn uninstall_plugin_state(
        &self,
        plugin_id: &str,
        actor_user_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let normalized = plugin_id.trim();
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            DELETE FROM installed_plugins
            WHERE plugin_id = ?1 COLLATE NOCASE
            "#,
        )
        .bind(normalized)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        for table in [
            "plugin_manifests",
            "plugin_configurations",
            "plugin_permissions",
            "plugin_runtime_instances",
        ] {
            let sql = format!("DELETE FROM {table} WHERE plugin_id = ?1 COLLATE NOCASE");
            // `table` comes exclusively from the static allowlist above; the plugin id remains a
            // bind parameter and is never interpolated into SQL.
            sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(normalized)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            r#"
            DELETE FROM package_installations
            WHERE package_guid = ?1 COLLATE NOCASE
            "#,
        )
        .bind(normalized)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO plugin_audit_log (id, plugin_id, action, actor_user_id, status, payload_json, created_at)
            VALUES (?1, ?2, 'Uninstall', ?3, 'Deleted', '{}', ?4)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(normalized)
        .bind(actor_user_id.map(|id| id.to_string()))
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
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

    pub async fn named_configurations(&self) -> anyhow::Result<Vec<NamedConfigurationPayload>> {
        let rows = sqlx::query_as::<_, NamedConfigurationListRow>(
            r#"
            SELECT key, payload_json
            FROM named_configurations
            ORDER BY key
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let payload = serde_json::from_str(&row.payload_json)
                    .context("invalid named configuration")?;
                Ok(NamedConfigurationPayload {
                    key: row.key,
                    payload,
                })
            })
            .collect()
    }

    pub async fn update_named_configuration(
        &self,
        key: &str,
        mut payload: Value,
    ) -> anyhow::Result<()> {
        let key = normalize_configuration_key(key);
        anyhow::ensure!(!key.is_empty(), "configuration key must not be empty");
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        if key == "livetv" {
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT payload_json FROM named_configurations WHERE key = ?1",
            )
            .bind(&key)
            .fetch_optional(&mut *transaction)
            .await?
            .map(|payload| {
                serde_json::from_str::<Value>(&payload).context("invalid named configuration")
            })
            .transpose()?;
            payload = self
                .protect_live_tv_named_configuration_in_connection(
                    &mut transaction,
                    payload,
                    existing.as_ref(),
                )
                .await?;
        }

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
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
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
                name, overview, short_overview, entry_type, severity, user_id, item_id, created_at
            )
            VALUES (?1, ?2, ?3, ?4, 'Information', ?5, ?6, ?7)
            "#,
        )
        .bind(trimmed_name)
        .bind(trimmed_optional_str(overview))
        .bind(trimmed_optional_str(short_overview))
        .bind(trimmed_type)
        .bind(user_id.map(|id| id.to_string()))
        .bind(item_id.map(|id| id.to_string()))
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
             activity_log_entries.user_id, activity_log_entries.item_id, activity_log_entries.created_at \
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

    pub async fn media_item_lyrics(
        &self,
        item_id: Uuid,
    ) -> anyhow::Result<Option<MediaItemLyrics>> {
        let row = sqlx::query_as::<_, MediaItemLyricsRow>(
            r#"
            SELECT item_id, lyrics_json, updated_at
            FROM media_item_lyrics
            WHERE item_id = ?1
            "#,
        )
        .bind(item_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn update_media_item_lyrics(
        &self,
        item_id: Uuid,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.media_item_by_id(item_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let lyrics_json = serde_json::to_string(&payload)?;
        sqlx::query(
            r#"
            INSERT INTO media_item_lyrics (item_id, lyrics_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(item_id) DO UPDATE SET
                lyrics_json = excluded.lyrics_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(item_id.to_string())
        .bind(lyrics_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_media_item_lyrics(&self, item_id: Uuid) -> anyhow::Result<bool> {
        self.media_item_by_id(item_id).await?;
        let result = sqlx::query(
            r#"
            DELETE FROM media_item_lyrics
            WHERE item_id = ?1
            "#,
        )
        .bind(item_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
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
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
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
            INSERT INTO users (id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at)
            VALUES (?1, ?2, 1, 0, ?3, ?4, ?4)
            ON CONFLICT(name) DO UPDATE SET
                is_administrator = 1,
                is_disabled = 0,
                sync_play_access = excluded.sync_play_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_name)
        .bind(DEFAULT_SYNC_PLAY_ACCESS)
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

    pub async fn create_user(&self, name: &str, password: Option<&str>) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        anyhow::ensure!(!trimmed_name.is_empty(), "user name must not be empty");
        let now = format_time(OffsetDateTime::now_utc())?;
        let user_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at)
            VALUES (?1, ?2, 0, 0, ?3, ?4, ?4)
            "#,
        )
        .bind(user_id.to_string())
        .bind(trimmed_name)
        .bind(DEFAULT_SYNC_PLAY_ACCESS)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        if let Some(password) = password.filter(|password| !password.is_empty()) {
            self.set_user_password(user_id, password).await?;
        }

        self.user_by_id(user_id).await
    }

    pub async fn delete_user(&self, user_id: Uuid) -> anyhow::Result<()> {
        let user = self.user_by_id(user_id).await?;
        if user.is_administrator {
            let admin_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users WHERE is_administrator = 1 AND is_disabled = 0",
            )
            .fetch_one(&self.pool)
            .await?;
            anyhow::ensure!(
                admin_count > 1,
                "cannot delete the last enabled administrator"
            );
        }

        sqlx::query("DELETE FROM users WHERE id = ?1")
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
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
        sync_play_access: &str,
    ) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        anyhow::ensure!(!trimmed_name.is_empty(), "user name must not be empty");
        self.user_by_id(user_id).await?;

        sqlx::query(
            r#"
            UPDATE users
            SET name = ?1, is_administrator = ?2, is_disabled = ?3, sync_play_access = ?4, updated_at = ?5
            WHERE id = ?6
            "#,
        )
        .bind(trimmed_name)
        .bind(is_administrator)
        .bind(is_disabled)
        .bind(sync_play_access.trim())
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

    pub async fn authenticate_user_by_id(
        &self,
        user_id: Uuid,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_id(user_id).await?;
        anyhow::ensure!(!user.is_disabled, "user is disabled");
        self.verify_user_password(user.id, password).await?;
        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn issue_device_token_for_user(
        &self,
        user_id: Uuid,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_id(user_id).await?;
        anyhow::ensure!(!user.is_disabled, "user is disabled");
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
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::AuthUserByToken, DatabasePoolRole::Api);
        let result = self.user_by_token_unobserved(token).await;
        observation.finish_result(&result, |_| 1);
        result
    }

    async fn user_by_token_unobserved(&self, token: &str) -> anyhow::Result<(User, DeviceToken)> {
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
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::AuthUserByApiKey, DatabasePoolRole::Api);
        let result = self.user_by_api_key_unobserved(api_key).await;
        observation.finish_result(&result, |_| 1);
        result
    }

    async fn user_by_api_key_unobserved(
        &self,
        api_key: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
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

    pub async fn initiate_quick_connect(
        &self,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<QuickConnectSession> {
        let now = OffsetDateTime::now_utc();
        let expires_at = now + Duration::minutes(10);
        let secret = Uuid::new_v4().simple().to_string();
        let code = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(6)
            .collect::<String>()
            .to_ascii_uppercase();
        sqlx::query(
            r#"
            INSERT INTO quick_connect_sessions (
                secret, code, device_id, device_name, client, version,
                user_id, authorized, created_at, updated_at, expires_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, ?7, ?7, ?8)
            "#,
        )
        .bind(&secret)
        .bind(&code)
        .bind(device_id)
        .bind(device_name)
        .bind(client)
        .bind(version)
        .bind(format_time(now)?)
        .bind(format_time(expires_at)?)
        .execute(&self.pool)
        .await?;

        self.quick_connect_by_secret(&secret).await
    }

    pub async fn quick_connect_by_secret(
        &self,
        secret: &str,
    ) -> anyhow::Result<QuickConnectSession> {
        let session = sqlx::query_as::<_, QuickConnectSessionRow>(
            r#"
            SELECT secret, code, device_id, device_name, client, version,
                   user_id, authorized, created_at, updated_at, expires_at
            FROM quick_connect_sessions
            WHERE secret = ?1
            "#,
        )
        .bind(secret)
        .fetch_optional(&self.pool)
        .await?
        .context("quick connect session not found")?;
        session.try_into()
    }

    pub async fn authorize_quick_connect(
        &self,
        code: &str,
        user_id: Uuid,
    ) -> anyhow::Result<QuickConnectSession> {
        self.user_by_id(user_id).await?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let result = sqlx::query(
            r#"
            UPDATE quick_connect_sessions
            SET user_id = ?1, authorized = 1, updated_at = ?2
            WHERE code = ?3 AND expires_at > ?2
            "#,
        )
        .bind(user_id.to_string())
        .bind(&now_text)
        .bind(code.trim().to_ascii_uppercase())
        .execute(&self.pool)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "quick connect code not found");
        let session = sqlx::query_as::<_, QuickConnectSessionRow>(
            r#"
            SELECT secret, code, device_id, device_name, client, version,
                   user_id, authorized, created_at, updated_at, expires_at
            FROM quick_connect_sessions
            WHERE code = ?1
            "#,
        )
        .bind(code.trim().to_ascii_uppercase())
        .fetch_one(&self.pool)
        .await?;
        session.try_into()
    }

    pub async fn delete_quick_connect_session(&self, secret: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM quick_connect_sessions WHERE secret = ?1")
            .bind(secret)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn backup_manifests(&self) -> anyhow::Result<Vec<BackupManifest>> {
        let rows = sqlx::query_as::<_, BackupManifestRow>(
            r#"
            SELECT path, server_version, backup_engine_version, options_json, restore_snapshot_json, created_at
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
            SELECT path, server_version, backup_engine_version, options_json, restore_snapshot_json, created_at
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
        restore_snapshot: Option<Value>,
    ) -> anyhow::Result<BackupManifest> {
        let now = OffsetDateTime::now_utc();
        let created_at = format_time(now)?;
        let path = format!("jellyrin-backup-{}.zip", Uuid::new_v4().simple());
        sqlx::query(
            r#"
            INSERT INTO backup_manifests (
                path, server_version, backup_engine_version, options_json, restore_snapshot_json, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(&path)
        .bind(server_version)
        .bind(backup_engine_version)
        .bind(serde_json::to_string(&options)?)
        .bind(restore_snapshot.as_ref().map(serde_json::to_string).transpose()?)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(BackupManifest {
            path,
            server_version: server_version.to_string(),
            backup_engine_version: backup_engine_version.to_string(),
            options,
            restore_snapshot,
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

        sqlx::query(
            r#"
            DELETE FROM active_viewing_sessions
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

        sqlx::query(
            r#"
            DELETE FROM active_viewing_sessions
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
            WHERE users.is_disabled = 0 AND (
                devices.user_id = ?1 OR EXISTS (
                    SELECT 1 FROM active_session_users
                    WHERE active_session_users.session_id = devices.access_token
                      AND active_session_users.user_id = ?1
                )
            )
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

    pub async fn ensure_device_session(&self, token: &DeviceToken) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            DELETE FROM devices
            WHERE user_id = ?1 AND device_id = ?2 AND access_token != ?3
            "#,
        )
        .bind(token.user_id.to_string())
        .bind(&token.device_id)
        .bind(&token.access_token)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO devices (
                access_token, user_id, device_id, device_name, client, version, created_at, last_activity_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(access_token) DO UPDATE SET
                user_id = excluded.user_id,
                device_id = excluded.device_id,
                device_name = excluded.device_name,
                client = excluded.client,
                version = excluded.version,
                last_activity_at = excluded.last_activity_at
            "#,
        )
        .bind(&token.access_token)
        .bind(token.user_id.to_string())
        .bind(&token.device_id)
        .bind(&token.device_name)
        .bind(&token.client)
        .bind(&token.version)
        .bind(now)
        .execute(&self.pool)
        .await?;
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
        let item_id = self.media_item_storage_id(playback.item_id).await?;
        let existing_stream_indexes =
            if playback.audio_stream_index.is_none() || playback.subtitle_stream_index.is_none() {
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
                .and_then(
                    |(stored_item_id, audio_stream_index, subtitle_stream_index)| {
                        (stored_item_id == item_id)
                            .then_some((audio_stream_index, subtitle_stream_index))
                    },
                )
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
        .bind(item_id)
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

    pub async fn upsert_active_viewing_session(
        &self,
        viewing: UpsertActiveViewingSession,
    ) -> anyhow::Result<()> {
        let trimmed_session_id = viewing.session_id.trim();
        anyhow::ensure!(
            !trimmed_session_id.is_empty(),
            "session id must not be empty"
        );
        let item_id = self.media_item_storage_id(viewing.item_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO active_viewing_sessions (session_id, user_id, item_id, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(session_id) DO UPDATE SET
                user_id = excluded.user_id,
                item_id = excluded.item_id,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(trimmed_session_id)
        .bind(viewing.user_id.to_string())
        .bind(item_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_active_viewing_session(&self, session_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_viewing_sessions WHERE session_id = ?1")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_viewing_sessions(&self) -> anyhow::Result<Vec<ActiveViewingSession>> {
        let rows = sqlx::query_as::<_, ActiveViewingSessionRow>(
            r#"
            SELECT active_viewing_sessions.session_id,
                   active_viewing_sessions.user_id,
                   active_viewing_sessions.updated_at AS viewing_updated_at,
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
            FROM active_viewing_sessions
            INNER JOIN media_items ON media_items.id = active_viewing_sessions.item_id
            WHERE media_items.missing_since IS NULL
            ORDER BY active_viewing_sessions.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn add_session_user(&self, session_id: &str, user_id: Uuid) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO active_session_users (session_id, user_id, added_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(session_id, user_id) DO UPDATE SET
                added_at = excluded.added_at
            "#,
        )
        .bind(session_id.trim())
        .bind(user_id.to_string())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_session_user(&self, session_id: &str, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM active_session_users WHERE session_id = ?1 AND user_id = ?2")
            .bind(session_id.trim())
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn active_session_users(&self) -> anyhow::Result<Vec<ActiveSessionUser>> {
        let rows = sqlx::query_as::<_, ActiveSessionUserRow>(
            r#"
            SELECT active_session_users.session_id,
                   active_session_users.user_id,
                   users.name AS user_name,
                   active_session_users.added_at
            FROM active_session_users
            INNER JOIN users ON users.id = active_session_users.user_id
            WHERE users.is_disabled = 0
            ORDER BY active_session_users.added_at ASC
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

        let item_id = self.media_item_storage_id(session.item_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO transcode_sessions (
                play_session_id, dedupe_key, device_id, user_id, item_id, media_source_id, audio_stream_index,
                subtitle_stream_index, video_stream_index, output_path, process_id, status,
                progress_percent, position_ticks, start_position_ticks, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
            ON CONFLICT(play_session_id) DO UPDATE SET
                dedupe_key = excluded.dedupe_key,
                device_id = excluded.device_id,
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
                start_position_ticks = excluded.start_position_ticks,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&play_session_id)
        .bind(session.dedupe_key)
        .bind(session.device_id)
        .bind(session.user_id.to_string())
        .bind(item_id)
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
        mut session: UpsertTranscodeSession,
    ) -> anyhow::Result<(TranscodeSession, bool)> {
        let dedupe_key = dedupe_key.trim();
        anyhow::ensure!(!dedupe_key.is_empty(), "dedupe key must not be empty");
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
        session.dedupe_key = Some(dedupe_key.to_string());

        let item_id = self.media_item_storage_id(session.item_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            INSERT OR IGNORE INTO transcode_sessions (
                play_session_id, dedupe_key, device_id, user_id, item_id, media_source_id, audio_stream_index,
                subtitle_stream_index, video_stream_index, output_path, process_id, status,
                progress_percent, position_ticks, start_position_ticks, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
            "#,
        )
        .bind(&play_session_id)
        .bind(dedupe_key)
        .bind(session.device_id)
        .bind(session.user_id.to_string())
        .bind(item_id)
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

        if result.rows_affected() > 0 {
            let claimed = self
                .transcode_session_by_play_session_id(&play_session_id)
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

    async fn media_item_storage_id(&self, item_id: Uuid) -> anyhow::Result<String> {
        let simple = item_id.simple().to_string();
        if sqlx::query_scalar::<_, Option<String>>("SELECT id FROM media_items WHERE id = ?1")
            .bind(&simple)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .is_some()
        {
            return Ok(simple);
        }

        let hyphenated = item_id.to_string();
        if sqlx::query_scalar::<_, Option<String>>("SELECT id FROM media_items WHERE id = ?1")
            .bind(&hyphenated)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .is_some()
        {
            return Ok(hyphenated);
        }

        anyhow::bail!("media item {simple} does not exist")
    }

    pub async fn transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.transcode_sessions_with_statuses(&[]).await
    }

    pub async fn transcode_session_output_paths(&self) -> anyhow::Result<Vec<String>> {
        sqlx::query_scalar("SELECT output_path FROM transcode_sessions")
            .fetch_all(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn active_transcode_sessions(&self) -> anyhow::Result<Vec<TranscodeSession>> {
        self.transcode_sessions_with_statuses(&["starting", "running"])
            .await
    }

    pub async fn trickplay_info(
        &self,
        item_id: Uuid,
        width: i64,
    ) -> anyhow::Result<Option<TrickplayInfo>> {
        let row = sqlx::query_as::<_, TrickplayInfoRow>(
            r#"
            SELECT item_id, width, height, tile_width, tile_height, thumbnail_count,
                   interval_ms, bandwidth, created_at, updated_at
            FROM trickplay_infos
            WHERE item_id = ?1 AND width = ?2
            "#,
        )
        .bind(item_id.to_string())
        .bind(width)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn upsert_trickplay_info(
        &self,
        info: TrickplayInfo,
    ) -> anyhow::Result<TrickplayInfo> {
        anyhow::ensure!(info.width > 0, "trickplay width must be positive");
        anyhow::ensure!(info.height > 0, "trickplay height must be positive");
        anyhow::ensure!(info.tile_width > 0, "trickplay tile width must be positive");
        anyhow::ensure!(
            info.tile_height > 0,
            "trickplay tile height must be positive"
        );
        anyhow::ensure!(
            info.thumbnail_count > 0,
            "trickplay thumbnail count must be positive"
        );
        anyhow::ensure!(info.interval_ms > 0, "trickplay interval must be positive");
        let now = format_time(OffsetDateTime::now_utc())?;

        sqlx::query(
            r#"
            INSERT INTO trickplay_infos (
                item_id, width, height, tile_width, tile_height, thumbnail_count,
                interval_ms, bandwidth, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
            ON CONFLICT(item_id, width) DO UPDATE SET
                height = excluded.height,
                tile_width = excluded.tile_width,
                tile_height = excluded.tile_height,
                thumbnail_count = excluded.thumbnail_count,
                interval_ms = excluded.interval_ms,
                bandwidth = excluded.bandwidth,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(info.item_id.to_string())
        .bind(info.width)
        .bind(info.height)
        .bind(info.tile_width)
        .bind(info.tile_height)
        .bind(info.thumbnail_count)
        .bind(info.interval_ms)
        .bind(info.bandwidth.max(0))
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.trickplay_info(info.item_id, info.width)
            .await?
            .context("trickplay info missing after upsert")
    }

    pub async fn active_transcode_session_by_dedupe_key(
        &self,
        dedupe_key: &str,
    ) -> anyhow::Result<Option<TranscodeSession>> {
        let row = sqlx::query_as::<_, TranscodeSessionRow>(
            r#"
            SELECT transcode_sessions.play_session_id,
                   transcode_sessions.dedupe_key,
                   transcode_sessions.device_id,
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
                   transcode_sessions.start_position_ticks,
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
            WHERE transcode_sessions.dedupe_key = ?1
              AND transcode_sessions.status IN ('starting', 'running')
              AND media_items.missing_since IS NULL
            ORDER BY transcode_sessions.updated_at DESC
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
                   transcode_sessions.dedupe_key,
                   transcode_sessions.device_id,
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
                   transcode_sessions.start_position_ticks,
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
                   transcode_sessions.dedupe_key,
                   transcode_sessions.device_id,
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
                   transcode_sessions.start_position_ticks,
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
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
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
        Ok(result.rows_affected())
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

    pub async fn update_task_run_progress(
        &self,
        run_id: Uuid,
        progress: Value,
    ) -> anyhow::Result<Option<TaskRun>> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let result_json = serde_json::to_string(&progress)?;
        let result = sqlx::query(
            r#"
            UPDATE task_runs
            SET result_json = ?1,
                updated_at = ?2
            WHERE id = ?3 AND status = 'running'
            "#,
        )
        .bind(result_json)
        .bind(now)
        .bind(run_id.to_string())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            Ok(None)
        } else {
            self.task_run_by_id(run_id).await.map(Some)
        }
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
        let trimmed_key = task_key.trim();
        anyhow::ensure!(!trimmed_key.is_empty(), "task key must not be empty");
        anyhow::ensure!(
            matches!(status, "completed" | "failed"),
            "imported task history status must be completed or failed"
        );

        let id = id.unwrap_or_else(Uuid::new_v4);
        let started_at = format_time(started_at)?;
        let completed_at = format_time(completed_at)?;
        let result_json = serde_json::to_string(&result)?;
        sqlx::query(
            r#"
            INSERT INTO task_runs (
                id, task_key, status, started_at, completed_at,
                result_json, error_message, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?5)
            ON CONFLICT(id) DO UPDATE SET
                task_key = excluded.task_key,
                status = excluded.status,
                started_at = excluded.started_at,
                completed_at = excluded.completed_at,
                result_json = excluded.result_json,
                error_message = excluded.error_message,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_key)
        .bind(status)
        .bind(started_at)
        .bind(completed_at)
        .bind(result_json)
        .bind(error)
        .execute(&self.pool)
        .await?;

        self.task_run_by_id(id).await
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

    pub async fn rename_virtual_folder(&self, name: &str, new_name: &str) -> anyhow::Result<bool> {
        let trimmed_name = name.trim();
        let trimmed_new_name = new_name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );
        anyhow::ensure!(
            !trimmed_new_name.is_empty(),
            "virtual folder new name must not be empty"
        );
        let Some(folder) = self.virtual_folder_by_name(trimmed_name).await? else {
            return Ok(false);
        };
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            UPDATE virtual_folders
            SET name = ?1, updated_at = ?2
            WHERE id = ?3
            "#,
        )
        .bind(trimmed_new_name)
        .bind(now)
        .bind(folder.id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_virtual_folder_path(
        &self,
        name: &str,
        path: &str,
        new_path: &str,
    ) -> anyhow::Result<bool> {
        let Some(mut folder) = self.virtual_folder_by_name(name).await? else {
            return Ok(false);
        };
        let trimmed_path = path.trim();
        let trimmed_new_path = new_path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );
        anyhow::ensure!(
            !trimmed_new_path.is_empty(),
            "virtual folder new path must not be empty"
        );
        let Some(index) = folder
            .locations
            .iter()
            .position(|location| location == trimmed_path)
        else {
            return Ok(false);
        };
        folder.locations[index] = trimmed_new_path.to_string();
        self.upsert_virtual_folder(
            &folder.name,
            folder.collection_type.as_deref(),
            folder.locations,
        )
        .await?;
        Ok(true)
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

    pub async fn media_items_by_collection_type(
        &self,
        collection_type: &str,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams_json,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL AND collection_type = ?1
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .bind(collection_type)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_items_by_name_search(
        &self,
        search_term: &str,
        collection_types: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogNameSearch, DatabasePoolRole::Api);
        let result = self
            .media_items_by_name_search_unobserved(search_term, collection_types, limit)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_by_name_search_unobserved(
        &self,
        search_term: &str,
        collection_types: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let search_term = search_term.trim();
        if search_term.is_empty() || collection_types.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, virtual_folder_id, name, path, media_type, collection_type, \
             file_size, runtime_ticks, bitrate, width, height, media_streams_json, \
             created_at, updated_at \
             FROM media_items \
             WHERE missing_since IS NULL AND name LIKE ",
        );
        query.push_bind(format!("%{search_term}%"));
        query.push(" COLLATE NOCASE AND LOWER(collection_type) IN (");
        let mut separated = query.separated(", ");
        for collection_type in collection_types {
            separated.push_bind(collection_type.to_ascii_lowercase());
        }
        separated.push_unseparated(") ORDER BY name COLLATE NOCASE LIMIT ");
        query.push_bind(limit as i64);

        let rows = query
            .build_query_as::<MediaItemRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// SQLite test/transition equivalent of the native PostgreSQL catalog repository.
    pub async fn media_item_catalog_page(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogPage> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api);
        let result = self.media_item_catalog_page_unobserved(query).await;
        observation.finish_result(&result, |page| {
            u64::try_from(page.items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_catalog_page_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogPage> {
        validate_media_item_catalog_query(query)?;
        let mut transaction = self.pool.begin().await?;

        let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) ");
        push_sqlite_catalog_from(&mut count, query);
        push_sqlite_catalog_filters(&mut count, query)?;
        let total_record_count = count
            .build_query_scalar::<i64>()
            .fetch_one(&mut *transaction)
            .await?;
        let total_record_count =
            usize::try_from(total_record_count).context("media catalog count exceeded usize")?;

        let effective_limit = query.limit.min(MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE);
        if effective_limit == 0 {
            transaction.commit().await?;
            return Ok(MediaItemCatalogPage {
                items: Vec::new(),
                total_record_count,
                start_index: query.start_index,
            });
        }

        let mut page = QueryBuilder::<Sqlite>::new(
            r#"SELECT item.id, item.virtual_folder_id, item.name, item.path,
                      item.media_type, item.collection_type, item.file_size,
                      item.runtime_ticks, item.bitrate, item.width, item.height,
                      item.media_streams_json, item.metadata_json,
                      item.created_at, item.updated_at,
                      playback.user_id AS playback_user_id,
                      playback.item_id AS playback_item_id,
                      playback.media_source_id AS playback_media_source_id,
                      playback.audio_stream_index AS playback_audio_stream_index,
                      playback.subtitle_stream_index AS playback_subtitle_stream_index,
                      playback.position_ticks AS playback_position_ticks,
                      playback.is_paused AS playback_is_paused,
                      playback.played AS playback_played,
                      playback.is_favorite AS playback_is_favorite,
                      playback.rating AS playback_rating,
                      playback.updated_at AS playback_updated_at "#,
        );
        push_sqlite_catalog_from(&mut page, query);
        push_sqlite_catalog_filters(&mut page, query)?;
        push_sqlite_catalog_order(&mut page, query);
        page.push(" LIMIT ")
            .push_bind(i64::try_from(effective_limit)?);
        page.push(" OFFSET ")
            .push_bind(i64::try_from(query.start_index)?);

        let rows = page
            .build_query_as::<MediaItemCatalogRow>()
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;

        Ok(MediaItemCatalogPage {
            items: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total_record_count,
            start_index: query.start_index,
        })
    }

    pub async fn media_item_catalog_counts(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogCounts> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogCounts, DatabasePoolRole::Api);
        let result = self.media_item_catalog_counts_unobserved(query).await;
        observation.finish_result(&result, |counts| counts.item_count);
        result
    }

    async fn media_item_catalog_counts_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogCounts> {
        validate_media_item_catalog_query(query)?;
        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Api);
        let transaction_result = self.pool.begin().await;
        acquire.finish_result(&transaction_result);
        let mut transaction = transaction_result?;

        let mut aggregate = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) AS item_count, ");
        aggregate
            .push("COALESCE(SUM(CASE WHEN (")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'movie' THEN 1 ELSE 0 END), 0) AS movie_count, COALESCE(SUM(CASE WHEN (")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'episode' THEN 1 ELSE 0 END), 0) AS episode_count, COALESCE(SUM(CASE WHEN (")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'audio' THEN 1 ELSE 0 END), 0) AS song_count, COALESCE(SUM(CASE WHEN (")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'musicvideo' THEN 1 ELSE 0 END), 0) AS music_video_count, COALESCE(SUM(CASE WHEN (")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'book' THEN 1 ELSE 0 END), 0) AS book_count ");
        push_sqlite_catalog_from(&mut aggregate, query);
        push_sqlite_catalog_filters(&mut aggregate, query)?;
        let row = aggregate
            .build_query_as::<SqliteCatalogAggregateRow>()
            .fetch_one(&mut *transaction)
            .await?;

        let mut projection = QueryBuilder::<Sqlite>::new("SELECT item.name, item.path, ");
        projection.push(SQLITE_MEDIA_ITEM_TYPE_SQL).push(
            " AS item_type, item.metadata_json -> '$.Album' AS album, \
                   item.metadata_json -> '$.AlbumName' AS album_name, \
                   item.metadata_json -> '$.Artists' AS artists, \
                   item.metadata_json -> '$.AlbumArtists' AS album_artists, \
                   item.metadata_json -> '$.RemoteTrailers' AS remote_trailers, \
                   item.metadata_json -> '$.Trailers' AS trailers ",
        );
        push_sqlite_catalog_from(&mut projection, query);
        push_sqlite_catalog_filters(&mut projection, query)?;
        projection
            .push(" AND ((")
            .push(SQLITE_MEDIA_ITEM_TYPE_SQL)
            .push(
                ") = 'episode' OR json_type(item.metadata_json, '$.Album') IS NOT NULL \
                   OR json_type(item.metadata_json, '$.AlbumName') IS NOT NULL \
                   OR json_type(item.metadata_json, '$.Artists') IS NOT NULL \
                   OR json_type(item.metadata_json, '$.AlbumArtists') IS NOT NULL \
                   OR json_type(item.metadata_json, '$.RemoteTrailers') IS NOT NULL \
                   OR json_type(item.metadata_json, '$.Trailers') IS NOT NULL)",
            );
        let mut series_names = BTreeSet::new();
        let mut metadata_counts = CatalogMetadataCountAccumulator::default();
        {
            let mut rows = projection
                .build_query_as::<SqliteCatalogCountProjectionRow>()
                .fetch(&mut *transaction);
            while let Some(projected) = rows.try_next().await? {
                if projected.item_type == "episode" {
                    series_names.insert(
                        tv_episode_path_info(&projected.name, &projected.path)
                            .series_name
                            .to_ascii_lowercase(),
                    );
                }
                metadata_counts.add_album(projected.album.as_deref())?;
                metadata_counts.add_album(projected.album_name.as_deref())?;
                metadata_counts.add_artist(projected.artists.as_deref())?;
                metadata_counts.add_artist(projected.album_artists.as_deref())?;
                metadata_counts.add_trailers(projected.remote_trailers.as_deref())?;
                metadata_counts.add_trailers(projected.trailers.as_deref())?;
            }
        }
        transaction.commit().await?;

        Ok(MediaItemCatalogCounts {
            movie_count: sqlite_nonnegative_catalog_count(row.movie_count, "movie")?,
            series_count: u64::try_from(series_names.len()).context("series count exceeded u64")?,
            episode_count: sqlite_nonnegative_catalog_count(row.episode_count, "episode")?,
            artist_count: u64::try_from(metadata_counts.artists.len())
                .context("artist count exceeded u64")?,
            trailer_count: metadata_counts.trailers,
            song_count: sqlite_nonnegative_catalog_count(row.song_count, "song")?,
            album_count: u64::try_from(metadata_counts.albums.len())
                .context("album count exceeded u64")?,
            music_video_count: sqlite_nonnegative_catalog_count(
                row.music_video_count,
                "music video",
            )?,
            book_count: sqlite_nonnegative_catalog_count(row.book_count, "book")?,
            item_count: sqlite_nonnegative_catalog_count(row.item_count, "item")?,
        })
    }

    pub async fn media_item_query_filter_values(
        &self,
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogFilterSummary,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_item_query_filter_values_unobserved(query, selection)
            .await;
        observation.finish_result(&result, |values| {
            sqlite_media_item_query_filter_value_count(values)
        });
        result
    }

    async fn media_item_query_filter_values_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        validate_media_item_catalog_query(query)?;
        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Api);
        let transaction_result = self.pool.begin().await;
        acquire.finish_result(&transaction_result);
        let mut transaction = transaction_result?;
        let mut coverage = QueryBuilder::<Sqlite>::new(
            "WITH selected_items AS (SELECT item.id, item.virtual_folder_id ",
        );
        push_sqlite_catalog_from(&mut coverage, query);
        push_sqlite_catalog_filters(&mut coverage, query)?;
        coverage.push(
            ") SELECT NOT EXISTS (\
             SELECT 1 FROM selected_items \
             LEFT JOIN media_item_query_filter_sources AS source \
               ON source.item_id = selected_items.id \
              AND source.virtual_folder_id = selected_items.virtual_folder_id \
              AND source.extractor_version = ",
        );
        coverage.push_bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION);
        coverage.push(
            " WHERE source.item_id IS NULL OR source.projected_value_count <> (\
                SELECT count(*) FROM media_item_query_filter_values AS projected \
                WHERE projected.item_id = selected_items.id \
                  AND projected.virtual_folder_id = selected_items.virtual_folder_id))",
        );
        let covered = coverage
            .build_query_scalar::<bool>()
            .fetch_one(&mut *transaction)
            .await?;
        let result = if covered {
            Self::media_item_query_filter_values_projected(query, selection, &mut transaction).await
        } else {
            let mut values =
                Self::media_item_query_filter_values_legacy(query, &mut transaction).await?;
            values.retain_selection(selection);
            Ok(values)
        };
        transaction.commit().await?;
        result
    }

    async fn media_item_query_filter_values_projected(
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
        transaction: &mut sqlx::Transaction<'_, Sqlite>,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let mut projected = QueryBuilder::<Sqlite>::new(
            "WITH selected_items AS (SELECT item.id, item.virtual_folder_id, lower(item.name) AS item_sort ",
        );
        push_sqlite_catalog_from(&mut projected, query);
        push_sqlite_catalog_filters(&mut projected, query)?;
        projected.push(
            "), candidates(field, normalized_value, display_value, item_sort, item_id, \
                            key_priority, position) AS (\
             SELECT value.value_kind, lower(trim(value.display_value)), value.display_value, \
                    selected_items.item_sort, selected_items.id, \
                    value.source_priority, value.source_position \
             FROM selected_items JOIN media_item_query_filter_values AS value \
               ON value.item_id = selected_items.id \
              AND value.virtual_folder_id = selected_items.virtual_folder_id \
             WHERE value.value_kind IN (",
        );
        {
            let mut fields = projected.separated(", ");
            for field in selection.projected_fields() {
                fields.push_bind(field);
            }
            fields.push_unseparated(")");
        }
        projected.push(
            "), ranked AS (\
             SELECT field, normalized_value, display_value, \
                    row_number() OVER (PARTITION BY field, normalized_value \
                      ORDER BY item_sort, item_id, key_priority, position) AS value_rank \
             FROM candidates), result AS (\
             SELECT field, normalized_value, display_value \
             FROM ranked WHERE value_rank = 1",
        );
        if selection.includes_scalars() {
            projected.push(
                " UNION ALL \
                 SELECT 'containers', lower(source.container_value), \
                        lower(source.container_value) \
                 FROM selected_items JOIN media_item_query_filter_sources AS source \
                   ON source.item_id = selected_items.id \
                  AND source.virtual_folder_id = selected_items.virtual_folder_id \
                 WHERE source.container_present = 1 \
                 GROUP BY lower(source.container_value) \
                 UNION ALL \
                 SELECT 'media_types', source.media_type, source.media_type \
                 FROM selected_items JOIN media_item_query_filter_sources AS source \
                   ON source.item_id = selected_items.id \
                  AND source.virtual_folder_id = selected_items.virtual_folder_id \
                 GROUP BY source.media_type \
                 UNION ALL \
                 SELECT 'video_types', 'videofile', 'VideoFile' WHERE EXISTS (\
                   SELECT 1 FROM selected_items \
                   JOIN media_item_query_filter_sources AS source \
                     ON source.item_id = selected_items.id \
                    AND source.virtual_folder_id = selected_items.virtual_folder_id \
                   WHERE source.is_video = 1) \
                 UNION ALL \
                 SELECT '__has_subtitles', 'true', '1' WHERE EXISTS (\
                   SELECT 1 FROM selected_items \
                   JOIN media_item_query_filter_sources AS source \
                     ON source.item_id = selected_items.id \
                    AND source.virtual_folder_id = selected_items.virtual_folder_id \
                   WHERE source.has_subtitles = 1) \
                 UNION ALL \
                 SELECT '__has_trailer', 'true', '1' WHERE EXISTS (\
                   SELECT 1 FROM selected_items \
                   JOIN media_item_query_filter_sources AS source \
                     ON source.item_id = selected_items.id \
                    AND source.virtual_folder_id = selected_items.virtual_folder_id \
                   WHERE source.has_trailer = 1)",
            );
        }
        projected.push(
            ") SELECT field, display_value FROM result \
             ORDER BY field, normalized_value COLLATE BINARY",
        );
        let rows = projected
            .build_query_as::<(String, String)>()
            .fetch_all(&mut **transaction)
            .await?;
        let mut values = MediaItemQueryFilterValues::default();
        for (field, display_value) in rows {
            push_media_item_query_filter_value(&mut values, &field, display_value);
        }
        Ok(values)
    }

    async fn media_item_query_filter_values_legacy(
        query: &MediaItemCatalogQuery,
        transaction: &mut sqlx::Transaction<'_, Sqlite>,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let mut metadata = QueryBuilder::<Sqlite>::new(
            "WITH RECURSIVE selected_items AS (\
             SELECT item.id, item.name, item.metadata_json ",
        );
        push_sqlite_catalog_from(&mut metadata, query);
        push_sqlite_catalog_filters(&mut metadata, query)?;
        metadata.push(
            "), raw_values(field, item_sort, item_id, key_priority, value_position, value, value_type) AS (",
        );
        let metadata_specs = [
            ("genres", "Genres", 0_i64),
            ("tags", "Tags", 0),
            ("official_ratings", "OfficialRating", 0),
            ("official_ratings", "OfficialRatings", 1),
            ("years", "ProductionYear", 0),
            ("years", "Years", 1),
            ("series_statuses", "SeriesStatus", 0),
            ("staff_names", "People", 0),
            ("staff_names", "SeriesPeople", 1),
            ("artists", "Artists", 0),
            ("artists", "AlbumArtists", 1),
            ("albums", "Album", 0),
            ("albums", "AlbumName", 1),
            ("studios", "Studios", 0),
        ];
        for (index, (field, key, priority)) in metadata_specs.into_iter().enumerate() {
            if index > 0 {
                metadata.push(" UNION ALL ");
            }
            let json_path = format!("$.{key}");
            metadata
                .push("SELECT ")
                .push_bind(field)
                .push(", lower(item.name), item.id, ")
                .push_bind(priority)
                .push(", '0000000000', json_extract(item.metadata_json, ")
                .push_bind(json_path.clone())
                .push("), json_type(item.metadata_json, ")
                .push_bind(json_path)
                .push(") FROM selected_items AS item");
        }
        metadata.push(
            " UNION ALL \
             SELECT '__trailer', lower(item.name), item.id, 0, '0000000000', \
                    trailer_field.value, trailer_field.type \
             FROM selected_items AS item \
             JOIN json_each(item.metadata_json) AS trailer_field \
               ON lower(trailer_field.key) IN ('remotetrailers', 'trailers')\
             ), expanded(field, item_sort, item_id, key_priority, value_position, value, value_type) AS (\
             SELECT field, item_sort, item_id, key_priority, value_position, value, value_type \
             FROM raw_values WHERE value_type IS NOT NULL \
             UNION ALL \
             SELECT expanded.field, expanded.item_sort, expanded.item_id, \
                    expanded.key_priority, \
                    expanded.value_position || '.' || printf('%010d', child.key), \
                    child.value, child.type \
             FROM expanded \
             JOIN json_each(CASE WHEN expanded.value_type = 'array' \
                                 THEN expanded.value ELSE '[]' END) AS child \
             WHERE expanded.value_type = 'array'\
             ), candidates AS (\
             SELECT field, item_sort, item_id, key_priority, value_position, \
                    trim(CASE \
                      WHEN value_type IN ('text', 'integer', 'real') THEN CAST(value AS TEXT) \
                      WHEN value_type = 'object' AND json_type(value, '$.Name') = 'text' \
                        THEN json_extract(value, '$.Name') \
                    END) AS display_value \
             FROM expanded WHERE field <> '__trailer'\
             ), ranked AS (\
             SELECT field, display_value, \
                    ROW_NUMBER() OVER (\
                      PARTITION BY field, CASE WHEN field = 'media_types' \
                        THEN display_value ELSE lower(display_value) END \
                      ORDER BY item_sort, item_id, key_priority, value_position\
                    ) AS value_rank \
             FROM candidates WHERE display_value IS NOT NULL AND display_value <> ''\
             ) \
             SELECT field, display_value FROM ranked WHERE value_rank = 1 \
             UNION ALL \
             SELECT '__has_trailer', '1' WHERE EXISTS (\
               SELECT 1 FROM expanded \
               WHERE field = '__trailer' AND (\
                 (value_type = 'text' AND trim(CAST(value AS TEXT)) <> '') OR \
                 (value_type = 'object' AND CASE \
                   WHEN json_type(value, '$.Url') IS NOT NULL THEN \
                     json_type(value, '$.Url') = 'text' \
                       AND trim(json_extract(value, '$.Url')) <> '' \
                   WHEN json_type(value, '$.url') IS NOT NULL THEN \
                     json_type(value, '$.url') = 'text' \
                       AND trim(json_extract(value, '$.url')) <> '' \
                   WHEN json_type(value, '$.Path') IS NOT NULL THEN \
                     json_type(value, '$.Path') = 'text' \
                       AND trim(json_extract(value, '$.Path')) <> '' \
                   WHEN json_type(value, '$.path') IS NOT NULL THEN \
                     json_type(value, '$.path') = 'text' \
                       AND trim(json_extract(value, '$.path')) <> '' \
                   ELSE 0 END)\
               )\
             ) \
             ORDER BY field, display_value COLLATE NOCASE",
        );

        let metadata_rows = metadata
            .build_query_as::<(String, String)>()
            .fetch_all(&mut **transaction)
            .await?;

        let mut scalar = QueryBuilder::<Sqlite>::new(
            "WITH RECURSIVE selected_items AS (\
             SELECT item.id, item.name, item.path, item.media_type, item.media_streams_json ",
        );
        push_sqlite_catalog_from(&mut scalar, query);
        push_sqlite_catalog_filters(&mut scalar, query)?;
        scalar.push(
            "), filenames(item_sort, item_id, filename) AS (\
             SELECT lower(name), id, path FROM selected_items \
             UNION ALL \
             SELECT item_sort, item_id, substr(filename, instr(filename, '/') + 1) \
             FROM filenames WHERE instr(filename, '/') > 0\
             ), extensions(item_sort, item_id, filename, suffix, dot_count) AS (\
             SELECT item_sort, item_id, filename, filename, 0 \
             FROM filenames WHERE instr(filename, '/') = 0 \
             UNION ALL \
             SELECT item_sort, item_id, filename, \
                    substr(suffix, instr(suffix, '.') + 1), dot_count + 1 \
             FROM extensions WHERE instr(suffix, '.') > 0\
             ), stream_values AS (\
             SELECT lower(item.name) AS item_sort, item.id AS item_id, stream.key AS position, \
                    lower(json_extract(stream.value, '$.Type')) AS stream_type, \
                    json_type(stream.value, '$.Type') AS stream_type_type, \
                    json_type(stream.value, '$.Language') AS language_type, \
                    CASE lower(trim(json_extract(stream.value, '$.Language'))) \
                      WHEN 'fre' THEN 'fra' WHEN 'ger' THEN 'deu' \
                      ELSE trim(json_extract(stream.value, '$.Language')) \
                    END AS display_language \
             FROM selected_items AS item \
             JOIN json_each(item.media_streams_json) AS stream\
             ), raw_values(field, item_sort, item_id, position, display_value) AS (\
             SELECT 'containers', item_sort, item_id, 0, lower(suffix) \
             FROM extensions \
             WHERE instr(suffix, '.') = 0 AND dot_count > 0 \
               AND filename NOT IN ('.', '..') \
               AND NOT (substr(filename, 1, 1) = '.' AND instr(substr(filename, 2), '.') = 0) \
             UNION ALL \
             SELECT 'media_types', lower(name), id, 0, media_type FROM selected_items \
             UNION ALL \
             SELECT 'video_types', lower(name), id, 0, 'VideoFile' FROM selected_items \
             WHERE lower(media_type) = 'video' \
             UNION ALL \
             SELECT 'audio_languages', item_sort, item_id, position, display_language \
             FROM stream_values WHERE stream_type_type = 'text' AND language_type = 'text' \
               AND stream_type = 'audio' \
             UNION ALL \
             SELECT 'subtitle_languages', item_sort, item_id, position, display_language \
             FROM stream_values WHERE stream_type_type = 'text' AND language_type = 'text' \
               AND stream_type = 'subtitle'\
             ), ranked AS (\
             SELECT field, display_value, \
                    ROW_NUMBER() OVER (\
                      PARTITION BY field, CASE WHEN field = 'media_types' \
                        THEN display_value ELSE lower(display_value) END \
                      ORDER BY item_sort, item_id, position\
                    ) AS value_rank \
             FROM raw_values \
             WHERE display_value IS NOT NULL \
               AND (field NOT IN ('audio_languages', 'subtitle_languages') \
                    OR (trim(display_value) <> '' AND lower(trim(display_value)) <> 'und'))\
             ) \
             SELECT field, display_value FROM ranked WHERE value_rank = 1 \
             UNION ALL \
             SELECT '__has_subtitles', '1' WHERE EXISTS (\
               SELECT 1 FROM stream_values \
               WHERE stream_type_type = 'text' AND stream_type = 'subtitle'\
             ) \
             ORDER BY field, display_value COLLATE NOCASE",
        );
        let scalar_rows = scalar
            .build_query_as::<(String, String)>()
            .fetch_all(&mut **transaction)
            .await?;

        let mut values = MediaItemQueryFilterValues::default();
        for (field, display_value) in metadata_rows.into_iter().chain(scalar_rows) {
            push_media_item_query_filter_value(&mut values, &field, display_value);
        }
        Ok(values)
    }

    pub async fn tv_series_lookup_candidates(&self) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let rows = sqlx::query_as::<_, MediaItemCatalogRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams_json, item.metadata_json,
                   item.created_at, item.updated_at,
                   CAST(NULL AS TEXT) AS playback_user_id,
                   CAST(NULL AS TEXT) AS playback_item_id,
                   CAST(NULL AS TEXT) AS playback_media_source_id,
                   CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                   CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                   CAST(NULL AS INTEGER) AS playback_position_ticks,
                   CAST(NULL AS INTEGER) AS playback_is_paused,
                   CAST(NULL AS INTEGER) AS playback_played,
                   CAST(NULL AS INTEGER) AS playback_is_favorite,
                   CAST(NULL AS REAL) AS playback_rating,
                   CAST(NULL AS TEXT) AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
            ORDER BY item.name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Episode candidates plus an explicit provider-series anchor, restricted to one persisted
    /// `SeriesId`.
    ///
    /// Opening a series otherwise materializes every episode in the library just to keep the
    /// handful
    /// that belong to it. The anchor lets a valid series with no episodes resolve its detail page
    /// without adding a synthetic member to the episode projection.
    pub async fn tv_series_lookup_candidates_for_series(
        &self,
        series_id: &str,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let rows = sqlx::query_as::<_, MediaItemCatalogRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams_json, item.metadata_json,
                   item.created_at, item.updated_at,
                   CAST(NULL AS TEXT) AS playback_user_id,
                   CAST(NULL AS TEXT) AS playback_item_id,
                   CAST(NULL AS TEXT) AS playback_media_source_id,
                   CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                   CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                   CAST(NULL AS INTEGER) AS playback_position_ticks,
                   CAST(NULL AS INTEGER) AS playback_is_paused,
                   CAST(NULL AS INTEGER) AS playback_played,
                   CAST(NULL AS INTEGER) AS playback_is_favorite,
                   CAST(NULL AS REAL) AS playback_rating,
                   CAST(NULL AS TEXT) AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
              AND (
                    item.media_type = 'Video'
                    OR (
                        item.media_type = 'Series'
                        AND lower(coalesce(
                            json_extract(item.metadata_json, '$.PluginVodKind'), ''
                        )) = 'series'
                    )
              )
              AND trim(json_extract(item.metadata_json, '$.SeriesId')) = ?1
            ORDER BY item.name COLLATE NOCASE
            "#,
        )
        .bind(series_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// The same candidates restricted to one persisted `SeasonId`.
    ///
    /// Opening a season otherwise materializes every episode in the library just to keep the
    /// handful
    /// that belong to it. The remaining predicates match `tv_series_lookup_candidates` exactly so
    /// the
    /// result is a strict subset of it.
    pub async fn tv_series_lookup_candidates_for_season(
        &self,
        season_id: &str,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let rows = sqlx::query_as::<_, MediaItemCatalogRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams_json, item.metadata_json,
                   item.created_at, item.updated_at,
                   CAST(NULL AS TEXT) AS playback_user_id,
                   CAST(NULL AS TEXT) AS playback_item_id,
                   CAST(NULL AS TEXT) AS playback_media_source_id,
                   CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                   CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                   CAST(NULL AS INTEGER) AS playback_position_ticks,
                   CAST(NULL AS INTEGER) AS playback_is_paused,
                   CAST(NULL AS INTEGER) AS playback_played,
                   CAST(NULL AS INTEGER) AS playback_is_favorite,
                   CAST(NULL AS REAL) AS playback_rating,
                   CAST(NULL AS TEXT) AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
              AND trim(json_extract(item.metadata_json, '$.SeasonId')) = ?1
            ORDER BY item.name COLLATE NOCASE
            "#,
        )
        .bind(season_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// The same candidates restricted to rows without a canonical persisted `SeriesId`.
    ///
    /// Only those episodes can carry a name-derived synthetic Series id, so this is the exact
    /// fallback scope when a canonical lookup finds nothing.
    pub async fn tv_series_lookup_candidates_without_canonical_series_id(
        &self,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let rows = sqlx::query_as::<_, MediaItemCatalogRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams_json, item.metadata_json,
                   item.created_at, item.updated_at,
                   CAST(NULL AS TEXT) AS playback_user_id,
                   CAST(NULL AS TEXT) AS playback_item_id,
                   CAST(NULL AS TEXT) AS playback_media_source_id,
                   CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                   CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                   CAST(NULL AS INTEGER) AS playback_position_ticks,
                   CAST(NULL AS INTEGER) AS playback_is_paused,
                   CAST(NULL AS INTEGER) AS playback_played,
                   CAST(NULL AS INTEGER) AS playback_is_favorite,
                   CAST(NULL AS REAL) AS playback_rating,
                   CAST(NULL AS TEXT) AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
              AND (
                  NULLIF(trim(json_extract(item.metadata_json, '$.SeriesId')), '') IS NULL
                  OR length(trim(json_extract(item.metadata_json, '$.SeriesId'))) <> 32
                  OR trim(json_extract(item.metadata_json, '$.SeriesId')) <>
                     lower(trim(json_extract(item.metadata_json, '$.SeriesId')))
                  OR trim(json_extract(item.metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
                  OR NULLIF(trim(json_extract(item.metadata_json, '$.SeriesName')), '') IS NULL
              )
            ORDER BY item.name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn tv_series_catalog_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
    ) -> anyhow::Result<Option<TvSeriesCatalogPage>> {
        self.tv_series_catalog_search_page(
            virtual_folder_id,
            start_index,
            limit,
            TvSeriesCatalogNameFilter::default(),
        )
        .await
    }

    pub async fn tv_series_catalog_search_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
        filter: TvSeriesCatalogNameFilter,
    ) -> anyhow::Result<Option<TvSeriesCatalogPage>> {
        let search_pattern = filter
            .search_term
            .as_deref()
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .map(|term| {
                format!(
                    "%{}%",
                    sqlite_escape_catalog_like_value(&term.to_ascii_lowercase())
                )
            });
        let starts_with_pattern = filter
            .starts_with
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| {
                format!(
                    "{}%",
                    sqlite_escape_catalog_like_value(&prefix.to_ascii_lowercase())
                )
            });
        let lower_bound = filter
            .starts_with_or_greater
            .as_deref()
            .map(str::trim)
            .filter(|bound| !bound.is_empty())
            .map(str::to_ascii_lowercase);
        let upper_bound = filter
            .less_than
            .as_deref()
            .map(str::trim)
            .filter(|bound| !bound.is_empty())
            .map(str::to_ascii_lowercase);
        let name_patterns = TvSeriesCatalogNamePatterns {
            search: search_pattern.as_deref(),
            starts_with: starts_with_pattern.as_deref(),
            lower_bound: lower_bound.as_deref(),
            upper_bound: upper_bound.as_deref(),
        };
        let mut transaction = self.pool.begin().await?;
        let virtual_folder_ids =
            virtual_folder_id.map(|id| (id.simple().to_string(), id.to_string()));
        let projection_covered: i64 = sqlx::query_scalar(
            r#"
            SELECT CASE
                WHEN ?1 IS NOT NULL THEN EXISTS (
                    SELECT 1
                    FROM media_item_tv_series_coverage AS coverage
                    WHERE coverage.virtual_folder_id IN (?1, ?2)
                      AND coverage.projection_version = ?3
                )
                ELSE NOT EXISTS (
                    SELECT 1
                    FROM virtual_folders AS folder
                    LEFT JOIN media_item_tv_series_coverage AS coverage
                      ON coverage.virtual_folder_id = folder.id
                     AND coverage.projection_version = ?3
                    WHERE lower(coalesce(folder.collection_type, ''))
                          IN ('tvshows', 'tvshow', 'series')
                      AND coverage.virtual_folder_id IS NULL
                ) AND NOT EXISTS (
                    SELECT 1
                    FROM virtual_folders AS folder
                    JOIN media_items AS item ON item.virtual_folder_id = folder.id
                    WHERE lower(coalesce(folder.collection_type, ''))
                          NOT IN ('tvshows', 'tvshow', 'series')
                      AND item.missing_since IS NULL
                      AND item.media_type = 'Video'
                      AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
                ) AND NOT EXISTS (
                    SELECT 1
                    FROM media_item_tv_series AS series
                    JOIN media_item_tv_series_coverage AS coverage
                      ON coverage.virtual_folder_id = series.virtual_folder_id
                     AND coverage.projection_version = ?3
                    GROUP BY series.series_id
                    HAVING count(*) > 1
                )
            END
            "#,
        )
        .bind(
            virtual_folder_ids
                .as_ref()
                .map(|(simple, _)| simple.as_str()),
        )
        .bind(
            virtual_folder_ids
                .as_ref()
                .map(|(_, dashed)| dashed.as_str()),
        )
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .fetch_one(&mut *transaction)
        .await?;
        if projection_covered == 0 {
            let page = Self::tv_series_catalog_page_from_live(
                &mut transaction,
                virtual_folder_ids.as_ref(),
                start_index,
                limit,
                name_patterns,
            )
            .await?;
            transaction.commit().await?;
            return Ok(page);
        }
        let requested_limit = limit;
        let limit = i64::try_from(limit.max(1))?;
        let offset = i64::try_from(start_index)?;
        let mut series = sqlx::query_as::<_, (String, String, i64)>(
            r#"
            SELECT series.series_id,
                   min(series.series_name) AS series_name,
                   COUNT(*) OVER () AS total_series
            FROM media_item_tv_series AS series
            JOIN media_item_tv_series_coverage AS coverage
              ON coverage.virtual_folder_id = series.virtual_folder_id
             AND coverage.projection_version = ?5
            WHERE (?1 IS NULL OR series.virtual_folder_id IN (?1, ?2))
              AND (?6 IS NULL OR lower(series.series_name) LIKE ?6 ESCAPE '\')
              AND (?7 IS NULL OR lower(series.series_name) LIKE ?7 ESCAPE '\')
              AND (?8 IS NULL OR lower(series.series_name) >= ?8)
              AND (?9 IS NULL OR lower(series.series_name) < ?9)
            GROUP BY series.series_id
            ORDER BY series_name COLLATE NOCASE, series_name, series_id
            LIMIT ?3 OFFSET ?4
            "#,
        )
        .bind(
            virtual_folder_ids
                .as_ref()
                .map(|(simple, _)| simple.as_str()),
        )
        .bind(
            virtual_folder_ids
                .as_ref()
                .map(|(_, dashed)| dashed.as_str()),
        )
        .bind(limit)
        .bind(offset)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .bind(search_pattern.as_deref())
        .bind(starts_with_pattern.as_deref())
        .bind(lower_bound.as_deref())
        .bind(upper_bound.as_deref())
        .fetch_all(&mut *transaction)
        .await?;
        let total = if let Some((_, _, total)) = series.first() {
            *total
        } else if start_index == 0 {
            0
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(DISTINCT series.series_id) FROM media_item_tv_series AS series JOIN media_item_tv_series_coverage AS coverage ON coverage.virtual_folder_id = series.virtual_folder_id AND coverage.projection_version = ?3 WHERE (?1 IS NULL OR series.virtual_folder_id IN (?1, ?2)) AND (?4 IS NULL OR lower(series.series_name) LIKE ?4 ESCAPE '\\') AND (?5 IS NULL OR lower(series.series_name) LIKE ?5 ESCAPE '\\') AND (?6 IS NULL OR lower(series.series_name) >= ?6) AND (?7 IS NULL OR lower(series.series_name) < ?7)",
            )
            .bind(virtual_folder_ids.as_ref().map(|(simple, _)| simple.as_str()))
            .bind(virtual_folder_ids.as_ref().map(|(_, dashed)| dashed.as_str()))
            .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
            .bind(search_pattern.as_deref())
            .bind(starts_with_pattern.as_deref())
            .bind(lower_bound.as_deref())
            .bind(upper_bound.as_deref())
            .fetch_one(&mut *transaction)
            .await?
        };
        if requested_limit == 0 {
            series.clear();
        }
        let mut rows = Vec::new();
        if !series.is_empty() {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT item.id, item.virtual_folder_id, item.name, item.path, item.media_type, item.collection_type, item.file_size, item.runtime_ticks, item.bitrate, item.width, item.height, item.media_streams_json, item.metadata_json, item.created_at, item.updated_at, CAST(NULL AS TEXT) AS playback_user_id, CAST(NULL AS TEXT) AS playback_item_id, CAST(NULL AS TEXT) AS playback_media_source_id, CAST(NULL AS INTEGER) AS playback_audio_stream_index, CAST(NULL AS INTEGER) AS playback_subtitle_stream_index, CAST(NULL AS INTEGER) AS playback_position_ticks, CAST(NULL AS INTEGER) AS playback_is_paused, CAST(NULL AS INTEGER) AS playback_played, CAST(NULL AS INTEGER) AS playback_is_favorite, CAST(NULL AS REAL) AS playback_rating, CAST(NULL AS TEXT) AS playback_updated_at FROM media_item_tv_series_members AS member JOIN media_items AS item ON item.id = member.item_id JOIN media_item_tv_series_coverage AS coverage ON coverage.virtual_folder_id = member.virtual_folder_id WHERE item.missing_since IS NULL AND coverage.projection_version = ",
            );
            query.push_bind(TV_SERIES_CATALOG_PROJECTION_VERSION);
            if let Some((simple, dashed)) = virtual_folder_ids.as_ref() {
                query.push(" AND member.virtual_folder_id IN (");
                query.push_bind(simple);
                query.push(", ");
                query.push_bind(dashed);
                query.push(")");
            }
            query.push(" AND member.series_id IN (");
            let mut separated = query.separated(", ");
            for (id, _, _) in &series {
                separated.push_bind(id);
            }
            separated.push_unseparated(") ORDER BY item.name COLLATE NOCASE, item.id");
            rows = query
                .build_query_as::<MediaItemCatalogRow>()
                .fetch_all(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(Some(TvSeriesCatalogPage {
            series: series
                .into_iter()
                .map(|(id, name, _)| TvSeriesCatalogKey { id, name })
                .collect(),
            episodes: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total_record_count: usize::try_from(total)?,
            start_index,
        }))
    }

    /// Bounded page computed directly from `media_items` when the durable projection rows exist but
    /// their coverage row was invalidated.
    ///
    /// `TvSeriesCatalogPage` reserves `None` for episodes without a canonical persisted
    /// SeriesId/SeriesName, so a merely stale coverage row must not push the caller onto the legacy
    /// path that materializes every episode in the library. The projection tables are deliberately
    /// not read here: only their coverage row certifies freshness, so this recomputes the same page
    /// from the live rows and keeps the fail-closed contract intact.
    async fn tv_series_catalog_page_from_live(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        virtual_folder_ids: Option<&(String, String)>,
        start_index: usize,
        limit: usize,
        name_patterns: TvSeriesCatalogNamePatterns<'_>,
    ) -> anyhow::Result<Option<TvSeriesCatalogPage>> {
        let simple = virtual_folder_ids.map(|(simple, _)| simple.as_str());
        let dashed = virtual_folder_ids.map(|(_, dashed)| dashed.as_str());
        let canonical: i64 = sqlx::query_scalar(
            r#"
            SELECT NOT EXISTS (
                SELECT 1
                FROM media_items AS invalid
                WHERE invalid.missing_since IS NULL
                  AND lower(invalid.collection_type) IN ('tvshows', 'tvshow', 'series')
                  AND (
                        invalid.media_type = 'Video'
                        OR (
                            invalid.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                invalid.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series'
                        )
                  )
                  AND (?1 IS NULL OR invalid.virtual_folder_id IN (?1, ?2))
                  AND (
                      NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesId')), '') IS NULL
                      OR length(trim(json_extract(invalid.metadata_json, '$.SeriesId'))) <> 32
                      OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) <>
                         lower(trim(json_extract(invalid.metadata_json, '$.SeriesId')))
                      OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
                      OR NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesName')), '')
                         IS NULL
                  )
            )
            "#,
        )
        .bind(simple)
        .bind(dashed)
        .fetch_one(&mut **tx)
        .await?;
        if canonical == 0 {
            return Ok(None);
        }

        // One grouped pass answers everything the coverage row would otherwise certify: whether the
        // data is still projection-eligible, how many series exist, and which belong on the page.
        // `min(...) <> max(...)` detects a second distinct value per group without the per-group
        // sort that `count(DISTINCT ...)` needs.
        let requested_limit = limit;
        let grouped = sqlx::query_as::<_, (i64, i64, i64, i64, Option<String>, Option<String>)>(
            r#"
            WITH series_source AS (
                SELECT item.virtual_folder_id,
                       trim(json_extract(item.metadata_json, '$.SeriesId')) AS series_id,
                       trim(json_extract(item.metadata_json, '$.SeriesName')) AS series_name,
                       CASE WHEN item.media_type = 'Series' THEN 1 ELSE 0 END AS is_anchor,
                       CASE
                           WHEN lower(coalesce(folder.collection_type, ''))
                                IN ('tvshows', 'tvshow', 'series') THEN 0
                           ELSE 1
                       END AS foreign_folder
                FROM media_items AS item
                LEFT JOIN virtual_folders AS folder ON folder.id = item.virtual_folder_id
                WHERE item.missing_since IS NULL
                  AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
                  AND (
                        item.media_type = 'Video'
                        OR (
                            item.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                item.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series'
                        )
                  )
                  AND (?1 IS NULL OR item.virtual_folder_id IN (?1, ?2))
            ), grouped AS (
                SELECT series_id,
                       coalesce(
                           min(CASE WHEN is_anchor = 1 THEN series_name END),
                           min(series_name)
                       ) AS series_name,
                       coalesce(
                           min(CASE WHEN is_anchor = 1 THEN series_name END),
                           max(series_name)
                       ) AS series_name_last,
                       min(replace(item.virtual_folder_id, '-', '')) AS folder_first,
                       max(replace(item.virtual_folder_id, '-', '')) AS folder_last,
                       max(foreign_folder) AS foreign_folder
                FROM series_source AS item
                GROUP BY series_id
            ), stats AS (
                SELECT coalesce(sum(CASE
                           WHEN (?5 IS NULL
                             OR lower(series_name) LIKE ?5 ESCAPE '\')
                            AND (?6 IS NULL
                             OR lower(series_name) LIKE ?6 ESCAPE '\')
                            AND (?7 IS NULL OR lower(series_name) >= ?7)
                            AND (?8 IS NULL OR lower(series_name) < ?8) THEN 1
                           ELSE 0
                       END), 0) AS total,
                       coalesce(max(CASE WHEN series_name <> series_name_last THEN 1 ELSE 0 END), 0)
                           AS name_conflict,
                       coalesce(max(CASE WHEN folder_first <> folder_last THEN 1 ELSE 0 END), 0)
                           AS folder_conflict,
                       coalesce(max(foreign_folder), 0) AS foreign_folder
                FROM grouped
            ), page AS (
                SELECT series_id, series_name
                FROM grouped
                WHERE (?5 IS NULL OR lower(series_name) LIKE ?5 ESCAPE '\')
                  AND (?6 IS NULL OR lower(series_name) LIKE ?6 ESCAPE '\')
                  AND (?7 IS NULL OR lower(series_name) >= ?7)
                  AND (?8 IS NULL OR lower(series_name) < ?8)
                ORDER BY series_name COLLATE NOCASE, series_name, series_id
                LIMIT ?3 OFFSET ?4
            )
            SELECT stats.total, stats.name_conflict, stats.folder_conflict,
                   stats.foreign_folder, page.series_id, page.series_name
            FROM stats
            LEFT JOIN page ON 1 = 1
            ORDER BY page.series_name COLLATE NOCASE, page.series_name, page.series_id
            "#,
        )
        .bind(simple)
        .bind(dashed)
        .bind(i64::try_from(limit.max(1))?)
        .bind(i64::try_from(start_index)?)
        .bind(name_patterns.search)
        .bind(name_patterns.starts_with)
        .bind(name_patterns.lower_bound)
        .bind(name_patterns.upper_bound)
        .fetch_all(&mut **tx)
        .await?;
        let Some(first) = grouped.first() else {
            return Ok(None);
        };
        let (total, name_conflict, folder_conflict, foreign_folder) =
            (first.0, first.1, first.2, first.3);
        // A SeriesId carrying two display names, spanning two folders, or living outside a TV
        // library is exactly what `rebuild_tv_series_catalog_projection_in_transaction` refuses to
        // project, so those keep deferring to the caller's legacy grouping.
        if name_conflict != 0
            || (virtual_folder_ids.is_none() && (folder_conflict != 0 || foreign_folder != 0))
        {
            return Ok(None);
        }
        let mut series = grouped
            .iter()
            .filter_map(|(_, _, _, _, id, name)| Some((id.clone()?, name.clone()?)))
            .collect::<Vec<_>>();
        if requested_limit == 0 {
            series.clear();
        }
        let mut rows = Vec::new();
        if !series.is_empty() {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT item.id, item.virtual_folder_id, item.name, item.path, item.media_type, item.collection_type, item.file_size, item.runtime_ticks, item.bitrate, item.width, item.height, item.media_streams_json, item.metadata_json, item.created_at, item.updated_at, CAST(NULL AS TEXT) AS playback_user_id, CAST(NULL AS TEXT) AS playback_item_id, CAST(NULL AS TEXT) AS playback_media_source_id, CAST(NULL AS INTEGER) AS playback_audio_stream_index, CAST(NULL AS INTEGER) AS playback_subtitle_stream_index, CAST(NULL AS INTEGER) AS playback_position_ticks, CAST(NULL AS INTEGER) AS playback_is_paused, CAST(NULL AS INTEGER) AS playback_played, CAST(NULL AS INTEGER) AS playback_is_favorite, CAST(NULL AS REAL) AS playback_rating, CAST(NULL AS TEXT) AS playback_updated_at FROM media_items AS item WHERE item.missing_since IS NULL AND item.media_type = 'Video' AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')",
            );
            if let Some((simple, dashed)) = virtual_folder_ids {
                query.push(" AND item.virtual_folder_id IN (");
                query.push_bind(simple);
                query.push(", ");
                query.push_bind(dashed);
                query.push(")");
            }
            query.push(" AND trim(json_extract(item.metadata_json, '$.SeriesId')) IN (");
            let mut separated = query.separated(", ");
            for (id, _) in &series {
                separated.push_bind(id);
            }
            separated.push_unseparated(") ORDER BY item.name COLLATE NOCASE, item.id");
            rows = query
                .build_query_as::<MediaItemCatalogRow>()
                .fetch_all(&mut **tx)
                .await?;
        }
        Ok(Some(TvSeriesCatalogPage {
            series: series
                .into_iter()
                .map(|(id, name)| TvSeriesCatalogKey { id, name })
                .collect(),
            episodes: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total_record_count: usize::try_from(total)?,
            start_index,
        }))
    }

    /// Publish the TV series projection for a folder when its coverage row is missing.
    ///
    /// Returns whether the projection is published; `false` means the folder's data is not
    /// projectable, which the caller must not retry in a loop.
    pub async fn ensure_tv_series_catalog_projection(
        &self,
        virtual_folder_id: Uuid,
    ) -> anyhow::Result<bool> {
        retry_transient_catalog_lock(catalog_lock_jitter_seed(virtual_folder_id), || {
            self.ensure_tv_series_catalog_projection_once(virtual_folder_id)
        })
        .await
    }

    async fn ensure_tv_series_catalog_projection_once(
        &self,
        virtual_folder_id: Uuid,
    ) -> anyhow::Result<bool> {
        let simple = virtual_folder_id.simple().to_string();
        let dashed = virtual_folder_id.to_string();
        let mut tx = self.pool.begin().await?;
        let published: i64 = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM media_item_tv_series_coverage AS coverage
                WHERE coverage.virtual_folder_id IN (?1, ?2)
                  AND coverage.projection_version = ?3
            )
            "#,
        )
        .bind(&simple)
        .bind(&dashed)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .fetch_one(&mut *tx)
        .await?;
        if published != 0 {
            tx.commit().await?;
            return Ok(true);
        }
        let stored_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM virtual_folders WHERE id IN (?1, ?2)")
                .bind(&simple)
                .bind(&dashed)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(stored_id) = stored_id else {
            tx.commit().await?;
            return Ok(false);
        };
        let rebuilt =
            Self::rebuild_tv_series_catalog_projection_in_transaction(&mut tx, &stored_id).await?;
        tx.commit().await?;
        Ok(rebuilt)
    }

    async fn rebuild_tv_series_catalog_projection_in_transaction(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        virtual_folder_id: &str,
    ) -> anyhow::Result<bool> {
        sqlx::query("DELETE FROM media_item_tv_series_coverage WHERE virtual_folder_id = ?1")
            .bind(virtual_folder_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM media_item_tv_series WHERE virtual_folder_id = ?1")
            .bind(virtual_folder_id)
            .execute(&mut **tx)
            .await?;

        let eligible: i64 = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM virtual_folders AS folder
                WHERE folder.id = ?1
                  AND lower(coalesce(folder.collection_type, ''))
                      IN ('tvshows', 'tvshow', 'series')
            ) AND NOT EXISTS (
                SELECT 1
                FROM media_items AS invalid
                WHERE invalid.virtual_folder_id = ?1
                  AND invalid.missing_since IS NULL
                  AND lower(invalid.collection_type) IN ('tvshows', 'tvshow', 'series')
                  AND (
                        invalid.media_type = 'Video'
                        OR (
                            invalid.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                invalid.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series'
                        )
                  )
                  AND (
                      NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesId')), '') IS NULL
                      OR length(trim(json_extract(invalid.metadata_json, '$.SeriesId'))) <> 32
                      OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) <>
                         lower(trim(json_extract(invalid.metadata_json, '$.SeriesId')))
                      OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
                      OR NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesName')), '') IS NULL
                  )
            ) AND NOT EXISTS (
                SELECT 1
                FROM media_items AS conflicting
                WHERE conflicting.virtual_folder_id = ?1
                  AND conflicting.missing_since IS NULL
                  AND lower(conflicting.collection_type) IN ('tvshows', 'tvshow', 'series')
                  AND (
                        conflicting.media_type = 'Video'
                        OR (
                            conflicting.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                conflicting.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series'
                        )
                  )
                GROUP BY trim(json_extract(conflicting.metadata_json, '$.SeriesId'))
                HAVING sum(CASE
                           WHEN conflicting.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                conflicting.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series' THEN 1
                           ELSE 0
                       END) = 0
                   AND count(DISTINCT trim(
                       json_extract(conflicting.metadata_json, '$.SeriesName')
                   )) > 1
            )
            "#,
        )
        .bind(virtual_folder_id)
        .fetch_one(&mut **tx)
        .await?;
        if eligible == 0 {
            return Ok(false);
        }

        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series (
                virtual_folder_id, series_id, series_name, episode_count
            )
            WITH series_source AS (
                SELECT item.virtual_folder_id,
                       trim(json_extract(item.metadata_json, '$.SeriesId')) AS series_id,
                       trim(json_extract(item.metadata_json, '$.SeriesName')) AS series_name,
                       CASE WHEN item.media_type = 'Series' THEN 1 ELSE 0 END AS is_anchor,
                       CASE WHEN item.media_type = 'Video' THEN 1 ELSE 0 END AS episode_count
                FROM media_items AS item
                WHERE item.virtual_folder_id = ?1
                  AND item.missing_since IS NULL
                  AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
                  AND (
                        item.media_type = 'Video'
                        OR (
                            item.media_type = 'Series'
                            AND lower(coalesce(json_extract(
                                item.metadata_json, '$.PluginVodKind'
                            ), '')) = 'series'
                        )
                  )
            )
            SELECT virtual_folder_id,
                   series_id,
                   coalesce(
                       min(CASE WHEN is_anchor = 1 THEN series_name END),
                       min(series_name)
                   ),
                   sum(episode_count)
            FROM series_source
            GROUP BY virtual_folder_id, series_id
            "#,
        )
        .bind(virtual_folder_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series_members (item_id, virtual_folder_id, series_id)
            SELECT item.id, item.virtual_folder_id,
                   trim(json_extract(item.metadata_json, '$.SeriesId'))
            FROM media_items AS item
            WHERE item.virtual_folder_id = ?1
              AND item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
            "#,
        )
        .bind(virtual_folder_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series_coverage (
                virtual_folder_id, projection_version, episode_count, series_count
            )
            SELECT ?1, ?2,
                   (SELECT count(*) FROM media_item_tv_series_members
                    WHERE virtual_folder_id = ?1),
                   (SELECT count(*) FROM media_item_tv_series
                    WHERE virtual_folder_id = ?1)
            "#,
        )
        .bind(virtual_folder_id)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .execute(&mut **tx)
        .await?;
        Ok(true)
    }

    /// Visible unplayed TV candidates for `/Shows/NextUp` without their `media_streams` payload.
    ///
    /// The one-per-series choice is derived from each episode's name and path, so the streams and
    /// metadata columns are never read by the selection and stay unfetched. Callers must hydrate
    /// the page they keep with `media_items_by_ids` before serializing it, because `media_streams`
    /// arrives empty.
    pub async fn tv_next_up_candidate_items(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogNextUpCandidates,
            DatabasePoolRole::Api,
        );
        let result: anyhow::Result<Vec<MediaItem>> = async {
            let rows = sqlx::query_as::<_, TvNextUpCandidateRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.created_at, item.updated_at
                FROM media_items AS item
                LEFT JOIN playback_states AS playback
                  ON playback.item_id = item.id AND playback.user_id = ?1
                WHERE item.missing_since IS NULL
                  AND item.media_type = 'Video'
                  AND item.collection_type = 'tvshows'
                  AND COALESCE(playback.played, 0) = 0
                "#,
            )
            .bind(user_id.to_string())
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter().map(TryInto::try_into).collect()
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    /// Exact visible items for a bounded id list, preserving the caller's order.
    ///
    /// This hydrates a page whose selection ran on rows without `media_streams`.
    pub async fn media_items_by_ids(&self, item_ids: &[Uuid]) -> anyhow::Result<Vec<MediaItem>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogItemById, DatabasePoolRole::Api);
        let result: anyhow::Result<Vec<MediaItem>> = async {
            let storage_ids = item_ids
                .iter()
                .flat_map(|item_id| [item_id.simple().to_string(), item_id.to_string()])
                .collect::<Vec<_>>();
            let mut by_id = HashMap::new();
            for chunk in storage_ids.chunks(500) {
                let mut query = QueryBuilder::<Sqlite>::new(
                    "SELECT id, virtual_folder_id, name, path, media_type, collection_type, \
                     file_size, runtime_ticks, bitrate, width, height, media_streams_json, \
                     created_at, updated_at FROM media_items \
                     WHERE missing_since IS NULL AND id IN (",
                );
                let mut separated = query.separated(", ");
                for storage_id in chunk {
                    separated.push_bind(storage_id);
                }
                separated.push_unseparated(")");
                let rows = query
                    .build_query_as::<MediaItemRow>()
                    .fetch_all(&self.pool)
                    .await?;
                for row in rows {
                    let item = MediaItem::try_from(row)?;
                    by_id.insert(item.id, item);
                }
            }
            Ok(item_ids
                .iter()
                .filter_map(|item_id| by_id.remove(item_id))
                .collect::<Vec<_>>())
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    pub async fn tv_next_up_candidates(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogNextUpCandidates,
            DatabasePoolRole::Api,
        );
        let result: anyhow::Result<Vec<MediaItemCatalogEntry>> = async {
            let rows = sqlx::query_as::<_, MediaItemCatalogRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.media_streams_json, item.metadata_json,
                       item.created_at, item.updated_at,
                       playback.user_id AS playback_user_id,
                       playback.item_id AS playback_item_id,
                       playback.media_source_id AS playback_media_source_id,
                       playback.audio_stream_index AS playback_audio_stream_index,
                       playback.subtitle_stream_index AS playback_subtitle_stream_index,
                       playback.position_ticks AS playback_position_ticks,
                       playback.is_paused AS playback_is_paused,
                       playback.played AS playback_played,
                       playback.is_favorite AS playback_is_favorite,
                       playback.rating AS playback_rating,
                       playback.updated_at AS playback_updated_at
                FROM media_items AS item
                LEFT JOIN playback_states AS playback
                  ON playback.item_id = item.id AND playback.user_id = ?1
                WHERE item.missing_since IS NULL
                  AND item.media_type = 'Video'
                  AND item.collection_type = 'tvshows'
                  AND COALESCE(playback.played, 0) = 0
                "#,
            )
            .bind(user_id.to_string())
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter().map(TryInto::try_into).collect()
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    pub async fn tv_upcoming_candidates(
        &self,
        now: OffsetDateTime,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogUpcomingCandidates,
            DatabasePoolRole::Api,
        );
        let result: anyhow::Result<Vec<MediaItemCatalogEntry>> = async {
            let mut rows = sqlx::query_as::<_, MediaItemCatalogRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.media_streams_json, item.metadata_json,
                       item.created_at, item.updated_at,
                       CAST(NULL AS TEXT) AS playback_user_id,
                       CAST(NULL AS TEXT) AS playback_item_id,
                       CAST(NULL AS TEXT) AS playback_media_source_id,
                       CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                       CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                       CAST(NULL AS INTEGER) AS playback_position_ticks,
                       CAST(NULL AS INTEGER) AS playback_is_paused,
                       CAST(NULL AS INTEGER) AS playback_played,
                       CAST(NULL AS INTEGER) AS playback_is_favorite,
                       CAST(NULL AS REAL) AS playback_rating,
                       CAST(NULL AS TEXT) AS playback_updated_at
                FROM media_items AS item
                JOIN media_item_upcoming_dates AS upcoming
                  ON upcoming.item_id = item.id
                WHERE item.missing_since IS NULL
                  AND item.media_type = 'Video'
                  AND item.collection_type = 'tvshows'
                  AND (
                       upcoming.unix_seconds > ?1
                       OR (upcoming.unix_seconds = ?1 AND upcoming.nanosecond > ?2)
                  )
                "#,
            )
            .bind(now.unix_timestamp())
            .bind(i32::try_from(now.nanosecond()).context("current nanosecond overflow")?)
            .fetch(&self.pool);
            let mut candidates = Vec::new();
            while let Some(row) = rows.try_next().await? {
                let entry = MediaItemCatalogEntry::try_from(row)?;
                if is_upcoming_media_item_entry(&entry, now) {
                    candidates.push(entry);
                }
            }
            Ok(candidates)
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    pub async fn media_items_with_metadata_by_effective_types(
        &self,
        item_types: &[String],
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogEffectiveTypeCandidates,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_items_with_metadata_by_effective_types_unobserved(item_types)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_with_metadata_by_effective_types_unobserved(
        &self,
        item_types: &[String],
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let scope = EffectiveTypeCandidateScope::from_effective_types(item_types);
        if scope.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams_json, item.metadata_json,
                   item.created_at, item.updated_at,
                   CAST(NULL AS TEXT) AS playback_user_id,
                   CAST(NULL AS TEXT) AS playback_item_id,
                   CAST(NULL AS TEXT) AS playback_media_source_id,
                   CAST(NULL AS INTEGER) AS playback_audio_stream_index,
                   CAST(NULL AS INTEGER) AS playback_subtitle_stream_index,
                   CAST(NULL AS INTEGER) AS playback_position_ticks,
                   CAST(NULL AS INTEGER) AS playback_is_paused,
                   CAST(NULL AS INTEGER) AS playback_played,
                   CAST(NULL AS INTEGER) AS playback_is_favorite,
                   CAST(NULL AS REAL) AS playback_rating,
                   CAST(NULL AS TEXT) AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
            "#,
        );

        if !scope.all_raw_media_types {
            query.push(" AND (");
            let mut has_predicate = false;
            if scope.all_video {
                query.push("item.media_type = 'Video'");
                has_predicate = true;
            } else if !scope.video_collection_types.is_empty() {
                query.push("(item.media_type = 'Video' AND item.collection_type IN (");
                {
                    let mut separated = query.separated(", ");
                    for collection_type in &scope.video_collection_types {
                        separated.push_bind(*collection_type);
                    }
                }
                query.push("))");
                has_predicate = true;
            }
            for media_type in &scope.raw_media_types {
                if has_predicate {
                    query.push(" OR ");
                }
                query.push("item.media_type = ").push_bind(*media_type);
                has_predicate = true;
            }
            debug_assert!(has_predicate);
            query.push(")");
        }

        let entries = query
            .build_query_as::<MediaItemCatalogRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(retain_entries_with_effective_types(entries, item_types))
    }

    pub async fn media_items_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogFolderItems, DatabasePoolRole::Api);
        let result = self
            .media_items_for_virtual_folders_unobserved(folder_ids)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_for_virtual_folders_unobserved(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItem>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, virtual_folder_id, name, path, media_type, collection_type, \
             file_size, runtime_ticks, bitrate, width, height, media_streams_json, \
             created_at, updated_at \
             FROM media_items \
             WHERE missing_since IS NULL AND virtual_folder_id IN (",
        );
        let mut separated = query.separated(", ");
        for id in folder_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(") ORDER BY name COLLATE NOCASE");

        let rows = query
            .build_query_as::<MediaItemRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_item_counts_by_virtual_folder(
        &self,
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogFolderCounts,
            DatabasePoolRole::Api,
        );
        let result = self.media_item_counts_by_virtual_folder_unobserved().await;
        observation.finish_result(&result, |counts| {
            u64::try_from(counts.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_counts_by_virtual_folder_unobserved(
        &self,
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        let rows = sqlx::query(
            "SELECT virtual_folder_id, COUNT(*) AS count \
             FROM media_items \
             WHERE missing_since IS NULL \
             GROUP BY virtual_folder_id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut counts = HashMap::new();
        for row in rows {
            let folder_id: String = row.try_get("virtual_folder_id")?;
            let count: i64 = row.try_get("count")?;
            if let Ok(folder_id) = Uuid::parse_str(&folder_id) {
                counts.insert(folder_id, count.max(0) as usize);
            }
        }
        Ok(counts)
    }

    pub async fn media_item_filter_summary_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<MediaItemFilterSummary> {
        if folder_ids.is_empty() {
            return Ok(MediaItemFilterSummary::default());
        }

        let genres = self
            .distinct_media_item_metadata_values_for_virtual_folders(folder_ids, "Genres")
            .await?;
        let tags = self
            .distinct_media_item_metadata_values_for_virtual_folders(folder_ids, "Tags")
            .await?;
        let mut summary = MediaItemFilterSummary {
            genres,
            tags,
            ..Default::default()
        };

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT path, media_type FROM media_items \
             WHERE missing_since IS NULL AND virtual_folder_id IN (",
        );
        let mut separated = query.separated(", ");
        for id in folder_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(")");
        let rows = query.build().fetch_all(&self.pool).await?;
        let mut containers = BTreeSet::new();
        let mut media_types = BTreeSet::new();
        for row in rows {
            let path: String = row.try_get("path")?;
            let media_type: String = row.try_get("media_type")?;
            if !media_type.trim().is_empty() {
                media_types.insert(media_type);
            }
            if let Some(extension) = Path::new(&path).extension().and_then(OsStr::to_str) {
                let extension = extension.trim().to_ascii_lowercase();
                if !extension.is_empty() {
                    containers.insert(extension);
                }
            }
        }
        summary.containers = containers.into_iter().collect();
        summary.media_types = media_types.into_iter().collect();
        Ok(summary)
    }

    pub async fn distinct_media_item_metadata_values_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
        key: &str,
    ) -> anyhow::Result<Vec<String>> {
        let json_path = format!("$.{key}");
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT DISTINCT json_each.value AS value \
             FROM media_items, json_each(media_items.metadata_json, ",
        );
        query.push_bind(json_path);
        query.push(") WHERE missing_since IS NULL AND virtual_folder_id IN (");
        let mut separated = query.separated(", ");
        for id in folder_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(") ORDER BY value COLLATE NOCASE");
        let rows = query.build().fetch_all(&self.pool).await?;
        let mut values = BTreeSet::new();
        for row in rows {
            let value: Option<String> = row.try_get("value")?;
            if let Some(value) = value {
                let value = value.trim();
                if !value.is_empty() {
                    values.insert(value.to_string());
                }
            }
        }
        Ok(values.into_iter().collect())
    }

    pub async fn media_item_facet_values(
        &self,
        kind: MediaItemFacetKind,
        virtual_folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItemFacetValue>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT normalized_value, display_value, stable_id, payload_json
            FROM (
                SELECT facet.normalized_value, facet.display_value, facet.stable_id,
                       facet.payload_json,
                       ROW_NUMBER() OVER (
                           PARTITION BY facet.normalized_value
                           ORDER BY item.created_at, facet.position, facet.item_id
                       ) AS facet_rank
                FROM media_item_facets AS facet
                JOIN media_items AS item ON item.id = facet.item_id
                WHERE item.missing_since IS NULL AND facet.facet_kind =
            "#,
        );
        query.push_bind(kind.as_str());
        if !virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in virtual_folder_ids {
                separated.push_bind(folder_id.to_string());
            }
            separated.push_unseparated(")");
        }
        query.push(
            ") AS ranked WHERE facet_rank = 1 ORDER BY normalized_value, display_value, stable_id",
        );
        let rows = query
            .build_query_as::<(String, String, String, String)>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(
                |(normalized_value, display_value, stable_id, payload_json)| -> anyhow::Result<_> {
                    Ok(MediaItemFacetValue {
                        normalized_value,
                        display_value,
                        stable_id,
                        payload: serde_json::from_str(&payload_json)
                            .context("invalid media item facet payload JSON")?,
                    })
                },
            )
            .collect::<anyhow::Result<Vec<_>>>()
    }

    pub async fn media_item_facet_by_entity_id(
        &self,
        kind: MediaItemFacetKind,
        entity_id: &str,
    ) -> anyhow::Result<Option<MediaItemFacetValue>> {
        let row = sqlx::query_as::<_, (String, String, String, String)>(
            r#"
            SELECT facet.normalized_value, facet.display_value, facet.stable_id, facet.payload_json
            FROM media_item_facets AS facet
            JOIN media_items AS item ON item.id = facet.item_id
            WHERE item.missing_since IS NULL
              AND facet.facet_kind = ?1
              AND (facet.stable_id = ?2 OR EXISTS (
                  SELECT 1 FROM media_item_facet_aliases AS alias
                  WHERE alias.item_id = facet.item_id
                    AND alias.facet_kind = facet.facet_kind
                    AND alias.normalized_value = facet.normalized_value
                    AND alias.entity_id = ?2
              ))
            ORDER BY item.created_at, facet.position, facet.item_id
            LIMIT 1
            "#,
        )
        .bind(kind.as_str())
        .bind(entity_id.trim().to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        row.map(
            |(normalized_value, display_value, stable_id, payload_json)| -> anyhow::Result<_> {
                Ok(MediaItemFacetValue {
                    normalized_value,
                    display_value,
                    stable_id,
                    payload: serde_json::from_str(&payload_json)
                        .context("invalid media item facet payload JSON")?,
                })
            },
        )
        .transpose()
    }

    pub async fn media_item_facet_by_normalized_value(
        &self,
        kind: MediaItemFacetKind,
        value: &str,
        virtual_folder_ids: &[Uuid],
    ) -> anyhow::Result<Option<MediaItemFacetValue>> {
        let normalized_value = value.trim().to_ascii_lowercase();
        if normalized_value.is_empty() {
            return Ok(None);
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT facet.normalized_value, facet.display_value, facet.stable_id, facet.payload_json
            FROM media_item_facets AS facet
            JOIN media_items AS item ON item.id = facet.item_id
            WHERE item.missing_since IS NULL
              AND facet.facet_kind =
            "#,
        );
        query.push_bind(kind.as_str());
        query
            .push(" AND facet.normalized_value = ")
            .push_bind(normalized_value);
        if !virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in virtual_folder_ids {
                separated.push_bind(folder_id.to_string());
            }
            separated.push_unseparated(")");
        }
        query.push(" ORDER BY item.created_at, facet.position, facet.item_id LIMIT 1");
        query
            .build_query_as::<(String, String, String, String)>()
            .fetch_optional(&self.pool)
            .await?
            .map(
                |(normalized_value, display_value, stable_id, payload_json)| {
                    Ok(MediaItemFacetValue {
                        normalized_value,
                        display_value,
                        stable_id,
                        payload: serde_json::from_str(&payload_json)
                            .context("invalid media item facet payload JSON")?,
                    })
                },
            )
            .transpose()
    }

    pub async fn media_item_ids_for_facets(
        &self,
        query_spec: &MediaItemFacetCandidateQuery,
    ) -> anyhow::Result<Vec<Uuid>> {
        let normalized_values = normalized_facet_query_values(&query_spec.normalized_values);
        let entity_ids = normalized_facet_query_values(&query_spec.entity_ids);
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT DISTINCT facet.item_id
            FROM media_item_facets AS facet
            JOIN media_items AS item ON item.id = facet.item_id
            WHERE item.missing_since IS NULL
            "#,
        );
        if let Some(kind) = query_spec.kind {
            query
                .push(" AND facet.facet_kind = ")
                .push_bind(kind.as_str());
        }
        if !query_spec.virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in &query_spec.virtual_folder_ids {
                separated.push_bind(folder_id.to_string());
            }
            separated.push_unseparated(")");
        }
        if !normalized_values.is_empty() || !entity_ids.is_empty() {
            query.push(" AND (");
            if !normalized_values.is_empty() {
                query.push("facet.normalized_value IN (");
                let mut separated = query.separated(", ");
                for value in &normalized_values {
                    separated.push_bind(value);
                }
                separated.push_unseparated(")");
            }
            if !normalized_values.is_empty() && !entity_ids.is_empty() {
                query.push(" OR ");
            }
            if !entity_ids.is_empty() {
                query.push("(facet.stable_id IN (");
                let mut separated = query.separated(", ");
                for entity_id in &entity_ids {
                    separated.push_bind(entity_id);
                }
                separated.push_unseparated(") OR EXISTS (SELECT 1 FROM media_item_facet_aliases AS alias WHERE alias.item_id = facet.item_id AND alias.facet_kind = facet.facet_kind AND alias.normalized_value = facet.normalized_value AND alias.entity_id IN (");
                let mut separated = query.separated(", ");
                for entity_id in &entity_ids {
                    separated.push_bind(entity_id);
                }
                separated.push_unseparated(")))");
            }
            query.push(")");
        }
        query.push(" ORDER BY facet.item_id");
        query
            .build_query_scalar::<String>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|id| Uuid::parse_str(&id).context("invalid media item facet owner id"))
            .collect()
    }

    pub async fn rebuild_media_item_facets(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM media_item_facets")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM media_item_genre_selectors")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM media_item_upcoming_dates")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM media_item_filter_selectors")
            .execute(&mut *tx)
            .await?;
        let mut last_item_id = None::<String>;
        loop {
            let rows = if let Some(last_item_id) = last_item_id.as_deref() {
                sqlx::query_as::<_, (String, String)>(
                    "SELECT id, metadata_json FROM media_items WHERE id > ?1 ORDER BY id LIMIT ?2",
                )
                .bind(last_item_id)
                .bind(FACET_REBUILD_BATCH_SIZE)
                .fetch_all(&mut *tx)
                .await?
            } else {
                sqlx::query_as::<_, (String, String)>(
                    "SELECT id, metadata_json FROM media_items ORDER BY id LIMIT ?1",
                )
                .bind(FACET_REBUILD_BATCH_SIZE)
                .fetch_all(&mut *tx)
                .await?
            };
            if rows.is_empty() {
                break;
            }
            for (item_id, metadata_json) in &rows {
                let metadata = serde_json::from_str::<Value>(metadata_json)
                    .with_context(|| format!("invalid metadata JSON for media item {item_id}"))?;
                replace_sqlite_media_item_facets(&mut tx, item_id, &metadata).await?;
            }
            last_item_id = rows.last().map(|(item_id, _)| item_id.clone());
        }
        sqlx::query(
            r#"
            INSERT INTO jellyrin_derived_projection_versions (
                projection_name, extractor_version, completed_at, source_item_count,
                projected_facet_count, projected_alias_count
            )
            SELECT ?1, ?2, ?3,
                   (SELECT COUNT(*) FROM media_items),
                   (SELECT COUNT(*) FROM media_item_facets),
                   (SELECT COUNT(*) FROM media_item_facet_aliases)
            ON CONFLICT(projection_name) DO UPDATE SET
                extractor_version = excluded.extractor_version,
                completed_at = excluded.completed_at,
                source_item_count = excluded.source_item_count,
                projected_facet_count = excluded.projected_facet_count,
                projected_alias_count = excluded.projected_alias_count
            "#,
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn rebuild_media_item_query_filter_projection(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM media_item_query_filter_sources")
            .execute(&mut *tx)
            .await?;
        let mut last_item_id = None::<String>;
        loop {
            let rows = if let Some(last_item_id) = last_item_id.as_deref() {
                sqlx::query_as::<_, (String, String, String, String, String, String)>(
                    "SELECT id, virtual_folder_id, path, media_type, media_streams_json, metadata_json \
                     FROM media_items WHERE id > ?1 ORDER BY id LIMIT ?2",
                )
                .bind(last_item_id)
                .bind(FACET_REBUILD_BATCH_SIZE)
                .fetch_all(&mut *tx)
                .await?
            } else {
                sqlx::query_as::<_, (String, String, String, String, String, String)>(
                    "SELECT id, virtual_folder_id, path, media_type, media_streams_json, metadata_json \
                     FROM media_items ORDER BY id LIMIT ?1",
                )
                .bind(FACET_REBUILD_BATCH_SIZE)
                .fetch_all(&mut *tx)
                .await?
            };
            if rows.is_empty() {
                break;
            }
            for (item_id, folder_id, path, media_type, media_streams_json, metadata_json) in &rows {
                let media_streams = serde_json::from_str::<Vec<Value>>(media_streams_json)
                    .with_context(|| format!("invalid media streams JSON for item {item_id}"))?;
                let metadata = serde_json::from_str::<Value>(metadata_json)
                    .with_context(|| format!("invalid metadata JSON for item {item_id}"))?;
                let projection = extract_media_item_query_filter_projection(
                    MediaItemQueryFilterProjectionSource {
                        path,
                        media_type,
                        media_streams: &media_streams,
                        metadata: &metadata,
                    },
                );
                replace_sqlite_media_item_query_filter_projection(
                    &mut tx,
                    item_id,
                    folder_id,
                    &projection,
                )
                .await?;
            }
            last_item_id = rows.last().map(|row| row.0.clone());
        }
        sqlx::query(
            r#"
            INSERT INTO jellyrin_derived_projection_versions (
                projection_name, extractor_version, completed_at, source_item_count,
                projected_facet_count, projected_alias_count
            ) SELECT ?1, ?2, ?3,
                     (SELECT count(*) FROM media_item_query_filter_sources),
                     (SELECT count(*) FROM media_item_query_filter_values), 0
            ON CONFLICT(projection_name) DO UPDATE SET
                extractor_version = excluded.extractor_version,
                completed_at = excluded.completed_at,
                source_item_count = excluded.source_item_count,
                projected_facet_count = excluded.projected_facet_count,
                projected_alias_count = 0
            "#,
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn replace_media_item_facets_in_transaction(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        item_id: &str,
        metadata: &Value,
    ) -> anyhow::Result<()> {
        replace_sqlite_media_item_facets(tx, item_id, metadata).await
    }

    pub async fn begin_remote_media_catalog_stage(
        &self,
        libraries: Vec<RemoteMediaLibraryStageSpec>,
    ) -> anyhow::Result<RemoteMediaCatalogStage> {
        self.begin_remote_media_catalog_stage_for_revision(libraries, "")
            .await
    }

    pub async fn begin_remote_media_catalog_stage_for_revision(
        &self,
        libraries: Vec<RemoteMediaLibraryStageSpec>,
        source_revision: &str,
    ) -> anyhow::Result<RemoteMediaCatalogStage> {
        let libraries = prepare_remote_media_library_stage_specs(libraries)?;
        let source_revision = remote_media_stage_source_revision(source_revision)?;
        let stage = RemoteMediaCatalogStage::new(Uuid::new_v4());
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO remote_media_catalog_stages (
                id, status, extractor_version, query_filter_extractor_version,
                source_revision, created_at, updated_at
            )
            VALUES (?1, 'open', ?2, ?3, ?4, ?5, ?5)
            "#,
        )
        .bind(stage.id())
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .bind(source_revision)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        for library in libraries {
            sqlx::query(
                r#"
                INSERT INTO remote_media_catalog_stage_libraries (
                    stage_id, library_key, position, library_name,
                    collection_type, source_location
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )
            .bind(stage.id())
            .bind(library.key)
            .bind(library.position)
            .bind(library.library_name)
            .bind(library.collection_type)
            .bind(library.source_location)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(stage)
    }

    pub async fn append_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
        library_key: &str,
        items: Vec<RemoteMediaItemUpsert>,
    ) -> anyhow::Result<()> {
        validate_remote_media_catalog_stage_append(&items)?;
        anyhow::ensure!(
            matches!(library_key, "movies" | "series"),
            "remote media catalogue stage library key must be movies or series"
        );
        stage.parsed_id()?;
        let prepared = items
            .into_iter()
            .map(PreparedSqliteRemoteMediaItem::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        let locked = sqlx::query(
            "UPDATE remote_media_catalog_stages SET updated_at = updated_at WHERE id = ?1",
        )
        .bind(stage.id())
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(
            locked.rows_affected() == 1,
            "remote media catalogue stage not found"
        );
        let (status, ready_at, extractor_version, query_filter_version) =
            sqlx::query_as::<_, (String, Option<String>, i32, i32)>(
                "SELECT status, ready_at, extractor_version, query_filter_extractor_version \
             FROM remote_media_catalog_stages WHERE id = ?1",
            )
            .bind(stage.id())
            .fetch_one(&mut *tx)
            .await?;
        anyhow::ensure!(status == "open", "remote media catalogue stage is not open");
        anyhow::ensure!(
            ready_at.is_none(),
            "remote media catalogue stage is already complete"
        );
        anyhow::ensure!(
            extractor_version == MEDIA_ITEM_FACET_PROJECTION_VERSION,
            "remote media catalogue stage extractor version is stale"
        );
        anyhow::ensure!(
            query_filter_version == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
            "remote media catalogue stage query-filter extractor version is stale"
        );
        let appended_count = i64::try_from(prepared.len())
            .context("remote media catalogue stage append count overflow")?;
        let count_update = sqlx::query(
            r#"
            UPDATE remote_media_catalog_stage_libraries
            SET item_count = item_count + ?3
            WHERE stage_id = ?1 AND library_key = ?2
              AND item_count + ?3 <= ?4
            "#,
        )
        .bind(stage.id())
        .bind(library_key)
        .bind(appended_count)
        .bind(
            i64::try_from(REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS)
                .context("remote media catalogue stage library limit overflow")?,
        )
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(
            count_update.rows_affected() == 1,
            "remote media catalogue stage library was not found or exceeded its item limit"
        );

        for item in &prepared {
            sqlx::query(
                r#"
                INSERT INTO remote_media_catalog_stage_items (
                    stage_id, library_key, id, name, path, media_type, collection_type,
                    runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
            )
            .bind(stage.id())
            .bind(library_key)
            .bind(&item.id)
            .bind(&item.name)
            .bind(&item.path)
            .bind(&item.media_type)
            .bind(&item.collection_type)
            .bind(item.runtime_ticks)
            .bind(item.bitrate)
            .bind(item.width)
            .bind(item.height)
            .bind(&item.media_streams_json)
            .bind(&item.metadata_json)
            .execute(&mut *tx)
            .await?;

            let metadata = serde_json::from_str::<Value>(&item.metadata_json)
                .with_context(|| format!("invalid metadata JSON for media item {}", item.id))?;
            let media_streams = serde_json::from_str::<Vec<Value>>(&item.media_streams_json)
                .with_context(|| {
                    format!("invalid media streams JSON for media item {}", item.id)
                })?;
            let projection =
                extract_media_item_query_filter_projection(MediaItemQueryFilterProjectionSource {
                    path: &item.path,
                    media_type: &item.media_type,
                    media_streams: &media_streams,
                    metadata: &metadata,
                });
            sqlx::query(
                "INSERT INTO remote_media_catalog_stage_query_filter_sources (stage_id, item_id, \
                 container_present, container_value, media_type, is_video, has_subtitles, \
                 has_trailer, projected_value_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .bind(stage.id())
            .bind(&item.id)
            .bind(projection.features.container_present)
            .bind(&projection.features.container)
            .bind(&projection.features.media_type)
            .bind(projection.features.is_video)
            .bind(projection.features.has_subtitles)
            .bind(projection.features.has_trailer)
            .bind(i64::try_from(projection.values.len()).context("projection value overflow")?)
            .execute(&mut *tx)
            .await?;
            for value in &projection.values {
                sqlx::query(
                    "INSERT INTO remote_media_catalog_stage_query_filter_values (stage_id, \
                     item_id, value_kind, display_value, source_key, source_priority, \
                     source_position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(stage.id())
                .bind(&item.id)
                .bind(value.kind.as_str())
                .bind(&value.display_value)
                .bind(&value.source_key)
                .bind(i64::from(value.source_priority))
                .bind(encode_media_item_query_filter_position(&value.position))
                .execute(&mut *tx)
                .await?;
            }
            let facets = extract_media_item_facets(&metadata);
            for facet in &facets {
                let position = i64::from(facet.position);
                sqlx::query(
                    r#"
                    INSERT INTO remote_media_catalog_stage_facets (
                        stage_id, item_id, facet_kind, normalized_value, display_value,
                        stable_id, position, payload_json
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    "#,
                )
                .bind(stage.id())
                .bind(&item.id)
                .bind(facet.kind.as_str())
                .bind(&facet.normalized_value)
                .bind(&facet.display_value)
                .bind(&facet.stable_id)
                .bind(position)
                .bind(serde_json::to_string(&facet.payload)?)
                .execute(&mut *tx)
                .await?;
                for alias in &facet.aliases {
                    sqlx::query(
                        r#"
                        INSERT INTO remote_media_catalog_stage_facet_aliases (
                            stage_id, item_id, facet_kind, normalized_value, entity_id
                        )
                        VALUES (?1, ?2, ?3, ?4, ?5)
                        "#,
                    )
                    .bind(stage.id())
                    .bind(&item.id)
                    .bind(facet.kind.as_str())
                    .bind(&facet.normalized_value)
                    .bind(alias)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            for selector in extract_media_item_genre_selectors(&metadata) {
                sqlx::query(
                    "INSERT INTO remote_media_catalog_stage_genre_selectors \
                     (stage_id, item_id, selector) VALUES (?1, ?2, ?3)",
                )
                .bind(stage.id())
                .bind(&item.id)
                .bind(selector)
                .execute(&mut *tx)
                .await?;
            }
            for (kind, selector) in extract_media_item_filter_selectors(&metadata) {
                sqlx::query(
                    "INSERT INTO remote_media_catalog_stage_filter_selectors \
                     (stage_id, item_id, selector_kind, selector) VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(stage.id())
                .bind(&item.id)
                .bind(kind.as_str())
                .bind(selector)
                .execute(&mut *tx)
                .await?;
            }
            if let Some((unix_seconds, nanosecond)) = upcoming_media_item_premiere_parts(&metadata)
            {
                sqlx::query(
                    "INSERT INTO remote_media_catalog_stage_upcoming_dates \
                     (stage_id, item_id, unix_seconds, nanosecond) VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(stage.id())
                .bind(&item.id)
                .bind(unix_seconds)
                .bind(nanosecond)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query("UPDATE remote_media_catalog_stages SET updated_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(stage.id())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn complete_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<()> {
        stage.parsed_id()?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let completed = sqlx::query(
            "UPDATE remote_media_catalog_stages SET ready_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status = 'open' AND ready_at IS NULL \
               AND extractor_version = ?3 AND query_filter_extractor_version = ?4 \
               AND (SELECT count(*) FROM remote_media_catalog_stage_libraries \
                    WHERE stage_id = ?2) = 2 \
               AND NOT EXISTS (SELECT 1 FROM remote_media_catalog_stage_libraries AS library \
                    WHERE library.stage_id = ?2 AND library.item_count <> \
                      (SELECT count(*) FROM remote_media_catalog_stage_items AS item \
                       WHERE item.stage_id = library.stage_id \
                         AND item.library_key = library.library_key))",
        )
        .bind(&now)
        .bind(stage.id())
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .execute(&self.pool)
        .await?;
        anyhow::ensure!(
            completed.rows_affected() == 1,
            "remote media catalogue stage is incomplete, stale, or not open"
        );
        Ok(())
    }

    pub async fn ready_remote_media_catalog_stage(
        &self,
        libraries: Vec<RemoteMediaLibraryStageSpec>,
        source_revision: &str,
    ) -> anyhow::Result<Option<ReadyRemoteMediaCatalogStage>> {
        let libraries = prepare_remote_media_library_stage_specs(libraries)?;
        let source_revision = remote_media_stage_source_revision(source_revision)?;
        let movies = &libraries[0];
        let series = &libraries[1];
        let row = sqlx::query_as::<_, (String, i64, i64)>(
            r#"
            SELECT stage.id, movies.item_count, series.item_count
            FROM remote_media_catalog_stages AS stage
            JOIN remote_media_catalog_stage_libraries AS movies
              ON movies.stage_id = stage.id AND movies.library_key = 'movies'
            JOIN remote_media_catalog_stage_libraries AS series
              ON series.stage_id = stage.id AND series.library_key = 'series'
            WHERE stage.status = 'open' AND stage.ready_at IS NOT NULL
              AND stage.extractor_version = ?1
              AND stage.query_filter_extractor_version = ?2
              AND stage.source_revision = ?3
              AND lower(movies.library_name) = lower(?4)
              AND movies.collection_type = ?5 AND movies.source_location = ?6
              AND lower(series.library_name) = lower(?7)
              AND series.collection_type = ?8 AND series.source_location = ?9
              AND (SELECT count(*) FROM remote_media_catalog_stage_libraries AS library
                   WHERE library.stage_id = stage.id) = 2
            ORDER BY stage.ready_at DESC
            LIMIT 1
            "#,
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .bind(source_revision)
        .bind(&movies.library_name)
        .bind(&movies.collection_type)
        .bind(&movies.source_location)
        .bind(&series.library_name)
        .bind(&series.collection_type)
        .bind(&series.source_location)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(id, movie_count, series_item_count)| {
            Ok(ReadyRemoteMediaCatalogStage {
                stage: RemoteMediaCatalogStage::try_from_id(id)?,
                movie_count: usize::try_from(movie_count)
                    .context("ready movie stage count overflow")?,
                series_item_count: usize::try_from(series_item_count)
                    .context("ready series stage count overflow")?,
            })
        })
        .transpose()
    }

    pub async fn abort_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<()> {
        stage.parsed_id()?;
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        let locked = sqlx::query(
            "UPDATE remote_media_catalog_stages SET updated_at = updated_at WHERE id = ?1",
        )
        .bind(stage.id())
        .execute(&mut *tx)
        .await?;
        if locked.rows_affected() == 1 {
            let (status, _extractor_version) = sqlx::query_as::<_, (String, i32)>(
                "SELECT status, extractor_version \
                 FROM remote_media_catalog_stages WHERE id = ?1",
            )
            .bind(stage.id())
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                status != "publishing",
                "remote media catalogue stage is publishing"
            );
            sqlx::query(
                "UPDATE remote_media_catalog_stages SET status = 'aborted', updated_at = ?1 \
                 WHERE id = ?2",
            )
            .bind(now)
            .bind(stage.id())
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM remote_media_catalog_stages WHERE id = ?1")
                .bind(stage.id())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn cleanup_abandoned_remote_media_catalog_stages(
        &self,
        older_than: OffsetDateTime,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "DELETE FROM remote_media_catalog_stages \
             WHERE ((status = 'open' AND ready_at IS NULL) OR status = 'aborted') \
               AND updated_at < ?1",
        )
        .bind(format_time(older_than)?)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn publish_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        let stage_id = stage.parsed_id()?;
        retry_transient_catalog_lock(catalog_lock_jitter_seed(stage_id), || {
            self.publish_remote_media_catalog_stage_once(stage)
        })
        .await
    }

    async fn publish_remote_media_catalog_stage_once(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let mut tx = self.pool.begin().await?;
        let locked = sqlx::query(
            "UPDATE remote_media_catalog_stages \
             SET status = 'publishing', updated_at = ?1 \
             WHERE id = ?2 AND status = 'open' AND ready_at IS NOT NULL \
               AND extractor_version = ?3 \
               AND query_filter_extractor_version = ?4",
        )
        .bind(&now)
        .bind(stage.id())
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(
            locked.rows_affected() == 1,
            "remote media catalogue stage was not ready or its extractor version is stale"
        );
        let rows = sqlx::query_as::<_, (String, i16, String, String, String, i64)>(
            r#"
            SELECT library_key, position, library_name, collection_type, source_location,
                   item_count
            FROM remote_media_catalog_stage_libraries
            WHERE stage_id = ?1
            ORDER BY position
            "#,
        )
        .bind(stage.id())
        .fetch_all(&mut *tx)
        .await?;
        let expected_counts = rows
            .iter()
            .map(|(key, _, _, _, _, item_count)| (key.clone(), *item_count))
            .collect::<HashMap<_, _>>();
        let libraries = prepare_remote_media_library_stage_specs(
            rows.into_iter()
                .map(
                    |(key, _position, library_name, collection_type, source_location, _count)| {
                        RemoteMediaLibraryStageSpec {
                            key,
                            library_name,
                            collection_type,
                            source_location,
                        }
                    },
                )
                .collect(),
        )?;

        let mut folders = Vec::with_capacity(2);
        for library in libraries {
            let item_count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM remote_media_catalog_stage_items \
                 WHERE stage_id = ?1 AND library_key = ?2",
            )
            .bind(stage.id())
            .bind(&library.key)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                item_count
                    == *expected_counts
                        .get(&library.key)
                        .context("remote media catalogue stage library count is missing")?,
                "remote media catalogue stage item count mismatch"
            );

            let existing_folder_id = sqlx::query_scalar::<_, String>(
                "SELECT id FROM virtual_folders WHERE name = ?1 COLLATE NOCASE",
            )
            .bind(&library.library_name)
            .fetch_optional(&mut *tx)
            .await?;
            let folder_id = existing_folder_id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let locations_json =
                serde_json::to_string(&normalized_locations(vec![library.source_location]))?;
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
            .bind(&folder_id)
            .bind(&library.library_name)
            .bind((!library.collection_type.is_empty()).then_some(library.collection_type.as_str()))
            .bind(locations_json)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            let sync_run_id = Uuid::new_v4().to_string();
            sqlx::query(
                r#"
                INSERT INTO catalog_sync_runs (
                    id, virtual_folder_id, generation_id, status, item_count, started_at
                )
                VALUES (?1, ?2, ?3, 'running', ?4, ?5)
                "#,
            )
            .bind(&sync_run_id)
            .bind(&folder_id)
            .bind(Uuid::new_v4().to_string())
            .bind(item_count)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            let external_conflicts = sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)
                FROM media_items AS current
                JOIN remote_media_catalog_stage_items AS staged
                  ON current.id = staged.id OR current.path = staged.path
                WHERE staged.stage_id = ?1 AND staged.library_key = ?2
                  AND current.virtual_folder_id <> ?3
                "#,
            )
            .bind(stage.id())
            .bind(&library.key)
            .bind(&folder_id)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                external_conflicts == 0,
                "remote snapshot contains ids or paths owned by another virtual folder"
            );

            sqlx::query(
                r#"
                UPDATE media_items AS current
                SET path = CASE
                        WHEN current.missing_since IS NULL
                        THEN 'jellyrin-tombstone://sqlite/' || current.id
                        ELSE current.path
                    END,
                    missing_since = COALESCE(current.missing_since, ?1),
                    updated_at = CASE
                        WHEN current.missing_since IS NULL THEN ?1
                        ELSE current.updated_at
                    END
                WHERE current.virtual_folder_id = ?2
                  AND (
                        NOT EXISTS (
                            SELECT 1
                            FROM remote_media_catalog_stage_items AS staged
                            WHERE staged.stage_id = ?3 AND staged.library_key = ?4
                              AND staged.id = current.id
                        )
                        OR EXISTS (
                            SELECT 1
                            FROM remote_media_catalog_stage_items AS staged
                            WHERE staged.stage_id = ?3 AND staged.library_key = ?4
                              AND staged.path = current.path AND staged.id <> current.id
                        )
                  )
                "#,
            )
            .bind(&now)
            .bind(&folder_id)
            .bind(stage.id())
            .bind(&library.key)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    created_at, updated_at, last_seen_at, missing_since, file_size, modified_at,
                    runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
                )
                SELECT staged.id, ?1, staged.name, staged.path, staged.media_type,
                       staged.collection_type, ?2, ?2, ?2, NULL, NULL, NULL,
                       staged.runtime_ticks, staged.bitrate, staged.width, staged.height,
                       staged.media_streams_json, staged.metadata_json
                FROM remote_media_catalog_stage_items AS staged
                WHERE staged.stage_id = ?3 AND staged.library_key = ?4
                ON CONFLICT(id) DO UPDATE SET
                    virtual_folder_id = excluded.virtual_folder_id,
                    name = excluded.name,
                    path = excluded.path,
                    media_type = excluded.media_type,
                    collection_type = excluded.collection_type,
                    updated_at = excluded.updated_at,
                    last_seen_at = excluded.last_seen_at,
                    missing_since = NULL,
                    file_size = NULL,
                    modified_at = NULL,
                    runtime_ticks = excluded.runtime_ticks,
                    bitrate = excluded.bitrate,
                    width = excluded.width,
                    height = excluded.height,
                    media_streams_json = excluded.media_streams_json,
                    metadata_json = CASE
                        WHEN json_extract(media_items.metadata_json, '$.XtreamKind') = 'vod'
                         AND json_extract(media_items.metadata_json, '$.XtreamVodInfo.Status') = 'Complete'
                         AND json_type(excluded.metadata_json, '$.XtreamVodInfo') IS NULL
                        THEN json_patch(media_items.metadata_json, excluded.metadata_json)
                        ELSE excluded.metadata_json
                    END
                WHERE media_items.missing_since IS NOT NULL
                   OR media_items.virtual_folder_id IS NOT excluded.virtual_folder_id
                   OR media_items.name IS NOT excluded.name
                   OR media_items.path IS NOT excluded.path
                   OR media_items.media_type IS NOT excluded.media_type
                   OR media_items.collection_type IS NOT excluded.collection_type
                   OR media_items.runtime_ticks IS NOT excluded.runtime_ticks
                   OR media_items.bitrate IS NOT excluded.bitrate
                   OR media_items.width IS NOT excluded.width
                   OR media_items.height IS NOT excluded.height
                   OR media_items.media_streams_json IS NOT excluded.media_streams_json
                   OR media_items.metadata_json IS NOT CASE
                        WHEN json_extract(media_items.metadata_json, '$.XtreamKind') = 'vod'
                         AND json_extract(media_items.metadata_json, '$.XtreamVodInfo.Status') = 'Complete'
                         AND json_type(excluded.metadata_json, '$.XtreamVodInfo') IS NULL
                        THEN json_patch(media_items.metadata_json, excluded.metadata_json)
                        ELSE excluded.metadata_json
                    END
                "#,
            )
            .bind(&folder_id)
            .bind(&now)
            .bind(stage.id())
            .bind(&library.key)
            .execute(&mut *tx)
            .await?;

            let projection_counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "SELECT count(*), coalesce(sum(source.projected_value_count), 0), \
                        (SELECT count(*) FROM remote_media_catalog_stage_query_filter_values AS value \
                         JOIN remote_media_catalog_stage_items AS item \
                           ON item.stage_id = value.stage_id AND item.id = value.item_id \
                         WHERE value.stage_id = ?1 AND item.library_key = ?2), \
                        (SELECT count(*) \
                         FROM remote_media_catalog_stage_query_filter_sources AS checked \
                         JOIN remote_media_catalog_stage_items AS checked_item \
                           ON checked_item.stage_id = checked.stage_id \
                          AND checked_item.id = checked.item_id \
                         WHERE checked.stage_id = ?1 AND checked_item.library_key = ?2 \
                           AND checked.projected_value_count <> (SELECT count(*) \
                             FROM remote_media_catalog_stage_query_filter_values AS checked_value \
                             WHERE checked_value.stage_id = checked.stage_id \
                               AND checked_value.item_id = checked.item_id)) \
                 FROM remote_media_catalog_stage_query_filter_sources AS source \
                 JOIN remote_media_catalog_stage_items AS item \
                   ON item.stage_id = source.stage_id AND item.id = source.item_id \
                 WHERE source.stage_id = ?1 AND item.library_key = ?2",
            )
            .bind(stage.id())
            .bind(&library.key)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                projection_counts.0 == item_count
                    && projection_counts.1 == projection_counts.2
                    && projection_counts.3 == 0,
                "remote media catalogue stage query-filter coverage mismatch"
            );
            sqlx::query(
                "DELETE FROM media_item_query_filter_sources WHERE item_id IN (\
                 SELECT id FROM remote_media_catalog_stage_items \
                 WHERE stage_id = ?1 AND library_key = ?2)",
            )
            .bind(stage.id())
            .bind(&library.key)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO media_item_query_filter_sources (item_id, virtual_folder_id, extractor_version, \
                 container_present, container_value, media_type, is_video, has_subtitles, \
                 has_trailer, projected_value_count, completed_at) \
                 SELECT source.item_id, ?3, ?4, source.container_present, source.container_value, \
                        source.media_type, source.is_video, source.has_subtitles, \
                        source.has_trailer, source.projected_value_count, ?5 \
                 FROM remote_media_catalog_stage_query_filter_sources AS source \
                 JOIN remote_media_catalog_stage_items AS item \
                   ON item.stage_id = source.stage_id AND item.id = source.item_id \
                 WHERE source.stage_id = ?1 AND item.library_key = ?2",
            )
            .bind(stage.id())
            .bind(&library.key)
            .bind(&folder_id)
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO media_item_query_filter_values (item_id, virtual_folder_id, value_kind, display_value, \
                 source_key, source_priority, source_position) \
                 SELECT value.item_id, ?3, value.value_kind, value.display_value, value.source_key, \
                        value.source_priority, value.source_position \
                 FROM remote_media_catalog_stage_query_filter_values AS value \
                 JOIN remote_media_catalog_stage_items AS item \
                   ON item.stage_id = value.stage_id AND item.id = value.item_id \
                 WHERE value.stage_id = ?1 AND item.library_key = ?2",
            )
            .bind(stage.id())
            .bind(&library.key)
            .bind(&folder_id)
            .execute(&mut *tx)
            .await?;

            let mut last_item_id = None::<String>;
            loop {
                let rows = if let Some(last_item_id) = last_item_id.as_deref() {
                    sqlx::query_as::<_, (String, String)>(
                        "SELECT id, metadata_json \
                         FROM remote_media_catalog_stage_items \
                         WHERE stage_id = ?1 AND library_key = ?2 AND id > ?3 \
                         ORDER BY id LIMIT ?4",
                    )
                    .bind(stage.id())
                    .bind(&library.key)
                    .bind(last_item_id)
                    .bind(FACET_REBUILD_BATCH_SIZE)
                    .fetch_all(&mut *tx)
                    .await?
                } else {
                    sqlx::query_as::<_, (String, String)>(
                        "SELECT id, metadata_json \
                         FROM remote_media_catalog_stage_items \
                         WHERE stage_id = ?1 AND library_key = ?2 \
                         ORDER BY id LIMIT ?3",
                    )
                    .bind(stage.id())
                    .bind(&library.key)
                    .bind(FACET_REBUILD_BATCH_SIZE)
                    .fetch_all(&mut *tx)
                    .await?
                };
                if rows.is_empty() {
                    break;
                }
                for (item_id, metadata_json) in &rows {
                    let metadata =
                        serde_json::from_str::<Value>(metadata_json).with_context(|| {
                            format!("invalid metadata JSON for media item {item_id}")
                        })?;
                    Self::replace_media_item_facets_in_transaction(&mut tx, item_id, &metadata)
                        .await?;
                }
                last_item_id = rows.last().map(|(item_id, _)| item_id.clone());
            }

            Self::rebuild_tv_series_catalog_projection_in_transaction(&mut tx, &folder_id).await?;

            sqlx::query(
                "UPDATE catalog_sync_runs SET status = 'completed', completed_at = ?1 \
                 WHERE id = ?2",
            )
            .bind(&now)
            .bind(&sync_run_id)
            .execute(&mut *tx)
            .await?;
            let row = sqlx::query_as::<_, VirtualFolderRow>(
                r#"
                SELECT id, name, collection_type, locations_json, created_at, updated_at
                FROM virtual_folders
                WHERE id = ?1
                "#,
            )
            .bind(folder_id)
            .fetch_one(&mut *tx)
            .await?;
            folders.push(row.try_into()?);
        }

        sqlx::query("DELETE FROM remote_media_catalog_stages WHERE id = ?1")
            .bind(stage.id())
            .execute(&mut *tx)
            .await?;
        let commit_observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogSyncCommit, DatabasePoolRole::Api);
        let commit_result = tx.commit().await.map_err(anyhow::Error::from);
        commit_observation.finish_result(&commit_result, |_| 0);
        commit_result?;
        Ok(folders)
    }

    pub async fn replace_remote_media_library_snapshot(
        &self,
        library_name: &str,
        collection_type: &str,
        source_location: &str,
        items: Vec<RemoteMediaItemUpsert>,
    ) -> anyhow::Result<VirtualFolder> {
        self.replace_remote_media_library_snapshots(vec![RemoteMediaLibrarySnapshot {
            library_name: library_name.to_owned(),
            collection_type: collection_type.to_owned(),
            source_location: source_location.to_owned(),
            items,
        }])
        .await?
        .pop()
        .context("remote media snapshot did not return its virtual folder")
    }

    /// SQLite conformance harness for the production PostgreSQL batch contract.
    ///
    /// SQLite is not a production driver, but retaining the same atomic/tombstone semantics makes
    /// provider tests capable of detecting a movie/series half-publication without PostgreSQL.
    pub async fn replace_remote_media_library_snapshots(
        &self,
        snapshots: Vec<RemoteMediaLibrarySnapshot>,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        let received_rows = snapshots.iter().fold(0u64, |total, snapshot| {
            total.saturating_add(u64::try_from(snapshot.items.len()).unwrap_or(u64::MAX))
        });
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogSyncPublish, DatabasePoolRole::Api);
        let result = self
            .replace_remote_media_library_snapshots_unobserved(snapshots)
            .await;
        observation.finish_result(&result, |_| received_rows);
        result
    }

    async fn replace_remote_media_library_snapshots_unobserved(
        &self,
        snapshots: Vec<RemoteMediaLibrarySnapshot>,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        if snapshots.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = snapshots
            .into_iter()
            .map(PreparedSqliteRemoteMediaLibrarySnapshot::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut names = prepared
            .iter()
            .map(|snapshot| snapshot.library_name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        names.sort_unstable();
        anyhow::ensure!(
            !names.windows(2).any(|names| names[0] == names[1]),
            "remote snapshot batch contains duplicate virtual folder names"
        );

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_remote_snapshot_stage (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                media_type TEXT NOT NULL,
                collection_type TEXT,
                runtime_ticks INTEGER,
                bitrate INTEGER,
                width INTEGER,
                height INTEGER,
                media_streams_json TEXT NOT NULL,
                metadata_json TEXT NOT NULL
            )
            "#,
        )
        .execute(&mut *tx)
        .await?;
        let mut folders = Vec::with_capacity(prepared.len());
        for snapshot in prepared {
            folders.push(
                self.replace_remote_media_library_snapshot_in_transaction(&mut tx, snapshot)
                    .await?,
            );
        }
        let commit_observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogSyncCommit, DatabasePoolRole::Api);
        let commit_result = tx.commit().await.map_err(anyhow::Error::from);
        commit_observation.finish_result(&commit_result, |_| 0);
        commit_result?;
        Ok(folders)
    }

    async fn replace_remote_media_library_snapshot_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        snapshot: PreparedSqliteRemoteMediaLibrarySnapshot,
    ) -> anyhow::Result<VirtualFolder> {
        let PreparedSqliteRemoteMediaLibrarySnapshot {
            library_name,
            collection_type,
            source_location,
            items,
        } = snapshot;
        let now = format_time(OffsetDateTime::now_utc())?;
        let existing_folder_id = sqlx::query_scalar::<_, String>(
            "SELECT id FROM virtual_folders WHERE name = ?1 COLLATE NOCASE",
        )
        .bind(&library_name)
        .fetch_optional(&mut **tx)
        .await?;
        let folder_id = existing_folder_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let locations_json = serde_json::to_string(&normalized_locations(vec![source_location]))?;

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
        .bind(&folder_id)
        .bind(&library_name)
        .bind((!collection_type.is_empty()).then_some(collection_type.as_str()))
        .bind(locations_json)
        .bind(&now)
        .execute(&mut **tx)
        .await?;

        let sync_run_id = Uuid::new_v4().to_string();
        let generation_id = Uuid::new_v4().to_string();
        sqlx::query(
            r#"
            INSERT INTO catalog_sync_runs (
                id, virtual_folder_id, generation_id, status, item_count, started_at
            )
            VALUES (?1, ?2, ?3, 'running', ?4, ?5)
            "#,
        )
        .bind(&sync_run_id)
        .bind(&folder_id)
        .bind(generation_id)
        .bind(i64::try_from(items.len()).context("remote snapshot item count overflow")?)
        .bind(&now)
        .execute(&mut **tx)
        .await?;

        let stage_observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogSyncStage, DatabasePoolRole::Api);
        let stage_result: anyhow::Result<u64> = async {
            sqlx::query("DELETE FROM jellyrin_remote_snapshot_stage")
                .execute(&mut **tx)
                .await?;
            let mut staged_rows = 0u64;
            for item in &items {
                let result = sqlx::query(
                    r#"
                INSERT INTO jellyrin_remote_snapshot_stage (
                    id, name, path, media_type, collection_type, runtime_ticks, bitrate,
                    width, height, media_streams_json, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    "#,
                )
                .bind(&item.id)
                .bind(&item.name)
                .bind(&item.path)
                .bind(&item.media_type)
                .bind(&item.collection_type)
                .bind(item.runtime_ticks)
                .bind(item.bitrate)
                .bind(item.width)
                .bind(item.height)
                .bind(&item.media_streams_json)
                .bind(&item.metadata_json)
                .execute(&mut **tx)
                .await?;
                staged_rows = staged_rows.saturating_add(result.rows_affected());
            }
            Ok(staged_rows)
        }
        .await;
        stage_observation.finish_result(&stage_result, |rows| *rows);
        stage_result?;

        let external_conflicts = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM media_items AS current
            JOIN jellyrin_remote_snapshot_stage AS staged
              ON current.id = staged.id OR current.path = staged.path
            WHERE current.virtual_folder_id <> ?1
            "#,
        )
        .bind(&folder_id)
        .fetch_one(&mut **tx)
        .await?;
        anyhow::ensure!(
            external_conflicts == 0,
            "remote snapshot contains ids or paths owned by another virtual folder"
        );

        // SQLite's historical schema has a full UNIQUE(path) constraint rather than PostgreSQL's
        // partial visible-path index. Moving hidden rows to an internal stable path preserves the
        // row and all dependent state while allowing path reuse and atomic path swaps.
        let tombstone_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncTombstone,
            DatabasePoolRole::Api,
        );
        let tombstone_result = sqlx::query(
            r#"
            UPDATE media_items AS current
            SET path = CASE
                    WHEN current.missing_since IS NULL
                    THEN 'jellyrin-tombstone://sqlite/' || current.id
                    ELSE current.path
                END,
                missing_since = COALESCE(current.missing_since, ?1),
                updated_at = CASE
                    WHEN current.missing_since IS NULL THEN ?1
                    ELSE current.updated_at
                END
            WHERE current.virtual_folder_id = ?2
              AND (
                    NOT EXISTS (
                        SELECT 1
                        FROM jellyrin_remote_snapshot_stage AS staged
                        WHERE staged.id = current.id
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM jellyrin_remote_snapshot_stage AS staged
                        WHERE staged.path = current.path AND staged.id <> current.id
                    )
              )
            "#,
        )
        .bind(&now)
        .bind(&folder_id)
        .execute(&mut **tx)
        .await
        .map_err(anyhow::Error::from);
        tombstone_observation.finish_result(&tombstone_result, |result| result.rows_affected());
        tombstone_result?;

        let merge_observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogSyncMerge, DatabasePoolRole::Api);
        let merge_result: anyhow::Result<u64> = async {
            let mut merged_rows = 0u64;
            for item in &items {
                let result = sqlx::query(
                    r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    created_at, updated_at, last_seen_at, missing_since, file_size, modified_at,
                    runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7, NULL, NULL, NULL,
                    ?8, ?9, ?10, ?11, ?12, ?13)
                ON CONFLICT(id) DO UPDATE SET
                    virtual_folder_id = excluded.virtual_folder_id,
                    name = excluded.name,
                    path = excluded.path,
                    media_type = excluded.media_type,
                    collection_type = excluded.collection_type,
                    updated_at = excluded.updated_at,
                    last_seen_at = excluded.last_seen_at,
                    missing_since = NULL,
                    file_size = NULL,
                    modified_at = NULL,
                    runtime_ticks = excluded.runtime_ticks,
                    bitrate = excluded.bitrate,
                    width = excluded.width,
                    height = excluded.height,
                    media_streams_json = excluded.media_streams_json,
                    metadata_json = CASE
                        WHEN json_extract(media_items.metadata_json, '$.XtreamKind') = 'vod'
                         AND json_extract(media_items.metadata_json, '$.XtreamVodInfo.Status') = 'Complete'
                         AND json_type(excluded.metadata_json, '$.XtreamVodInfo') IS NULL
                        THEN json_patch(media_items.metadata_json, excluded.metadata_json)
                        ELSE excluded.metadata_json
                    END
                WHERE media_items.missing_since IS NOT NULL
                   OR media_items.virtual_folder_id IS NOT excluded.virtual_folder_id
                   OR media_items.name IS NOT excluded.name
                   OR media_items.path IS NOT excluded.path
                   OR media_items.media_type IS NOT excluded.media_type
                   OR media_items.collection_type IS NOT excluded.collection_type
                   OR media_items.runtime_ticks IS NOT excluded.runtime_ticks
                   OR media_items.bitrate IS NOT excluded.bitrate
                   OR media_items.width IS NOT excluded.width
                   OR media_items.height IS NOT excluded.height
                   OR media_items.media_streams_json IS NOT excluded.media_streams_json
                   OR media_items.metadata_json IS NOT CASE
                        WHEN json_extract(media_items.metadata_json, '$.XtreamKind') = 'vod'
                         AND json_extract(media_items.metadata_json, '$.XtreamVodInfo.Status') = 'Complete'
                         AND json_type(excluded.metadata_json, '$.XtreamVodInfo') IS NULL
                        THEN json_patch(media_items.metadata_json, excluded.metadata_json)
                        ELSE excluded.metadata_json
                    END
                    "#,
                )
                .bind(&item.id)
                .bind(&folder_id)
                .bind(&item.name)
                .bind(&item.path)
                .bind(&item.media_type)
                .bind(&item.collection_type)
                .bind(&now)
                .bind(item.runtime_ticks)
                .bind(item.bitrate)
                .bind(item.width)
                .bind(item.height)
                .bind(&item.media_streams_json)
                .bind(&item.metadata_json)
                .execute(&mut **tx)
                .await?;
                merged_rows = merged_rows.saturating_add(result.rows_affected());
            }
            Ok(merged_rows)
        }
        .await;
        merge_observation.finish_result(&merge_result, |rows| *rows);
        merge_result?;

        for item in &items {
            let metadata = serde_json::from_str::<Value>(&item.metadata_json)
                .with_context(|| format!("invalid metadata JSON for media item {}", item.id))?;
            Self::replace_media_item_facets_in_transaction(tx, &item.id, &metadata).await?;
            replace_sqlite_media_item_query_filter_projection_from_live(tx, &item.id).await?;
        }

        Self::rebuild_tv_series_catalog_projection_in_transaction(tx, &folder_id).await?;

        sqlx::query(
            r#"
            UPDATE catalog_sync_runs
            SET status = 'completed', completed_at = ?1
            WHERE id = ?2
            "#,
        )
        .bind(&now)
        .bind(&sync_run_id)
        .execute(&mut **tx)
        .await?;

        let row = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            WHERE id = ?1
            "#,
        )
        .bind(folder_id)
        .fetch_one(&mut **tx)
        .await?;
        row.try_into()
    }

    pub async fn media_item_by_id(&self, item_id: Uuid) -> anyhow::Result<MediaItem> {
        let item_id = self.media_item_storage_id(item_id).await?;
        let row = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams_json,
                   created_at, updated_at
            FROM media_items
            WHERE id = ?1 AND missing_since IS NULL
            "#,
        )
        .bind(item_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn media_item_exists(&self, item_id: Uuid) -> anyhow::Result<bool> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogItemExists, DatabasePoolRole::Api);
        let result = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM media_items
                WHERE id IN (?1, ?2) AND missing_since IS NULL
            )
            "#,
        )
        .bind(item_id.simple().to_string())
        .bind(item_id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(anyhow::Error::from);
        observation.finish_result(&result, |exists| u64::from(*exists));
        result
    }

    pub async fn media_item_by_id_visible(
        &self,
        item_id: Uuid,
    ) -> anyhow::Result<Option<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogItemById, DatabasePoolRole::Api);
        let result = async {
            let row = sqlx::query_as::<_, MediaItemRow>(
                r#"
                SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                       file_size, runtime_ticks, bitrate, width, height, media_streams_json,
                       created_at, updated_at
                FROM media_items
                WHERE id IN (?1, ?2) AND missing_since IS NULL
                ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END
                LIMIT 1
                "#,
            )
            .bind(item_id.simple().to_string())
            .bind(item_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
            row.map(TryInto::try_into).transpose()
        }
        .await;
        observation.finish_result(&result, |item| u64::from(item.is_some()));
        result
    }

    pub async fn delete_media_items(
        &self,
        item_ids: Vec<Uuid>,
        deleted_by_user_id: Option<Uuid>,
    ) -> anyhow::Result<u64> {
        let ids = dedupe_uuids(item_ids)
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(0);
        }
        let visible_items = self.visible_media_item_paths_by_ids(&ids).await?;
        if visible_items.is_empty() {
            return Ok(0);
        }
        let visible_ids = visible_items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let now = format_time(OffsetDateTime::now_utc())?;
        for item in &visible_items {
            sqlx::query(
                r#"
                INSERT INTO media_item_deletions (path, item_id, deleted_by_user_id, deleted_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(path) DO UPDATE SET
                    item_id = excluded.item_id,
                    deleted_by_user_id = excluded.deleted_by_user_id,
                    deleted_at = excluded.deleted_at
                "#,
            )
            .bind(&item.path)
            .bind(&item.id)
            .bind(deleted_by_user_id.map(|id| id.to_string()))
            .bind(&now)
            .execute(&self.pool)
            .await?;
        }

        self.delete_from_item_ref_table("active_playback_sessions", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("active_viewing_sessions", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("transcode_sessions", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("playback_states", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("media_list_items", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("media_item_lyrics", "item_id", &visible_ids)
            .await?;
        self.delete_from_item_ref_table("trickplay_infos", "item_id", &visible_ids)
            .await?;
        self.delete_media_item_versions_for_items(&visible_ids)
            .await?;

        let mut query = QueryBuilder::<Sqlite>::new("UPDATE media_items SET missing_since = ");
        query
            .push_bind(&now)
            .push(", updated_at = ")
            .push_bind(&now)
            .push(" WHERE missing_since IS NULL AND id IN (");
        let mut separated = query.separated(", ");
        for id in &visible_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        let result = query.build().execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    pub async fn media_item_versions(&self, item_id: Uuid) -> anyhow::Result<Vec<MediaItem>> {
        let item_id = item_id.to_string();
        let links = sqlx::query_as::<_, MediaItemVersionRow>(
            r#"
            SELECT primary_item_id, alternate_item_id
            FROM media_item_versions
            WHERE primary_item_id = ?1 OR alternate_item_id = ?1
            "#,
        )
        .bind(&item_id)
        .fetch_all(&self.pool)
        .await?;

        if links.is_empty() {
            return Ok(Vec::new());
        }

        let mut ids = HashSet::new();
        for link in links {
            ids.insert(link.primary_item_id);
            ids.insert(link.alternate_item_id);
        }
        ids.remove(&item_id);
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, virtual_folder_id, name, path, media_type, collection_type, \
             file_size, runtime_ticks, bitrate, width, height, media_streams_json, \
             created_at, updated_at \
             FROM media_items \
             WHERE missing_since IS NULL AND id IN (",
        );
        let mut separated = query.separated(", ");
        for id in ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(") ORDER BY name COLLATE NOCASE");
        let rows = query
            .build_query_as::<MediaItemRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn merge_media_item_versions(
        &self,
        primary_item_id: Uuid,
        alternate_item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        let primary_item_id = primary_item_id.to_string();
        let mut ids = Vec::new();
        ids.push(primary_item_id.clone());
        ids.extend(
            alternate_item_ids
                .into_iter()
                .map(|id| id.to_string())
                .filter(|id| id != &primary_item_id),
        );
        ids.sort();
        ids.dedup();

        let mut tx = self.pool.begin().await?;
        for id in &ids {
            sqlx::query(
                r#"
                DELETE FROM media_item_versions
                WHERE primary_item_id = ?1 OR alternate_item_id = ?1
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }

        let now = format_time(OffsetDateTime::now_utc())?;
        for alternate_id in ids.iter().filter(|id| *id != &primary_item_id) {
            sqlx::query(
                r#"
                INSERT INTO media_item_versions (primary_item_id, alternate_item_id, created_at)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(primary_item_id, alternate_item_id) DO NOTHING
                "#,
            )
            .bind(&primary_item_id)
            .bind(alternate_id)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn clear_media_item_versions(&self, item_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM media_item_versions
            WHERE primary_item_id = ?1 OR alternate_item_id = ?1
            "#,
        )
        .bind(item_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
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

    pub async fn latest_media_items_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<MediaItem>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, virtual_folder_id, name, path, media_type, collection_type, \
             file_size, runtime_ticks, bitrate, width, height, media_streams_json, \
             created_at, updated_at \
             FROM media_items \
             WHERE missing_since IS NULL AND virtual_folder_id IN (",
        );
        let mut separated = query.separated(", ");
        for id in folder_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(") ORDER BY updated_at DESC, name COLLATE NOCASE LIMIT ");
        query.push_bind(limit.max(0));

        let rows = query
            .build_query_as::<MediaItemRow>()
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
        let item_id = self.media_item_storage_id(item_id).await?;
        let media_streams_json = serde_json::to_string(&media_streams)?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET runtime_ticks = ?2, bitrate = ?3, width = ?4, height = ?5, media_streams_json = ?6
            WHERE id = ?1
        "#,
        )
        .bind(&item_id)
        .bind(runtime_ticks)
        .bind(bitrate)
        .bind(width)
        .bind(height)
        .bind(media_streams_json)
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        replace_sqlite_media_item_query_filter_projection_from_live(&mut tx, &item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_media_item_media_info_and_metadata(
        &self,
        item_id: Uuid,
        runtime_ticks: Option<i64>,
        bitrate: Option<i64>,
        width: Option<i32>,
        height: Option<i32>,
        media_streams: Vec<Value>,
        metadata: Value,
    ) -> anyhow::Result<()> {
        let item_id = self.media_item_storage_id(item_id).await?;
        let media_streams_json = serde_json::to_string(&media_streams)?;
        let metadata_json = serde_json::to_string(&metadata)?;
        let mut tx = self.pool.begin().await?;
        let media_info_changed = sqlx::query(
            r#"
            UPDATE media_items
            SET runtime_ticks = ?2, bitrate = ?3, width = ?4, height = ?5,
                media_streams_json = ?6
            WHERE id = ?1
              AND (runtime_ticks IS NOT ?2 OR bitrate IS NOT ?3 OR width IS NOT ?4
                   OR height IS NOT ?5 OR media_streams_json IS NOT ?6)
            "#,
        )
        .bind(&item_id)
        .bind(runtime_ticks)
        .bind(bitrate)
        .bind(width)
        .bind(height)
        .bind(&media_streams_json)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        let metadata_changed = sqlx::query(
            r#"
            UPDATE media_items
            SET metadata_json = ?2, updated_at = ?3
            WHERE id = ?1 AND metadata_json IS NOT ?2
            "#,
        )
        .bind(&item_id)
        .bind(&metadata_json)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !media_info_changed && !metadata_changed {
            tx.commit().await?;
            return Ok(());
        }
        if metadata_changed {
            Self::replace_media_item_facets_in_transaction(&mut tx, &item_id, &metadata).await?;
        }
        replace_sqlite_media_item_query_filter_projection_from_live(&mut tx, &item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_media_item_metadata(
        &self,
        item_id: Uuid,
        metadata: Value,
    ) -> anyhow::Result<()> {
        let item_id = self.media_item_storage_id(item_id).await?;
        let metadata_json = serde_json::to_string(&metadata)?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET metadata_json = ?2, updated_at = ?3
            WHERE id = ?1
        "#,
        )
        .bind(&item_id)
        .bind(metadata_json)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        Self::replace_media_item_facets_in_transaction(&mut tx, &item_id, &metadata).await?;
        replace_sqlite_media_item_query_filter_projection_from_live(&mut tx, &item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Update a media item's metadata_json by string ID (used for image tag population).
    pub async fn update_media_item_metadata_json(
        &self,
        item_id: &str,
        metadata: &Value,
    ) -> anyhow::Result<()> {
        let metadata_json = serde_json::to_string(metadata)?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET metadata_json = ?2, updated_at = ?3
            WHERE id = ?1
            "#,
        )
        .bind(item_id)
        .bind(metadata_json)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        Self::replace_media_item_facets_in_transaction(&mut tx, item_id, metadata).await?;
        replace_sqlite_media_item_query_filter_projection_from_live(&mut tx, item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Get media items that don't have a PrimaryImageTag set yet.
    pub async fn media_items_without_primary_image_tag(
        &self,
    ) -> anyhow::Result<Vec<MediaItemForImageTag>> {
        Ok(sqlx::query_as::<_, MediaItemForImageTagRow>(
            r#"
            SELECT id, path, metadata_json
            FROM media_items
            WHERE missing_since IS NULL
              AND media_type IN ('Video', 'Audio', 'Photo', 'Book')
            "#,
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| row.into())
        .collect())
    }

    pub async fn media_item_metadata(&self) -> anyhow::Result<Vec<MediaItemMetadata>> {
        let rows = sqlx::query_as::<_, MediaItemMetadataRow>(
            r#"
            SELECT id, metadata_json
            FROM media_items
            WHERE missing_since IS NULL
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_item_metadata_by_item_ids(
        &self,
        item_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<Vec<MediaItemMetadata>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogMetadataByIds,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_item_metadata_by_item_ids_unobserved(item_ids)
            .await;
        observation.finish_result(&result, |metadata| {
            u64::try_from(metadata.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_metadata_by_item_ids_unobserved(
        &self,
        item_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<Vec<MediaItemMetadata>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let item_ids = item_ids
            .iter()
            .flat_map(|item_id| [item_id.simple().to_string(), item_id.to_string()])
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut metadata = Vec::new();
        for chunk in item_ids.chunks(500) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT id, metadata_json FROM media_items WHERE missing_since IS NULL AND id IN (",
            );
            let mut separated = query.separated(", ");
            for item_id in chunk {
                separated.push_bind(item_id);
            }
            separated.push_unseparated(")");
            let rows = query
                .build_query_as::<MediaItemMetadataRow>()
                .fetch_all(&self.pool)
                .await?;
            metadata.extend(
                rows.into_iter()
                    .map(TryInto::try_into)
                    .collect::<anyhow::Result<Vec<_>>>()?,
            );
        }

        Ok(metadata)
    }

    pub async fn create_media_list(
        &self,
        kind: &str,
        name: &str,
        collection_type: Option<&str>,
        owner_user_id: Option<Uuid>,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<MediaList> {
        let kind = kind.trim();
        let name = name.trim();
        anyhow::ensure!(!kind.is_empty(), "media list kind must not be empty");
        anyhow::ensure!(!name.is_empty(), "media list name must not be empty");
        let id = Uuid::new_v4();
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO media_lists (
                id, kind, name, collection_type, owner_user_id, metadata_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, ?6)
            "#,
        )
        .bind(id.to_string())
        .bind(kind)
        .bind(name)
        .bind(collection_type)
        .bind(owner_user_id.map(|id| id.to_string()))
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.add_media_list_items(id, item_ids).await?;
        self.media_list_by_id(id).await
    }

    pub async fn media_lists(&self, kind: &str) -> anyhow::Result<Vec<MediaList>> {
        let rows = sqlx::query_as::<_, MediaListRow>(
            r#"
            SELECT id, kind, name, collection_type, owner_user_id, metadata_json, created_at, updated_at
            FROM media_lists
            WHERE kind = ?1
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_list_by_id(&self, list_id: Uuid) -> anyhow::Result<MediaList> {
        let row = sqlx::query_as::<_, MediaListRow>(
            r#"
            SELECT id, kind, name, collection_type, owner_user_id, metadata_json, created_at, updated_at
            FROM media_lists
            WHERE id = ?1
            "#,
        )
        .bind(list_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn media_list_item_counts(
        &self,
        list_ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        if list_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT list_item.list_id, COUNT(*) AS item_count \
             FROM media_list_items AS list_item \
             INNER JOIN media_items AS item ON item.id = list_item.item_id \
             WHERE item.missing_since IS NULL AND list_item.list_id IN (",
        );
        let mut separated = query.separated(", ");
        for list_id in list_ids {
            separated.push_bind(list_id.to_string());
        }
        separated.push_unseparated(") GROUP BY list_item.list_id");

        let rows = query.build().fetch_all(&self.pool).await?;
        let mut counts = HashMap::with_capacity(rows.len());
        for row in rows {
            let list_id: String = row.try_get("list_id")?;
            let item_count: i64 = row.try_get("item_count")?;
            counts.insert(Uuid::parse_str(&list_id)?, item_count.max(0) as usize);
        }
        Ok(counts)
    }

    pub async fn media_list_ids_with_user_permission(
        &self,
        user_id: Uuid,
        list_ids: &[Uuid],
    ) -> anyhow::Result<HashSet<Uuid>> {
        if list_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT list_id FROM media_list_user_permissions \
             WHERE user_id = ",
        );
        query
            .push_bind(user_id.to_string())
            .push(" AND list_id IN (");
        let mut separated = query.separated(", ");
        for list_id in list_ids {
            separated.push_bind(list_id.to_string());
        }
        separated.push_unseparated(")");
        let rows = query.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let list_id: String = row.try_get("list_id")?;
                Ok(Uuid::parse_str(&list_id)?)
            })
            .collect()
    }

    pub async fn update_media_list_name(
        &self,
        list_id: Uuid,
        name: &str,
    ) -> anyhow::Result<MediaList> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "media list name must not be empty");
        sqlx::query(
            r#"
            UPDATE media_lists
            SET name = ?2, updated_at = ?3
            WHERE id = ?1
            "#,
        )
        .bind(list_id.to_string())
        .bind(name)
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&self.pool)
        .await?;

        self.media_list_by_id(list_id).await
    }

    pub async fn add_media_list_items(
        &self,
        list_id: Uuid,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        self.media_list_by_id(list_id).await?;
        let mut position = self.next_media_list_position(list_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        for item_id in dedupe_uuids(item_ids) {
            self.media_item_by_id(item_id).await?;
            sqlx::query(
                r#"
                INSERT INTO media_list_items (
                    list_id, item_id, playlist_item_id, position, added_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(list_id, item_id) DO NOTHING
                "#,
            )
            .bind(list_id.to_string())
            .bind(item_id.to_string())
            .bind(Uuid::new_v4().to_string())
            .bind(position)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            position += 1;
        }
        self.touch_media_list(list_id).await
    }

    pub async fn remove_media_list_items(
        &self,
        list_id: Uuid,
        item_ids: Vec<Uuid>,
        playlist_item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        self.media_list_by_id(list_id).await?;
        for item_id in dedupe_uuids(item_ids) {
            sqlx::query("DELETE FROM media_list_items WHERE list_id = ?1 AND item_id = ?2")
                .bind(list_id.to_string())
                .bind(item_id.to_string())
                .execute(&self.pool)
                .await?;
        }
        for playlist_item_id in dedupe_uuids(playlist_item_ids) {
            sqlx::query(
                "DELETE FROM media_list_items WHERE list_id = ?1 AND playlist_item_id = ?2",
            )
            .bind(list_id.to_string())
            .bind(playlist_item_id.to_string())
            .execute(&self.pool)
            .await?;
        }
        self.reindex_media_list(list_id).await?;
        self.touch_media_list(list_id).await
    }

    pub async fn move_media_list_item(
        &self,
        list_id: Uuid,
        target_id: Uuid,
        new_index: i64,
    ) -> anyhow::Result<()> {
        let mut rows = self.media_list_item_ids(list_id).await?;
        let Some(current_index) = rows
            .iter()
            .position(|row| row.0 == target_id || row.1 == target_id)
        else {
            anyhow::bail!("media list item not found");
        };
        let row = rows.remove(current_index);
        let target = new_index.max(0).min(rows.len() as i64) as usize;
        rows.insert(target, row);
        self.update_media_list_positions(list_id, rows).await?;
        self.touch_media_list(list_id).await
    }

    pub async fn media_list_items(&self, list_id: Uuid) -> anyhow::Result<Vec<MediaListItem>> {
        self.media_list_by_id(list_id).await?;
        let rows = sqlx::query_as::<_, MediaListItemRow>(
            r#"
            SELECT media_list_items.playlist_item_id,
                   media_list_items.position,
                   media_list_items.added_at,
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
            FROM media_list_items
            INNER JOIN media_items ON media_items.id = media_list_items.item_id
            WHERE media_list_items.list_id = ?1
              AND media_items.missing_since IS NULL
            ORDER BY media_list_items.position ASC, media_items.name COLLATE NOCASE
            "#,
        )
        .bind(list_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_list_user_permissions(
        &self,
        list_id: Uuid,
    ) -> anyhow::Result<Vec<MediaListUserPermission>> {
        self.media_list_by_id(list_id).await?;
        let rows = sqlx::query_as::<_, MediaListUserPermissionRow>(
            r#"
            SELECT media_list_user_permissions.list_id,
                   media_list_user_permissions.can_edit,
                   media_list_user_permissions.created_at AS permission_created_at,
                   media_list_user_permissions.updated_at AS permission_updated_at,
                   users.id,
                   users.name,
                   users.is_administrator,
                   users.is_disabled,
                   users.sync_play_access,
                   users.created_at,
                   users.updated_at
            FROM media_list_user_permissions
            INNER JOIN users ON users.id = media_list_user_permissions.user_id
            WHERE media_list_user_permissions.list_id = ?1
            ORDER BY users.name COLLATE NOCASE
            "#,
        )
        .bind(list_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<MediaListUserPermission>> {
        let row = sqlx::query_as::<_, MediaListUserPermissionRow>(
            r#"
            SELECT media_list_user_permissions.list_id,
                   media_list_user_permissions.can_edit,
                   media_list_user_permissions.created_at AS permission_created_at,
                   media_list_user_permissions.updated_at AS permission_updated_at,
                   users.id,
                   users.name,
                   users.is_administrator,
                   users.is_disabled,
                   users.sync_play_access,
                   users.created_at,
                   users.updated_at
            FROM media_list_user_permissions
            INNER JOIN users ON users.id = media_list_user_permissions.user_id
            WHERE media_list_user_permissions.list_id = ?1
              AND media_list_user_permissions.user_id = ?2
            "#,
        )
        .bind(list_id.to_string())
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn upsert_media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
        can_edit: bool,
    ) -> anyhow::Result<()> {
        self.media_list_by_id(list_id).await?;
        self.user_by_id(user_id).await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO media_list_user_permissions (
                list_id, user_id, can_edit, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?4)
            ON CONFLICT(list_id, user_id) DO UPDATE SET
                can_edit = excluded.can_edit,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(list_id.to_string())
        .bind(user_id.to_string())
        .bind(can_edit)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM media_list_user_permissions WHERE list_id = ?1 AND user_id = ?2")
            .bind(list_id.to_string())
            .bind(user_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn next_media_list_position(&self, list_id: Uuid) -> anyhow::Result<i64> {
        let max_position: Option<i64> =
            sqlx::query_scalar("SELECT MAX(position) FROM media_list_items WHERE list_id = ?1")
                .bind(list_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        Ok(max_position.map_or(0, |position| position + 1))
    }

    async fn media_list_item_ids(&self, list_id: Uuid) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
        let rows = sqlx::query_as::<_, MediaListItemIdRow>(
            r#"
            SELECT item_id, playlist_item_id
            FROM media_list_items
            WHERE list_id = ?1
            ORDER BY position ASC
            "#,
        )
        .bind(list_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn reindex_media_list(&self, list_id: Uuid) -> anyhow::Result<()> {
        let rows = self.media_list_item_ids(list_id).await?;
        self.update_media_list_positions(list_id, rows).await
    }

    async fn update_media_list_positions(
        &self,
        list_id: Uuid,
        rows: Vec<(Uuid, Uuid)>,
    ) -> anyhow::Result<()> {
        for (position, (item_id, playlist_item_id)) in rows.into_iter().enumerate() {
            sqlx::query(
                r#"
                UPDATE media_list_items
                SET position = ?3
                WHERE list_id = ?1 AND item_id = ?2 AND playlist_item_id = ?4
                "#,
            )
            .bind(list_id.to_string())
            .bind(item_id.to_string())
            .bind(position as i64)
            .bind(playlist_item_id.to_string())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn touch_media_list(&self, list_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE media_lists SET updated_at = ?2 WHERE id = ?1")
            .bind(list_id.to_string())
            .bind(format_time(OffsetDateTime::now_utc())?)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_playback_state(&self, playback: UpsertPlaybackState) -> anyhow::Result<()> {
        let item_id = self.media_item_storage_id(playback.item_id).await?;
        let existing_user_item_data =
            if playback.audio_stream_index.is_none() || playback.subtitle_stream_index.is_none() {
                sqlx::query_as::<_, (Option<i64>, Option<i64>, bool, Option<f64>)>(
                    r#"
                    SELECT audio_stream_index, subtitle_stream_index, is_favorite, rating
                    FROM playback_states
                    WHERE user_id = ?1 AND item_id = ?2
                    "#,
                )
                .bind(playback.user_id.to_string())
                .bind(&item_id)
                .fetch_optional(&self.pool)
                .await?
                .map(
                    |(audio_stream_index, subtitle_stream_index, is_favorite, rating)| {
                        ExistingUserItemData {
                            audio_stream_index,
                            subtitle_stream_index,
                            is_favorite,
                            rating,
                        }
                    },
                )
                .unwrap_or_default()
            } else {
                self.existing_user_item_data(playback.user_id, playback.item_id)
                    .await?
            };
        let audio_stream_index = playback
            .audio_stream_index
            .or(existing_user_item_data.audio_stream_index);
        let subtitle_stream_index = playback
            .subtitle_stream_index
            .or(existing_user_item_data.subtitle_stream_index);
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                position_ticks, is_paused, played, is_favorite, rating, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                media_source_id = excluded.media_source_id,
                audio_stream_index = excluded.audio_stream_index,
                subtitle_stream_index = excluded.subtitle_stream_index,
                position_ticks = excluded.position_ticks,
                is_paused = excluded.is_paused,
                played = excluded.played,
                is_favorite = excluded.is_favorite,
                rating = excluded.rating,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(playback.user_id.to_string())
        .bind(item_id)
        .bind(playback.media_source_id)
        .bind(audio_stream_index)
        .bind(subtitle_stream_index)
        .bind(playback.position_ticks.max(0))
        .bind(playback.is_paused)
        .bind(playback.played)
        .bind(existing_user_item_data.is_favorite)
        .bind(existing_user_item_data.rating)
        .bind(now)
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
        let item_id = self.media_item_storage_id(item_id).await?;
        let existing = self
            .existing_user_item_data_by_storage_id(user_id, &item_id)
            .await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                position_ticks, is_paused, played, is_favorite, rating, updated_at
            )
            VALUES (?1, ?2, NULL, ?3, ?4, 0, 0, 0, ?5, ?6, ?7)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                is_favorite = excluded.is_favorite,
                updated_at = excluded.updated_at
        "#,
        )
        .bind(user_id.to_string())
        .bind(item_id)
        .bind(existing.audio_stream_index)
        .bind(existing.subtitle_stream_index)
        .bind(is_favorite)
        .bind(existing.rating)
        .bind(now)
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
        let item_id = self.media_item_storage_id(item_id).await?;
        let existing = self
            .existing_user_item_data_by_storage_id(user_id, &item_id)
            .await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            INSERT INTO playback_states (
                user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                position_ticks, is_paused, played, is_favorite, rating, updated_at
            )
            VALUES (?1, ?2, NULL, ?3, ?4, 0, 0, 0, ?5, ?6, ?7)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                rating = excluded.rating,
                updated_at = excluded.updated_at
        "#,
        )
        .bind(user_id.to_string())
        .bind(item_id)
        .bind(existing.audio_stream_index)
        .bind(existing.subtitle_stream_index)
        .bind(existing.is_favorite)
        .bind(rating)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn existing_user_item_data(
        &self,
        user_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<ExistingUserItemData> {
        let item_id = self.media_item_storage_id(item_id).await?;
        self.existing_user_item_data_by_storage_id(user_id, &item_id)
            .await
    }

    async fn existing_user_item_data_by_storage_id(
        &self,
        user_id: Uuid,
        item_id: &str,
    ) -> anyhow::Result<ExistingUserItemData> {
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, bool, Option<f64>)>(
            r#"
            SELECT audio_stream_index, subtitle_stream_index, is_favorite, rating
            FROM playback_states
            WHERE user_id = ?1 AND item_id = ?2
        "#,
        )
        .bind(user_id.to_string())
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(
                |(audio_stream_index, subtitle_stream_index, is_favorite, rating)| {
                    ExistingUserItemData {
                        audio_stream_index,
                        subtitle_stream_index,
                        is_favorite,
                        rating,
                    }
                },
            )
            .unwrap_or_default())
    }

    pub async fn playback_state_for_item(
        &self,
        user_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<Option<PlaybackState>> {
        let item_id = self.media_item_storage_id(item_id).await?;
        let row = sqlx::query_as::<_, PlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                   position_ticks, is_paused, played, is_favorite, rating, updated_at
            FROM playback_states
            WHERE user_id = ?1 AND item_id = ?2
        "#,
        )
        .bind(user_id.to_string())
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    /// Fetches user data for a set of catalog items without issuing one query per item.
    pub async fn playback_states_for_items(
        &self,
        user_id: Uuid,
        item_ids: &[Uuid],
    ) -> anyhow::Result<Vec<PlaybackState>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let storage_ids = item_ids
            .iter()
            .flat_map(|item_id| [item_id.to_string(), item_id.simple().to_string()])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut states = Vec::new();
        // Leave ample room below SQLite's conservative 999-variable limit for the user id.
        for chunk in storage_ids.chunks(400) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT user_id, item_id, media_source_id, audio_stream_index, \
                 subtitle_stream_index, position_ticks, is_paused, played, \
                 is_favorite, rating, updated_at \
                 FROM playback_states WHERE user_id = ",
            );
            query
                .push_bind(user_id.to_string())
                .push(" AND item_id IN (");
            let mut separated = query.separated(", ");
            for item_id in chunk {
                separated.push_bind(item_id);
            }
            separated.push_unseparated(")");
            let rows = query
                .build_query_as::<PlaybackStateRow>()
                .fetch_all(&self.pool)
                .await?;
            states.extend(
                rows.into_iter()
                    .map(TryInto::try_into)
                    .collect::<anyhow::Result<Vec<_>>>()?,
            );
        }
        Ok(states)
    }

    pub async fn playback_states_for_user(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<PlaybackState>> {
        let rows = sqlx::query_as::<_, PlaybackStateRow>(
            r#"
            SELECT user_id, item_id, media_source_id, audio_stream_index, subtitle_stream_index,
                   position_ticks, is_paused, played, is_favorite, rating, updated_at
            FROM playback_states
            WHERE user_id = ?1
        "#,
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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
                playback_states.is_favorite, playback_states.rating,
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

    /// Returns one policy-filtered resume page without materializing every playback row.
    ///
    /// Count and page share a SQLite read transaction so `TotalRecordCount` describes the same
    /// snapshot as the returned items.
    pub async fn resume_items_page_for_user(
        &self,
        user_id: Uuid,
        query: ResumeItemsPageQuery,
    ) -> anyhow::Result<ResumeItemsPage> {
        let mut transaction = self.pool.begin().await?;
        let total_record_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM playback_states
            INNER JOIN media_items ON media_items.id = playback_states.item_id
            WHERE playback_states.user_id = ?1
              AND media_items.missing_since IS NULL
              AND playback_states.position_ticks > 0
              AND playback_states.played = 0
              AND (
                    media_items.runtime_ticks IS NULL
                 OR media_items.runtime_ticks <= 0
                 OR (
                        media_items.runtime_ticks >= ?2
                    AND playback_states.position_ticks * 100.0
                          / media_items.runtime_ticks >= ?3
                    AND playback_states.position_ticks * 100.0
                          / media_items.runtime_ticks < ?4
                 )
              )
            "#,
        )
        .bind(user_id.to_string())
        .bind(query.min_duration_ticks.max(0))
        .bind(query.min_pct.clamp(0, 100))
        .bind(query.max_pct.clamp(query.min_pct.clamp(0, 100), 100))
        .fetch_one(&mut *transaction)
        .await?;

        let rows = sqlx::query_as::<_, ResumeItemRow>(
            r#"
            SELECT
                media_items.id, media_items.virtual_folder_id, media_items.name, media_items.path,
                media_items.media_type, media_items.collection_type, media_items.file_size,
                media_items.runtime_ticks, media_items.bitrate, media_items.width, media_items.height,
                media_items.media_streams_json, media_items.created_at, media_items.updated_at,
                playback_states.user_id, playback_states.item_id, playback_states.media_source_id,
                playback_states.audio_stream_index, playback_states.subtitle_stream_index,
                playback_states.position_ticks, playback_states.is_paused, playback_states.played,
                playback_states.is_favorite, playback_states.rating,
                playback_states.updated_at AS playback_updated_at
            FROM playback_states
            INNER JOIN media_items ON media_items.id = playback_states.item_id
            WHERE playback_states.user_id = ?1
              AND media_items.missing_since IS NULL
              AND playback_states.position_ticks > 0
              AND playback_states.played = 0
              AND (
                    media_items.runtime_ticks IS NULL
                 OR media_items.runtime_ticks <= 0
                 OR (
                        media_items.runtime_ticks >= ?2
                    AND playback_states.position_ticks * 100.0
                          / media_items.runtime_ticks >= ?3
                    AND playback_states.position_ticks * 100.0
                          / media_items.runtime_ticks < ?4
                 )
              )
            ORDER BY playback_states.updated_at DESC
            LIMIT ?5 OFFSET ?6
            "#,
        )
        .bind(user_id.to_string())
        .bind(query.min_duration_ticks.max(0))
        .bind(query.min_pct.clamp(0, 100))
        .bind(query.max_pct.clamp(query.min_pct.clamp(0, 100), 100))
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
                let path_string = path.to_string_lossy().to_string();
                if self.media_item_path_is_deleted(&path_string).await? {
                    continue;
                }
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

                found_paths.insert(path_string);
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
        let mut media_info = probe_media_info(Path::new(&path), media_type).await;
        if let Some(nfo_metadata) = read_local_nfo_metadata(Path::new(&path)).await {
            media_info.metadata = merge_metadata_values(media_info.metadata, nfo_metadata);
        }
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
            let mut tx = self.pool.begin().await?;
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
            .bind(&missing_id)
            .execute(&mut *tx)
            .await?;
            replace_sqlite_media_item_query_filter_projection_from_live(&mut tx, &missing_id)
                .await?;
            tx.commit().await?;
            return Ok(());
        }

        let existing_id = exact_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut base_tx = self.pool.begin().await?;
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
        .bind(&existing_id)
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
        .execute(&mut *base_tx)
        .await?;
        replace_sqlite_media_item_query_filter_projection_from_live(&mut base_tx, &existing_id)
            .await?;
        base_tx.commit().await?;

        if media_info
            .metadata
            .as_object()
            .is_some_and(|metadata| !metadata.is_empty())
        {
            self.merge_scanned_media_item_metadata(&existing_id, media_info.metadata)
                .await?;
        }

        Ok(())
    }

    async fn merge_scanned_media_item_metadata(
        &self,
        item_id: &str,
        scanned_metadata: Value,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        let current =
            sqlx::query_scalar::<_, String>("SELECT metadata_json FROM media_items WHERE id = ?1")
                .bind(item_id)
                .fetch_optional(&mut *transaction)
                .await?
                .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                .unwrap_or_else(|| json!({}));
        let mut merged = current.as_object().cloned().unwrap_or_default();
        if metadata_lock_data(&merged) {
            replace_sqlite_media_item_query_filter_projection_from_live(&mut transaction, item_id)
                .await?;
            transaction.commit().await?;
            return Ok(());
        }
        let locked_fields = metadata_locked_fields(&merged);
        if let Some(scanned) = scanned_metadata.as_object() {
            for (key, value) in scanned {
                if !locked_fields.contains(&metadata_lock_key(key)) {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
        let metadata = Value::Object(merged);
        let metadata_json = serde_json::to_string(&metadata)?;
        sqlx::query("UPDATE media_items SET metadata_json = ?2 WHERE id = ?1")
            .bind(item_id)
            .bind(metadata_json)
            .execute(&mut *transaction)
            .await?;
        replace_sqlite_media_item_facets(&mut transaction, item_id, &metadata).await?;
        replace_sqlite_media_item_query_filter_projection_from_live(&mut transaction, item_id)
            .await?;
        transaction.commit().await?;
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
            sync_play_access: DEFAULT_SYNC_PLAY_ACCESS.to_string(),
            created_at: now,
            updated_at: now,
        };

        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(user.id.to_string())
        .bind(&user.name)
        .bind(user.is_administrator)
        .bind(user.is_disabled)
        .bind(&user.sync_play_access)
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
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
            FROM users
            WHERE name = ?1
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn user_by_id(&self, user_id: Uuid) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
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
            SELECT id, name, overview, short_overview, entry_type, severity, user_id, item_id, created_at
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

    async fn visible_media_item_paths_by_ids(
        &self,
        item_ids: &[String],
    ) -> anyhow::Result<Vec<MediaItemPathRow>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, path FROM media_items WHERE missing_since IS NULL AND id IN (",
        );
        let mut separated = query.separated(", ");
        for id in item_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        query
            .build_query_as::<MediaItemPathRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(Into::into)
    }

    async fn media_item_path_is_deleted(&self, path: &str) -> anyhow::Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_item_deletions WHERE path = ?1)")
                .bind(path)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    async fn delete_from_item_ref_table(
        &self,
        table: &'static str,
        column: &'static str,
        item_ids: &[String],
    ) -> anyhow::Result<u64> {
        if item_ids.is_empty() {
            return Ok(0);
        }

        let mut query =
            QueryBuilder::<Sqlite>::new(format!("DELETE FROM {table} WHERE {column} IN ("));
        let mut separated = query.separated(", ");
        for id in item_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        let result = query.build().execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    async fn delete_media_item_versions_for_items(
        &self,
        item_ids: &[String],
    ) -> anyhow::Result<u64> {
        if item_ids.is_empty() {
            return Ok(0);
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "DELETE FROM media_item_versions WHERE primary_item_id IN (",
        );
        let mut separated = query.separated(", ");
        for id in item_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(") OR alternate_item_id IN (");
        let mut separated = query.separated(", ");
        for id in item_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        let result = query.build().execute(&self.pool).await?;
        Ok(result.rows_affected())
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

    /// Mark a single media item as missing by its path (for incremental scan).
    pub async fn mark_media_item_missing_by_path(&self, path: &str) -> anyhow::Result<bool> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET missing_since = ?1, updated_at = ?1
            WHERE path = ?2 AND missing_since IS NULL
            "#,
        )
        .bind(&now)
        .bind(path)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Incremental scan: upsert a single file (new or modified).
    /// Returns true if the item was created or updated.
    pub async fn scan_single_file(&self, path: &Path) -> anyhow::Result<bool> {
        let path_string = path.to_string_lossy().to_string();
        if self.media_item_path_is_deleted(&path_string).await? {
            return Ok(false);
        }
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
        else {
            return Ok(false);
        };
        let Some(media_type) = media_type_for_path(path) else {
            return Ok(false);
        };
        // Find the virtual folder that contains this path
        let folders = self.virtual_folders().await?;
        for folder in folders {
            for location in &folder.locations {
                if path_string.starts_with(location) {
                    self.upsert_media_item(&folder, &name, path, media_type)
                        .await?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(any(test, feature = "sqlite"))]
async fn configure_sqlite_connection(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(&mut *connection)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
fn push_activity_log_join_and_filters(
    query: &mut QueryBuilder<Sqlite>,
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

    if let Some(item_id) = filter.item_id {
        push_activity_log_where(query, &mut first_filter);
        query.push("activity_log_entries.item_id = ");
        query.push_bind(item_id.to_string());
    }

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

#[cfg(any(test, feature = "sqlite"))]
fn push_activity_log_filter_clause(
    query: &mut QueryBuilder<Sqlite>,
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

#[cfg(any(test, feature = "sqlite"))]
fn push_activity_log_exact_clause(
    query: &mut QueryBuilder<Sqlite>,
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

#[cfg(any(test, feature = "sqlite"))]
fn push_activity_log_where(query: &mut QueryBuilder<Sqlite>, first_filter: &mut bool) {
    if *first_filter {
        query.push(" WHERE ");
        *first_filter = false;
    } else {
        query.push(" AND ");
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn push_activity_log_order_by(
    query: &mut QueryBuilder<Sqlite>,
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

#[cfg(any(test, feature = "sqlite"))]
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

#[cfg(any(test, feature = "sqlite"))]
fn trimmed_filter_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(test, feature = "sqlite"))]
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

#[cfg(any(test, feature = "sqlite"))]
#[derive(sqlx::FromRow)]
struct DisplayPreferencesRow {
    payload_json: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemLyricsRow {
    item_id: String,
    lyrics_json: String,
    updated_at: String,
}

#[cfg(any(test, feature = "sqlite"))]
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
    sync_play_access: String,
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
            sync_play_access: row.sync_play_access,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

#[cfg(any(test, feature = "sqlite"))]
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
struct QuickConnectSessionRow {
    secret: String,
    code: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
    user_id: Option<String>,
    authorized: bool,
    created_at: String,
    updated_at: String,
    expires_at: String,
}

#[derive(sqlx::FromRow)]
struct SystemConfigurationPayloadsRow {
    content_types_json: String,
    metadata_options_json: String,
    path_substitutions_json: String,
    plugin_repositories_json: String,
    server_options_json: String,
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(sqlx::FromRow)]
struct NamedConfigurationRow {
    payload_json: String,
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(sqlx::FromRow)]
struct NamedConfigurationListRow {
    key: String,
    payload_json: String,
}

#[cfg(any(test, feature = "sqlite"))]
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
    restore_snapshot_json: Option<String>,
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
struct MediaItemCatalogRow {
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
    metadata_json: String,
    created_at: String,
    updated_at: String,
    playback_user_id: Option<String>,
    playback_item_id: Option<String>,
    playback_media_source_id: Option<String>,
    playback_audio_stream_index: Option<i64>,
    playback_subtitle_stream_index: Option<i64>,
    playback_position_ticks: Option<i64>,
    playback_is_paused: Option<bool>,
    playback_played: Option<bool>,
    playback_is_favorite: Option<bool>,
    playback_rating: Option<f64>,
    playback_updated_at: Option<String>,
}

#[derive(sqlx::FromRow)]
#[cfg(any(test, feature = "sqlite"))]
struct SqliteCatalogAggregateRow {
    item_count: i64,
    movie_count: i64,
    episode_count: i64,
    song_count: i64,
    music_video_count: i64,
    book_count: i64,
}

#[derive(sqlx::FromRow)]
#[cfg(any(test, feature = "sqlite"))]
struct SqliteCatalogCountProjectionRow {
    name: String,
    path: String,
    item_type: String,
    album: Option<String>,
    album_name: Option<String>,
    artists: Option<String>,
    album_artists: Option<String>,
    remote_trailers: Option<String>,
    trailers: Option<String>,
}

#[cfg(any(test, feature = "sqlite"))]
fn sqlite_nonnegative_catalog_count(value: i64, label: &str) -> anyhow::Result<u64> {
    u64::try_from(value).with_context(|| format!("{label} catalog count was negative"))
}

#[derive(sqlx::FromRow)]
struct MediaItemMetadataRow {
    id: String,
    metadata_json: String,
}

/// Lightweight struct for image tag population.
pub struct MediaItemForImageTag {
    pub id: String,
    pub path: String,
    pub metadata_json: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemForImageTagRow {
    id: String,
    path: String,
    metadata_json: String,
}

impl From<MediaItemForImageTagRow> for MediaItemForImageTag {
    fn from(row: MediaItemForImageTagRow) -> Self {
        Self {
            id: row.id,
            path: row.path,
            metadata_json: row.metadata_json,
        }
    }
}

#[derive(sqlx::FromRow)]
struct MediaListRow {
    id: String,
    kind: String,
    name: String,
    collection_type: Option<String>,
    owner_user_id: Option<String>,
    metadata_json: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MediaListItemRow {
    playlist_item_id: String,
    position: i64,
    added_at: String,
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
struct MediaListItemIdRow {
    item_id: String,
    playlist_item_id: String,
}

#[derive(sqlx::FromRow)]
struct MediaListUserPermissionRow {
    list_id: String,
    can_edit: bool,
    permission_created_at: String,
    permission_updated_at: String,
    id: String,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: String,
    updated_at: String,
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(sqlx::FromRow)]
struct MediaItemIdRow {
    id: String,
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(sqlx::FromRow)]
struct MediaItemVersionRow {
    primary_item_id: String,
    alternate_item_id: String,
}

#[cfg(any(test, feature = "sqlite"))]
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
    is_favorite: bool,
    rating: Option<f64>,
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
    is_favorite: bool,
    rating: Option<f64>,
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
struct ActiveViewingSessionRow {
    session_id: String,
    user_id: String,
    viewing_updated_at: String,
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
struct ActiveSessionUserRow {
    session_id: String,
    user_id: String,
    user_name: String,
    added_at: String,
}

#[derive(sqlx::FromRow)]
struct TranscodeSessionRow {
    play_session_id: String,
    dedupe_key: Option<String>,
    device_id: Option<String>,
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
    start_position_ticks: i64,
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
struct TrickplayInfoRow {
    item_id: String,
    width: i64,
    height: i64,
    tile_width: i64,
    tile_height: i64,
    thumbnail_count: i64,
    interval_ms: i64,
    bandwidth: i64,
    created_at: String,
    updated_at: String,
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
    item_id: Option<String>,
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

/// A NextUp candidate row without `media_streams`; see `tv_next_up_candidate_items`.
#[derive(sqlx::FromRow)]
struct TvNextUpCandidateRow {
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
    created_at: String,
    updated_at: String,
}

impl TryFrom<TvNextUpCandidateRow> for MediaItem {
    type Error = anyhow::Error;

    fn try_from(row: TvNextUpCandidateRow) -> Result<Self, Self::Error> {
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
            media_streams: Vec::new(),
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

impl TryFrom<MediaItemCatalogRow> for MediaItemCatalogEntry {
    type Error = anyhow::Error;

    fn try_from(row: MediaItemCatalogRow) -> Result<Self, Self::Error> {
        let playback_state = if let Some(user_id) = row.playback_user_id.as_deref() {
            Some(PlaybackState {
                user_id: Uuid::parse_str(user_id)
                    .context("invalid catalog playback user id in database")?,
                item_id: Uuid::parse_str(
                    row.playback_item_id
                        .as_deref()
                        .context("catalog playback row is missing item id")?,
                )
                .context("invalid catalog playback item id in database")?,
                media_source_id: row.playback_media_source_id,
                audio_stream_index: row.playback_audio_stream_index,
                subtitle_stream_index: row.playback_subtitle_stream_index,
                position_ticks: row
                    .playback_position_ticks
                    .context("catalog playback row is missing position ticks")?,
                is_paused: row
                    .playback_is_paused
                    .context("catalog playback row is missing paused flag")?,
                played: row
                    .playback_played
                    .context("catalog playback row is missing played flag")?,
                is_favorite: row
                    .playback_is_favorite
                    .context("catalog playback row is missing favorite flag")?,
                rating: row.playback_rating,
                updated_at: parse_time(
                    row.playback_updated_at
                        .as_deref()
                        .context("catalog playback row is missing updated timestamp")?,
                )?,
            })
        } else {
            None
        };
        Ok(Self {
            item: MediaItem {
                id: Uuid::parse_str(&row.id).context("invalid catalog media item id")?,
                virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                    .context("invalid catalog virtual folder id")?,
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
            metadata: serde_json::from_str(&row.metadata_json)
                .context("invalid catalog media item metadata json")?,
            playback_state,
        })
    }
}

impl TryFrom<MediaItemMetadataRow> for MediaItemMetadata {
    type Error = anyhow::Error;

    fn try_from(row: MediaItemMetadataRow) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: Uuid::parse_str(&row.id).context("invalid media item metadata id")?,
            payload: serde_json::from_str(&row.metadata_json)
                .context("invalid media item metadata json")?,
        })
    }
}

impl TryFrom<MediaItemLyricsRow> for MediaItemLyrics {
    type Error = anyhow::Error;

    fn try_from(row: MediaItemLyricsRow) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: Uuid::parse_str(&row.item_id).context("invalid media item lyrics id")?,
            payload: serde_json::from_str(&row.lyrics_json)
                .context("invalid media item lyrics json")?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<MediaListRow> for MediaList {
    type Error = anyhow::Error;

    fn try_from(row: MediaListRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid media list id")?,
            kind: row.kind,
            name: row.name,
            collection_type: row.collection_type,
            owner_user_id: row
                .owner_user_id
                .as_deref()
                .map(Uuid::parse_str)
                .transpose()
                .context("invalid media list owner user id")?,
            metadata: serde_json::from_str(&row.metadata_json)
                .context("invalid media list metadata json")?,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<MediaListItemRow> for MediaListItem {
    type Error = anyhow::Error;

    fn try_from(row: MediaListItemRow) -> Result<Self, Self::Error> {
        Ok(Self {
            item: MediaItem {
                id: Uuid::parse_str(&row.id).context("invalid media list item id")?,
                virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                    .context("invalid media list item virtual folder id")?,
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
            playlist_item_id: Uuid::parse_str(&row.playlist_item_id)
                .context("invalid playlist item id")?,
            position: row.position,
            added_at: parse_time(&row.added_at)?,
        })
    }
}

impl TryFrom<MediaListUserPermissionRow> for MediaListUserPermission {
    type Error = anyhow::Error;

    fn try_from(row: MediaListUserPermissionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            list_id: Uuid::parse_str(&row.list_id)
                .context("invalid media list permission list id")?,
            user: User {
                id: Uuid::parse_str(&row.id).context("invalid media list permission user id")?,
                name: row.name,
                is_administrator: row.is_administrator,
                is_disabled: row.is_disabled,
                sync_play_access: row.sync_play_access,
                created_at: parse_time(&row.created_at)?,
                updated_at: parse_time(&row.updated_at)?,
            },
            can_edit: row.can_edit,
            created_at: parse_time(&row.permission_created_at)?,
            updated_at: parse_time(&row.permission_updated_at)?,
        })
    }
}

impl TryFrom<MediaListItemIdRow> for (Uuid, Uuid) {
    type Error = anyhow::Error;

    fn try_from(row: MediaListItemIdRow) -> Result<Self, Self::Error> {
        Ok((
            Uuid::parse_str(&row.item_id).context("invalid media list item id")?,
            Uuid::parse_str(&row.playlist_item_id).context("invalid playlist item id")?,
        ))
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
            is_favorite: row.is_favorite,
            rating: row.rating,
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
            is_favorite: row.is_favorite,
            rating: row.rating,
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

impl TryFrom<ActiveViewingSessionRow> for ActiveViewingSession {
    type Error = anyhow::Error;

    fn try_from(row: ActiveViewingSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            user_id: Uuid::parse_str(&row.user_id).context("invalid active viewing user id")?,
            item: MediaItem {
                id: Uuid::parse_str(&row.id).context("invalid active viewing item id")?,
                virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                    .context("invalid active viewing virtual folder id")?,
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
            updated_at: parse_time(&row.viewing_updated_at)?,
        })
    }
}

impl TryFrom<ActiveSessionUserRow> for ActiveSessionUser {
    type Error = anyhow::Error;

    fn try_from(row: ActiveSessionUserRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            user_id: Uuid::parse_str(&row.user_id).context("invalid active session user id")?,
            user_name: row.user_name,
            added_at: parse_time(&row.added_at)?,
        })
    }
}

impl TryFrom<TranscodeSessionRow> for TranscodeSession {
    type Error = anyhow::Error;

    fn try_from(row: TranscodeSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            play_session_id: row.play_session_id,
            dedupe_key: row.dedupe_key,
            device_id: row.device_id,
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
            start_position_ticks: row.start_position_ticks,
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

impl TryFrom<TrickplayInfoRow> for TrickplayInfo {
    type Error = anyhow::Error;

    fn try_from(row: TrickplayInfoRow) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: Uuid::parse_str(&row.item_id)
                .context("invalid trickplay info item id in database")?,
            width: row.width,
            height: row.height,
            tile_width: row.tile_width,
            tile_height: row.tile_height,
            thumbnail_count: row.thumbnail_count,
            interval_ms: row.interval_ms,
            bandwidth: row.bandwidth,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
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
            item_id: row.item_id.as_deref().map(Uuid::parse_str).transpose()?,
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

impl TryFrom<QuickConnectSessionRow> for QuickConnectSession {
    type Error = anyhow::Error;

    fn try_from(row: QuickConnectSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            secret: row.secret,
            code: row.code,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
            user_id: row
                .user_id
                .as_deref()
                .map(Uuid::parse_str)
                .transpose()
                .context("invalid quick connect user id")?,
            authorized: row.authorized,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
            expires_at: parse_time(&row.expires_at)?,
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
            restore_snapshot: row
                .restore_snapshot_json
                .map(|snapshot| serde_json::from_str(&snapshot).context("invalid backup snapshot"))
                .transpose()?,
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
    let trimmed = value.trim();
    if let Ok(parsed) = OffsetDateTime::parse(trimmed, &Rfc3339) {
        return Ok(parsed);
    }

    let mut normalized = trimmed.replacen(' ', "T", 1);
    if !normalized.ends_with('Z') && !normalized.get(10..).is_some_and(|tail| tail.contains('+')) {
        normalized.push('Z');
    }
    OffsetDateTime::parse(&normalized, &Rfc3339).context("failed to parse timestamp")
}

fn parse_media_streams_json(value: &str) -> anyhow::Result<Vec<Value>> {
    serde_json::from_str(value).context("invalid media streams json in database")
}

#[cfg(any(test, feature = "sqlite"))]
fn dedupe_uuids(values: Vec<Uuid>) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(*value))
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
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
    probe_media_info_input(path.as_os_str(), media_type, &[], None).await
}

pub async fn probe_remote_media_info(url: &str, media_type: &str) -> MediaInfo {
    probe_remote_media_info_with_permit(url, media_type, None).await
}

pub async fn probe_remote_media_info_admitted(
    url: &str,
    media_type: &str,
    permit: TranscodeJobPermit,
) -> MediaInfo {
    probe_remote_media_info_with_permit(url, media_type, Some(permit)).await
}

pub fn record_ffprobe_capacity_unavailable() {
    ffprobe_telemetry()
        .start()
        .finish(FfprobeOutcome::CapacityUnavailable);
}

async fn probe_remote_media_info_with_permit(
    url: &str,
    media_type: &str,
    permit: Option<TranscodeJobPermit>,
) -> MediaInfo {
    let remote_read_timeout_us = configured_ffprobe_timeout()
        .as_micros()
        .min(u128::from(u64::MAX))
        .to_string();
    probe_media_info_input(
        url,
        media_type,
        &["-rw_timeout", remote_read_timeout_us.as_str()],
        permit,
    )
    .await
}

pub fn configured_ffprobe_timeout_seconds() -> u64 {
    configured_ffprobe_timeout().as_secs()
}

pub fn configured_ffprobe_nice() -> Option<i32> {
    jellyrin_transcode::configured_ffmpeg_nice()
}

fn configured_ffprobe_timeout() -> StdDuration {
    static TIMEOUT: OnceLock<StdDuration> = OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        StdDuration::from_secs(ffprobe_timeout_seconds_from_value(
            std::env::var("JELLYRIN_FFPROBE_TIMEOUT_SECONDS")
                .ok()
                .as_deref(),
        ))
    })
}

fn ffprobe_timeout_seconds_from_value(value: Option<&str>) -> u64 {
    value
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=MAX_FFPROBE_TIMEOUT_SECONDS).contains(value))
        .unwrap_or(DEFAULT_FFPROBE_TIMEOUT_SECONDS)
}

async fn run_ffprobe_command(
    command: Command,
    timeout: StdDuration,
) -> Result<Vec<u8>, FfprobeOutcome> {
    let output = run_bounded_command_output(
        command,
        BoundedCommandOutputOptions::new(
            timeout,
            FFPROBE_STDOUT_MAX_BYTES,
            FFPROBE_STDERR_MAX_BYTES,
        ),
    )
    .await
    .map_err(|error| match error {
        BoundedCommandOutputError::TimedOut => FfprobeOutcome::TimedOut,
        BoundedCommandOutputError::OutputLimitExceeded { .. } => FfprobeOutcome::OutputLimited,
        BoundedCommandOutputError::Io(_) => FfprobeOutcome::IoFailed,
    })?;
    output
        .status
        .success()
        .then_some(output.stdout)
        .ok_or(FfprobeOutcome::NonZeroExit)
}

async fn probe_media_info_input(
    input: impl AsRef<OsStr>,
    media_type: &str,
    input_args: &[&str],
    admitted_permit: Option<TranscodeJobPermit>,
) -> MediaInfo {
    if !matches!(media_type, "Video" | "Audio") {
        return MediaInfo::default();
    }
    let attempt = ffprobe_telemetry().start();
    let _probe_permit = match admitted_permit {
        Some(permit) => permit,
        None => match acquire_multimedia_probe().await {
            Ok(permit) => permit,
            Err(_) => {
                attempt.finish(FfprobeOutcome::CapacityUnavailable);
                return MediaInfo::default();
            }
        },
    };

    let input = input.as_ref();
    let mut command = Command::new("ffprobe");
    command
        .arg("-v")
        .arg("error")
        .arg("-threads")
        .arg("1")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg("-show_chapters");
    for arg in input_args {
        command.arg(arg);
    }
    command.arg(input);
    let output = match run_ffprobe_command(command, configured_ffprobe_timeout()).await {
        Ok(output) => output,
        Err(outcome) => {
            attempt.finish(outcome);
            return MediaInfo::default();
        }
    };
    match serde_json::from_slice::<Value>(&output) {
        Ok(mut value) => {
            // `-show_data` applies to every stream. On Matroska files with embedded fonts that
            // can dump many megabytes of attachment data and make an otherwise valid probe hit
            // the bounded-output limit before its audio/subtitle inventory can be persisted.
            // Only request binary stream data in a second, subtitle-only pass when the primary
            // inventory proves that DVB teletext descriptors are actually needed.
            if ffprobe_has_dvb_teletext_stream(&value) {
                let mut data_command = Command::new("ffprobe");
                data_command
                    .arg("-v")
                    .arg("error")
                    .arg("-threads")
                    .arg("1")
                    .arg("-print_format")
                    .arg("json")
                    .arg("-select_streams")
                    .arg("s")
                    .arg("-show_entries")
                    .arg("stream=index,extradata")
                    .arg("-show_data");
                for arg in input_args {
                    data_command.arg(arg);
                }
                data_command.arg(input);
                if let Ok(data_output) =
                    run_ffprobe_command(data_command, configured_ffprobe_timeout()).await
                    && let Ok(data) = serde_json::from_slice::<Value>(&data_output)
                {
                    merge_ffprobe_stream_extradata(&mut value, &data);
                }
            }
            attempt.finish(FfprobeOutcome::Succeeded);
            parse_ffprobe_media_info(&value)
        }
        Err(_) => {
            attempt.finish(FfprobeOutcome::InvalidJson);
            MediaInfo::default()
        }
    }
}

fn ffprobe_has_dvb_teletext_stream(value: &Value) -> bool {
    value
        .get("streams")
        .and_then(Value::as_array)
        .is_some_and(|streams| {
            streams.iter().any(|stream| {
                stream
                    .get("codec_name")
                    .and_then(Value::as_str)
                    .is_some_and(|codec| codec.eq_ignore_ascii_case("dvb_teletext"))
            })
        })
}

fn merge_ffprobe_stream_extradata(primary: &mut Value, supplemental: &Value) {
    let Some(primary_streams) = primary.get_mut("streams").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(supplemental_streams) = supplemental.get("streams").and_then(Value::as_array) else {
        return;
    };
    for supplemental_stream in supplemental_streams {
        let Some(index) = supplemental_stream.get("index").and_then(Value::as_i64) else {
            continue;
        };
        let Some(extradata) = supplemental_stream.get("extradata").and_then(Value::as_str) else {
            continue;
        };
        if let Some(primary_stream) = primary_streams
            .iter_mut()
            .find(|stream| stream.get("index").and_then(Value::as_i64) == Some(index))
            && let Some(primary_stream) = primary_stream.as_object_mut()
        {
            primary_stream.insert(
                "extradata".to_string(),
                Value::String(extradata.to_string()),
            );
        }
    }
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
        metadata: ffprobe_tags_to_metadata(value),
    }
}

fn ffprobe_tags_to_metadata(value: &Value) -> Value {
    let mut tags = Vec::<&Value>::new();
    if let Some(format_tags) = value.pointer("/format/tags") {
        tags.push(format_tags);
    }
    if let Some(streams) = value.get("streams").and_then(Value::as_array) {
        tags.extend(streams.iter().filter_map(|stream| stream.get("tags")));
    }

    let album = first_tag_value(&tags, &["album"]);
    let artists = first_tag_value(&tags, &["artist", "artists"])
        .map(|value| split_tag_values(&value))
        .unwrap_or_default();
    let album_artists = first_tag_value(
        &tags,
        &[
            "album_artist",
            "album artist",
            "albumartist",
            "albumartists",
        ],
    )
    .map(|value| split_tag_values(&value))
    .unwrap_or_default();
    let genres = first_tag_value(&tags, &["genre"])
        .map(|value| split_tag_values(&value))
        .unwrap_or_default();

    let mut metadata = serde_json::Map::new();
    if let Some(album) = album {
        metadata.insert("Album".to_string(), Value::String(album));
    }
    if !artists.is_empty() {
        metadata.insert("Artists".to_string(), json!(artists));
    }
    if !album_artists.is_empty() {
        metadata.insert("AlbumArtists".to_string(), json!(album_artists));
    }
    if !genres.is_empty() {
        metadata.insert("Genres".to_string(), json!(genres));
        metadata.insert("MusicGenres".to_string(), json!(genres));
    }
    let chapters = ffprobe_chapters_to_metadata(value);
    if !chapters.is_empty() {
        metadata.insert("Chapters".to_string(), json!(chapters));
    }
    Value::Object(metadata)
}

fn ffprobe_chapters_to_metadata(value: &Value) -> Vec<Value> {
    value
        .get("chapters")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .filter_map(|(position, chapter)| {
            let start_ticks = chapter
                .get("start_time")
                .and_then(json_number_or_string_f64)
                .or_else(|| {
                    let start = chapter.get("start").and_then(json_number_or_string_f64)?;
                    let time_base = chapter.get("time_base").and_then(Value::as_str)?;
                    Some(start * parse_ffprobe_time_base(time_base)?)
                })
                .map(seconds_to_ticks)?;
            let name = chapter
                .get("tags")
                .and_then(|tags| first_tag_value(&[tags], &["title"]))
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| format!("Chapter {}", position + 1));
            Some(json!({
                "StartPositionTicks": start_ticks,
                "Name": name,
                "ImageDateModified": "0001-01-01T00:00:00.0000000Z"
            }))
        })
        .collect()
}

fn parse_ffprobe_time_base(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.trim().parse::<f64>().ok()?;
    let denominator = denominator.trim().parse::<f64>().ok()?;
    (denominator != 0.0).then_some(numerator / denominator)
}

async fn read_local_nfo_metadata(path: &Path) -> Option<Value> {
    let mut candidates = Vec::new();
    candidates.push(path.with_extension("nfo"));
    if let Some(parent) = path.parent() {
        candidates.push(parent.join("movie.nfo"));
        candidates.push(parent.join("tvshow.nfo"));
        candidates.push(parent.join("album.nfo"));
    }

    for candidate in candidates {
        let Ok(contents) = tokio::fs::read_to_string(&candidate).await else {
            continue;
        };
        let metadata = parse_local_nfo_metadata(&contents);
        if metadata
            .as_object()
            .is_some_and(|metadata| !metadata.is_empty())
        {
            return Some(metadata);
        }
    }
    None
}

fn parse_local_nfo_metadata(contents: &str) -> Value {
    let mut metadata = serde_json::Map::new();
    insert_nfo_text(&mut metadata, contents, "title", "Name");
    insert_nfo_text(&mut metadata, contents, "sorttitle", "SortName");
    insert_nfo_text(&mut metadata, contents, "originaltitle", "OriginalTitle");
    insert_nfo_text(&mut metadata, contents, "plot", "Overview");
    insert_nfo_text(&mut metadata, contents, "outline", "ShortOverview");
    insert_nfo_text(&mut metadata, contents, "tagline", "Tagline");
    insert_nfo_text(&mut metadata, contents, "mpaa", "OfficialRating");
    insert_nfo_text(&mut metadata, contents, "premiered", "PremiereDate");
    insert_nfo_number(&mut metadata, contents, "year", "ProductionYear");
    insert_nfo_array(&mut metadata, contents, "genre", "Genres");
    insert_nfo_array(&mut metadata, contents, "studio", "Studios");
    insert_nfo_array(&mut metadata, contents, "tag", "Tags");
    insert_nfo_people(&mut metadata, contents, "director", "Director");
    insert_nfo_actor_people(&mut metadata, contents);

    let provider_ids = nfo_unique_elements(contents, "uniqueid")
        .into_iter()
        .filter_map(|element| {
            let provider = nfo_attribute(&element, "type")
                .or_else(|| nfo_attribute(&element, "default"))
                .unwrap_or_else(|| "Unknown".to_string());
            let id = xml_decode(&strip_xml_tags(&element)).trim().to_string();
            (!provider.is_empty() && !id.is_empty()).then_some((provider_key(&provider), id))
        })
        .chain(
            ["imdbid", "tmdbid", "tvdbid"]
                .into_iter()
                .filter_map(|tag| nfo_first_text(contents, tag).map(|id| (provider_key(tag), id))),
        )
        .fold(serde_json::Map::new(), |mut ids, (key, id)| {
            ids.insert(key, Value::String(id));
            ids
        });
    if !provider_ids.is_empty() {
        metadata.insert("ProviderIds".to_string(), Value::Object(provider_ids));
    }

    // Extract image URLs/paths from NFO
    // <thumb> can be a direct URL or a local path
    if let Some(thumb) = nfo_first_text(contents, "thumb") {
        let thumb = thumb.trim().to_string();
        if !thumb.is_empty() {
            if thumb.starts_with("http://") || thumb.starts_with("https://") {
                metadata.insert("PrimaryImageUrl".to_string(), Value::String(thumb));
            } else {
                metadata.insert("PrimaryImagePath".to_string(), Value::String(thumb));
            }
        }
    }
    // <fanart><thumb>URL</thumb></fanart> for backdrop
    if let Some(fanart_thumb) = nfo_fanart_thumb(contents) {
        let fanart_thumb = fanart_thumb.trim().to_string();
        if !fanart_thumb.is_empty() {
            if fanart_thumb.starts_with("http://") || fanart_thumb.starts_with("https://") {
                metadata.insert("BackdropImageUrl".to_string(), Value::String(fanart_thumb));
            } else {
                metadata.insert("BackdropImagePath".to_string(), Value::String(fanart_thumb));
            }
        }
    }
    // <banner>URL</banner>
    if let Some(banner) = nfo_first_text(contents, "banner") {
        let banner = banner.trim().to_string();
        if !banner.is_empty() && (banner.starts_with("http://") || banner.starts_with("https://")) {
            metadata.insert("ThumbImageUrl".to_string(), Value::String(banner));
        }
    }

    Value::Object(metadata)
}

/// Extract the first <thumb> inside a <fanart> element.
fn nfo_fanart_thumb(contents: &str) -> Option<String> {
    let fanart_start = contents.find("<fanart")?;
    let fanart_section = &contents[fanart_start..];
    let fanart_end = fanart_section
        .find("</fanart>")
        .unwrap_or(fanart_section.len());
    let fanart_inner = &fanart_section[..fanart_end];
    nfo_first_text(fanart_inner, "thumb")
}

fn insert_nfo_text(
    metadata: &mut serde_json::Map<String, Value>,
    contents: &str,
    tag: &str,
    key: &str,
) {
    if let Some(value) = nfo_first_text(contents, tag) {
        metadata.insert(key.to_string(), Value::String(value));
    }
}

fn insert_nfo_number(
    metadata: &mut serde_json::Map<String, Value>,
    contents: &str,
    tag: &str,
    key: &str,
) {
    if let Some(value) = nfo_first_text(contents, tag).and_then(|value| value.parse::<i64>().ok()) {
        metadata.insert(key.to_string(), json!(value));
    }
}

fn insert_nfo_array(
    metadata: &mut serde_json::Map<String, Value>,
    contents: &str,
    tag: &str,
    key: &str,
) {
    let values = nfo_text_values(contents, tag);
    if !values.is_empty() {
        metadata.insert(key.to_string(), json!(values));
    }
}

fn insert_nfo_people(
    metadata: &mut serde_json::Map<String, Value>,
    contents: &str,
    tag: &str,
    role: &str,
) {
    let people = nfo_text_values(contents, tag)
        .into_iter()
        .map(|name| json!({ "Name": name, "Type": role }))
        .collect::<Vec<_>>();
    if !people.is_empty() {
        append_metadata_people(metadata, people);
    }
}

fn insert_nfo_actor_people(metadata: &mut serde_json::Map<String, Value>, contents: &str) {
    let people = nfo_unique_elements(contents, "actor")
        .into_iter()
        .filter_map(|actor| {
            nfo_first_text(&actor, "name").map(|name| {
                let role = nfo_first_text(&actor, "role");
                json!({
                    "Name": name,
                    "Role": role,
                    "Type": "Actor"
                })
            })
        })
        .collect::<Vec<_>>();
    if !people.is_empty() {
        append_metadata_people(metadata, people);
    }
}

fn append_metadata_people(metadata: &mut serde_json::Map<String, Value>, people: Vec<Value>) {
    let entry = metadata
        .entry("People".to_string())
        .or_insert_with(|| json!([]));
    if let Some(existing) = entry.as_array_mut() {
        existing.extend(people);
    }
}

fn nfo_first_text(contents: &str, tag: &str) -> Option<String> {
    nfo_unique_elements(contents, tag)
        .into_iter()
        .map(|element| xml_decode(&strip_xml_tags(&element)))
        .find(|value| !value.is_empty())
}

fn nfo_text_values(contents: &str, tag: &str) -> Vec<String> {
    nfo_unique_elements(contents, tag)
        .into_iter()
        .map(|element| xml_decode(&strip_xml_tags(&element)))
        .flat_map(|value| split_tag_values(&value))
        .collect()
}

fn nfo_unique_elements(contents: &str, tag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut offset = 0usize;
    let lower = contents.to_ascii_lowercase();
    while let Some(start) = lower[offset..].find(&open) {
        let start = offset + start;
        let after_tag = start + open.len();
        if !lower[after_tag..]
            .chars()
            .next()
            .is_some_and(|ch| ch == '>' || ch.is_ascii_whitespace())
        {
            offset = after_tag;
            continue;
        }
        let Some(open_end) = lower[start..].find('>').map(|index| start + index + 1) else {
            break;
        };
        let Some(end) = lower[open_end..]
            .find(&close)
            .map(|index| open_end + index + close.len())
        else {
            break;
        };
        values.push(contents[start..end].to_string());
        offset = end;
    }
    values
}

fn nfo_attribute(element: &str, name: &str) -> Option<String> {
    let lower = element.to_ascii_lowercase();
    let attr = format!("{name}=");
    let start = lower.find(&attr)? + attr.len();
    let quote = element[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = element[value_start..].find(quote)? + value_start;
    Some(xml_decode(&element[value_start..value_end]))
}

fn strip_xml_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn xml_decode(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

fn provider_key(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "imdb" | "imdbid" => "Imdb".to_string(),
        "tmdb" | "tmdbid" => "Tmdb".to_string(),
        "tvdb" | "tvdbid" => "Tvdb".to_string(),
        other => other
            .split(['_', '-', ' '])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect::<String>(),
    }
}

fn merge_metadata_values(base: Value, overlay: Value) -> Value {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    if let Some(overlay) = overlay.as_object() {
        for (key, value) in overlay {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

fn metadata_lock_data(metadata: &serde_json::Map<String, Value>) -> bool {
    metadata
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("LockData"))
        .and_then(|(_, value)| value.as_bool())
        .unwrap_or(false)
}

fn metadata_locked_fields(metadata: &serde_json::Map<String, Value>) -> HashSet<String> {
    metadata
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("LockedFields"))
        .and_then(|(_, value)| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|field| {
            let key = metadata_lock_key(field);
            let mut fields = vec![key.clone()];
            fields.extend(locked_field_aliases(&key));
            fields
        })
        .collect()
}

fn metadata_lock_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn locked_field_aliases(key: &str) -> Vec<String> {
    let aliases: &[&str] = match key {
        "overview" => &["plot", "shortoverview"],
        "productionyear" => &["year"],
        "premieredate" => &["premiered"],
        "genres" => &["genre", "musicgenres"],
        "studios" => &["studio"],
        "people" => &["actors", "director", "directors"],
        "providerids" => &["imdbid", "tmdbid", "tvdbid", "uniqueid"],
        _ => &[],
    };
    aliases
        .iter()
        .map(|alias| metadata_lock_key(alias))
        .collect()
}

fn first_tag_value(tags: &[&Value], names: &[&str]) -> Option<String> {
    tags.iter()
        .filter_map(|tag| tag.as_object())
        .flat_map(|tag| tag.iter())
        .find_map(|(key, value)| {
            names
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name))
                .then(|| {
                    value
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                })
                .flatten()
                .map(ToOwned::to_owned)
        })
}

fn split_tag_values(value: &str) -> Vec<String> {
    let mut values = Vec::<String>::new();
    for part in value.split([';', '/']) {
        let part = part.trim();
        if part.is_empty() || values.iter().any(|value| value.eq_ignore_ascii_case(part)) {
            continue;
        }
        values.push(part.to_string());
    }
    values
}

fn stream_tag_value(stream: &Value, names: &[&str]) -> Option<String> {
    stream
        .get("tags")
        .and_then(Value::as_object)
        .and_then(|tags| {
            tags.iter().find_map(|(key, value)| {
                names
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
                    .then(|| {
                        value
                            .as_str()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                    })
                    .flatten()
                    .map(ToOwned::to_owned)
            })
        })
}

fn stream_disposition_flag(stream: &Value, name: &str) -> bool {
    stream
        .get("disposition")
        .and_then(|disposition| disposition.get(name))
        .and_then(json_number_or_string_i64)
        .is_some_and(|value| value != 0)
}

fn codec_display_name(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "ac3" => "AC3".to_string(),
        "eac3" => "EAC3".to_string(),
        "aac" => "AAC".to_string(),
        "dts" => "DTS".to_string(),
        "truehd" => "TrueHD".to_string(),
        "flac" => "FLAC".to_string(),
        "hevc" | "h265" => "HEVC".to_string(),
        "h264" => "H264".to_string(),
        "hdmv_pgs_subtitle" => "PGS".to_string(),
        "subrip" => "SRT".to_string(),
        "ass" | "ssa" => codec.to_ascii_uppercase(),
        _ => codec.to_string(),
    }
}

fn language_display_name(language: Option<&str>) -> Option<String> {
    let language = language?.trim();
    if language.is_empty() || language.eq_ignore_ascii_case("und") {
        return None;
    }
    Some(
        match language.to_ascii_lowercase().as_str() {
            "eng" | "en" => "English",
            "spa" | "es" => "Spanish",
            "fre" | "fra" | "fr" => "French",
            "ger" | "deu" | "de" => "German",
            "dan" | "da" => "Danish",
            "fin" | "fi" => "Finnish",
            "nob" | "nor" | "no" => "Norwegian",
            "swe" | "sv" => "Swedish",
            _ => language,
        }
        .to_string(),
    )
}

fn audio_channel_display(channels: Option<i64>, channel_layout: Option<&str>) -> Option<String> {
    if let Some(layout) = channel_layout {
        let normalized = layout.trim().trim_end_matches("(side)").trim();
        if !normalized.is_empty() && normalized != "0" {
            return Some(normalized.to_string());
        }
    }
    match channels {
        Some(1) => Some("Mono".to_string()),
        Some(2) => Some("Stereo".to_string()),
        Some(6) => Some("5.1".to_string()),
        Some(8) => Some("7.1".to_string()),
        Some(value) if value > 0 => Some(format!("{value} ch")),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn media_stream_display_title(
    stream_type: &str,
    codec: &str,
    language: Option<&str>,
    title: Option<&str>,
    channels: Option<i64>,
    channel_layout: Option<&str>,
    is_default: bool,
    is_forced: bool,
) -> String {
    let mut parts = Vec::<String>::new();
    if let Some(language) = language_display_name(language) {
        parts.push(language);
    }
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        parts.push(title.to_string());
    }
    match stream_type {
        "Audio" => {
            parts.push(codec_display_name(codec));
            if let Some(channels) = audio_channel_display(channels, channel_layout) {
                parts.push(channels);
            }
        }
        "Subtitle" => {
            parts.push(codec_display_name(codec));
        }
        _ => parts.push(codec_display_name(codec)),
    }
    if is_default {
        parts.push("Default".to_string());
    }
    if is_forced {
        parts.push("Forced".to_string());
    }
    parts.join(" - ")
}

fn is_text_subtitle_codec(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "vtt" | "mov_text"
    )
}

const MAX_TELETEXT_SERVICES: usize = 64;

fn ffprobe_hex_dump_bytes(value: &str, max_bytes: usize) -> Option<Vec<u8>> {
    if max_bytes == 0 || value.len() > 16 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    for line in value.lines().filter(|line| !line.trim().is_empty()) {
        let (_, columns) = line.split_once(':')?;
        // `-show_data` separates its hexadecimal column from the printable rendering with two
        // spaces. Never parse the rendering: text such as "dead" could otherwise look like data.
        let hexadecimal = columns.split("  ").next()?.trim();
        for group in hexadecimal.split_ascii_whitespace() {
            if group.is_empty() || group.len() > 8 || group.len() % 2 != 0 {
                return None;
            }
            for pair in group.as_bytes().chunks_exact(2) {
                let pair = std::str::from_utf8(pair).ok()?;
                bytes.push(u8::from_str_radix(pair, 16).ok()?);
                if bytes.len() > max_bytes {
                    return None;
                }
            }
        }
    }
    (!bytes.is_empty()).then_some(bytes)
}

fn normalized_teletext_languages(stream: &Value) -> Vec<Option<String>> {
    stream
        .get("tags")
        .and_then(|tags| tags.get("language"))
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(|languages| languages.split(','))
        .take(MAX_TELETEXT_SERVICES)
        .map(|language| {
            let language = language.trim();
            (language.len() == 3 && language.bytes().all(|byte| byte.is_ascii_alphabetic()))
                .then(|| language.to_ascii_lowercase())
        })
        .collect()
}

fn ffprobe_teletext_services(stream: &Value) -> Vec<Value> {
    let Some(extradata) = stream.get("extradata").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(descriptors) =
        ffprobe_hex_dump_bytes(extradata, MAX_TELETEXT_SERVICES.saturating_mul(2))
    else {
        return Vec::new();
    };
    if descriptors.len() % 2 != 0 {
        return Vec::new();
    }
    let languages = normalized_teletext_languages(stream);
    descriptors
        .chunks_exact(2)
        .take(MAX_TELETEXT_SERVICES)
        .enumerate()
        .filter_map(|(index, descriptor)| {
            let teletext_type = descriptor[0] >> 3;
            let magazine = match descriptor[0] & 0x07 {
                0 => 8,
                value => value,
            };
            let page_tens = descriptor[1] >> 4;
            let page_ones = descriptor[1] & 0x0f;
            if page_tens > 9 || page_ones > 9 || !(1..=8).contains(&magazine) {
                return None;
            }
            let page = u16::from(magazine) * 100 + u16::from(page_tens) * 10 + u16::from(page_ones);
            let language = languages.get(index).cloned().flatten();
            Some(serde_json::json!({
                "Page": page,
                "Language": language,
                "TeletextType": teletext_type,
                "IsSubtitle": matches!(teletext_type, 2 | 5),
                "IsHearingImpaired": teletext_type == 5,
            }))
        })
        .collect()
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
    let title = stream_tag_value(stream, &["title"]);
    let bit_rate = stream.get("bit_rate").and_then(json_number_or_string_i64);
    let is_default = stream_disposition_flag(stream, "default");
    let is_forced = stream_disposition_flag(stream, "forced");

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
            "IsForced": is_forced,
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
            "Title": title,
            "DisplayTitle": media_stream_display_title(
                "Audio",
                codec,
                language,
                title.as_deref(),
                stream.get("channels").and_then(json_number_or_string_i64),
                stream.get("channel_layout").and_then(Value::as_str),
                is_default,
                is_forced,
            ),
            "IsInterlaced": false,
            "BitRate": bit_rate,
            "BitDepth": stream.get("bits_per_sample").and_then(json_number_or_string_i64),
            "Channels": stream.get("channels").and_then(json_number_or_string_i64),
            "ChannelLayout": stream.get("channel_layout").and_then(Value::as_str),
            "SampleRate": stream.get("sample_rate").and_then(json_number_or_string_i64),
            "IsDefault": is_default,
            "IsForced": is_forced,
            "Type": "Audio",
            "Index": index,
            "IsExternal": false,
            "Path": null
        })),
        "subtitle" => {
            let teletext_services = if codec.eq_ignore_ascii_case("dvb_teletext") {
                ffprobe_teletext_services(stream)
            } else {
                Vec::new()
            };
            Some(serde_json::json!({
                "Codec": codec,
                "Language": language,
                "Title": title,
                "DisplayTitle": media_stream_display_title(
                    "Subtitle",
                    codec,
                    language,
                    title.as_deref(),
                    None,
                    None,
                    is_default,
                    is_forced,
                ),
                "IsDefault": is_default,
                "IsForced": is_forced,
                "Type": "Subtitle",
                "Index": index,
                "IsExternal": false,
                "Path": null,
                "IsTextSubtitleStream": is_text_subtitle_codec(codec),
                "SupportsExternalStream": false,
                "TeletextServices": teletext_services
            }))
        }
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
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".jellyrin-"))
        {
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

#[cfg(any(test, feature = "sqlite"))]
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

#[cfg(any(test, feature = "sqlite"))]
fn trimmed_optional_str(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(Debug, Clone)]
struct PluginRepositoryModel {
    id: String,
    name: String,
    url: String,
    enabled: bool,
    payload: Value,
}

#[cfg(any(test, feature = "sqlite"))]
#[derive(Debug, Clone)]
struct PackageCatalogModel {
    id: String,
    repository_url: String,
    package_guid: Option<String>,
    package_name: String,
    package_version: String,
    runtime: String,
    target_abi: String,
    payload: Value,
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_repository_models_from_config(value: &Value) -> Vec<PluginRepositoryModel> {
    let Some(repositories) = value.as_array() else {
        return Vec::new();
    };
    repositories
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            let name = json_string_case_insensitive(value, "Name")?;
            let url = json_string_case_insensitive(value, "Url")?;
            let enabled = object
                .get("Enabled")
                .or_else(|| object.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Some(PluginRepositoryModel {
                id: stable_plugin_model_id("repository", &url),
                name,
                url,
                enabled,
                payload: value.clone(),
            })
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
fn package_catalog_models_from_repositories(
    repositories: &[PluginRepositoryModel],
) -> Vec<PackageCatalogModel> {
    let mut packages = Vec::new();
    for repository in repositories.iter().filter(|repository| repository.enabled) {
        let Some(repository_packages) =
            json_array_case_insensitive(&repository.payload, "Packages")
        else {
            continue;
        };
        for package in repository_packages {
            let Some(package_name) = json_string_case_insensitive(package, "Name") else {
                continue;
            };
            let package_guid = json_string_case_insensitive(package, "Guid")
                .or_else(|| json_string_case_insensitive(package, "Id"))
                .or_else(|| json_string_case_insensitive(package, "AssemblyGuid"));
            let package_runtime = json_string_case_insensitive(package, "Runtime")
                .unwrap_or_else(|| "DotNetJellyfin".to_string());
            let versions = json_array_case_insensitive(package, "Versions")
                .map(|versions| versions.to_vec())
                .unwrap_or_else(|| vec![package.clone()]);
            for version in versions {
                let package_version = json_string_case_insensitive(&version, "Version")
                    .unwrap_or_else(|| "0.0.0.0".to_string());
                let runtime = json_string_case_insensitive(&version, "Runtime")
                    .unwrap_or_else(|| package_runtime.clone());
                let target_abi = json_string_case_insensitive(&version, "TargetAbi")
                    .or_else(|| json_string_case_insensitive(package, "TargetAbi"))
                    .unwrap_or_default();
                let payload = json!({
                    "RepositoryName": repository.name,
                    "RepositoryUrl": repository.url,
                    "Package": package,
                    "Version": version
                });
                packages.push(PackageCatalogModel {
                    id: stable_plugin_model_id(
                        "package",
                        &format!("{}:{}:{}", repository.url, package_name, package_version),
                    ),
                    repository_url: repository.url.clone(),
                    package_guid: package_guid.clone(),
                    package_name: package_name.clone(),
                    package_version,
                    runtime,
                    target_abi,
                    payload,
                });
            }
        }
    }
    packages
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_repositories_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT name, url, enabled, payload_json
        FROM plugin_repositories
        ORDER BY name COLLATE NOCASE, url COLLATE NOCASE
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid plugin repository payload")?;
            Ok(json!({
                "Name": row.get::<String, _>("name"),
                "Url": row.get::<String, _>("url"),
                "Enabled": row.get::<i64, _>("enabled") != 0,
                "Payload": payload
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn package_catalog_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT repository_url, package_guid, package_name, package_version, runtime, target_abi, payload_json
        FROM package_catalog_cache
        ORDER BY package_name COLLATE NOCASE, package_version COLLATE NOCASE
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid package catalog payload")?;
            Ok(json!({
                "RepositoryUrl": row.get::<String, _>("repository_url"),
                "Guid": row.get::<Option<String>, _>("package_guid"),
                "Name": row.get::<String, _>("package_name"),
                "Version": row.get::<String, _>("package_version"),
                "Runtime": row.get::<String, _>("runtime"),
                "TargetAbi": row.get::<String, _>("target_abi"),
                "Payload": payload
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn package_installations_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT package_name, package_guid, version, runtime, status, source_url,
            payload_json, installed_at, updated_at
        FROM package_installations
        ORDER BY package_name COLLATE NOCASE, version COLLATE NOCASE
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid package installation payload")?;
            Ok(json!({
                "Name": row.get::<String, _>("package_name"),
                "Guid": row.get::<Option<String>, _>("package_guid"),
                "Version": row.get::<String, _>("version"),
                "Runtime": row.get::<String, _>("runtime"),
                "Status": row.get::<String, _>("status"),
                "SourceUrl": row.get::<Option<String>, _>("source_url"),
                "Payload": payload,
                "InstalledAt": row.get::<Option<String>, _>("installed_at"),
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn installed_plugins_backup_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT plugin_id, name, version, runtime, runtime_version, target_abi,
            server_compatibility_json, status, capabilities_json, permissions_json,
            configuration_state, last_error, health_json, manifest_json, installed_at, updated_at
        FROM installed_plugins
        ORDER BY name COLLATE NOCASE, version COLLATE NOCASE
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let mut value = plugin_row_to_json(&row)?;
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "InstalledAt".to_string(),
                    json!(row.get::<Option<String>, _>("installed_at")),
                );
                object.insert(
                    "UpdatedAt".to_string(),
                    json!(row.get::<String, _>("updated_at")),
                );
            }
            Ok(value)
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_manifests_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, manifest_json, updated_at FROM plugin_manifests ORDER BY plugin_id COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let manifest: Value = serde_json::from_str(row.get::<&str, _>("manifest_json"))
                .context("invalid plugin manifest payload")?;
            Ok(json!({
                "PluginId": row.get::<String, _>("plugin_id"),
                "Manifest": manifest,
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_configurations_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, configuration_json, updated_at FROM plugin_configurations ORDER BY plugin_id COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let configuration: Value =
                serde_json::from_str(row.get::<&str, _>("configuration_json"))
                    .context("invalid plugin configuration payload")?;
            Ok(json!({
                "PluginId": row.get::<String, _>("plugin_id"),
                "Configuration": configuration,
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_permissions_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT plugin_id, permissions_json, updated_at FROM plugin_permissions ORDER BY plugin_id COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let permissions: Value = serde_json::from_str(row.get::<&str, _>("permissions_json"))
                .context("invalid plugin permissions payload")?;
            Ok(json!({
                "PluginId": row.get::<String, _>("plugin_id"),
                "Permissions": permissions,
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_runtime_instances_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT instance_id, plugin_id, runtime, runtime_version, status, process_id, endpoint,
            health_json, last_error, started_at, updated_at
        FROM plugin_runtime_instances
        ORDER BY plugin_id COLLATE NOCASE, instance_id COLLATE NOCASE
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let health: Value = serde_json::from_str(row.get::<&str, _>("health_json"))
                .context("invalid plugin runtime health payload")?;
            Ok(json!({
                "InstanceId": row.get::<String, _>("instance_id"),
                "PluginId": row.get::<Option<String>, _>("plugin_id"),
                "Runtime": row.get::<String, _>("runtime"),
                "RuntimeVersion": row.get::<String, _>("runtime_version"),
                "Status": row.get::<String, _>("status"),
                "ProcessId": row.get::<Option<i64>, _>("process_id"),
                "Endpoint": row.get::<Option<String>, _>("endpoint"),
                "Health": health,
                "LastError": row.get::<Option<String>, _>("last_error"),
                "StartedAt": row.get::<Option<String>, _>("started_at"),
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_host_events_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, runtime, event_type, severity, message, payload_json, created_at
        FROM plugin_host_events
        ORDER BY created_at, id
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid plugin host event payload")?;
            Ok(json!({
                "Id": row.get::<String, _>("id"),
                "PluginId": row.get::<Option<String>, _>("plugin_id"),
                "Runtime": row.get::<Option<String>, _>("runtime"),
                "EventType": row.get::<String, _>("event_type"),
                "Severity": row.get::<String, _>("severity"),
                "Message": row.get::<String, _>("message"),
                "Payload": payload,
                "CreatedAt": row.get::<String, _>("created_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_audit_log_snapshot(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, action, actor_user_id, status, payload_json, created_at
        FROM plugin_audit_log
        ORDER BY created_at, id
        "#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid plugin audit payload")?;
            Ok(json!({
                "Id": row.get::<String, _>("id"),
                "PluginId": row.get::<Option<String>, _>("plugin_id"),
                "Action": row.get::<String, _>("action"),
                "ActorUserId": row.get::<Option<String>, _>("actor_user_id"),
                "Status": row.get::<String, _>("status"),
                "Payload": payload,
                "CreatedAt": row.get::<String, _>("created_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_row_to_json(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Value> {
    let server_compatibility: Value =
        serde_json::from_str(row.get::<&str, _>("server_compatibility_json"))
            .context("invalid plugin server compatibility payload")?;
    let capabilities: Value = serde_json::from_str(row.get::<&str, _>("capabilities_json"))
        .context("invalid plugin capabilities payload")?;
    let permissions: Value = serde_json::from_str(row.get::<&str, _>("permissions_json"))
        .context("invalid plugin permissions payload")?;
    let health: Value = serde_json::from_str(row.get::<&str, _>("health_json"))
        .context("invalid plugin health payload")?;
    let manifest: Value = serde_json::from_str(row.get::<&str, _>("manifest_json"))
        .context("invalid plugin manifest payload")?;
    Ok(json!({
        "Id": row.get::<String, _>("plugin_id"),
        "Guid": row.get::<String, _>("plugin_id"),
        "Name": row.get::<String, _>("name"),
        "Version": row.get::<String, _>("version"),
        "Runtime": row.get::<String, _>("runtime"),
        "RuntimeVersion": row.get::<String, _>("runtime_version"),
        "TargetAbi": row.get::<String, _>("target_abi"),
        "ServerCompatibility": server_compatibility,
        "Status": row.get::<String, _>("status"),
        "Capabilities": capabilities,
        "Permissions": permissions,
        "ConfigurationState": row.get::<String, _>("configuration_state"),
        "LastError": row.get::<Option<String>, _>("last_error"),
        "Health": health,
        "Manifest": manifest
    }))
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_runtime_instance_id(plugin_id: &str, runtime: &str) -> String {
    format!(
        "{}:{}",
        plugin_id.trim().to_ascii_lowercase(),
        runtime.trim().to_ascii_lowercase()
    )
}

#[cfg(any(test, feature = "sqlite"))]
fn live_tv_channel_select_builder() -> QueryBuilder<Sqlite> {
    QueryBuilder::new(
        r#"
        SELECT c.channel_id, c.tuner_id, c.remote_id, c.category_id,
            cat.name AS category_name,
            c.name, c.sort_name, c.number, c.stream_url, c.logo_url,
            c.channel_type, c.metadata_json
        FROM live_tv_channels c
        LEFT JOIN live_tv_categories cat ON cat.category_id = c.category_id
        WHERE c.enabled = 1
        "#,
    )
}

#[cfg(any(test, feature = "sqlite"))]
fn append_live_tv_channel_filters(builder: &mut QueryBuilder<Sqlite>, query: &LiveTvChannelQuery) {
    let tuner_ids = query
        .tuner_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if !tuner_ids.is_empty() {
        builder.push(" AND c.tuner_id IN (");
        let mut separated = builder.separated(", ");
        for id in &tuner_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(")");
    }
    let category_ids = query
        .category_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if !category_ids.is_empty() {
        builder.push(" AND c.category_id IN (");
        let mut separated = builder.separated(", ");
        for id in &category_ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(")");
    }
    if let Some(search_term) = query.search_term.as_deref().map(str::trim)
        && !search_term.is_empty()
    {
        builder.push(" AND c.name LIKE ");
        builder.push_bind(format!("%{search_term}%"));
    }
}

#[cfg(any(test, feature = "sqlite"))]
fn live_tv_channel_record_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<LiveTvChannelRecord> {
    let metadata_json = row.get::<String, _>("metadata_json");
    Ok(LiveTvChannelRecord {
        channel_id: row.get("channel_id"),
        tuner_id: row.get("tuner_id"),
        remote_id: row.get("remote_id"),
        category_id: row.get("category_id"),
        category_name: row.get("category_name"),
        name: row.get("name"),
        sort_name: row.get("sort_name"),
        number: row.get("number"),
        stream_url: row.get("stream_url"),
        logo_url: row.get("logo_url"),
        channel_type: row.get("channel_type"),
        metadata: serde_json::from_str(&metadata_json)
            .context("invalid live TV channel metadata")?,
    })
}

#[cfg(any(test, feature = "sqlite"))]
fn live_tv_stream_probe_record_from_sqlite_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<LiveTvStreamProbeRecord> {
    Ok(LiveTvStreamProbeRecord {
        channel_id: row.get("channel_id"),
        tuner_id: row.get("tuner_id"),
        remote_id: row.get("remote_id"),
        source_revision: row.get("source_revision"),
        probe_version: i16::try_from(row.get::<i64, _>("probe_version"))
            .context("invalid Live TV stream probe version")?,
        outcome: LiveTvStreamProbeOutcome::from_stored(&row.get::<String, _>("outcome"))?,
        streams: serde_json::from_str(&row.get::<String, _>("streams_json"))
            .context("invalid Live TV stream probe streams")?,
        observed_at: parse_time(&row.get::<String, _>("observed_at"))?,
        completed_at: parse_time(&row.get::<String, _>("completed_at"))?,
        expires_at: parse_time(&row.get::<String, _>("expires_at"))?,
    })
}

#[cfg(any(test, feature = "sqlite"))]
async fn enrich_plugin_runtime_state(pool: &SqlitePool, plugin: &mut Value) -> anyhow::Result<()> {
    let Some(plugin_id) = plugin.get("Id").and_then(Value::as_str).map(str::to_string) else {
        return Ok(());
    };
    plugin["RuntimeInstances"] =
        Value::Array(plugin_runtime_instances_for_plugin(pool, &plugin_id).await?);
    plugin["RecentEvents"] =
        Value::Array(plugin_host_events_for_plugin(pool, &plugin_id, 25).await?);
    Ok(())
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_runtime_instances_for_plugin(
    pool: &SqlitePool,
    plugin_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT instance_id, plugin_id, runtime, runtime_version, status, process_id,
            endpoint, health_json, last_error, started_at, updated_at
        FROM plugin_runtime_instances
        WHERE plugin_id = ?1 COLLATE NOCASE
        ORDER BY updated_at DESC
        "#,
    )
    .bind(plugin_id.trim())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let health: Value = serde_json::from_str(row.get::<&str, _>("health_json"))
                .context("invalid plugin runtime health payload")?;
            Ok(json!({
                "InstanceId": row.get::<String, _>("instance_id"),
                "PluginId": row.get::<Option<String>, _>("plugin_id"),
                "Runtime": row.get::<String, _>("runtime"),
                "RuntimeVersion": row.get::<String, _>("runtime_version"),
                "Status": row.get::<String, _>("status"),
                "ProcessId": row.get::<Option<i64>, _>("process_id"),
                "Endpoint": row.get::<Option<String>, _>("endpoint"),
                "Health": health,
                "LastError": row.get::<Option<String>, _>("last_error"),
                "StartedAt": row.get::<Option<String>, _>("started_at"),
                "UpdatedAt": row.get::<String, _>("updated_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
async fn plugin_host_events_for_plugin(
    pool: &SqlitePool,
    plugin_id: &str,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT id, plugin_id, runtime, event_type, severity, message, payload_json, created_at
        FROM plugin_host_events
        WHERE plugin_id = ?1 COLLATE NOCASE
        ORDER BY created_at DESC
        LIMIT ?2
        "#,
    )
    .bind(plugin_id.trim())
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(row.get::<&str, _>("payload_json"))
                .context("invalid plugin host event payload")?;
            Ok(json!({
                "Id": row.get::<String, _>("id"),
                "PluginId": row.get::<Option<String>, _>("plugin_id"),
                "Runtime": row.get::<Option<String>, _>("runtime"),
                "EventType": row.get::<String, _>("event_type"),
                "Severity": row.get::<String, _>("severity"),
                "Message": row.get::<String, _>("message"),
                "Payload": payload,
                "CreatedAt": row.get::<String, _>("created_at")
            }))
        })
        .collect()
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_items<'a>(snapshot: &'a Value, section: &str) -> anyhow::Result<&'a Vec<Value>> {
    snapshot
        .get(section)
        .and_then(|section| section.get("Items"))
        .and_then(Value::as_array)
        .with_context(|| format!("plugin snapshot section {section}.Items must be an array"))
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_value<'a>(item: &'a Value, field: &str) -> Option<&'a Value> {
    item.as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .map(|(_, value)| value)
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_string(item: &Value, field: &str) -> anyhow::Result<String> {
    plugin_snapshot_optional_string(item, field)
        .with_context(|| format!("plugin snapshot item is missing {field}"))
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_optional_string(item: &Value, field: &str) -> Option<String> {
    plugin_snapshot_value(item, field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_bool(item: &Value, field: &str) -> Option<bool> {
    plugin_snapshot_value(item, field).and_then(Value::as_bool)
}

#[cfg(any(test, feature = "sqlite"))]
fn plugin_snapshot_json_string(
    item: &Value,
    field: &str,
    default: Value,
) -> anyhow::Result<String> {
    serde_json::to_string(plugin_snapshot_value(item, field).unwrap_or(&default))
        .context("plugin snapshot JSON serialization failed")
}

#[cfg(any(test, feature = "sqlite"))]
fn json_string_case_insensitive(value: &Value, field: &str) -> Option<String> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .and_then(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(test, feature = "sqlite"))]
fn json_array_case_insensitive<'a>(value: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(field))
        .and_then(|(_, value)| value.as_array())
}

#[cfg(any(test, feature = "sqlite"))]
fn stable_plugin_model_id(kind: &str, value: &str) -> String {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("{kind}:{normalized}")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        time::Duration as StdDuration,
    };

    use super::{
        ActivityLogFilter, ActivityLogSortField, Database, DatabaseDriver, DatabasePoolRole,
        InstallPluginPackage, LiveTvChannelUpsert, LiveTvStreamProbeOutcome,
        LiveTvStreamProbeUpsert, LiveTvTunerUpsert, MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS,
        MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE, MEDIA_ITEM_FACET_PROJECTION_NAME,
        MEDIA_ITEM_FACET_PROJECTION_VERSION, MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME,
        MediaItemCatalogQuery, MediaItemCatalogSearchScope, MediaItemFacetCandidateQuery,
        MediaItemFacetKind, MediaItemFavoriteFilter, MediaItemQueryFilterSelection,
        PluginRuntimeInstanceUpsert, ProviderSecretReference, ProviderSecretVault,
        REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS, RemoteMediaItemUpsert,
        RemoteMediaLibrarySnapshot, RemoteMediaLibraryStageSpec, ResumeItemsPageQuery,
        SortDirection, SystemConfigurationPayloads, TvSeriesCatalogNameFilter,
        UpsertActivePlaybackSession, UpsertActiveViewingSession, UpsertPlaybackState,
        UpsertTranscodeSession, parse_ffprobe_media_info, parse_local_nfo_metadata,
    };
    use serde_json::{Value, json};
    use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
    use uuid::Uuid;

    #[test]
    fn upcoming_date_projection_preserves_strict_precedence_offsets_and_nanoseconds() {
        let metadata = json!({"PremiereDate": "2000-01-01T01:00:00.123456789+01:00"});
        let parsed = super::upcoming_media_item_premiere_date(&metadata).unwrap();
        assert_eq!(
            super::upcoming_media_item_premiere_parts(&metadata),
            Some((parsed.unix_timestamp(), 123_456_789))
        );

        assert!(
            super::upcoming_media_item_premiere_parts(&json!({
                "PremiereDate": "invalid",
                "AirDate": "2035-02-03T04:05:06Z"
            }))
            .is_none()
        );
        assert!(
            super::upcoming_media_item_premiere_parts(&json!({
                "PremiereDate": null,
                "AirDate": "2035-02-03T04:05:06Z"
            }))
            .is_none()
        );
        assert!(
            super::upcoming_media_item_premiere_parts(&json!({
                "premieredate": "2035-02-03T04:05:06Z"
            }))
            .is_none()
        );
        assert!(
            super::upcoming_media_item_premiere_parts(&json!({
                "PremiereDate": "2035-02-03"
            }))
            .is_none()
        );
        assert!(
            super::upcoming_media_item_premiere_parts(&json!({
                "AirDate": "2035-02-03T04:05:06Z"
            }))
            .is_some()
        );
    }

    #[tokio::test]
    async fn sqlite_runtime_settings_enable_busy_timeout_and_foreign_keys() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();

        assert_eq!(busy_timeout, 5_000);
        assert_eq!(foreign_keys, 1);
    }

    #[tokio::test]
    async fn sqlite_live_tv_stream_probe_cache_is_revisioned_bounded_and_snapshot_safe() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let tuner_id = "probe-tuner";
        let channel_id = "probe-channel";
        let remote_id = "42";
        let tuner = || LiveTvTunerUpsert {
            tuner_id: tuner_id.to_string(),
            provider_type: "xtream".to_string(),
            name: "Probe tuner".to_string(),
            source_url: None,
            configuration: json!({"Id": tuner_id, "Type": "xtream"}),
        };
        let channel = || LiveTvChannelUpsert {
            channel_id: channel_id.to_string(),
            tuner_id: tuner_id.to_string(),
            remote_id: remote_id.to_string(),
            category_id: None,
            name: "Probe channel".to_string(),
            sort_name: "probe channel".to_string(),
            number: None,
            stream_url: "https://example.invalid/live.ts".to_string(),
            logo_url: None,
            channel_type: "TV".to_string(),
            metadata: json!({}),
        };
        db.replace_live_tv_tuner_snapshot(tuner(), Vec::new(), vec![channel()])
            .await
            .unwrap();
        let observed_at = OffsetDateTime::parse("2026-08-13T10:00:00Z", &Rfc3339).unwrap();
        let revision = "abcdef0123456789abcdef0123456789";
        let probe = LiveTvStreamProbeUpsert {
            channel_id: channel_id.to_string(),
            tuner_id: tuner_id.to_string(),
            remote_id: remote_id.to_string(),
            source_revision: revision.to_string(),
            probe_version: 1,
            outcome: LiveTvStreamProbeOutcome::Tracks,
            streams: json!([{"Index": 2, "Codec": "dvb_teletext", "Language": "spa"}]),
            observed_at,
            completed_at: observed_at + Duration::seconds(1),
            expires_at: observed_at + Duration::minutes(30),
        };
        db.upsert_live_tv_stream_probe(probe).await.unwrap();
        assert!(
            db.live_tv_stream_probe(channel_id, revision, 1, observed_at)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.live_tv_stream_probe(channel_id, "0000000000000000", 1, observed_at)
                .await
                .unwrap()
                .is_none()
        );

        // Publishing the same channel identity must preserve the derived cache row.
        db.replace_live_tv_tuner_snapshot(tuner(), Vec::new(), vec![channel()])
            .await
            .unwrap();
        assert!(
            db.live_tv_stream_probe(channel_id, revision, 1, observed_at)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            db.cleanup_live_tv_stream_probes(observed_at + Duration::hours(1), 100)
                .await
                .unwrap(),
            1
        );
        assert!(
            db.live_tv_stream_probe(channel_id, revision, 1, observed_at)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn sqlite_diagnostics_report_pool_and_redacted_catalog_sync_summary() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let runtime = db.runtime_diagnostics();
        assert_eq!(runtime.driver, DatabaseDriver::Sqlite);
        assert_eq!(
            runtime.api_pool.max_connections,
            super::SQLITE_MAX_CONNECTIONS
        );
        assert_eq!(
            runtime.api_pool.idle + runtime.api_pool.in_use,
            runtime.api_pool.size
        );
        assert_eq!(runtime.worker_pool, None);

        let cloned = db.clone();
        assert!(std::sync::Arc::ptr_eq(&db.telemetry, &cloned.telemetry));
        let telemetry = db.telemetry_diagnostics();
        assert_eq!(
            telemetry.coverage,
            crate::DatabaseTelemetryCoverage::SelectedHotPaths
        );
        assert_eq!(telemetry.api_acquire.attempts, 0);
        assert!(telemetry.worker_acquire.is_none());
        assert!(telemetry.operations.is_empty());

        let empty = db.catalog_sync_diagnostics().await.unwrap();
        assert_eq!(empty.total, 0);
        assert_eq!(empty.last_run, None);

        let folder = db
            .upsert_virtual_folder("Diagnostics", Some("movies"), vec!["/diagnostics".into()])
            .await
            .unwrap();
        for (id, generation, status, count, started, completed, error) in [
            (
                "sync-running",
                "generation-running",
                "running",
                10_i64,
                "2026-08-09T00:00:00Z",
                None,
                None,
            ),
            (
                "sync-completed",
                "generation-completed",
                "completed",
                20_i64,
                "2026-08-09T00:01:00Z",
                Some("2026-08-09T00:01:01Z"),
                None,
            ),
            (
                "sync-failed",
                "generation-failed",
                "failed",
                30_i64,
                "2026-08-09T00:02:00Z",
                Some("2026-08-09T00:02:01.500Z"),
                Some("https://user:secret@provider.invalid/live?token=secret"),
            ),
        ] {
            sqlx::query(
                r#"
                INSERT INTO catalog_sync_runs (
                    id, virtual_folder_id, generation_id, status, item_count,
                    started_at, completed_at, error_message
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )
            .bind(id)
            .bind(folder.id.to_string())
            .bind(generation)
            .bind(status)
            .bind(count)
            .bind(started)
            .bind(completed)
            .bind(error)
            .execute(db.pool())
            .await
            .unwrap();
        }

        let diagnostics = db.catalog_sync_diagnostics().await.unwrap();
        assert_eq!(diagnostics.total, 3);
        assert_eq!(diagnostics.running, 1);
        assert_eq!(diagnostics.completed, 1);
        assert_eq!(diagnostics.failed, 1);
        let last = diagnostics.last_run.as_ref().unwrap();
        assert_eq!(last.status, "failed");
        assert_eq!(last.item_count, 30);
        assert_eq!(last.duration_millis, Some(1_500));
        let debug = format!("{diagnostics:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("provider.invalid"));
        assert!(!debug.contains("generation-failed"));
    }

    #[tokio::test]
    async fn sqlite_telemetry_records_rows_failures_and_sync_phases_without_sensitive_data() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db.first_user().await.unwrap();
        let (_, device_token) = db
            .issue_device_token_for_user(user.id, "telemetry-device", "Telemetry", "Test", "1")
            .await
            .unwrap();
        db.user_by_token(&device_token.access_token).await.unwrap();
        assert!(db.user_by_token("invalid-sensitive-token").await.is_err());

        let api_key = db
            .issue_api_key_for_user(user.id, "telemetry-key")
            .await
            .unwrap();
        db.user_by_api_key(&api_key).await.unwrap();
        assert!(
            db.user_by_api_key("invalid-sensitive-api-key")
                .await
                .is_err()
        );

        let item_id = Uuid::new_v4();
        let folders = db
            .replace_remote_media_library_snapshots(vec![RemoteMediaLibrarySnapshot {
                library_name: "Telemetry Catalog".to_string(),
                collection_type: "movies".to_string(),
                source_location: "provider://sensitive-source".to_string(),
                items: vec![RemoteMediaItemUpsert {
                    id: item_id.to_string(),
                    name: "Telemetry Movie".to_string(),
                    path: "provider://sensitive-source/movie.mkv".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata: json!({"PrivateMarker": "must-not-leak"}),
                }],
            }])
            .await
            .unwrap();
        assert_eq!(
            db.media_items_by_name_search("Telemetry", &["movies"], 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.media_item_catalog_page(&MediaItemCatalogQuery::default())
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(
            db.media_items_for_virtual_folders(&[folders[0].id])
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.media_item_counts_by_virtual_folder()
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.media_item_metadata_by_item_ids(&std::collections::HashSet::from([item_id]))
                .await
                .unwrap()
                .len(),
            1
        );
        db.update_transcode_session_progress("unknown-session", Some(1.0), 1)
            .await
            .unwrap();
        assert!(
            db.update_transcode_session_progress("", Some(1.0), 1)
                .await
                .is_err()
        );

        sqlx::query("DROP TABLE media_items")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.media_item_counts_by_virtual_folder().await.is_err());

        let telemetry = db.telemetry_diagnostics();
        let operation = |name: &str| {
            telemetry
                .operations
                .iter()
                .find(|operation| {
                    operation.name == name && operation.pool == super::DatabasePoolRole::Api
                })
                .unwrap_or_else(|| panic!("missing telemetry operation {name}"))
        };
        for (name, rows) in [
            ("catalog.name_search", 1),
            ("catalog.page", 1),
            ("catalog.folder_items", 1),
            ("catalog.metadata_by_ids", 1),
            ("catalog_sync.publish", 1),
            ("catalog_sync.stage", 1),
            ("catalog_sync.merge", 1),
        ] {
            let metric = operation(name);
            assert_eq!(metric.calls, 1, "{name}");
            assert_eq!(metric.succeeded, 1, "{name}");
            assert_eq!(metric.rows.total, rows, "{name}");
        }
        let token_metric = operation("auth.user_by_token");
        assert_eq!(
            (
                token_metric.calls,
                token_metric.succeeded,
                token_metric.errors
            ),
            (2, 1, 1)
        );
        assert_eq!(token_metric.rows.total, 1);
        let api_key_metric = operation("auth.user_by_api_key");
        assert_eq!(
            (
                api_key_metric.calls,
                api_key_metric.succeeded,
                api_key_metric.errors
            ),
            (2, 1, 1)
        );
        assert_eq!(api_key_metric.rows.total, 1);
        let counts = operation("catalog.folder_counts");
        assert_eq!((counts.calls, counts.succeeded, counts.errors), (2, 1, 1));
        assert_eq!(counts.rows.total, 1);
        assert_eq!(counts.errors_by_class.database, 1);
        let progress = operation("transcode.progress_write");
        assert_eq!(
            (progress.calls, progress.succeeded, progress.errors),
            (2, 1, 1)
        );
        assert_eq!(progress.rows.total, 0);
        assert_eq!(operation("catalog_sync.tombstone").rows.total, 0);
        assert_eq!(operation("catalog_sync.commit").succeeded, 1);

        let debug = format!("{telemetry:?}");
        let item_id_string = item_id.to_string();
        for sensitive in [
            device_token.access_token.as_str(),
            api_key.as_str(),
            "invalid-sensitive-token",
            "invalid-sensitive-api-key",
            "provider://sensitive-source",
            "must-not-leak",
            item_id_string.as_str(),
        ] {
            assert!(!debug.contains(sensitive));
        }
    }

    #[tokio::test]
    async fn legacy_xtream_configs_backfill_to_one_secret_and_rotation_changes_revision() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let raw = json!({
            "Id": "xtream-plugin",
            "Type": "xtream",
            "Url": "https://provider.invalid",
            "Username": "vault-user",
            "Password": "vault-password"
        });
        let now = "2026-08-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at) VALUES (?1, ?2, ?3)",
        )
        .bind("jellyrin-xtream-provider")
        .bind(raw.to_string())
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO live_tv_tuners (
                tuner_id, provider_type, name, source_url, enabled, configuration_json,
                last_sync_at, created_at, updated_at
            ) VALUES (?1, 'xtream', 'Xtream', NULL, 1, ?2, NULL, ?3, ?3)
            "#,
        )
        .bind("xtream-plugin")
        .bind(raw.to_string())
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO named_configurations (key, payload_json, updated_at) VALUES ('livetv', ?1, ?2)",
        )
        .bind(json!({ "TunerHosts": [raw] }).to_string())
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();

        assert_eq!(db.backfill_legacy_provider_secrets().await.unwrap(), 3);
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
        let ciphertext =
            sqlx::query_scalar::<_, Vec<u8>>("SELECT ciphertext FROM provider_secrets LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(
            !ciphertext
                .windows(b"vault-user".len())
                .any(|window| window == b"vault-user")
        );
        assert!(
            !ciphertext
                .windows(b"vault-password".len())
                .any(|window| window == b"vault-password")
        );

        let plugin = db
            .plugin_configuration_json("jellyrin-xtream-provider")
            .await
            .unwrap()
            .unwrap();
        let tuner = db
            .live_tv_tuner_configuration_by_id("xtream-plugin")
            .await
            .unwrap()
            .unwrap();
        let named = db.named_configuration("livetv").await.unwrap().unwrap();
        let named_tuner = &named["TunerHosts"][0];
        for persisted in [&plugin, &tuner, named_tuner] {
            let serialized = persisted.to_string();
            assert!(!serialized.contains("vault-user"));
            assert!(!serialized.contains("vault-password"));
            assert!(persisted.get("Username").is_none());
            assert!(persisted.get("Password").is_none());
        }
        let plugin_reference = ProviderSecretReference::from_configuration(&plugin).unwrap();
        assert_eq!(
            ProviderSecretReference::from_configuration(&tuner)
                .unwrap()
                .id,
            plugin_reference.id
        );
        assert_eq!(
            ProviderSecretReference::from_configuration(named_tuner)
                .unwrap()
                .id,
            plugin_reference.id
        );
        assert_eq!(db.backfill_legacy_provider_secrets().await.unwrap(), 0);

        let resolved = db.resolve_provider_configuration(&plugin).await.unwrap();
        let revision_before = resolved["JellyrinConfigurationRevision"]
            .as_str()
            .unwrap()
            .to_string();
        let protected_again = db
            .protect_provider_configuration("xtream", resolved)
            .await
            .unwrap();
        let no_op_reference =
            ProviderSecretReference::from_configuration(&protected_again).unwrap();
        assert_eq!(no_op_reference.revision, plugin_reference.revision);

        let rotated_vault = ProviderSecretVault::new("test-v2", vec![0x6b; 32])
            .unwrap()
            .with_decryption_key("test-v1", vec![0x5a; 32])
            .unwrap();
        let rotated_db = db.clone().with_provider_secret_vault(rotated_vault);
        assert_eq!(
            rotated_db
                .rotate_provider_secrets_to_active_key()
                .await
                .unwrap(),
            1
        );
        let resolved_after = rotated_db
            .resolve_provider_configuration(&plugin)
            .await
            .unwrap();
        assert_ne!(
            resolved_after["JellyrinConfigurationRevision"]
                .as_str()
                .unwrap(),
            revision_before
        );
        assert_eq!(resolved_after["Username"], "vault-user");
        assert_eq!(resolved_after["Password"], "vault-password");
    }

    #[tokio::test]
    async fn provider_secret_rolls_back_when_plugin_configuration_write_fails() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let now = "2026-08-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO plugin_manifests (plugin_id, manifest_json, updated_at) VALUES (?1, '{}', ?2)",
        )
        .bind("jellyrin-xtream-provider")
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at) VALUES (?1, '{}', ?2)",
        )
        .bind("jellyrin-xtream-provider")
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER reject_xtream_configuration
            BEFORE UPDATE OF configuration_json ON plugin_configurations
            WHEN NEW.plugin_id = 'jellyrin-xtream-provider'
            BEGIN
                SELECT RAISE(ABORT, 'forced configuration failure');
            END
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        let result = db
            .update_plugin_configuration_json(
                "jellyrin-xtream-provider",
                json!({
                    "Url": "https://provider.invalid",
                    "Username": "atomic-user",
                    "Password": "atomic-password"
                }),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(db.provider_secret_count().await.unwrap(), 0);
        assert_eq!(
            db.plugin_configuration_json("jellyrin-xtream-provider")
                .await
                .unwrap(),
            Some(json!({}))
        );
    }

    #[tokio::test]
    async fn transactional_plugin_writer_is_idempotent_and_updates_in_place() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let now = "2026-08-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO plugin_manifests (plugin_id, manifest_json, updated_at) VALUES (?1, '{}', ?2)",
        )
        .bind("jellyrin-xtream-provider")
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        let configuration = json!({
            "Url": "https://provider.invalid",
            "Username": "idempotent-user",
            "Password": "idempotent-password"
        });

        assert!(
            db.update_plugin_configuration_json("jellyrin-xtream-provider", configuration.clone(),)
                .await
                .unwrap()
        );
        let first = db
            .plugin_configuration_json("jellyrin-xtream-provider")
            .await
            .unwrap()
            .unwrap();
        let first_reference = ProviderSecretReference::from_configuration(&first).unwrap();
        assert!(
            db.update_plugin_configuration_json("jellyrin-xtream-provider", configuration.clone(),)
                .await
                .unwrap()
        );
        let unchanged = db
            .plugin_configuration_json("jellyrin-xtream-provider")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ProviderSecretReference::from_configuration(&unchanged).unwrap(),
            first_reference
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);

        let mut changed = configuration;
        changed["Password"] = json!("updated-password");
        assert!(
            db.update_plugin_configuration_json("jellyrin-xtream-provider", changed)
                .await
                .unwrap()
        );
        let updated = db
            .plugin_configuration_json("jellyrin-xtream-provider")
            .await
            .unwrap()
            .unwrap();
        let updated_reference = ProviderSecretReference::from_configuration(&updated).unwrap();
        assert_eq!(updated_reference.id, first_reference.id);
        assert_eq!(updated_reference.revision, first_reference.revision + 1);
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
        assert_eq!(
            db.resolve_provider_configuration(&updated).await.unwrap()["Password"],
            "updated-password"
        );
    }

    #[tokio::test]
    async fn standalone_protection_uses_copy_on_write_for_changed_credentials() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let original = db
            .protect_provider_configuration(
                "xtream",
                json!({
                    "Username": "copy-on-write-user",
                    "Password": "old-password"
                }),
            )
            .await
            .unwrap();
        let original_reference = ProviderSecretReference::from_configuration(&original).unwrap();
        let mut changed = db.resolve_provider_configuration(&original).await.unwrap();
        changed["Password"] = json!("new-password");

        let protected_changed = db
            .protect_provider_configuration("xtream", changed)
            .await
            .unwrap();
        let changed_reference =
            ProviderSecretReference::from_configuration(&protected_changed).unwrap();
        assert_ne!(changed_reference.id, original_reference.id);
        assert_eq!(db.provider_secret_count().await.unwrap(), 2);
        assert_eq!(
            db.resolve_provider_configuration(&original).await.unwrap()["Password"],
            "old-password"
        );
        assert_eq!(
            db.resolve_provider_configuration(&protected_changed)
                .await
                .unwrap()["Password"],
            "new-password"
        );
    }

    #[tokio::test]
    async fn tuner_snapshot_returns_the_protected_configuration_it_committed() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let protected = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: "atomic-return-tuner".to_string(),
                    provider_type: "xtream".to_string(),
                    name: "Atomic return".to_string(),
                    source_url: None,
                    configuration: json!({
                        "Id": "atomic-return-tuner",
                        "Type": "xtream",
                        "Username": "return-user",
                        "Password": "return-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();

        assert!(ProviderSecretReference::from_configuration(&protected).is_some());
        assert!(protected.get("Username").is_none());
        assert!(protected.get("Password").is_none());
        assert_eq!(
            db.live_tv_tuner_configuration_by_id("atomic-return-tuner")
                .await
                .unwrap(),
            Some(protected)
        );
    }

    #[tokio::test]
    async fn sqlite_tuner_delete_collects_only_an_unreferenced_secret_envelope() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let first_tuner_id = "shared-secret-tuner-a";
        let second_tuner_id = "shared-secret-tuner-b";
        let first_configuration = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: first_tuner_id.to_string(),
                    provider_type: "xtream".to_string(),
                    name: "Shared secret A".to_string(),
                    source_url: None,
                    configuration: json!({
                        "Id": first_tuner_id,
                        "Type": "xtream",
                        "Username": "shared-user",
                        "Password": "shared-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let reference = ProviderSecretReference::from_configuration(&first_configuration).unwrap();
        let mut second_configuration = first_configuration;
        second_configuration["Id"] = json!(second_tuner_id);
        let second_configuration = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: second_tuner_id.to_string(),
                    provider_type: "xtream".to_string(),
                    name: "Shared secret B".to_string(),
                    source_url: None,
                    configuration: second_configuration,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            ProviderSecretReference::from_configuration(&second_configuration)
                .unwrap()
                .id,
            reference.id
        );

        db.delete_live_tv_tuner_state(&first_tuner_id.to_ascii_uppercase())
            .await
            .unwrap();
        let envelope_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM provider_secrets WHERE secret_id = ?1")
                .bind(&reference.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(envelope_count, 1);
        let (_, credentials) = db
            .provider_credentials_for_configuration(&second_configuration)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username(), "shared-user");
        assert_eq!(credentials.password(), "shared-password");

        db.delete_live_tv_tuner_state(second_tuner_id)
            .await
            .unwrap();
        let envelope_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM provider_secrets WHERE secret_id = ?1")
                .bind(&reference.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(envelope_count, 0);
    }

    #[tokio::test]
    async fn sqlite_provider_secret_reconciliation_collects_historical_orphans_fail_closed() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let persisted = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: "reconciliation-live".to_string(),
                    provider_type: "xtream".to_string(),
                    name: "Reconciliation live".to_string(),
                    source_url: None,
                    configuration: json!({
                        "Id": "reconciliation-live",
                        "Type": "xtream",
                        "Username": "live-user",
                        "Password": "live-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let live_reference = ProviderSecretReference::from_configuration(&persisted).unwrap();
        let orphan = db
            .protect_provider_configuration(
                "xtream",
                json!({"Username": "orphan-user", "Password": "orphan-password"}),
            )
            .await
            .unwrap();
        let orphan_reference = ProviderSecretReference::from_configuration(&orphan).unwrap();
        assert_ne!(live_reference.id, orphan_reference.id);

        assert_eq!(db.reconcile_orphaned_provider_secrets().await.unwrap(), 1);
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
        assert_eq!(
            db.provider_credentials_for_configuration(&persisted)
                .await
                .unwrap()
                .unwrap()
                .1
                .username(),
            "live-user"
        );

        let retained_on_error = db
            .protect_provider_configuration(
                "xtream",
                json!({"Username": "retained-user", "Password": "retained-password"}),
            )
            .await
            .unwrap();
        let retained_reference =
            ProviderSecretReference::from_configuration(&retained_on_error).unwrap();
        sqlx::query(
            "INSERT INTO named_configurations (key, payload_json, updated_at) VALUES (?1, ?2, ?3)",
        )
        .bind("malformed-provider-reference")
        .bind(
            json!({
                "JellyrinProviderSecretRef": {
                    "Id": "unknown",
                    "Provider": "xtream"
                }
            })
            .to_string(),
        )
        .bind("2026-08-08T00:00:00Z")
        .execute(db.pool())
        .await
        .unwrap();

        assert!(db.reconcile_orphaned_provider_secrets().await.is_err());
        let retained_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM provider_secrets WHERE secret_id = ?1")
                .bind(&retained_reference.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(retained_count, 1);
    }

    #[tokio::test]
    async fn provider_secret_write_readiness_requires_an_explicit_vault() {
        let in_memory = Database::connect("sqlite::memory:").await.unwrap();
        in_memory
            .validate_provider_secret_write_readiness()
            .unwrap();
        assert!(
            in_memory
                .provider_credentials_for_configuration(&json!({"Username": "not-a-ref"}))
                .await
                .unwrap()
                .is_none()
        );

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("provider-write-readiness.db");
        std::fs::File::create(&path).unwrap();
        let database_url = format!("sqlite://{}", path.display());
        let persistent = Database::connect(&database_url).await.unwrap();
        assert!(
            persistent
                .validate_provider_secret_write_readiness()
                .unwrap_err()
                .to_string()
                .contains("cannot be stored")
        );
        persistent
            .with_provider_secret_vault(ProviderSecretVault::new("test", vec![0x74; 32]).unwrap())
            .validate_provider_secret_write_readiness()
            .unwrap();
    }

    #[tokio::test]
    async fn persistent_legacy_sqlite_uses_rollback_journal_until_wal_fix_is_pinned() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("rollback-journal.db");
        std::fs::File::create(&path).unwrap();
        let database_url = format!("sqlite://{}", path.display());
        let db = Database::connect(&database_url).await.unwrap();

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "delete");
    }

    #[tokio::test]
    async fn plugin_tuner_snapshot_encrypts_core_credentials_and_preserves_opaque_reference() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.validate_provider_secret_write_readiness().unwrap();
        let plugin_id = Uuid::new_v4();
        let provider_type = format!("plugin:{plugin_id}");
        let tuner_id = format!("magstv-plugin-tuner-{plugin_id}");
        let public_configuration = json!({
            "PluginId": plugin_id,
            "Provider": "MAGSTV",
            "PortalUrl": "https://magstv.invalid",
            "SecretReference": {
                "Namespace": "magstv",
                "Key": format!("tuners/{tuner_id}/credentials")
            }
        });

        let persisted = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: public_configuration.clone(),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();

        assert_eq!(persisted, public_configuration);
        let (stored_provider_type, stored_configuration): (String, String) = sqlx::query_as(
            "SELECT provider_type, configuration_json FROM live_tv_tuners WHERE tuner_id = ?1",
        )
        .bind(&tuner_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(stored_provider_type, provider_type);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored_configuration).unwrap(),
            public_configuration
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 0);

        let mut submitted = public_configuration.clone();
        submitted["Username"] = json!("magstv-user");
        submitted["Password"] = json!("magstv-password");
        let protected = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: submitted,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let reference = ProviderSecretReference::from_configuration(&protected).unwrap();
        assert_eq!(reference.provider_type, format!("plugin-{plugin_id}"));
        assert!(protected.get("Username").is_none());
        assert!(protected.get("Password").is_none());
        assert_eq!(
            protected["SecretReference"],
            public_configuration["SecretReference"]
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
        let (resolved_reference, credentials) = db
            .provider_credentials_for_configuration(&protected)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved_reference, reference);
        assert_eq!(credentials.username(), "magstv-user");
        assert_eq!(credentials.password(), "magstv-password");

        let partial = json!({
            "PluginId": plugin_id,
            "Provider": "MAGSTV",
            "SecretReference": public_configuration["SecretReference"].clone(),
            "Password": "updated-password"
        });
        let updated = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: partial,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let updated_reference = ProviderSecretReference::from_configuration(&updated).unwrap();
        assert_eq!(updated_reference.id, reference.id);
        assert_eq!(updated_reference.provider_type, reference.provider_type);
        assert_eq!(updated_reference.revision, reference.revision + 1);
        let (_, updated_credentials) = db
            .provider_credentials_for_configuration(&updated)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated_credentials.username(), "magstv-user");
        assert_eq!(updated_credentials.password(), "updated-password");

        sqlx::query(
            r#"
            CREATE TRIGGER reject_plugin_tuner_update
            BEFORE UPDATE ON live_tv_tuners
            BEGIN
                SELECT RAISE(ABORT, 'forced plugin tuner failure');
            END
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();
        let failed_update = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": plugin_id,
                        "Password": "must-roll-back"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await;
        assert!(failed_update.is_err());
        let after_rollback = db
            .live_tv_tuner_configuration_by_id(&tuner_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after_rollback, updated);
        let (reference_after_rollback, credentials_after_rollback) = db
            .provider_credentials_for_configuration(&after_rollback)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reference_after_rollback, updated_reference);
        assert_eq!(credentials_after_rollback.password(), "updated-password");

        let core_reference_result = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": plugin_id,
                        "JellyrinProviderSecretRef": {
                            "Id": "ps_foreign",
                            "Provider": "xtream",
                            "Revision": 1
                        }
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await;
        assert!(core_reference_result.is_err());
        assert_eq!(
            db.live_tv_tuner_configuration_by_id(&tuner_id)
                .await
                .unwrap(),
            Some(updated)
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn named_live_tv_plugin_credentials_use_the_canonical_vault_namespace() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let plugin_id = Uuid::new_v4();
        let opaque_reference = json!({
            "Namespace": "magstv",
            "Key": "tuners/named-plugin/credentials"
        });

        db.update_named_configuration(
            "livetv",
            json!({
                "TunerHosts": [{
                    "Id": "named-plugin",
                    "Type": "plugin",
                    "PluginId": plugin_id,
                    "SecretReference": opaque_reference,
                    "Username": "named-user",
                    "Password": "named-password"
                }]
            }),
        )
        .await
        .unwrap();

        let configuration = db.named_configuration("livetv").await.unwrap().unwrap();
        let host = &configuration["TunerHosts"][0];
        let reference = ProviderSecretReference::from_configuration(host).unwrap();
        assert_eq!(host["Type"], "plugin");
        assert_eq!(host["SecretReference"], opaque_reference);
        assert!(host.get("Username").is_none());
        assert!(host.get("Password").is_none());
        assert_eq!(reference.provider_type, format!("plugin-{plugin_id}"));
        let (_, credentials) = db
            .provider_credentials_for_configuration(host)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username(), "named-user");
        assert_eq!(credentials.password(), "named-password");

        db.update_named_configuration(
            "livetv",
            json!({
                "TunerHosts": [{
                    "Id": "named-plugin",
                    "Type": "plugin",
                    "PluginId": plugin_id,
                    "SecretReference": opaque_reference,
                    "PortalUrl": "https://updated.magstv.invalid"
                }]
            }),
        )
        .await
        .unwrap();
        let partially_updated = db.named_configuration("livetv").await.unwrap().unwrap();
        let partially_updated_host = &partially_updated["TunerHosts"][0];
        assert_eq!(
            ProviderSecretReference::from_configuration(partially_updated_host).unwrap(),
            reference
        );
        let (_, credentials) = db
            .provider_credentials_for_configuration(partially_updated_host)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username(), "named-user");
        assert_eq!(credentials.password(), "named-password");

        let malformed = db
            .update_named_configuration(
                "livetv",
                json!({
                    "TunerHosts": [{
                        "Id": "named-plugin",
                        "Type": "plugin",
                        "PluginId": plugin_id,
                        "JellyrinProviderSecretRef": {
                            "Id": "",
                            "Provider": format!("plugin-{plugin_id}"),
                            "Revision": 0
                        }
                    }]
                }),
            )
            .await;
        assert!(malformed.is_err());
        assert_eq!(
            db.named_configuration("livetv").await.unwrap().unwrap(),
            partially_updated
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn named_live_tv_rejects_core_secret_fields_without_a_supported_provider_type() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let plugin_id = Uuid::new_v4();
        let public_configuration = json!({
            "TunerHosts": [{
                "Id": "opaque-plugin-tuner",
                "Type": format!("plugin:{plugin_id}"),
                "PluginId": plugin_id,
                "SecretReference": {
                    "Namespace": "magstv",
                    "Key": "tuners/opaque-plugin-tuner/credentials"
                }
            }]
        });
        db.update_named_configuration("livetv", public_configuration.clone())
            .await
            .unwrap();
        assert_eq!(
            db.named_configuration("livetv").await.unwrap().unwrap(),
            public_configuration
        );
        assert_eq!(db.provider_secret_count().await.unwrap(), 0);

        let invalid_hosts = [
            json!({
                "Id": "missing-type-credentials",
                "Username": "must-not-persist",
                "Password": "must-not-persist"
            }),
            json!({
                "Id": "missing-type-reference",
                "JellyrinProviderSecretRef": {
                    "Id": "ps_must_not_persist",
                    "Provider": "xtream",
                    "Revision": 1
                }
            }),
            json!({
                "Id": "unknown-type-credentials",
                "Type": "magstv",
                "UserName": "must-not-persist",
                "Password": "must-not-persist"
            }),
            json!({
                "Id": "unknown-type-placeholder",
                "Type": "unsupported-provider",
                "Password": "********"
            }),
        ];
        for host in invalid_hosts {
            let result = db
                .update_named_configuration("livetv", json!({"TunerHosts": [host]}))
                .await;
            assert!(result.is_err());
            assert_eq!(
                db.named_configuration("livetv").await.unwrap().unwrap(),
                public_configuration
            );
            assert_eq!(db.provider_secret_count().await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn plugin_tuner_identity_change_requires_complete_replacement_credentials() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let first_plugin = Uuid::new_v4();
        let second_plugin = Uuid::new_v4();
        let tuner_id = "plugin-identity-change";
        let first = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.to_string(),
                    provider_type: format!("plugin:{first_plugin}"),
                    name: "First plugin".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": first_plugin,
                        "Username": "first-user",
                        "Password": "first-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let first_reference = ProviderSecretReference::from_configuration(&first).unwrap();

        let incomplete_change = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.to_string(),
                    provider_type: format!("plugin:{second_plugin}"),
                    name: "Second plugin".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": second_plugin,
                        "Password": "second-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await;
        assert!(
            incomplete_change
                .unwrap_err()
                .to_string()
                .contains("complete provider credentials")
        );
        assert_eq!(
            db.live_tv_tuner_configuration_by_id(tuner_id)
                .await
                .unwrap(),
            Some(first.clone())
        );

        let second = db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.to_string(),
                    provider_type: format!("plugin:{second_plugin}"),
                    name: "Second plugin".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": second_plugin,
                        "Username": "second-user",
                        "Password": "second-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let second_reference = ProviderSecretReference::from_configuration(&second).unwrap();
        assert_ne!(second_reference.id, first_reference.id);
        assert_eq!(
            second_reference.provider_type,
            format!("plugin-{second_plugin}")
        );
        let (_, credentials) = db
            .provider_credentials_for_configuration(&second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username(), "second-user");
        assert_eq!(credentials.password(), "second-password");
        assert_eq!(db.provider_secret_count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn tuner_and_named_writers_rollback_their_secret_envelopes_on_failure() {
        let tuner_db = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER reject_tuner_configuration
            BEFORE INSERT ON live_tv_tuners
            BEGIN
                SELECT RAISE(ABORT, 'forced tuner failure');
            END
            "#,
        )
        .execute(tuner_db.pool())
        .await
        .unwrap();
        let tuner_result = tuner_db
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: "rollback-tuner".to_string(),
                    provider_type: "xtream".to_string(),
                    name: "Rollback tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "Username": "tuner-user",
                        "Password": "tuner-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await;
        assert!(tuner_result.is_err());
        assert_eq!(tuner_db.provider_secret_count().await.unwrap(), 0);

        let named_db = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER reject_named_configuration
            BEFORE INSERT ON named_configurations
            WHEN NEW.key = 'livetv'
            BEGIN
                SELECT RAISE(ABORT, 'forced named configuration failure');
            END
            "#,
        )
        .execute(named_db.pool())
        .await
        .unwrap();
        let named_result = named_db
            .update_named_configuration(
                "livetv",
                json!({
                    "TunerHosts": [{
                        "Id": "rollback-named",
                        "Type": "xtream",
                        "Username": "named-user",
                        "Password": "named-password"
                    }]
                }),
            )
            .await;
        assert!(named_result.is_err());
        assert_eq!(named_db.provider_secret_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn legacy_backfill_rolls_back_envelope_and_earlier_rewrites_on_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let raw = json!({
            "Id": "xtream-plugin",
            "Type": "xtream",
            "Url": "https://provider.invalid",
            "Username": "rollback-user",
            "Password": "rollback-password"
        });
        let now = "2026-08-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO plugin_configurations (plugin_id, configuration_json, updated_at) VALUES (?1, ?2, ?3)",
        )
        .bind("jellyrin-xtream-provider")
        .bind(raw.to_string())
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO named_configurations (key, payload_json, updated_at) VALUES ('livetv', ?1, ?2)",
        )
        .bind(json!({ "TunerHosts": [raw] }).to_string())
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER reject_livetv_backfill
            BEFORE UPDATE OF payload_json ON named_configurations
            WHEN NEW.key = 'livetv'
            BEGIN
                SELECT RAISE(ABORT, 'forced backfill failure');
            END
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        assert!(db.backfill_legacy_provider_secrets().await.is_err());
        assert_eq!(db.provider_secret_count().await.unwrap(), 0);
        let persisted_plugin = db
            .plugin_configuration_json("jellyrin-xtream-provider")
            .await
            .unwrap()
            .unwrap();
        let persisted_named = db.named_configuration("livetv").await.unwrap().unwrap();
        assert_eq!(persisted_plugin["Password"], "rollback-password");
        assert_eq!(
            persisted_named["TunerHosts"][0]["Password"],
            "rollback-password"
        );
    }

    #[tokio::test]
    async fn provider_key_rotation_is_all_or_nothing() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for suffix in ["a", "b"] {
            db.protect_provider_configuration(
                "xtream",
                json!({
                    "Username": format!("rotation-{suffix}"),
                    "Password": format!("secret-{suffix}")
                }),
            )
            .await
            .unwrap();
        }
        let revisions_before = sqlx::query_as::<_, (String, i64)>(
            "SELECT key_id, revision FROM provider_secrets ORDER BY secret_id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER reject_last_secret_rotation
            BEFORE UPDATE ON provider_secrets
            WHEN OLD.secret_id = (SELECT max(secret_id) FROM provider_secrets)
            BEGIN
                SELECT RAISE(ABORT, 'forced rotation failure');
            END
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();
        let rotated_vault = ProviderSecretVault::new("test-v2", vec![0x6b; 32])
            .unwrap()
            .with_decryption_key("test-v1", vec![0x5a; 32])
            .unwrap();
        let rotated_db = db.clone().with_provider_secret_vault(rotated_vault);

        assert!(
            rotated_db
                .rotate_provider_secrets_to_active_key()
                .await
                .is_err()
        );
        let revisions_after = sqlx::query_as::<_, (String, i64)>(
            "SELECT key_id, revision FROM provider_secrets ORDER BY secret_id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(revisions_after, revisions_before);
    }

    #[tokio::test]
    async fn sqlite_catalog_page_keeps_exact_total_and_batches_user_data() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db.first_user().await.unwrap();
        let alpha_id = Uuid::new_v4();
        let beta_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Catalog",
            "movies",
            "provider://catalog",
            vec![
                RemoteMediaItemUpsert {
                    id: alpha_id.to_string(),
                    name: "Alpha Feature".to_string(),
                    path: "provider://catalog/alpha.mkv".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: Some(100),
                    bitrate: Some(1_000),
                    width: Some(3840),
                    height: Some(2160),
                    media_streams: vec![
                        json!({"Type": "Video"}),
                        json!({"Type": "Audio", "Language": "fre"}),
                        json!({"Type": "Subtitle", "Language": "spa"}),
                    ],
                    metadata: json!({
                        "Overview": "A hidden needle",
                        "Album": "Alpha Album",
                        "Artists": [{"Name": "Artist Two", "Id": "forbidden-id"}],
                        "Genres": [{"Name": "Drama", "Id": "Imported-Drama"}],
                        "seriesgenres": [{"Id": "Id-Only-Genre"}],
                        "pEoPlE": [{"Name": "Jane Doe", "Id": "Imported-Person"}],
                        "SeriesPeople": ["Suppressed Person"],
                        "Studios": [
                            {"Id": "Studio-Id-Only"},
                            {"Name": "HBO", "Id": "Imported-HBO"}
                        ],
                        "Tags": ["Featured"]
                    }),
                },
                RemoteMediaItemUpsert {
                    id: beta_id.to_string(),
                    name: "Beta Feature".to_string(),
                    path: "provider://catalog/beta.mp4".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: Some(200),
                    bitrate: Some(2_000),
                    width: Some(1920),
                    height: Some(1080),
                    media_streams: vec![json!({"Type": "Video"})],
                    // Searching metadata must inspect scalar values, not object keys.
                    metadata: json!({
                        "Needle": "absent",
                        "AlbumName": "100%_\\ Mix",
                        "Artists": ["Artist One"],
                        "Genres": ["Comedy"],
                        "People": ["Other Person"],
                        "Studios": ["Other Studio"],
                        "Tags": ["Archive"]
                    }),
                },
            ],
        )
        .await
        .unwrap();

        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: alpha_id,
            media_source_id: Some(alpha_id.to_string()),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(2),
            position_ticks: 90,
            is_paused: false,
            played: true,
        })
        .await
        .unwrap();

        let page = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                start_index: 1,
                limit: 1,
                search_term: Some("feature".to_string()),
                include_item_types: vec!["Movie".to_string()],
                user_id: Some(user.id),
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total_record_count, 2);
        assert_eq!(page.start_index, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].item.id, beta_id);
        assert!(page.items[0].playback_state.is_none());

        let count_only = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 0,
                search_term: Some("feature".to_string()),
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(count_only.total_record_count, 2);
        assert!(count_only.items.is_empty());

        let metadata_search = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                search_term: Some("needle".to_string()),
                search_scope: MediaItemCatalogSearchScope::AllMetadataScalars,
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(metadata_search.total_record_count, 1);
        assert_eq!(metadata_search.items[0].item.id, alpha_id);
        assert_eq!(
            metadata_search.items[0].metadata["Overview"],
            "A hidden needle"
        );

        let hint_page = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 1,
                search_term: Some("artist".to_string()),
                search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(hint_page.total_record_count, 2);
        assert_eq!(hint_page.items.len(), 1);
        assert_eq!(hint_page.items[0].item.id, alpha_id);
        for excluded_term in ["hidden needle", "forbidden-id", "Needle"] {
            let excluded = db
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    search_term: Some(excluded_term.to_string()),
                    search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                    ..MediaItemCatalogQuery::default()
                })
                .await
                .unwrap();
            assert_eq!(excluded.total_record_count, 0, "term={excluded_term}");
        }
        let literal_wildcards = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                search_term: Some("%_\\".to_string()),
                search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(literal_wildcards.total_record_count, 1);
        assert_eq!(literal_wildcards.items[0].item.id, beta_id);

        for selector in [
            "drama".to_string(),
            jellyrin_core::stable_entity_id("Genre", "Drama"),
            "imported-drama".to_string(),
            "id-only-genre".to_string(),
        ] {
            let genre = db
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    genre_ids: vec![selector],
                    ..MediaItemCatalogQuery::default()
                })
                .await
                .unwrap();
            assert_eq!(genre.total_record_count, 1);
            assert_eq!(genre.items[0].item.id, alpha_id);
        }
        let genre_or_page = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                start_index: 1,
                limit: 1,
                genre_ids: vec!["DRAMA".to_string(), "comedy".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(genre_or_page.total_record_count, 2);
        assert_eq!(genre_or_page.items[0].item.id, beta_id);

        for (field, selector) in [
            ("person", "jane doe".to_string()),
            (
                "person",
                jellyrin_core::stable_entity_id("Person", "Jane Doe"),
            ),
            ("person", "imported-person".to_string()),
            ("studio", "hbo".to_string()),
            ("studio", jellyrin_core::stable_entity_id("Studio", "HBO")),
            ("studio", "imported-hbo".to_string()),
            ("studio", "studio-id-only".to_string()),
            ("tag", "featured".to_string()),
        ] {
            let mut filter = MediaItemCatalogQuery {
                limit: 10,
                ..MediaItemCatalogQuery::default()
            };
            match field {
                "person" => filter.person_ids.push(selector),
                "studio" => filter.studio_ids.push(selector),
                "tag" => filter.tags.push(selector),
                _ => unreachable!(),
            }
            let page = db.media_item_catalog_page(&filter).await.unwrap();
            assert_eq!(page.total_record_count, 1, "field={field}");
            assert_eq!(page.items[0].item.id, alpha_id, "field={field}");
        }
        let combined = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                person_ids: vec!["Jane Doe".to_string()],
                studio_ids: vec!["HBO".to_string()],
                tags: vec!["FEATURED".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(combined.total_record_count, 1);
        assert_eq!(combined.items[0].item.id, alpha_id);
        for filter in [
            MediaItemCatalogQuery {
                limit: 10,
                person_ids: vec!["Suppressed Person".to_string()],
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                limit: 10,
                person_ids: vec!["Jane Doe".to_string()],
                tags: vec!["Archive".to_string()],
                ..MediaItemCatalogQuery::default()
            },
        ] {
            assert_eq!(
                db.media_item_catalog_page(&filter)
                    .await
                    .unwrap()
                    .total_record_count,
                0
            );
        }

        let oversized_genres = MediaItemCatalogQuery {
            genre_ids: (0..=MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS)
                .map(|index| format!("genre-{index}"))
                .collect(),
            ..MediaItemCatalogQuery::default()
        };
        assert!(db.media_item_catalog_page(&oversized_genres).await.is_err());
        assert!(
            db.media_item_catalog_counts(&oversized_genres)
                .await
                .is_err()
        );
        assert!(
            db.media_item_query_filter_values(
                &oversized_genres,
                MediaItemQueryFilterSelection::ALL,
            )
            .await
            .is_err()
        );
        let duplicate_genres = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                genre_ids: vec!["Drama".to_string(); MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS + 1],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(duplicate_genres.total_record_count, 1);
        for oversized in [
            MediaItemCatalogQuery {
                person_ids: (0..=MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS)
                    .map(|index| format!("person-{index}"))
                    .collect(),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                studio_ids: (0..=MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS)
                    .map(|index| format!("studio-{index}"))
                    .collect(),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                tags: (0..=MEDIA_ITEM_CATALOG_MAX_FACET_SELECTORS)
                    .map(|index| format!("tag-{index}"))
                    .collect(),
                ..MediaItemCatalogQuery::default()
            },
        ] {
            assert!(db.media_item_catalog_page(&oversized).await.is_err());
        }

        let french_audio = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                audio_languages: vec!["fra".to_string()],
                has_subtitles: Some(true),
                is_4k: Some(true),
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(french_audio.total_record_count, 1);
        assert_eq!(french_audio.items[0].item.id, alpha_id);

        let played = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                user_id: Some(user.id),
                is_played: Some(true),
                favorite: Some(MediaItemFavoriteFilter::Favorite(false)),
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(played.total_record_count, 1);
        assert!(
            played.items[0]
                .playback_state
                .as_ref()
                .is_some_and(|state| state.played)
        );

        let batched = db
            .playback_states_for_items(user.id, &[alpha_id, beta_id])
            .await
            .unwrap();
        assert_eq!(batched.len(), 1);
        assert_eq!(batched[0].item_id, alpha_id);
    }

    #[tokio::test]
    async fn sqlite_catalog_advanced_metadata_filters_are_sql_pushed_down() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let matching_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Advanced Filters",
            "movies",
            "provider://advanced-filters",
            vec![
                RemoteMediaItemUpsert {
                    id: matching_id.to_string(),
                    name: "Matching Movie".to_string(),
                    path: "provider://advanced-filters/matching.mkv".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: Some(1920),
                    height: Some(1080),
                    media_streams: Vec::new(),
                    metadata: json!({
                        "OfficialRating": "PG-13",
                        "SeriesStatus": "Continuing",
                        "ProductionYear": 2025,
                        "RemoteTrailers": [{"Url": "https://example.invalid/trailer"}],
                        "sErIeSoVeRvIeW": "Present",
                        "ProviderIds": {"IMDb": "tt123", "Tmdb": "456"},
                        "SeriesProviderIds": {"TVDB": "789"},
                        "LockData": true,
                        "CommunityRating": 8.5,
                        "CriticRating": "75",
                        "PremiereDate": "2025-02-03T04:05:06Z"
                    }),
                },
                RemoteMediaItemUpsert {
                    id: Uuid::new_v4().to_string(),
                    name: "Non Matching Movie".to_string(),
                    path: "provider://advanced-filters/other.mkv".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: Some(1280),
                    height: Some(720),
                    media_streams: Vec::new(),
                    metadata: json!({
                        "SeriesStatus": "Ended",
                        "ProductionYear": 2020,
                        "CommunityRating": 5.0,
                        "CriticRating": 40,
                        "PremiereDate": "2020-01-01T00:00:00Z"
                    }),
                },
            ],
        )
        .await
        .unwrap();

        let threshold = OffsetDateTime::parse("2025-01-01T00:00:00Z", &Rfc3339).unwrap();
        let filters = [
            MediaItemCatalogQuery {
                official_ratings: vec!["pg-13".to_string()],
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                series_statuses: vec!["continuing".to_string()],
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                years: vec!["2025".to_string()],
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_trailer: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_overview: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_imdb_id: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_tmdb_id: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_tvdb_id: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                has_official_rating: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                is_locked: Some(true),
                ..MediaItemCatalogQuery::default()
            },
            MediaItemCatalogQuery {
                min_community_rating: Some(8.0),
                max_community_rating: Some(9.0),
                min_critic_rating: Some(70.0),
                max_critic_rating: Some(80.0),
                min_premiere_date: Some(threshold),
                ..MediaItemCatalogQuery::default()
            },
        ];
        for filter in filters {
            let page = db
                .media_item_catalog_page(&filter)
                .await
                .unwrap_or_else(|error| panic!("filter={filter:?}: {error:#}"));
            assert_eq!(page.total_record_count, 1, "filter={filter:?}");
            assert_eq!(page.items[0].item.id, matching_id, "filter={filter:?}");
        }
    }

    #[tokio::test]
    async fn sqlite_next_up_candidates_filter_played_and_unrelated_items_in_sql() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db.create_user("next-up-sqlite", None).await.unwrap();
        let played_id = Uuid::new_v4();
        let unplayed_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Next Up Shows",
            "tvshows",
            "provider://next-up",
            vec![
                RemoteMediaItemUpsert {
                    id: played_id.to_string(),
                    name: "SQL Show S01E01".to_string(),
                    path: "provider://next-up/SQL Show/Season 01/SQL Show S01E01.mp4".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "tvshows".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata: json!({"SeriesName": "SQL Show"}),
                },
                RemoteMediaItemUpsert {
                    id: unplayed_id.to_string(),
                    name: "SQL Show S01E02".to_string(),
                    path: "provider://next-up/SQL Show/Season 01/SQL Show S01E02.mp4".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "tvshows".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata: json!({"SeriesName": "SQL Show"}),
                },
            ],
        )
        .await
        .unwrap();
        db.replace_remote_media_library_snapshot(
            "Unrelated Movies",
            "movies",
            "provider://movies",
            vec![RemoteMediaItemUpsert {
                id: Uuid::new_v4().to_string(),
                name: "Must Not Leak".to_string(),
                path: "provider://movies/leak.mp4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({}),
            }],
        )
        .await
        .unwrap();
        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: played_id,
            media_source_id: None,
            audio_stream_index: None,
            subtitle_stream_index: None,
            position_ticks: 0,
            is_paused: false,
            played: true,
        })
        .await
        .unwrap();

        let candidates = db.tv_next_up_candidates(user.id).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].item.id, unplayed_id);
        assert_eq!(candidates[0].metadata["SeriesName"], "SQL Show");
        assert!(candidates[0].playback_state.is_none());
    }

    #[tokio::test]
    async fn sqlite_upcoming_candidates_scope_tv_videos_and_include_metadata() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let now = time::OffsetDateTime::parse(
            "2000-01-01T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let premiere_id = Uuid::new_v4();
        let air_id = Uuid::new_v4();
        let created_id = Uuid::new_v4();
        let undated_id = Uuid::new_v4();
        let invalid_precedence_id = Uuid::new_v4();
        let equal_id = Uuid::new_v4();
        let extra_id = Uuid::new_v4();
        let audio_id = Uuid::new_v4();
        let item =
            |id: Uuid, name: &str, path: &str, media_type: &str, metadata: serde_json::Value| {
                RemoteMediaItemUpsert {
                    id: id.to_string(),
                    name: name.to_string(),
                    path: format!("provider://upcoming/{path}"),
                    media_type: media_type.to_string(),
                    collection_type: "tvshows".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata,
                }
            };
        db.replace_remote_media_library_snapshot(
            "Upcoming Shows",
            "tvshows",
            "provider://upcoming",
            vec![
                item(
                    premiere_id,
                    "Example Show S01E01",
                    "Example Show/Season 01/Example Show S01E01.mp4",
                    "Video",
                    json!({"PremiereDate": "2000-01-01T02:00:00Z"}),
                ),
                item(
                    air_id,
                    "Example Show S01E02",
                    "Example Show/Season 01/Example Show S01E02.mp4",
                    "Video",
                    json!({"AirDate": "2000-01-01T01:00:00Z"}),
                ),
                item(
                    created_id,
                    "Example Show S01E03",
                    "Example Show/Season 01/Example Show S01E03.mp4",
                    "Video",
                    json!({"DateCreated": "2000-01-01T03:00:00Z"}),
                ),
                item(
                    undated_id,
                    "Example Show S01E04",
                    "Example Show/Season 01/Example Show S01E04.mp4",
                    "Video",
                    json!({"SeriesName": "Example Show"}),
                ),
                item(
                    invalid_precedence_id,
                    "Example Show S01E05",
                    "Example Show/Season 01/Example Show S01E05.mp4",
                    "Video",
                    json!({
                        "PremiereDate": "invalid",
                        "AirDate": "2000-01-01T04:00:00Z"
                    }),
                ),
                item(
                    equal_id,
                    "Example Show S01E06",
                    "Example Show/Season 01/Example Show S01E06.mp4",
                    "Video",
                    json!({"PremiereDate": "2000-01-01T00:00:00Z"}),
                ),
                item(
                    extra_id,
                    "Behind the Scenes",
                    "Example Show/Season 01/extras/Behind the Scenes.mp4",
                    "Video",
                    json!({"PremiereDate": "2000-01-01T05:00:00Z"}),
                ),
                item(
                    audio_id,
                    "Example Show Theme",
                    "Example Show Theme.flac",
                    "Audio",
                    json!({"PremiereDate": "2000-01-01T05:00:00Z"}),
                ),
            ],
        )
        .await
        .unwrap();
        db.replace_remote_media_library_snapshot(
            "Unrelated Movies",
            "movies",
            "provider://movies",
            vec![RemoteMediaItemUpsert {
                id: Uuid::new_v4().to_string(),
                name: "Future Movie".to_string(),
                path: "provider://movies/future.mp4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({"PremiereDate": "2000-01-01T05:00:00Z"}),
            }],
        )
        .await
        .unwrap();

        let candidates = db.tv_upcoming_candidates(now).await.unwrap();
        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.item.id)
            .collect::<HashSet<_>>();
        assert_eq!(
            candidate_ids,
            HashSet::from([premiere_id, air_id, created_id])
        );
        let air = candidates
            .iter()
            .find(|candidate| candidate.item.id == air_id)
            .unwrap();
        assert_eq!(air.metadata["AirDate"], "2000-01-01T01:00:00Z");
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.playback_state.is_none())
        );
        let telemetry = db.telemetry_diagnostics();
        let operation = telemetry
            .operations
            .iter()
            .find(|operation| operation.name == "catalog.upcoming_candidates")
            .unwrap();
        assert_eq!(
            (operation.calls, operation.succeeded, operation.rows.total),
            (1, 1, 3)
        );
    }

    #[tokio::test]
    async fn sqlite_tv_series_lookup_candidates_exclude_unrelated_catalog_and_include_metadata() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let empty_page = db
            .tv_series_catalog_page(None, 0, 20)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(empty_page.total_record_count, 0);
        assert!(empty_page.episodes.is_empty());
        let movies = (0..512)
            .map(|index| RemoteMediaItemUpsert {
                id: Uuid::new_v4().to_string(),
                name: format!("Movie {index:04}"),
                path: format!("provider://movies/{index}.mp4"),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({"SeriesId": Uuid::new_v4().to_string()}),
            })
            .collect();
        db.replace_remote_media_library_snapshot(
            "Many Movies",
            "movies",
            "provider://movies",
            movies,
        )
        .await
        .unwrap();
        let episode_id = Uuid::new_v4();
        let canonical_series_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Shows",
            "tvshows",
            "provider://shows",
            vec![RemoteMediaItemUpsert {
                id: episode_id.to_string(),
                name: "Example Show S01E01".to_string(),
                path: "provider://shows/Example Show/Season 01/Example Show S01E01.mp4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "tvshows".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({
                    "SeriesId": canonical_series_id.simple().to_string(),
                    "SeriesName": "Example Show"
                }),
            }],
        )
        .await
        .unwrap();

        let candidates = db.tv_series_lookup_candidates().await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].item.id, episode_id);
        assert_eq!(
            candidates[0].metadata["SeriesId"],
            canonical_series_id.simple().to_string()
        );
        assert_eq!(candidates[0].metadata["SeriesName"], "Example Show");
        assert!(candidates[0].playback_state.is_none());
        let page = db
            .tv_series_catalog_page(None, 0, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page.total_record_count, 1);
        assert_eq!(page.series.len(), 1);
        assert_eq!(page.series[0].id, canonical_series_id.simple().to_string());
        assert_eq!(page.series[0].name, "Example Show");
        assert_eq!(page.episodes.len(), 1);
        assert_eq!(page.episodes[0].item.id, episode_id);
        let searched = db
            .tv_series_catalog_search_page(
                None,
                0,
                20,
                TvSeriesCatalogNameFilter {
                    search_term: Some("example".to_string()),
                    ..TvSeriesCatalogNameFilter::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(searched.total_record_count, 1);
        assert_eq!(searched.series.len(), 1);
        assert_eq!(searched.series[0].name, "Example Show");
        let letter_page = db
            .tv_series_catalog_search_page(
                None,
                0,
                20,
                TvSeriesCatalogNameFilter {
                    starts_with: Some("E".to_string()),
                    starts_with_or_greater: Some("E".to_string()),
                    less_than: Some("F".to_string()),
                    ..TvSeriesCatalogNameFilter::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(letter_page.total_record_count, 1);
        assert_eq!(letter_page.series[0].name, "Example Show");
        let no_match = db
            .tv_series_catalog_search_page(
                None,
                0,
                20,
                TvSeriesCatalogNameFilter {
                    search_term: Some("missing".to_string()),
                    ..TvSeriesCatalogNameFilter::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(no_match.total_record_count, 0);
        assert!(no_match.series.is_empty());
        assert!(no_match.episodes.is_empty());
        let empty = db
            .tv_series_catalog_page(None, 1, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(empty.total_record_count, 1);
        assert!(empty.series.is_empty());
        assert!(empty.episodes.is_empty());
    }

    #[tokio::test]
    async fn sqlite_tv_series_projection_preserves_empty_series_anchors_without_duplicates() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let populated_series_id = Uuid::new_v4();
        let empty_series_id = Uuid::new_v4();
        let item = |id: Uuid, name: &str, media_type: &str, metadata: serde_json::Value| {
            RemoteMediaItemUpsert {
                id: id.to_string(),
                name: name.to_string(),
                path: format!("plugin-vod://test/{id}"),
                media_type: media_type.to_string(),
                collection_type: "tvshows".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata,
            }
        };
        let episode_id = Uuid::new_v4();
        let folder = db
            .replace_remote_media_library_snapshot(
                "Anchored Shows",
                "tvshows",
                "plugin-vod://test/series",
                vec![
                    item(
                        Uuid::new_v4(),
                        "Populated",
                        "Series",
                        json!({
                            "PluginVodKind": "series",
                            "SeriesId": populated_series_id.simple().to_string(),
                            "SeriesName": "Populated"
                        }),
                    ),
                    item(
                        episode_id,
                        "Populated S01E01",
                        "Video",
                        json!({
                            "SeriesId": populated_series_id.simple().to_string(),
                            "SeriesName": "Provider Episode Alias"
                        }),
                    ),
                    item(
                        Uuid::new_v4(),
                        "Empty",
                        "Series",
                        json!({
                            "PluginVodKind": "series",
                            "SeriesId": empty_series_id.simple().to_string(),
                            "SeriesName": "Empty"
                        }),
                    ),
                ],
            )
            .await
            .unwrap();

        let coverage = sqlx::query_as::<_, (i64, i64)>(
            "SELECT episode_count, series_count FROM media_item_tv_series_coverage \
             WHERE virtual_folder_id = ?1",
        )
        .bind(folder.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(coverage, (1, 2));
        let projected = db
            .tv_series_catalog_page(Some(folder.id), 0, 20)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projected.total_record_count, 2);
        assert_eq!(projected.series.len(), 2);
        assert_eq!(projected.episodes.len(), 1);
        assert_eq!(projected.episodes[0].item.id, episode_id);
        assert_eq!(
            projected
                .series
                .iter()
                .find(|series| series.id == populated_series_id.simple().to_string())
                .unwrap()
                .name,
            "Populated"
        );

        sqlx::query("DELETE FROM media_item_tv_series_coverage WHERE virtual_folder_id = ?1")
            .bind(folder.id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        let live = db
            .tv_series_catalog_page(Some(folder.id), 0, 20)
            .await
            .unwrap()
            .expect("anchors must also be visible before projection is republished");
        assert_eq!(live.total_record_count, 2);
        assert_eq!(live.series.len(), 2);
        assert_eq!(live.episodes.len(), 1);
        assert_eq!(
            live.series
                .iter()
                .find(|series| series.id == populated_series_id.simple().to_string())
                .unwrap()
                .name,
            "Populated"
        );

        let empty_candidates = db
            .tv_series_lookup_candidates_for_series(&empty_series_id.simple().to_string())
            .await
            .unwrap();
        assert_eq!(empty_candidates.len(), 1);
        assert_eq!(empty_candidates[0].item.media_type, "Series");
    }

    #[tokio::test]
    async fn sqlite_tv_series_projection_is_atomic_fail_closed_and_collision_safe() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let series_id = Uuid::new_v4();
        let first_episode_id = Uuid::new_v4();
        let episode = |id: Uuid, name: &str, series_name: &str, root: &str| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: name.to_string(),
            path: format!("provider://{root}/{name}.mp4"),
            media_type: "Video".to_string(),
            collection_type: "tvshows".to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({
                "SeriesId": series_id.simple().to_string(),
                "SeriesName": series_name
            }),
        };
        let first = episode(first_episode_id, "Projected S01E01", "Projected", "first");
        let first_folder = db
            .replace_remote_media_library_snapshot(
                "Projected Shows",
                "tvshows",
                "provider://first",
                vec![first.clone()],
            )
            .await
            .unwrap();

        let projection_counts = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT coverage.episode_count, coverage.series_count, count(member.item_id) \
             FROM media_item_tv_series_coverage AS coverage \
             LEFT JOIN media_item_tv_series_members AS member \
               ON member.virtual_folder_id = coverage.virtual_folder_id \
             WHERE coverage.virtual_folder_id = ?1 \
             GROUP BY coverage.episode_count, coverage.series_count",
        )
        .bind(first_folder.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(projection_counts, (1, 1, 1));

        sqlx::query("UPDATE media_items SET name = name || ' changed' WHERE id = ?1")
            .bind(first_episode_id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        // The invalidation trigger drops the coverage row on any media_items change. A stale
        // projection must still serve a bounded page computed from the live rows instead of pushing
        // the caller onto the legacy path that materializes every episode.
        let stale = db
            .tv_series_catalog_page(Some(first_folder.id), 0, 20)
            .await
            .unwrap()
            .expect("stale coverage must still serve a bounded live page");
        assert_eq!(stale.total_record_count, 1);
        assert_eq!(stale.series.len(), 1);
        assert_eq!(stale.series[0].id, series_id.simple().to_string());
        assert_eq!(stale.series[0].name, "Projected");
        assert_eq!(stale.episodes.len(), 1);
        assert_eq!(stale.episodes[0].item.id, first_episode_id);

        db.replace_remote_media_library_snapshot(
            "Projected Shows",
            "tvshows",
            "provider://first",
            vec![first],
        )
        .await
        .unwrap();
        assert!(
            db.tv_series_catalog_page(Some(first_folder.id), 0, 20)
                .await
                .unwrap()
                .is_some()
        );

        let second_folder = db
            .replace_remote_media_library_snapshot(
                "Second Shows",
                "tvshows",
                "provider://second",
                vec![episode(
                    Uuid::new_v4(),
                    "Projected S02E01",
                    "Projected",
                    "second",
                )],
            )
            .await
            .unwrap();
        assert!(
            db.tv_series_catalog_page(None, 0, 20)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.tv_series_catalog_page(Some(second_folder.id), 0, 20)
                .await
                .unwrap()
                .is_some()
        );

        let inconsistent_folder = db
            .replace_remote_media_library_snapshot(
                "Inconsistent Shows",
                "tvshows",
                "provider://inconsistent",
                vec![
                    episode(Uuid::new_v4(), "Conflict S01E01", "One", "inconsistent"),
                    episode(Uuid::new_v4(), "Conflict S01E02", "Two", "inconsistent"),
                ],
            )
            .await
            .unwrap();
        assert!(
            db.tv_series_catalog_page(Some(inconsistent_folder.id), 0, 20)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn sqlite_effective_type_candidates_are_exact_and_include_visible_metadata() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let movie_id = Uuid::new_v4();
        let audio_id = Uuid::new_v4();
        let extra_id = Uuid::new_v4();
        let hidden_extra_id = Uuid::new_v4();
        let item = |id: Uuid,
                    name: &str,
                    path: &str,
                    media_type: &str,
                    collection_type: &str,
                    marker: &str| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            media_type: media_type.to_string(),
            collection_type: collection_type.to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({"Marker": marker}),
        };
        db.replace_remote_media_library_snapshot(
            "Typed Candidates",
            "mixed",
            "provider://typed",
            vec![
                item(
                    Uuid::new_v4(),
                    "Episode",
                    "provider://typed/show/season/episode.mkv",
                    "Video",
                    "tvshows",
                    "excluded",
                ),
                item(
                    movie_id,
                    "alpha",
                    "provider://typed/alpha.mp4",
                    "Video",
                    "movies",
                    "movie",
                ),
                item(
                    audio_id,
                    "Beta",
                    "provider://typed/beta.flac",
                    "Audio",
                    "music",
                    "audio",
                ),
                item(
                    extra_id,
                    "Final Extra",
                    "provider://typed/show/Season 01/ Extras /clip.mkv",
                    "Video",
                    "tvshows",
                    "extra",
                ),
                item(
                    hidden_extra_id,
                    "Hidden Extra",
                    "provider://typed/show/Featurettes/hidden.mkv",
                    "Video",
                    "tvshows",
                    "hidden",
                ),
            ],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE media_items SET missing_since = ?1 WHERE id = ?2")
            .bind("2026-08-09T00:00:00Z")
            .bind(hidden_extra_id.to_string())
            .execute(db.pool())
            .await
            .unwrap();

        let candidates = db
            .media_items_with_metadata_by_effective_types(&[
                "aUdIo".to_string(),
                "MOVIE".to_string(),
                "Video".to_string(),
            ])
            .await
            .unwrap();
        let by_id = candidates
            .iter()
            .map(|entry| (entry.item.id, &entry.metadata))
            .collect::<HashMap<_, _>>();
        assert_eq!(
            by_id.keys().copied().collect::<HashSet<_>>(),
            HashSet::from([movie_id, audio_id, extra_id])
        );
        assert_eq!(by_id[&movie_id]["Marker"], "movie");
        assert_eq!(by_id[&audio_id]["Marker"], "audio");
        assert_eq!(by_id[&extra_id]["Marker"], "extra");
        assert!(
            candidates
                .iter()
                .all(|entry| entry.playback_state.is_none())
        );
        assert!(
            db.media_items_with_metadata_by_effective_types(&[])
                .await
                .unwrap()
                .is_empty()
        );
        let telemetry = db.telemetry_diagnostics();
        let operation = telemetry
            .operations
            .iter()
            .find(|operation| {
                operation.name == "catalog.effective_type_candidates"
                    && operation.pool == DatabasePoolRole::Api
            })
            .expect("effective-type candidate telemetry");
        assert_eq!((operation.calls, operation.succeeded), (2, 2));
        assert_eq!(operation.rows.total, 3);
    }

    #[tokio::test]
    async fn sqlite_visible_item_point_contract_accepts_both_storage_id_forms() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let simple_id = Uuid::new_v4();
        let hyphenated_id = Uuid::new_v4();
        let missing_id = Uuid::new_v4();
        let item = |id: Uuid, name: &str| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: name.to_string(),
            path: format!("provider://point/{id}.mp4"),
            media_type: "Video".to_string(),
            collection_type: "movies".to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({}),
        };
        db.replace_remote_media_library_snapshot(
            "Point Lookups",
            "movies",
            "provider://point",
            vec![
                item(simple_id, "Simple"),
                item(hyphenated_id, "Hyphenated"),
                item(missing_id, "Missing"),
            ],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE media_items SET id = ?1 WHERE id = ?2")
            .bind(simple_id.simple().to_string())
            .bind(simple_id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                created_at, updated_at, last_seen_at, missing_since, file_size, modified_at,
                runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
            )
            SELECT ?1, virtual_folder_id, 'Hyphenated Twin', ?2, media_type, collection_type,
                   created_at, updated_at, last_seen_at, missing_since, file_size, modified_at,
                   runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
            FROM media_items WHERE id = ?3
            "#,
        )
        .bind(simple_id.to_string())
        .bind("provider://point/simple-twin.mp4")
        .bind(simple_id.simple().to_string())
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE media_items SET missing_since = CURRENT_TIMESTAMP WHERE id = ?1")
            .bind(missing_id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();

        for (id, expected_name) in [(simple_id, "Simple"), (hyphenated_id, "Hyphenated")] {
            assert!(db.media_item_exists(id).await.unwrap());
            assert_eq!(
                db.media_item_by_id_visible(id).await.unwrap().unwrap().name,
                expected_name
            );
        }
        assert_eq!(
            db.media_item_by_id_visible(simple_id)
                .await
                .unwrap()
                .unwrap()
                .path,
            format!("provider://point/{simple_id}.mp4"),
            "the legacy simple storage id must win when both text forms exist"
        );
        assert!(!db.media_item_exists(missing_id).await.unwrap());
        assert!(
            db.media_item_by_id_visible(missing_id)
                .await
                .unwrap()
                .is_none()
        );
        let absent_id = Uuid::new_v4();
        assert!(!db.media_item_exists(absent_id).await.unwrap());
        assert!(
            db.media_item_by_id_visible(absent_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn sqlite_catalog_counts_preserve_exact_metadata_series_and_playback_semantics() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db.first_user().await.unwrap();
        let movie_id = Uuid::new_v4();
        let item = |id: Uuid,
                    name: &str,
                    path: &str,
                    media_type: &str,
                    collection_type: &str,
                    metadata: serde_json::Value| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            media_type: media_type.to_string(),
            collection_type: collection_type.to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata,
        };
        db.replace_remote_media_library_snapshot(
            "Count Catalog",
            "mixed",
            "provider://counts",
            vec![
                item(
                    movie_id,
                    "Count Movie",
                    "provider://counts/movie.mkv",
                    "Video",
                    "movies",
                    json!({
                        "Album": [[" Album "], 7, 7.0, {"Name": "Nested"}, "\u{a0}Écho\u{a0}", "écho"],
                        "AlbumName": "album",
                        "Artists": ["ARTIST", "artist", {"Name": "Other"}, [9]],
                        "RemoteTrailers": [" https://one ", [
                            {"Url": "https://two"}, {"path": "https://three"},
                            {"Url": null, "url": "https://ignored"}, ""
                        ]],
                        "Trailers": {"Path": "https://four"}
                    }),
                ),
                item(Uuid::new_v4(), "Song", "provider://counts/song.flac", "Audio", "music", json!({})),
                item(Uuid::new_v4(), "Show S01E01", "provider://counts/Show/Season 01/Show S01E01.mkv", "Video", "tvshows", json!({})),
                item(Uuid::new_v4(), "Show S01E02", "provider://counts/Show/Season 01/Show S01E02.mkv", "Video", "tvshows", json!({})),
                item(Uuid::new_v4(), "Clip", "provider://counts/clip.mkv", "Video", "musicvideos", json!({})),
                item(Uuid::new_v4(), "Book", "provider://counts/book.epub", "Book", "books", json!({})),
            ],
        )
        .await
        .unwrap();

        let counts = db
            .media_item_catalog_counts(&MediaItemCatalogQuery::default())
            .await
            .unwrap();
        assert_eq!(counts.item_count, 6);
        assert_eq!(counts.movie_count, 1);
        assert_eq!(counts.episode_count, 2);
        assert_eq!(counts.series_count, 1);
        assert_eq!(counts.song_count, 1);
        assert_eq!(counts.music_video_count, 1);
        assert_eq!(counts.book_count, 1);
        assert_eq!(counts.album_count, 6);
        assert_eq!(counts.artist_count, 3);
        assert_eq!(counts.trailer_count, 4);

        db.upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: movie_id,
            media_source_id: None,
            audio_stream_index: None,
            subtitle_stream_index: None,
            position_ticks: 10,
            is_paused: false,
            played: true,
        })
        .await
        .unwrap();
        let played = db
            .media_item_catalog_counts(&MediaItemCatalogQuery {
                user_id: Some(user.id),
                is_played: Some(true),
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(played.item_count, 1);
        assert_eq!(played.movie_count, 1);
        assert_eq!(played.album_count, 6);
    }

    #[tokio::test]
    async fn sqlite_catalog_page_caps_rows_without_truncating_total() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let items = (0..=MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE)
            .map(|index| RemoteMediaItemUpsert {
                id: Uuid::new_v4().to_string(),
                name: format!("Bulk {index:04}"),
                path: format!("provider://bulk/{index:04}.mkv"),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({}),
            })
            .collect();
        db.replace_remote_media_library_snapshot(
            "Bulk Catalog",
            "movies",
            "provider://bulk",
            items,
        )
        .await
        .unwrap();

        let page = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: usize::MAX,
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(
            page.total_record_count,
            MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE + 1
        );
        assert_eq!(page.items.len(), MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE);
    }

    #[tokio::test]
    async fn sqlite_remote_library_batch_rolls_back_every_library_on_late_conflict() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let movie_id = Uuid::new_v4();
        let series_id = Uuid::new_v4();
        let new_movie_id = Uuid::new_v4();
        let item =
            |id: Uuid, name: &str, path: &str, collection_type: &str| RemoteMediaItemUpsert {
                id: id.to_string(),
                name: name.to_string(),
                path: path.to_string(),
                media_type: "Video".to_string(),
                collection_type: collection_type.to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({}),
            };

        db.replace_remote_media_library_snapshots(vec![
            RemoteMediaLibrarySnapshot {
                library_name: "Atomic Movies".to_string(),
                collection_type: "movies".to_string(),
                source_location: "xtream://atomic/movies/v1".to_string(),
                items: vec![item(
                    movie_id,
                    "Original Movie",
                    "xtream://atomic/movies/original.mp4",
                    "movies",
                )],
            },
            RemoteMediaLibrarySnapshot {
                library_name: "Atomic Series".to_string(),
                collection_type: "tvshows".to_string(),
                source_location: "xtream://atomic/series/v1".to_string(),
                items: vec![item(
                    series_id,
                    "Original Episode",
                    "xtream://atomic/series/original.mp4",
                    "tvshows",
                )],
            },
        ])
        .await
        .unwrap();

        let failed = db
            .replace_remote_media_library_snapshots(vec![
                RemoteMediaLibrarySnapshot {
                    library_name: "Atomic Movies".to_string(),
                    collection_type: "movies".to_string(),
                    source_location: "xtream://atomic/movies/v2".to_string(),
                    items: vec![item(
                        new_movie_id,
                        "Uncommitted Movie",
                        "xtream://atomic/movies/new.mp4",
                        "movies",
                    )],
                },
                RemoteMediaLibrarySnapshot {
                    library_name: "Atomic Series".to_string(),
                    collection_type: "tvshows".to_string(),
                    source_location: "xtream://atomic/series/v2".to_string(),
                    // This id is owned by the movie folder. The error occurs only after the
                    // first library has been applied inside the transaction.
                    items: vec![item(
                        movie_id,
                        "Cross-folder Conflict",
                        "xtream://atomic/series/conflict.mp4",
                        "tvshows",
                    )],
                },
            ])
            .await;
        assert!(failed.is_err());

        let visible = db.media_items().await.unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|item| item.id == movie_id));
        assert!(visible.iter().any(|item| item.id == series_id));
        assert!(!visible.iter().any(|item| item.id == new_movie_id));
        let folders = db.virtual_folders().await.unwrap();
        assert!(folders.iter().any(|folder| {
            folder.name == "Atomic Movies"
                && folder.locations == ["xtream://atomic/movies/v1".to_string()]
        }));
        assert!(folders.iter().any(|folder| {
            folder.name == "Atomic Series"
                && folder.locations == ["xtream://atomic/series/v1".to_string()]
        }));
        let completed_generations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM catalog_sync_runs WHERE status = 'completed'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(completed_generations, 2);
    }

    #[tokio::test]
    async fn sqlite_durable_remote_media_stage_is_invisible_and_publishes_all_projections() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let stage = db
            .begin_remote_media_catalog_stage(vec![
                RemoteMediaLibraryStageSpec {
                    key: "movies".to_string(),
                    library_name: "Staged Movies".to_string(),
                    collection_type: "movies".to_string(),
                    source_location: "xtream://staged/movies".to_string(),
                },
                RemoteMediaLibraryStageSpec {
                    key: "series".to_string(),
                    library_name: "Staged Series".to_string(),
                    collection_type: "tvshows".to_string(),
                    source_location: "xtream://staged/series".to_string(),
                },
            ])
            .await
            .unwrap();
        let movie_id = Uuid::new_v4();
        let episode_id = Uuid::new_v4();
        let item =
            |id: Uuid, path: &str, collection_type: &str, metadata: Value| RemoteMediaItemUpsert {
                id: id.to_string(),
                name: id.to_string(),
                path: path.to_string(),
                media_type: "Video".to_string(),
                collection_type: collection_type.to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata,
            };
        db.append_remote_media_catalog_stage(
            &stage,
            "movies",
            vec![item(
                movie_id,
                "xtream://staged/movies/movie.mp4",
                "movies",
                json!({
                    "Genres": ["Drama"],
                    "People": [{"Name": "Jane Doe", "Id": "person-id"}],
                    "Studios": ["Studio One"],
                    "Tags": ["Featured"],
                    "PremiereDate": "2030-01-01T00:00:00Z"
                }),
            )],
        )
        .await
        .unwrap();
        db.append_remote_media_catalog_stage(
            &stage,
            "series",
            vec![item(
                episode_id,
                "xtream://staged/series/show/season-1/episode-1.mp4",
                "tvshows",
                json!({"SeriesName": "Show"}),
            )],
        )
        .await
        .unwrap();

        assert!(db.media_items().await.unwrap().is_empty());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM remote_media_catalog_stage_items WHERE stage_id = ?1"
            )
            .bind(stage.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            2
        );

        db.complete_remote_media_catalog_stage(&stage)
            .await
            .unwrap();
        let folders = db.publish_remote_media_catalog_stage(&stage).await.unwrap();
        assert_eq!(
            folders
                .iter()
                .map(|folder| folder.name.as_str())
                .collect::<Vec<_>>(),
            ["Staged Movies", "Staged Series"]
        );
        assert_eq!(db.media_items().await.unwrap().len(), 2);
        for count in [
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_facets WHERE item_id = ?1",
            )
            .bind(movie_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_facet_aliases WHERE item_id = ?1",
            )
            .bind(movie_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_genre_selectors WHERE item_id = ?1",
            )
            .bind(movie_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_filter_selectors WHERE item_id = ?1",
            )
            .bind(movie_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id = ?1",
            )
            .bind(movie_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap(),
        ] {
            assert!(count > 0);
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM remote_media_catalog_stages WHERE id = ?1"
            )
            .bind(stage.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn sqlite_durable_remote_media_stage_rolls_back_late_publish_failure_and_retries() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let specs = vec![
            RemoteMediaLibraryStageSpec {
                key: "series".to_string(),
                library_name: "Retry Series".to_string(),
                collection_type: "tvshows".to_string(),
                source_location: "xtream://retry/series".to_string(),
            },
            RemoteMediaLibraryStageSpec {
                key: "movies".to_string(),
                library_name: "Retry Movies".to_string(),
                collection_type: "movies".to_string(),
                source_location: "xtream://retry/movies".to_string(),
            },
        ];
        let stage = db
            .begin_remote_media_catalog_stage_for_revision(specs.clone(), "revision-a")
            .await
            .unwrap();
        let item = |path: &str, collection_type: &str| RemoteMediaItemUpsert {
            id: Uuid::new_v4().to_string(),
            name: path.to_string(),
            path: path.to_string(),
            media_type: "Video".to_string(),
            collection_type: collection_type.to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({}),
        };
        db.append_remote_media_catalog_stage(
            &stage,
            "movies",
            vec![item("xtream://retry/movies/movie.mp4", "movies")],
        )
        .await
        .unwrap();
        db.append_remote_media_catalog_stage(
            &stage,
            "series",
            vec![item("xtream://retry/series/episode.mp4", "tvshows")],
        )
        .await
        .unwrap();
        db.complete_remote_media_catalog_stage(&stage)
            .await
            .unwrap();
        let ready = db
            .ready_remote_media_catalog_stage(specs.clone(), "revision-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready.stage, stage);
        assert_eq!((ready.movie_count, ready.series_item_count), (1, 1));
        assert!(
            db.ready_remote_media_catalog_stage(specs.clone(), "revision-b")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.append_remote_media_catalog_stage(
                &stage,
                "movies",
                vec![item("xtream://retry/movies/late.mp4", "movies")],
            )
            .await
            .is_err()
        );
        sqlx::query(
            r#"
            CREATE TRIGGER fail_staged_series_publish
            BEFORE INSERT ON media_items
            WHEN NEW.path LIKE 'xtream://retry/series/%'
            BEGIN
                SELECT RAISE(ABORT, 'forced late stage failure');
            END
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        assert!(db.publish_remote_media_catalog_stage(&stage).await.is_err());
        assert!(db.media_items().await.unwrap().is_empty());
        assert!(db.virtual_folders().await.unwrap().is_empty());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM remote_media_catalog_stages WHERE id = ?1"
            )
            .bind(stage.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            "open"
        );
        assert!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT ready_at FROM remote_media_catalog_stages WHERE id = ?1"
            )
            .bind(stage.id())
            .fetch_one(db.pool())
            .await
            .unwrap()
            .is_some()
        );
        assert_eq!(
            db.ready_remote_media_catalog_stage(specs, "revision-a")
                .await
                .unwrap()
                .unwrap()
                .stage,
            stage
        );
        sqlx::query("DROP TRIGGER fail_staged_series_publish")
            .execute(db.pool())
            .await
            .unwrap();
        db.publish_remote_media_catalog_stage(&stage).await.unwrap();
        assert_eq!(db.media_items().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sqlite_durable_stage_retries_busy_publication_without_partial_visibility() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("catalog-lock-retry.db");
        std::fs::File::create(&path).unwrap();
        let db = Database::connect(&format!("sqlite://{}", path.display()))
            .await
            .unwrap();
        let specs = || {
            vec![
                RemoteMediaLibraryStageSpec {
                    key: "movies".to_string(),
                    library_name: "Busy Movies".to_string(),
                    collection_type: "movies".to_string(),
                    source_location: "provider://busy/movies".to_string(),
                },
                RemoteMediaLibraryStageSpec {
                    key: "series".to_string(),
                    library_name: "Busy Series".to_string(),
                    collection_type: "tvshows".to_string(),
                    source_location: "provider://busy/series".to_string(),
                },
            ]
        };
        let episode = |generation: usize, index: usize| {
            let item_id = Uuid::new_v4();
            let series_id = Uuid::new_v4();
            RemoteMediaItemUpsert {
                id: item_id.to_string(),
                name: format!("Generation {generation} Episode {index}"),
                path: format!("provider://busy/series/{generation}/{index}.mp4"),
                media_type: "Video".to_string(),
                collection_type: "tvshows".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({
                    "SeriesId": series_id.simple().to_string(),
                    "SeriesName": format!("Generation {generation} Series {index}")
                }),
            }
        };

        let old = db.begin_remote_media_catalog_stage(specs()).await.unwrap();
        db.append_remote_media_catalog_stage(
            &old,
            "series",
            (0..2).map(|index| episode(1, index)).collect(),
        )
        .await
        .unwrap();
        db.complete_remote_media_catalog_stage(&old).await.unwrap();
        db.publish_remote_media_catalog_stage(&old).await.unwrap();

        let replacement = db.begin_remote_media_catalog_stage(specs()).await.unwrap();
        db.append_remote_media_catalog_stage(
            &replacement,
            "series",
            (0..64).map(|index| episode(2, index)).collect(),
        )
        .await
        .unwrap();
        db.complete_remote_media_catalog_stage(&replacement)
            .await
            .unwrap();

        // Busy timeout is connection-local. Configure every pool slot before taking the write
        // lock so the first publication attempt deterministically receives SQLITE_BUSY quickly.
        let mut connections = Vec::new();
        for _ in 0..super::SQLITE_MAX_CONNECTIONS {
            connections.push(db.pool.acquire().await.unwrap());
        }
        for connection in &mut connections {
            sqlx::query("PRAGMA busy_timeout = 20")
                .execute(&mut **connection)
                .await
                .unwrap();
        }
        drop(connections);

        let mut blocker = db.pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let publishing_db = db.clone();
        let publishing_stage = replacement.clone();
        let publishing = tokio::spawn(async move {
            publishing_db
                .publish_remote_media_catalog_stage(&publishing_stage)
                .await
        });

        tokio::time::sleep(StdDuration::from_millis(100)).await;
        assert!(
            !publishing.is_finished(),
            "the first SQLITE_BUSY escaped instead of scheduling a fresh transaction"
        );
        let visible_during_lock: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM media_items AS item JOIN virtual_folders AS folder \
             ON folder.id = item.virtual_folder_id WHERE folder.name = 'Busy Series' \
             AND item.missing_since IS NULL",
        )
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
        assert_eq!(
            visible_during_lock, 2,
            "a partial generation became visible"
        );

        sqlx::query("COMMIT").execute(&mut *blocker).await.unwrap();
        drop(blocker);
        let folders = tokio::time::timeout(StdDuration::from_secs(5), publishing)
            .await
            .expect("publication retry did not finish")
            .expect("publication retry task panicked")
            .expect("publication retry failed");
        let series_folder = folders
            .iter()
            .find(|folder| folder.name == "Busy Series")
            .unwrap();
        let published = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT count(item.id), count(DISTINCT series.series_id), coverage.series_count \
             FROM media_items AS item \
             JOIN media_item_tv_series_members AS member ON member.item_id = item.id \
             JOIN media_item_tv_series AS series ON series.virtual_folder_id = member.virtual_folder_id \
                AND series.series_id = member.series_id \
             JOIN media_item_tv_series_coverage AS coverage \
                ON coverage.virtual_folder_id = item.virtual_folder_id \
             WHERE item.virtual_folder_id = ?1 AND item.missing_since IS NULL \
             GROUP BY coverage.series_count",
        )
        .bind(series_folder.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(published, (64, 64, 64));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM remote_media_catalog_stages WHERE id = ?1",
            )
            .bind(replacement.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn sqlite_durable_remote_media_stage_rejects_duplicates_and_cleans_orphans() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let specs = || {
            vec![
                RemoteMediaLibraryStageSpec {
                    key: "movies".to_string(),
                    library_name: format!("Cleanup Movies {}", Uuid::new_v4()),
                    collection_type: "movies".to_string(),
                    source_location: "xtream://cleanup/movies".to_string(),
                },
                RemoteMediaLibraryStageSpec {
                    key: "series".to_string(),
                    library_name: format!("Cleanup Series {}", Uuid::new_v4()),
                    collection_type: "tvshows".to_string(),
                    source_location: "xtream://cleanup/series".to_string(),
                },
            ]
        };
        let stage = db.begin_remote_media_catalog_stage(specs()).await.unwrap();
        let id = Uuid::new_v4();
        let item = |id: Uuid, path: &str| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: path.to_string(),
            path: path.to_string(),
            media_type: "Video".to_string(),
            collection_type: "movies".to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({"Genres": ["Drama"]}),
        };
        let oversized = (0..=REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS)
            .map(|index| {
                item(
                    Uuid::new_v4(),
                    &format!("xtream://cleanup/oversized-{index}.mp4"),
                )
            })
            .collect();
        assert!(
            db.append_remote_media_catalog_stage(&stage, "series", oversized)
                .await
                .is_err()
        );
        db.append_remote_media_catalog_stage(
            &stage,
            "movies",
            vec![item(id, "xtream://cleanup/shared.mp4")],
        )
        .await
        .unwrap();
        assert!(
            db.append_remote_media_catalog_stage(
                &stage,
                "series",
                vec![item(id, "xtream://cleanup/other.mp4")],
            )
            .await
            .is_err()
        );
        assert!(
            db.append_remote_media_catalog_stage(
                &stage,
                "series",
                vec![item(Uuid::new_v4(), "xtream://cleanup/shared.mp4")],
            )
            .await
            .is_err()
        );
        assert!(
            db.append_remote_media_catalog_stage(&stage, "series", Vec::new())
                .await
                .is_err()
        );

        let large_stage = db.begin_remote_media_catalog_stage(specs()).await.unwrap();
        sqlx::query(
            "UPDATE remote_media_catalog_stage_libraries SET item_count = 100000 \
             WHERE stage_id = ?1 AND library_key = 'movies'",
        )
        .bind(large_stage.id())
        .execute(db.pool())
        .await
        .unwrap();
        db.append_remote_media_catalog_stage(
            &large_stage,
            "movies",
            vec![item(
                Uuid::new_v4(),
                "xtream://cleanup/above-legacy-cap.mp4",
            )],
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT item_count FROM remote_media_catalog_stage_libraries \
                 WHERE stage_id = ?1 AND library_key = 'movies'"
            )
            .bind(large_stage.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            100_001
        );
        sqlx::query(
            "UPDATE remote_media_catalog_stage_libraries SET item_count = 1000000 \
             WHERE stage_id = ?1 AND library_key = 'movies'",
        )
        .bind(large_stage.id())
        .execute(db.pool())
        .await
        .unwrap();
        assert!(
            db.append_remote_media_catalog_stage(
                &large_stage,
                "movies",
                vec![item(Uuid::new_v4(), "xtream://cleanup/over-hard-cap.mp4",)],
            )
            .await
            .is_err()
        );
        db.abort_remote_media_catalog_stage(&large_stage)
            .await
            .unwrap();

        sqlx::query(
            "UPDATE remote_media_catalog_stages SET updated_at = '2000-01-01T00:00:00Z' \
             WHERE id = ?1",
        )
        .bind(stage.id())
        .execute(db.pool())
        .await
        .unwrap();
        let publishing = db.begin_remote_media_catalog_stage(specs()).await.unwrap();
        sqlx::query(
            "UPDATE remote_media_catalog_stages \
             SET status = 'publishing', updated_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
        )
        .bind(publishing.id())
        .execute(db.pool())
        .await
        .unwrap();
        let cutoff = OffsetDateTime::parse("2020-01-01T00:00:00Z", &Rfc3339).unwrap();
        assert_eq!(
            db.cleanup_abandoned_remote_media_catalog_stages(cutoff)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM remote_media_catalog_stages WHERE id = ?1"
            )
            .bind(publishing.id())
            .fetch_one(db.pool())
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn sqlite_remote_library_batch_publishes_two_empty_generations_and_keeps_tombstones() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let movie_id = Uuid::new_v4();
        let episode_id = Uuid::new_v4();
        let item = |id: Uuid, path: &str, collection_type: &str| RemoteMediaItemUpsert {
            id: id.to_string(),
            name: id.to_string(),
            path: path.to_string(),
            media_type: "Video".to_string(),
            collection_type: collection_type.to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: json!({}),
        };
        let snapshot = |movies, series| {
            vec![
                RemoteMediaLibrarySnapshot {
                    library_name: "Empty Movies".to_string(),
                    collection_type: "movies".to_string(),
                    source_location: "xtream://empty/movies".to_string(),
                    items: movies,
                },
                RemoteMediaLibrarySnapshot {
                    library_name: "Empty Series".to_string(),
                    collection_type: "tvshows".to_string(),
                    source_location: "xtream://empty/series".to_string(),
                    items: series,
                },
            ]
        };

        db.replace_remote_media_library_snapshots(snapshot(
            vec![item(movie_id, "xtream://empty/movies/movie.mp4", "movies")],
            vec![item(
                episode_id,
                "xtream://empty/series/episode.mp4",
                "tvshows",
            )],
        ))
        .await
        .unwrap();
        db.replace_remote_media_library_snapshots(snapshot(Vec::new(), Vec::new()))
            .await
            .unwrap();

        assert!(db.media_items().await.unwrap().is_empty());
        let tombstones: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE missing_since IS NOT NULL")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(tombstones, 2);
        let completed_empty_generations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM catalog_sync_runs WHERE status = 'completed' AND item_count = 0",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(completed_empty_generations, 2);
    }

    #[tokio::test]
    async fn sqlite_identical_remote_snapshot_keeps_item_timestamps_stable() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let item_id = Uuid::new_v4();
        let snapshot = || RemoteMediaLibrarySnapshot {
            library_name: "No-op Movies".to_string(),
            collection_type: "movies".to_string(),
            source_location: "xtream://noop/movies".to_string(),
            items: vec![RemoteMediaItemUpsert {
                id: item_id.to_string(),
                name: "Stable Movie".to_string(),
                path: "xtream://noop/movies/stable.mp4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: Some(100),
                bitrate: Some(1_000),
                width: Some(1920),
                height: Some(1080),
                media_streams: vec![json!({"Type": "Video"})],
                metadata: json!({"Provider": "xtream"}),
            }],
        };

        db.replace_remote_media_library_snapshots(vec![snapshot()])
            .await
            .unwrap();
        let sentinel = "2000-01-01T00:00:00Z";
        sqlx::query("UPDATE media_items SET updated_at = ?1, last_seen_at = ?1 WHERE id = ?2")
            .bind(sentinel)
            .bind(item_id.to_string())
            .execute(db.pool())
            .await
            .unwrap();

        db.replace_remote_media_library_snapshots(vec![snapshot()])
            .await
            .unwrap();
        let timestamps = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT updated_at, last_seen_at FROM media_items WHERE id = ?1",
        )
        .bind(item_id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(timestamps.0, sentinel);
        assert_eq!(timestamps.1.as_deref(), Some(sentinel));
        let generations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM catalog_sync_runs WHERE status = 'completed'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(generations, 2);
    }

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

        db.update_named_configuration("livetv", json!({ "Enabled": true }))
            .await
            .unwrap();
        let configurations = db.named_configurations().await.unwrap();
        assert_eq!(configurations.len(), 2);
        assert_eq!(configurations[0].key, "livetv");
        assert_eq!(configurations[0].payload, json!({ "Enabled": true }));
        assert_eq!(configurations[1].key, "network");
        assert_eq!(
            configurations[1].payload,
            json!({ "InternalHttpPort": 8098 })
        );
    }

    #[tokio::test]
    async fn plugin_platform_state_migrates_repositories_and_catalog_cache() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.update_system_configuration_payloads(SystemConfigurationPayloads {
            plugin_repositories: json!([
                {
                    "Name": "Stable",
                    "Url": "https://repo.example/stable.json",
                    "Enabled": true,
                    "Packages": [
                        {
                            "Name": "DotNet Fixture",
                            "Guid": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                            "Runtime": "DotNetJellyfin",
                            "Versions": [
                                {
                                    "Version": "1.0.0.0",
                                    "TargetAbi": "12.0.0.0",
                                    "SourceUrl": "https://repo.example/dotnet.zip"
                                }
                            ]
                        },
                        {
                            "Name": "Rust Fixture",
                            "Guid": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                            "Runtime": "RustWasi",
                            "Versions": [
                                {
                                    "Version": "0.1.0",
                                    "TargetAbi": "jellyrin-wasi-0.1",
                                    "SourceUrl": "https://repo.example/rust.wasm"
                                }
                            ]
                        }
                    ]
                },
                {
                    "Name": "Disabled",
                    "Url": "https://repo.example/disabled.json",
                    "Enabled": false,
                    "Packages": [
                        { "Name": "Hidden", "Version": "1.0.0.0" }
                    ]
                }
            ]),
            ..SystemConfigurationPayloads::default()
        })
        .await
        .unwrap();

        let snapshot = db.plugin_platform_snapshot().await.unwrap();
        assert_eq!(snapshot["ModelVersion"], 1);
        assert_eq!(snapshot["Repositories"]["Count"], 2);
        assert_eq!(snapshot["PackageCatalogCache"]["Count"], 2);
        assert_eq!(
            snapshot["PackageCatalogCache"]["Items"][0]["Runtime"],
            "DotNetJellyfin"
        );
        assert_eq!(
            snapshot["PackageCatalogCache"]["Items"][1]["Runtime"],
            "RustWasi"
        );
        assert_eq!(snapshot["InstalledPlugins"]["Count"], 0);
        assert_eq!(snapshot["PluginRuntimeInstances"]["Count"], 0);
    }

    #[tokio::test]
    async fn plugin_platform_state_survives_sqlite_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("jellyrin-plugin-platform.db");
        std::fs::File::create(&db_path).unwrap();
        let database_url = format!("sqlite://{}", db_path.display());
        {
            let db = Database::connect(&database_url).await.unwrap();
            db.update_system_configuration_payloads(SystemConfigurationPayloads {
                plugin_repositories: json!([{
                    "Name": "Persistent",
                    "Url": "https://repo.example/persistent.json",
                    "Packages": [{
                        "Name": "Persistent Plugin",
                        "Guid": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        "Versions": [{ "Version": "2.0.0.0" }]
                    }]
                }]),
                ..SystemConfigurationPayloads::default()
            })
            .await
            .unwrap();
            let snapshot = db.plugin_platform_snapshot().await.unwrap();
            assert_eq!(snapshot["PackageCatalogCache"]["Count"], 1);
        }

        let reopened = Database::connect(&database_url).await.unwrap();
        let snapshot = reopened.plugin_platform_snapshot().await.unwrap();
        assert_eq!(snapshot["Repositories"]["Count"], 1);
        assert_eq!(snapshot["PackageCatalogCache"]["Count"], 1);
        assert_eq!(
            snapshot["PackageCatalogCache"]["Items"][0]["Name"],
            "Persistent Plugin"
        );
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
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Activity Movie.mp4");
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

        db.add_activity_log_entry_with_item(
            "Alpha event",
            Some("First overview"),
            Some("Alpha short"),
            "System",
            Some(user.id),
            Some(item.id),
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
        assert_eq!(entries[0].item_id, Some(item.id));

        let (entries, total) = db
            .activity_log_entries(
                0,
                10,
                ActivityLogFilter {
                    item_id: Some(item.id),
                    ..ActivityLogFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(entries[0].name, "Alpha event");

        let (entries, total) = db
            .activity_log_entries(
                0,
                10,
                ActivityLogFilter {
                    item_id: Some(Uuid::new_v4()),
                    ..ActivityLogFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(total, 0);
        assert!(entries.is_empty());

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
                Some(json!({ "Version": 1 })),
            )
            .await
            .unwrap();
        assert!(created.path.starts_with("jellyrin-backup-"));
        assert_eq!(created.server_version, "12.0.0");
        assert_eq!(created.backup_engine_version, "1");
        assert_eq!(created.options["Database"], true);
        assert_eq!(created.restore_snapshot.as_ref().unwrap()["Version"], 1);

        let manifests = db.backup_manifests().await.unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].path, created.path);

        let manifest = db.backup_manifest(&created.path).await.unwrap().unwrap();
        assert_eq!(manifest.path, created.path);
        assert_eq!(manifest.options["Metadata"], true);
        assert_eq!(manifest.restore_snapshot.as_ref().unwrap()["Version"], 1);
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
    async fn api_key_device_sessions_replace_same_named_device() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();
        let first_key = db.issue_api_key_for_user(user.id, "golden").await.unwrap();
        let second_key = db.issue_api_key_for_user(user.id, "golden").await.unwrap();
        let (_, first_token) = db.user_by_api_key(&first_key).await.unwrap();
        let (_, second_token) = db.user_by_api_key(&second_key).await.unwrap();

        db.ensure_device_session(&first_token).await.unwrap();
        db.ensure_device_session(&second_token).await.unwrap();

        let sessions = db.device_sessions_for_user(user.id).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].access_token, second_key);
        assert_eq!(sessions[0].device_id, "api-key:golden");
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
    async fn active_viewing_sessions_track_and_clear_now_viewing() {
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

        db.upsert_active_viewing_session(UpsertActiveViewingSession {
            session_id: token.access_token.clone(),
            user_id: user.id,
            item_id: item.id,
        })
        .await
        .unwrap();
        let sessions = db.active_viewing_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, token.access_token);
        assert_eq!(sessions[0].user_id, user.id);
        assert_eq!(sessions[0].item.id, item.id);

        db.upsert_active_viewing_session(UpsertActiveViewingSession {
            session_id: token.access_token.clone(),
            user_id: user.id,
            item_id: item.id,
        })
        .await
        .unwrap();
        let sessions = db.active_viewing_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].item.id, item.id);

        db.clear_active_viewing_session(&token.access_token)
            .await
            .unwrap();
        assert!(db.active_viewing_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_session_users_round_trip_and_scope_sessions() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let owner = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();
        let guest = db.create_user("guest", Some("secret")).await.unwrap();
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

        assert!(
            db.device_sessions_for_user(guest.id)
                .await
                .unwrap()
                .is_empty()
        );
        db.add_session_user(&token.access_token, guest.id)
            .await
            .unwrap();
        let users = db.active_session_users().await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].session_id, token.access_token);
        assert_eq!(users[0].user_id, guest.id);
        assert_eq!(users[0].user_name, "guest");
        let guest_sessions = db.device_sessions_for_user(guest.id).await.unwrap();
        assert_eq!(guest_sessions.len(), 1);
        assert_eq!(guest_sessions[0].user_id, owner.id);

        db.remove_session_user(&token.access_token, guest.id)
            .await
            .unwrap();
        assert!(db.active_session_users().await.unwrap().is_empty());
        assert!(
            db.device_sessions_for_user(guest.id)
                .await
                .unwrap()
                .is_empty()
        );
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
        sqlx::query("UPDATE media_items SET id = ?1 WHERE id = ?2")
            .bind(item.id.simple().to_string())
            .bind(item.id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();

        let session = db
            .upsert_transcode_session(UpsertTranscodeSession {
                play_session_id: "play-session-1".to_string(),
                dedupe_key: Some("dedupe:play-session-1".to_string()),
                device_id: Some("device-1".to_string()),
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
                start_position_ticks: 123,
            })
            .await
            .unwrap();

        assert_eq!(session.play_session_id, "play-session-1");
        assert_eq!(session.dedupe_key.as_deref(), Some("dedupe:play-session-1"));
        assert_eq!(session.device_id.as_deref(), Some("device-1"));
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
        assert_eq!(
            db.transcode_session_output_paths().await.unwrap(),
            vec![session.output_path.clone()]
        );
        let active_sessions = db.active_transcode_sessions().await.unwrap();
        assert_eq!(active_sessions.len(), 1);
        assert_eq!(active_sessions[0].play_session_id, "play-session-1");

        let (claimed, claimed_new) = db
            .claim_transcode_session(
                "dedupe:play-session-1",
                UpsertTranscodeSession {
                    play_session_id: "play-session-2".to_string(),
                    dedupe_key: None,
                    device_id: Some("device-2".to_string()),
                    user_id: user.id,
                    item_id: item.id,
                    media_source_id: Some(item.id.simple().to_string()),
                    audio_stream_index: Some(1),
                    subtitle_stream_index: Some(-1),
                    video_stream_index: Some(0),
                    output_path: "/tmp/jellyrin-transcodes/play-session-2/main.m3u8".to_string(),
                    process_id: None,
                    status: "starting".to_string(),
                    progress_percent: None,
                    position_ticks: 0,
                    start_position_ticks: 0,
                },
            )
            .await
            .unwrap();
        assert!(!claimed_new);
        assert_eq!(claimed.play_session_id, "play-session-1");
        assert!(
            db.transcode_session_by_play_session_id("play-session-2")
                .await
                .unwrap()
                .is_none()
        );

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
        let progressed = db
            .update_task_run_progress(
                run.id,
                json!({
                    "Phase": "Scanning",
                    "ProgressPercentage": 25.0
                }),
            )
            .await
            .unwrap()
            .expect("running task progress should update");
        assert_eq!(progressed.result_json.unwrap()["Phase"], "Scanning");
        assert_eq!(
            db.current_task_run("RefreshLibrary")
                .await
                .unwrap()
                .unwrap()
                .result_json
                .unwrap()["ProgressPercentage"],
            25.0
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
        assert!(
            db.update_task_run_progress(run.id, json!({ "ProgressPercentage": 50.0 }))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn task_runs_can_be_cancelled_and_stale_runs_expire() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let run = db.start_task_run("RefreshLibrary").await.unwrap();
        db.update_task_run_progress(run.id, json!({ "ProgressPercentage": 10.0 }))
            .await
            .unwrap();
        let failed = db
            .fail_current_task_run("RefreshLibrary", "cancelled")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.id, run.id);
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.error_message.as_deref(), Some("cancelled"));
        assert_eq!(failed.result_json.unwrap()["ProgressPercentage"], 10.0);

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
                "bit_rate": "3000000",
                "tags": {
                    "album": "Example Album",
                    "artist": "Artist One; Artist Two",
                    "album_artist": "Album Artist",
                    "genre": "Rock/Jazz"
                }
            },
            "chapters": [
                {
                    "start_time": "0.000000",
                    "tags": { "title": "Opening" }
                },
                {
                    "time_base": "1/1000",
                    "start": 182432,
                    "tags": {}
                }
            ]
        });
        let info = parse_ffprobe_media_info(&value);
        assert_eq!(info.runtime_ticks, Some(1_234_560_000));
        assert_eq!(info.bitrate, Some(3_000_000));
        assert_eq!(info.width, Some(1920));
        assert_eq!(info.height, Some(1080));
        assert_eq!(info.metadata["Album"], "Example Album");
        assert_eq!(
            info.metadata["Artists"],
            json!(["Artist One", "Artist Two"])
        );
        assert_eq!(info.metadata["AlbumArtists"], json!(["Album Artist"]));
        assert_eq!(info.metadata["MusicGenres"], json!(["Rock", "Jazz"]));
        assert_eq!(info.metadata["Genres"], json!(["Rock", "Jazz"]));
        assert_eq!(info.metadata["Chapters"][0]["Name"], "Opening");
        assert_eq!(info.metadata["Chapters"][0]["StartPositionTicks"], 0);
        assert_eq!(info.metadata["Chapters"][1]["Name"], "Chapter 2");
        assert_eq!(
            info.metadata["Chapters"][1]["StartPositionTicks"],
            1_824_320_000
        );
    }

    #[test]
    fn parses_bounded_dvb_teletext_services_from_ffprobe_descriptor() {
        let value = json!({
            "streams": [{
                "index": 2,
                "codec_name": "dvb_teletext",
                "codec_type": "subtitle",
                "extradata": "\n00000000: 1001 1002 2858 1003 1004                 ....(X....\n",
                "tags": { "language": "spa,eng,esl,cat,eus" },
                "disposition": {}
            }],
            "format": {}
        });

        let info = parse_ffprobe_media_info(&value);
        let stream = &info.media_streams[0];
        assert_eq!(stream["Codec"], "dvb_teletext");
        assert_eq!(stream["TeletextServices"].as_array().unwrap().len(), 5);
        assert_eq!(stream["TeletextServices"][0]["Page"], 801);
        assert_eq!(stream["TeletextServices"][0]["Language"], "spa");
        assert_eq!(stream["TeletextServices"][0]["TeletextType"], 2);
        assert_eq!(stream["TeletextServices"][0]["IsSubtitle"], true);
        assert_eq!(stream["TeletextServices"][0]["IsHearingImpaired"], false);
        assert_eq!(stream["TeletextServices"][2]["Page"], 858);
        assert_eq!(stream["TeletextServices"][2]["Language"], "esl");
        assert_eq!(stream["TeletextServices"][2]["TeletextType"], 5);
        assert_eq!(stream["TeletextServices"][2]["IsHearingImpaired"], true);
        assert_eq!(stream["TeletextServices"][4]["Page"], 804);
        // The parser retains only the explicit normalized structure, never ffprobe's formatted
        // binary dump.
        assert!(stream.get("extradata").is_none());
    }

    #[test]
    fn merges_only_selected_stream_extradata_into_primary_probe() {
        let mut primary = json!({
            "streams": [
                { "index": 0, "codec_name": "h264", "codec_type": "video" },
                { "index": 2, "codec_name": "dvb_teletext", "codec_type": "subtitle" },
                { "index": 3, "codec_name": "attachment", "codec_type": "attachment" }
            ]
        });
        let supplemental = json!({
            "streams": [
                { "index": 2, "extradata": "00000000: 1001" },
                { "index": 99, "extradata": "ignored" }
            ]
        });

        assert!(super::ffprobe_has_dvb_teletext_stream(&primary));
        super::merge_ffprobe_stream_extradata(&mut primary, &supplemental);

        assert_eq!(primary["streams"][2].get("extradata"), None);
        assert_eq!(primary["streams"][1]["extradata"], "00000000: 1001");
    }

    #[test]
    fn rejects_malformed_or_oversized_teletext_descriptors() {
        let invalid_bcd = json!({
            "extradata": "00000000: 10fa  ....",
            "tags": { "language": "spa" }
        });
        assert!(super::ffprobe_teletext_services(&invalid_bcd).is_empty());

        let odd = json!({
            "extradata": "00000000: 100  ...",
            "tags": { "language": "spa" }
        });
        assert!(super::ffprobe_teletext_services(&odd).is_empty());

        let oversized = format!("00000000: {}  data", "1001".repeat(65));
        assert!(super::ffprobe_teletext_services(&json!({"extradata": oversized})).is_empty());
    }

    #[test]
    fn ffprobe_timeout_policy_is_bounded() {
        assert_eq!(super::ffprobe_timeout_seconds_from_value(None), 15);
        assert_eq!(super::ffprobe_timeout_seconds_from_value(Some("1")), 1);
        assert_eq!(super::ffprobe_timeout_seconds_from_value(Some("120")), 120);
        assert_eq!(super::ffprobe_timeout_seconds_from_value(Some("0")), 15);
        assert_eq!(super::ffprobe_timeout_seconds_from_value(Some("121")), 15);
        assert_eq!(
            super::ffprobe_timeout_seconds_from_value(Some("invalid")),
            15
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffprobe_process_timeout_kills_and_reaps_child() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30");
        let started = std::time::Instant::now();
        let output =
            super::run_ffprobe_command(command, std::time::Duration::from_millis(25)).await;

        assert_eq!(output, Err(super::FfprobeOutcome::TimedOut));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn parses_local_nfo_metadata_json() {
        let metadata = parse_local_nfo_metadata(
            r#"
            <movie>
              <title>Local &amp; Exact Title</title>
              <sorttitle>Exact Title, Local</sorttitle>
              <originaltitle>Original Local Title</originaltitle>
              <plot>NFO overview</plot>
              <outline>Short NFO overview</outline>
              <tagline>Local tagline</tagline>
              <year>1984</year>
              <premiered>1984-06-01</premiered>
              <mpaa>PG</mpaa>
              <genre>Drama / Mystery</genre>
              <genre>Science Fiction</genre>
              <studio>Studio One</studio>
              <tag>Imported</tag>
              <uniqueid type="imdb">tt1234567</uniqueid>
              <tmdbid>9876</tmdbid>
              <director>Jane Director</director>
              <actor>
                <name>John Actor</name>
                <role>Detective</role>
              </actor>
            </movie>
            "#,
        );

        assert_eq!(metadata["Name"], "Local & Exact Title");
        assert_eq!(metadata["SortName"], "Exact Title, Local");
        assert_eq!(metadata["OriginalTitle"], "Original Local Title");
        assert_eq!(metadata["Overview"], "NFO overview");
        assert_eq!(metadata["ShortOverview"], "Short NFO overview");
        assert_eq!(metadata["Tagline"], "Local tagline");
        assert_eq!(metadata["ProductionYear"], 1984);
        assert_eq!(metadata["PremiereDate"], "1984-06-01");
        assert_eq!(metadata["OfficialRating"], "PG");
        assert_eq!(
            metadata["Genres"],
            json!(["Drama", "Mystery", "Science Fiction"])
        );
        assert_eq!(metadata["Studios"], json!(["Studio One"]));
        assert_eq!(metadata["Tags"], json!(["Imported"]));
        assert_eq!(metadata["ProviderIds"]["Imdb"], "tt1234567");
        assert_eq!(metadata["ProviderIds"]["Tmdb"], "9876");
        assert_eq!(metadata["People"][0]["Name"], "Jane Director");
        assert_eq!(metadata["People"][0]["Type"], "Director");
        assert_eq!(metadata["People"][1]["Name"], "John Actor");
        assert_eq!(metadata["People"][1]["Role"], "Detective");
    }

    #[tokio::test]
    async fn scan_imports_local_nfo_and_respects_locked_metadata_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Nfo Movie.mp4");
        let nfo = tmp.path().join("Nfo Movie.nfo");
        tokio::fs::write(&movie, b"fake video").await.unwrap();
        tokio::fs::write(
            &nfo,
            r#"
            <movie>
              <title>NFO Movie Title</title>
              <plot>NFO overview one</plot>
              <genre>Drama</genre>
              <studio>Studio One</studio>
              <uniqueid type="imdb">tt0000001</uniqueid>
            </movie>
            "#,
        )
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
        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);
        let item = db.media_items().await.unwrap().remove(0);
        let metadata = db
            .media_item_metadata()
            .await
            .unwrap()
            .into_iter()
            .find(|metadata| metadata.item_id == item.id)
            .unwrap()
            .payload;
        assert_eq!(metadata["Name"], "NFO Movie Title");
        assert_eq!(metadata["Overview"], "NFO overview one");
        assert_eq!(metadata["Genres"], json!(["Drama"]));
        assert_eq!(metadata["ProviderIds"]["Imdb"], "tt0000001");
        let projected_genre = db
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                genre_ids: vec!["drama".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(projected_genre.total_record_count, 1);
        assert_eq!(projected_genre.items[0].item.id, item.id);

        db.update_media_item_metadata(
            item.id,
            json!({
                "Overview": "Manual locked overview",
                "Genres": ["Manual Genre"],
                "LockedFields": ["Overview", "Genres"]
            }),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &nfo,
            r#"
            <movie>
              <title>NFO Movie Retitled</title>
              <plot>NFO overview two</plot>
              <genre>Comedy</genre>
              <studio>Studio Two</studio>
              <uniqueid type="imdb">tt0000002</uniqueid>
            </movie>
            "#,
        )
        .await
        .unwrap();

        assert_eq!(db.scan_virtual_folder_items(folder.id).await.unwrap(), 1);
        let metadata = db
            .media_item_metadata()
            .await
            .unwrap()
            .into_iter()
            .find(|metadata| metadata.item_id == item.id)
            .unwrap()
            .payload;
        assert_eq!(metadata["Name"], "NFO Movie Retitled");
        assert_eq!(metadata["Overview"], "Manual locked overview");
        assert_eq!(metadata["Genres"], json!(["Manual Genre"]));
        assert_eq!(metadata["Studios"], json!(["Studio Two"]));
        assert_eq!(metadata["ProviderIds"]["Imdb"], "tt0000002");
        assert_eq!(
            db.media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                genre_ids: vec!["manual genre".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap()
            .total_record_count,
            1
        );
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

        let playback_states = db.playback_states_for_user(user.id).await.unwrap();
        assert_eq!(playback_states.len(), 1);
        assert_eq!(playback_states[0].item_id, item.id);
        assert_eq!(playback_states[0].audio_stream_index, Some(1));
        assert_eq!(playback_states[0].subtitle_stream_index, Some(-1));
        assert_eq!(playback_states[0].position_ticks, 84);
        assert!(playback_states[0].is_paused);
    }

    #[tokio::test]
    async fn sqlite_resume_page_filters_policy_before_offset_beyond_five_hundred() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("resume-page-admin".to_string(), "secret")
            .await
            .unwrap();
        let item_ids = (0..513).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        db.replace_remote_media_library_snapshot(
            "Large Resume Library",
            "movies",
            "provider://large-resume",
            item_ids
                .iter()
                .enumerate()
                .map(|(index, item_id)| RemoteMediaItemUpsert {
                    id: item_id.to_string(),
                    name: format!("Resume Movie {index:04}"),
                    path: format!("provider://large-resume/{item_id}.mkv"),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: Some(10_000_000_000),
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata: json!({}),
                })
                .collect(),
        )
        .await
        .unwrap();
        for item_id in &item_ids {
            db.upsert_playback_state(UpsertPlaybackState {
                user_id: user.id,
                item_id: *item_id,
                media_source_id: None,
                audio_stream_index: None,
                subtitle_stream_index: None,
                position_ticks: 1_000_000_000,
                is_paused: false,
                played: false,
            })
            .await
            .unwrap();
        }

        let page = db
            .resume_items_page_for_user(
                user.id,
                ResumeItemsPageQuery {
                    start_index: 500,
                    limit: 13,
                    min_pct: 5,
                    max_pct: 90,
                    min_duration_ticks: 3_000_000_000,
                },
            )
            .await
            .unwrap();

        assert_eq!(page.total_record_count, 513);
        assert_eq!(page.start_index, 500);
        assert_eq!(page.items.len(), 13);
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

    #[tokio::test]
    async fn sqlite_query_filter_values_are_set_based_exact_and_unbounded() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let mut items = (0..512_u128)
            .map(|index| RemoteMediaItemUpsert {
                id: Uuid::from_u128(index + 1).to_string(),
                name: format!("Noise {index:03}"),
                path: format!("provider://filter/noise/{index:03}.mp4"),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: if index == 511 {
                    json!({ "Genres": ["Tail Genre"] })
                } else {
                    json!({})
                },
            })
            .collect::<Vec<_>>();
        items.extend([
            RemoteMediaItemUpsert {
                id: Uuid::from_u128(20_000).to_string(),
                name: "A Filter Target".to_string(),
                path: "provider://filter/target-a.MP4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: Some(1920),
                height: Some(1080),
                media_streams: vec![
                    json!({ "Type": "Audio", "Language": "fre" }),
                    json!({ "Type": "Subtitle", "Language": "spa" }),
                    json!({ "Type": " Audio ", "Language": "ita" }),
                    json!({ "Type": "Audio", "Language": "und" }),
                    json!({ "Type": "Audio", "Language": 123 }),
                ],
                metadata: json!({
                    "Genres": [
                        "Drama",
                        ["Nested"],
                        { "Name": "Object Genre", "Ignored": "Nope" },
                        { "Name": 123 }
                    ],
                    "SeriesGenres": ["Excluded Series Genre"],
                    "Tags": ["Featured"],
                    "OfficialRating": "PG-13",
                    "OfficialRatings": ["R"],
                    "SeriesOfficialRating": "Excluded Rating",
                    "ProductionYear": 2024,
                    "Years": [2025],
                    "SeriesStatus": "Continuing",
                    "Status": "Excluded Status",
                    "People": [{ "Name": "Primary Person" }],
                    "SeriesPeople": ["Series Person"],
                    "Cast": ["Excluded Cast"],
                    "Artists": ["Track Artist"],
                    "AlbumArtists": ["Album Artist"],
                    "Album": "Primary Album",
                    "AlbumName": "Alternate Album",
                    "Albums": ["Excluded Album"],
                    "Studios": [{ "Name": "Primary Studio" }],
                    "SeriesStudios": ["Excluded Studio"],
                    "remoteTrailers": [{ "path": "https://example.test/trailer.mp4" }]
                }),
            },
            RemoteMediaItemUpsert {
                id: Uuid::from_u128(20_001).to_string(),
                name: "Z Filter Target".to_string(),
                path: "provider://filter/target-z.mp4".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: Some(1280),
                height: Some(720),
                media_streams: vec![json!({ "Type": "Audio", "Language": "ENG" })],
                metadata: json!({ "Genres": ["drama"] }),
            },
        ]);
        let folder = db
            .replace_remote_media_library_snapshot(
                "Filter Values",
                "movies",
                "provider://filter",
                items,
            )
            .await
            .unwrap();

        let query = MediaItemCatalogQuery {
            virtual_folder_ids: vec![folder.id],
            include_item_types: vec!["Movie".to_string()],
            media_types: vec!["Video".to_string()],
            containers: vec!["mp4".to_string()],
            ..MediaItemCatalogQuery::default()
        };
        let values = db
            .media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ALL)
            .await
            .unwrap();
        assert_eq!(
            values.genres,
            ["Drama", "Nested", "Object Genre", "Tail Genre"]
        );
        assert_eq!(values.tags, ["Featured"]);
        assert_eq!(values.official_ratings, ["PG-13", "R"]);
        assert_eq!(values.years, ["2024", "2025"]);
        assert_eq!(values.containers, ["mp4"]);
        assert_eq!(values.media_types, ["Video"]);
        assert_eq!(values.video_types, ["VideoFile"]);
        assert_eq!(values.series_statuses, ["Continuing"]);
        assert_eq!(values.staff_names, ["Primary Person", "Series Person"]);
        assert_eq!(values.artists, ["Album Artist", "Track Artist"]);
        assert_eq!(values.albums, ["Alternate Album", "Primary Album"]);
        assert_eq!(values.studios, ["Primary Studio"]);
        assert_eq!(values.audio_languages, ["ENG", "fra"]);
        assert_eq!(values.subtitle_languages, ["spa"]);
        assert!(values.has_subtitles);
        assert!(values.has_trailer);

        let items_filter_values = db
            .media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ITEMS_FILTERS)
            .await
            .unwrap();
        assert_eq!(items_filter_values.genres, values.genres);
        assert_eq!(items_filter_values.containers, values.containers);
        assert_eq!(items_filter_values.staff_names, values.staff_names);
        assert!(items_filter_values.audio_languages.is_empty());
        assert!(items_filter_values.subtitle_languages.is_empty());
        assert!(items_filter_values.has_subtitles);
        assert!(items_filter_values.has_trailer);

        let filters2_values = db
            .media_item_query_filter_values(&query, MediaItemQueryFilterSelection::FILTERS2)
            .await
            .unwrap();
        assert_eq!(filters2_values.genres, values.genres);
        assert_eq!(filters2_values.tags, values.tags);
        assert_eq!(filters2_values.audio_languages, values.audio_languages);
        assert_eq!(
            filters2_values.subtitle_languages,
            values.subtitle_languages
        );
        assert!(filters2_values.official_ratings.is_empty());
        assert!(filters2_values.containers.is_empty());
        assert!(filters2_values.media_types.is_empty());
        assert!(!filters2_values.has_subtitles);
        assert!(!filters2_values.has_trailer);
        for excluded in [
            "Excluded Series Genre",
            "Excluded Rating",
            "Excluded Status",
            "Excluded Cast",
            "Excluded Album",
            "Excluded Studio",
            "ita",
            "und",
        ] {
            assert!(
                !format!("{values:?}").contains(excluded),
                "unexpected broadened mapping: {excluded}"
            );
        }

        let narrowed = db
            .media_item_query_filter_values(
                &MediaItemCatalogQuery {
                    audio_languages: vec!["fra".to_string()],
                    ..query
                },
                MediaItemQueryFilterSelection::ALL,
            )
            .await
            .unwrap();
        assert_eq!(narrowed.genres, ["Drama", "Nested", "Object Genre"]);
        assert_eq!(narrowed.audio_languages, ["fra"]);

        let extension_items = [
            ".hidden",
            ".foo.bar",
            "foo.",
            "foo.tar.gz",
            "slash/",
            "back\\slash.mkv",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, path)| RemoteMediaItemUpsert {
            id: Uuid::from_u128(30_000 + index as u128).to_string(),
            name: format!("Extension {index}"),
            path: format!("provider://extensions/{path}"),
            media_type: "Video".to_string(),
            collection_type: "movies".to_string(),
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            metadata: if index == 0 {
                json!({
                    "Trailers": [
                        {
                            "Url": "",
                            "url": "https://example.test/must-not-fallback-empty.mp4"
                        },
                        {
                            "Url": null,
                            "url": "https://example.test/must-not-fallback-null.mp4"
                        },
                        {
                            "Url": 123,
                            "url": "https://example.test/must-not-fallback-number.mp4"
                        }
                    ]
                })
            } else {
                json!({})
            },
        })
        .collect();
        let extension_folder = db
            .replace_remote_media_library_snapshot(
                "Filter Extensions",
                "movies",
                "provider://extensions",
                extension_items,
            )
            .await
            .unwrap();
        let extension_values = db
            .media_item_query_filter_values(
                &MediaItemCatalogQuery {
                    virtual_folder_ids: vec![extension_folder.id],
                    ..MediaItemCatalogQuery::default()
                },
                MediaItemQueryFilterSelection::ALL,
            )
            .await
            .unwrap();
        assert_eq!(extension_values.containers, ["", "bar", "gz", "mkv"]);
        assert!(!extension_values.has_trailer);
    }

    #[tokio::test]
    async fn sqlite_query_filter_projection_invalidates_on_folder_move() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let item_id = Uuid::from_u128(40_000);
        let source = db
            .replace_remote_media_library_snapshot(
                "Projection move source",
                "movies",
                "provider://projection-move-source",
                vec![RemoteMediaItemUpsert {
                    id: item_id.to_string(),
                    name: "Moved item".to_string(),
                    path: "provider://projection-move-source/movie.mkv".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: Vec::new(),
                    metadata: json!({"Genres": ["Moved"]}),
                }],
            )
            .await
            .unwrap();
        let destination = db
            .replace_remote_media_library_snapshot(
                "Projection move destination",
                "movies",
                "provider://projection-move-destination",
                Vec::new(),
            )
            .await
            .unwrap();

        sqlx::query("UPDATE media_items SET virtual_folder_id = ?1 WHERE id = ?2")
            .bind(destination.id.to_string())
            .bind(item_id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM media_item_query_filter_sources WHERE item_id = ?1",
        )
        .bind(item_id.to_string())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
        assert!(
            db.media_item_query_filter_values(
                &MediaItemCatalogQuery {
                    virtual_folder_ids: vec![source.id],
                    ..MediaItemCatalogQuery::default()
                },
                MediaItemQueryFilterSelection::ALL
            )
            .await
            .unwrap()
            .genres
            .is_empty()
        );
        assert_eq!(
            db.media_item_query_filter_values(
                &MediaItemCatalogQuery {
                    virtual_folder_ids: vec![destination.id],
                    ..MediaItemCatalogQuery::default()
                },
                MediaItemQueryFilterSelection::ALL
            )
            .await
            .unwrap()
            .genres,
            ["Moved"]
        );
    }

    #[tokio::test]
    async fn sqlite_query_filter_projection_is_exact_and_fails_closed_on_corruption() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let item_id = Uuid::from_u128(40_001);
        let folder = db
            .replace_remote_media_library_snapshot(
                "Projection fallback",
                "movies",
                "provider://projection-fallback",
                vec![RemoteMediaItemUpsert {
                    id: item_id.to_string(),
                    name: "Projection item".to_string(),
                    path: "provider://projection-fallback/movie.ÉXT".to_string(),
                    media_type: "Video".to_string(),
                    collection_type: "movies".to_string(),
                    runtime_ticks: None,
                    bitrate: None,
                    width: None,
                    height: None,
                    media_streams: vec![json!({"Type": "Subtitle", "Language": "spa"})],
                    metadata: json!({"Genres": ["Straße", "STRASSE"], "Tags": ["One"]}),
                }],
            )
            .await
            .unwrap();
        let query = MediaItemCatalogQuery {
            virtual_folder_ids: vec![folder.id],
            ..MediaItemCatalogQuery::default()
        };
        let projected = db
            .media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ALL)
            .await
            .unwrap();
        assert_eq!(projected.containers, ["Éxt"]);
        assert_eq!(projected.genres, ["STRASSE", "Straße"]);

        sqlx::query("DELETE FROM media_item_query_filter_sources WHERE item_id = ?1")
            .bind(item_id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            db.media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ALL)
                .await
                .unwrap(),
            projected
        );

        db.rebuild_media_item_query_filter_projection()
            .await
            .unwrap();
        sqlx::query(
            "UPDATE media_item_query_filter_sources SET projected_value_count = \
             projected_value_count + 1 WHERE item_id = ?1",
        )
        .bind(item_id.to_string())
        .execute(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            db.media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ALL)
                .await
                .unwrap(),
            projected
        );

        sqlx::query("UPDATE media_items SET metadata_json = ?1 WHERE id = ?2")
            .bind(r#"{"Genres":["Changed"]}"#)
            .bind(item_id.to_string())
            .execute(&db.pool)
            .await
            .unwrap();
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM media_item_query_filter_sources WHERE item_id = ?1",
        )
        .bind(item_id.to_string())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(
            db.media_item_query_filter_values(&query, MediaItemQueryFilterSelection::ALL)
                .await
                .unwrap()
                .genres,
            ["Changed"]
        );
    }

    #[tokio::test]
    async fn sqlite_media_item_facets_are_atomic_idempotent_and_rebuilt_in_batches() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let item_ids = (0..501)
            .map(|index| Uuid::from_u128(index + 1))
            .collect::<Vec<_>>();
        let items = item_ids
            .iter()
            .enumerate()
            .map(|(index, item_id)| RemoteMediaItemUpsert {
                id: item_id.to_string(),
                name: format!("Facet Item {index:03}"),
                path: format!("provider://facets/{index:03}.mp3"),
                media_type: "Audio".to_string(),
                collection_type: "music".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: if index == 500 {
                    json!({
                        "Genres": [" Drama ", "drama"],
                        "Artists": ["Track Artist"],
                        "AlbumArtists": ["Album Artist"],
                        "People": [
                            {
                                "Name": "Jane Doe",
                                "Id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                                "Role": "Lead"
                            },
                            { "Name": "Legacy Person", "Id": "IMPORTED-PERSON" }
                        ],
                        "Tags": [format!("Tag {index:03}")],
                        "PremiereDate": "2035-02-03T04:05:06.123456789Z"
                    })
                } else {
                    json!({ "Tags": [format!("Tag {index:03}")] })
                },
            })
            .collect::<Vec<_>>();
        let folder = db
            .replace_remote_media_library_snapshot(
                "Facet Music",
                "music",
                "provider://facets",
                items.clone(),
            )
            .await
            .unwrap();

        sqlx::query("DELETE FROM media_item_facets")
            .execute(&db.pool)
            .await
            .unwrap();
        db.rebuild_media_item_facets().await.unwrap();
        db.rebuild_media_item_facets().await.unwrap();
        let tags = db
            .media_item_facet_values(MediaItemFacetKind::Tag, &[folder.id])
            .await
            .unwrap();
        assert_eq!(tags.len(), 501, "rebuild must cross the 500-row batch");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id IN (?1, ?2)",
            )
            .bind(item_ids[500].simple().to_string())
            .bind(item_ids[500].to_string())
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            1
        );
        let genres = db
            .media_item_facet_values(MediaItemFacetKind::Genre, &[folder.id])
            .await
            .unwrap();
        assert_eq!(genres.len(), 1);
        assert_eq!(genres[0].display_value, "Drama");
        assert_eq!(genres[0].payload, json!(" Drama "));
        assert_eq!(
            db.media_item_facet_values(MediaItemFacetKind::MusicArtist, &[folder.id])
                .await
                .unwrap()[0]
                .display_value,
            "Track Artist"
        );
        assert_eq!(
            db.media_item_facet_values(MediaItemFacetKind::MusicAlbumArtist, &[folder.id])
                .await
                .unwrap()[0]
                .display_value,
            "Album Artist"
        );

        let person = db
            .media_item_facet_by_entity_id(
                MediaItemFacetKind::Person,
                "aaaaaaaabbbbccccddddeeeeeeeeeeee",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(person.display_value, "Jane Doe");
        assert_eq!(person.payload["Role"], "Lead");
        assert_eq!(
            db.media_item_facet_by_entity_id(
                MediaItemFacetKind::Person,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            )
            .await
            .unwrap(),
            Some(person.clone())
        );
        assert_eq!(
            db.media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                .await
                .unwrap()
                .unwrap()
                .display_value,
            "Legacy Person"
        );
        assert_eq!(
            db.media_item_facet_by_normalized_value(
                MediaItemFacetKind::Person,
                " JANE DOE ",
                &[folder.id],
            )
            .await
            .unwrap(),
            Some(person.clone())
        );
        assert!(
            db.media_item_facet_by_normalized_value(
                MediaItemFacetKind::Person,
                "Jane Doe",
                &[Uuid::new_v4()],
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(
            db.media_item_facet_by_entity_id(MediaItemFacetKind::Person, &person.stable_id)
                .await
                .unwrap(),
            Some(person.clone())
        );
        assert_eq!(
            db.media_item_ids_for_facets(&MediaItemFacetCandidateQuery {
                kind: Some(MediaItemFacetKind::Person),
                entity_ids: vec!["aaaaaaaabbbbccccddddeeeeeeeeeeee".to_string()],
                virtual_folder_ids: vec![folder.id],
                ..MediaItemFacetCandidateQuery::default()
            })
            .await
            .unwrap(),
            vec![item_ids[500]]
        );

        db.update_media_item_metadata(
            item_ids[500],
            json!({ "Tags": ["Current Tag"], "People": ["New Person"] }),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id IN (?1, ?2)",
            )
            .bind(item_ids[500].simple().to_string())
            .bind(item_ids[500].to_string())
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            0,
            "metadata update must remove a stale Upcoming date"
        );
        assert!(
            db.media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query(
            "CREATE TRIGGER fail_facet_update BEFORE INSERT ON media_item_facets \
             WHEN NEW.display_value = 'ROLLBACK' BEGIN \
             SELECT RAISE(ABORT, 'facet rollback test'); END",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        assert!(
            db.update_media_item_metadata(
                item_ids[500],
                json!({
                    "Tags": ["ROLLBACK"],
                    "PremiereDate": "2040-01-01T00:00:00Z"
                })
            )
            .await
            .is_err()
        );
        let payload: String =
            sqlx::query_scalar("SELECT metadata_json FROM media_items WHERE id IN (?1, ?2)")
                .bind(item_ids[500].simple().to_string())
                .bind(item_ids[500].to_string())
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["Tags"][0],
            "Current Tag"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id IN (?1, ?2)",
            )
            .bind(item_ids[500].simple().to_string())
            .bind(item_ids[500].to_string())
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            0,
            "failed metadata update must roll back its derived Upcoming date"
        );
        sqlx::query("DROP TRIGGER fail_facet_update")
            .execute(&db.pool)
            .await
            .unwrap();

        db.replace_remote_media_library_snapshot(
            "Facet Music",
            "music",
            "provider://facets",
            items[..500].to_vec(),
        )
        .await
        .unwrap();
        assert!(
            db.media_item_ids_for_facets(&MediaItemFacetCandidateQuery {
                kind: Some(MediaItemFacetKind::Person),
                normalized_values: vec!["Jane Doe".to_string()],
                ..MediaItemFacetCandidateQuery::default()
            })
            .await
            .unwrap()
            .is_empty(),
            "tombstoned facet owners must not be visible"
        );
        db.replace_remote_media_library_snapshot(
            "Facet Music",
            "music",
            "provider://facets",
            items,
        )
        .await
        .unwrap();
        assert!(
            db.media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                .await
                .unwrap()
                .is_some(),
            "resurrection must republish facets atomically"
        );

        let storage_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE id IN (?1, ?2)")
                .bind(item_ids[500].simple().to_string())
                .bind(item_ids[500].to_string())
                .fetch_one(&db.pool)
                .await
                .unwrap();
        sqlx::query("DELETE FROM media_items WHERE id = ?1")
            .bind(&storage_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let facet_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_item_facets WHERE item_id = ?1")
                .bind(&storage_id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let alias_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_item_facet_aliases WHERE item_id = ?1")
                .bind(&storage_id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let upcoming_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id = ?1")
                .bind(&storage_id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let filter_selector_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_item_filter_selectors WHERE item_id = ?1",
        )
        .bind(&storage_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            (
                facet_count,
                alias_count,
                upcoming_count,
                filter_selector_count
            ),
            (0, 0, 0, 0)
        );
    }

    #[tokio::test]
    async fn sqlite_connect_reconciles_query_filter_marker_without_rebuild() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("query-filter-marker.db");
        std::fs::File::create(&path).unwrap();
        let database_url = format!("sqlite://{}", path.display());
        let db = Database::connect(&database_url).await.unwrap();
        let item_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Persistent query filters",
            "movies",
            "provider://persistent-query-filters",
            vec![RemoteMediaItemUpsert {
                id: item_id.to_string(),
                name: "Persistent filters".to_string(),
                path: "provider://persistent-query-filters/movie.mkv".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({"Genres": ["Drama"], "Tags": ["Stable"]}),
            }],
        )
        .await
        .unwrap();
        let source_completed_at: String = sqlx::query_scalar(
            "SELECT completed_at FROM media_item_query_filter_sources WHERE item_id = ?1",
        )
        .bind(item_id.to_string())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_query_filter_rebuild BEFORE DELETE \
             ON media_item_query_filter_sources BEGIN \
             SELECT RAISE(ABORT, 'query-filter projection was rebuilt'); END",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        db.pool.close().await;

        let reconciled = Database::connect(&database_url).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT completed_at FROM media_item_query_filter_sources WHERE item_id = ?1",
            )
            .bind(item_id.to_string())
            .fetch_one(&reconciled.pool)
            .await
            .unwrap(),
            source_completed_at
        );
        let marker = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT source_item_count, projected_facet_count, completed_at \
             FROM jellyrin_derived_projection_versions WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
        .fetch_one(&reconciled.pool)
        .await
        .unwrap();
        assert_eq!((marker.0, marker.1), (1, 2));
        reconciled.pool.close().await;

        let unchanged = Database::connect(&database_url).await.unwrap();
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, String)>(
                "SELECT source_item_count, projected_facet_count, completed_at \
                 FROM jellyrin_derived_projection_versions WHERE projection_name = ?1",
            )
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
            .fetch_one(&unchanged.pool)
            .await
            .unwrap(),
            marker
        );
    }

    #[tokio::test]
    async fn sqlite_connect_rebuilds_stale_facets_once_and_rejects_future_extractors() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("facet-projection.db");
        std::fs::File::create(&path).unwrap();
        let database_url = format!("sqlite://{}", path.display());
        let db = Database::connect(&database_url).await.unwrap();
        let item_id = Uuid::new_v4();
        db.replace_remote_media_library_snapshot(
            "Persistent facets",
            "movies",
            "provider://persistent-facets",
            vec![RemoteMediaItemUpsert {
                id: item_id.to_string(),
                name: "Persistent Genre".to_string(),
                path: "provider://persistent-facets/movie.mkv".to_string(),
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks: None,
                bitrate: None,
                width: None,
                height: None,
                media_streams: Vec::new(),
                metadata: json!({
                    "Genres": ["Rebuilt Genre"],
                    "PremiereDate": "2035-02-03T04:05:06.123456789Z",
                    "People": [{"Name": "Rebuilt Person", "Id": "Person-ID"}],
                    "Studios": [{"Name": "Rebuilt Studio", "Id": "Studio-ID"}],
                    "Tags": ["Rebuilt Tag"]
                }),
            }],
        )
        .await
        .unwrap();
        sqlx::query("DELETE FROM media_item_genre_selectors")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM media_item_upcoming_dates")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM media_item_filter_selectors")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jellyrin_derived_projection_versions SET extractor_version = 3 \
             WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .execute(&db.pool)
        .await
        .unwrap();
        db.pool.close().await;

        let rebuilt = Database::connect(&database_url).await.unwrap();
        let page = rebuilt
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                genre_ids: vec!["rebuilt genre".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total_record_count, 1);
        assert_eq!(page.items[0].item.id, item_id);
        let upcoming_date = sqlx::query_as::<_, (i64, i32)>(
            "SELECT unix_seconds, nanosecond FROM media_item_upcoming_dates \
             WHERE item_id IN (?1, ?2)",
        )
        .bind(item_id.simple().to_string())
        .bind(item_id.to_string())
        .fetch_one(&rebuilt.pool)
        .await
        .unwrap();
        assert_eq!(upcoming_date.1, 123_456_789);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM media_item_filter_selectors \
                 WHERE item_id IN (?1, ?2)",
            )
            .bind(item_id.simple().to_string())
            .bind(item_id.to_string())
            .fetch_one(&rebuilt.pool)
            .await
            .unwrap(),
            7,
            "v3 to v4 rebuild must publish raw/stable/imported entity selectors and tag"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i32>(
                "SELECT extractor_version FROM jellyrin_derived_projection_versions \
                 WHERE projection_name = ?1",
            )
            .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
            .fetch_one(&rebuilt.pool)
            .await
            .unwrap(),
            MEDIA_ITEM_FACET_PROJECTION_VERSION
        );
        let completed_at = sqlx::query_scalar::<_, String>(
            "SELECT completed_at FROM jellyrin_derived_projection_versions \
             WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .fetch_one(&rebuilt.pool)
        .await
        .unwrap();
        rebuilt.pool.close().await;

        let current = Database::connect(&database_url).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT completed_at FROM jellyrin_derived_projection_versions \
                 WHERE projection_name = ?1",
            )
            .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
            .fetch_one(&current.pool)
            .await
            .unwrap(),
            completed_at,
            "a current SQLite projection must not be rebuilt on every connection"
        );
        sqlx::query(
            "UPDATE jellyrin_derived_projection_versions SET extractor_version = ?2 \
             WHERE projection_name = ?1",
        )
        .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION + 1)
        .execute(&current.pool)
        .await
        .unwrap();
        current.pool.close().await;

        let error = match Database::connect(&database_url).await {
            Ok(_) => panic!("future SQLite facet extractor must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("newer than supported"));
    }

    #[tokio::test]
    async fn plugin_runtime_instance_updates_installed_plugin_health_and_events() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let plugin_id = "dddddddd-dddd-dddd-dddd-dddddddddddd";
        db.install_plugin_package(
            InstallPluginPackage {
                plugin_id: plugin_id.to_string(),
                name: "Runtime Fixture".to_string(),
                version: "0.1.0".to_string(),
                runtime: "RustWasi".to_string(),
                target_abi: "jellyrin-wasi-0.1".to_string(),
                package: json!({
                    "Guid": plugin_id,
                    "Name": "Runtime Fixture",
                    "Runtime": "RustWasi"
                }),
                manifest: json!({
                    "Guid": plugin_id,
                    "Name": "Runtime Fixture",
                    "Version": "0.1.0",
                    "Runtime": "RustWasi"
                }),
            },
            None,
        )
        .await
        .unwrap();

        let recorded = db
            .upsert_plugin_runtime_instance(
                PluginRuntimeInstanceUpsert {
                    plugin_id: plugin_id.to_string(),
                    runtime: "RustWasi".to_string(),
                    runtime_version: "0.1.0".to_string(),
                    status: "Active".to_string(),
                    process_id: Some(4242),
                    endpoint: Some("stdio".to_string()),
                    health: json!({
                        "Status": "Healthy",
                        "Message": "RustWasi sidecar loaded metadata."
                    }),
                    capabilities: vec!["ScheduledTask".to_string(), "MetadataProvider".to_string()],
                    last_error: None,
                },
                None,
            )
            .await
            .unwrap();
        assert!(recorded);

        let plugin = db.installed_plugin_json(plugin_id).await.unwrap().unwrap();
        assert_eq!(plugin["Status"], "Active");
        assert_eq!(plugin["RuntimeVersion"], "0.1.0");
        assert_eq!(plugin["Health"]["Status"], "Healthy");
        assert_eq!(plugin["Capabilities"][0], "ScheduledTask");
        assert_eq!(plugin["RuntimeInstances"].as_array().unwrap().len(), 1);
        assert_eq!(plugin["RuntimeInstances"][0]["Status"], "Active");
        assert_eq!(plugin["RuntimeInstances"][0]["Endpoint"], "stdio");
        assert_eq!(plugin["RecentEvents"][0]["EventType"], "RuntimeStatus");

        let snapshot = db.plugin_platform_snapshot().await.unwrap();
        assert_eq!(snapshot["PluginRuntimeInstances"]["Count"], 1);
        assert!(
            snapshot["PluginHostEvents"]["Items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["EventType"] == "RuntimeStatus")
        );
    }
}
