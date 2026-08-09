use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose};
use jellyrin_core::{
    LIVE_TV_REMOTE_USER_AGENT, LIVE_TV_XTREAM_DEFAULT_EPG_LIMIT, LIVE_TV_XTREAM_MAX_EPG_CHANNELS,
    LIVE_TV_XTREAM_MAX_IMPORT_LIMIT, format_time_for_json, json_string_field,
    json_string_list_field, live_tv_stable_id, live_tv_u64_field, stable_entity_id,
};
use jellyrin_db::{
    LiveTvChannelUpsert, REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS,
    REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS, RemoteMediaCatalogStage, RemoteMediaItemUpsert,
    RemoteMediaLibrarySnapshot, RemoteMediaLibraryStageSpec, XtreamCatalogStore,
};
use reqwest::Client as HttpClient;
use serde::de::{DeserializeOwned, DeserializeSeed, Error as _, SeqAccess, Visitor};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

/// Xtream provider type identifier.
pub const XTREAM_PROVIDER_TYPE: &str = "xtream";
const XTREAM_PRIMARY_TUNER_ID: &str = "xtream-plugin";

const XTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const XTREAM_DNS_TIMEOUT: Duration = Duration::from_secs(10);
const XTREAM_MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const XTREAM_MAX_BASE_URL_BYTES: usize = 16 * 1024;
const XTREAM_MAX_IMAGE_URL_BYTES: usize = 16 * 1024;
const XTREAM_MAX_CREDENTIAL_BYTES: usize = 4 * 1024;
const XTREAM_MAX_CATALOG_BODY_BYTES: usize = 64 * 1024 * 1024;
const XTREAM_MAX_CATEGORY_BODY_BYTES: usize = 8 * 1024 * 1024;
const XTREAM_MAX_SERIES_INFO_BODY_BYTES: usize = 16 * 1024 * 1024;
const XTREAM_MAX_EPG_BODY_BYTES: usize = 2 * 1024 * 1024;
const XTREAM_MAX_CATEGORY_ITEMS: usize = 10_000;
const XTREAM_MAX_SERIES_REQUESTS: usize = 100_000;
const XTREAM_MAX_EPISODES_PER_SERIES: usize = 10_000;
const XTREAM_MAX_EPG_LISTINGS: usize = 256;
const XTREAM_ARRAY_CHUNK_ITEMS: usize = 500;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum XtreamFetchError {
    InvalidInput,
    Dns,
    AddressNotAllowed,
    Client,
    Request { timeout: bool, connect: bool },
    Http(reqwest::StatusCode),
    BodyTooLarge,
    InvalidJson,
    InvalidCatalog,
    TooManyItems,
}

/// A provider import failed before a complete catalogue snapshot was available.
///
/// The operation name is intentionally coarse and the error never contains the
/// provider URL or credentials, so it is safe to surface through scheduled-task
/// diagnostics.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct XtreamImportError {
    operation: &'static str,
    source: XtreamFetchError,
}

impl XtreamImportError {
    fn new(operation: &'static str, source: XtreamFetchError) -> Self {
        Self { operation, source }
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Display for XtreamImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.source {
            XtreamFetchError::InvalidInput => "invalid input",
            XtreamFetchError::Dns => "DNS resolution failed",
            XtreamFetchError::AddressNotAllowed => "provider address is not allowed",
            XtreamFetchError::Client => "HTTP client setup failed",
            XtreamFetchError::Request { timeout: true, .. } => "request timed out",
            XtreamFetchError::Request { connect: true, .. } => "connection failed",
            XtreamFetchError::Request { .. } => "request failed",
            XtreamFetchError::Http(status) => {
                return write!(
                    formatter,
                    "Xtream {} failed with HTTP status {}",
                    self.operation,
                    status.as_u16()
                );
            }
            XtreamFetchError::BodyTooLarge => "response body exceeded its limit",
            XtreamFetchError::InvalidJson => "response was not a valid catalogue",
            XtreamFetchError::InvalidCatalog => "catalogue contained malformed or duplicate items",
            XtreamFetchError::TooManyItems => "catalogue exceeded its item limit",
        };
        write!(formatter, "Xtream {} failed: {reason}", self.operation)
    }
}

impl std::error::Error for XtreamImportError {}

struct ValidatedXtreamClient {
    base_url: reqwest::Url,
    client: HttpClient,
}

impl ValidatedXtreamClient {
    async fn new(base_url: &str) -> Result<Self, XtreamFetchError> {
        let base_url = validated_xtream_base_url(base_url)?;
        let host = base_url
            .host_str()
            .ok_or(XtreamFetchError::InvalidInput)?
            .to_string();
        let port = base_url
            .port_or_known_default()
            .ok_or(XtreamFetchError::InvalidInput)?;
        let allow_private = private_provider_networks_allowed();
        let resolved = tokio::time::timeout(
            XTREAM_DNS_TIMEOUT,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await
        .map_err(|_| XtreamFetchError::Dns)?
        .map_err(|_| XtreamFetchError::Dns)?;
        let mut pinned = resolved
            .filter(|address| provider_address_allowed(address.ip(), allow_private))
            .collect::<Vec<_>>();
        pinned.sort_unstable();
        pinned.dedup();
        if pinned.is_empty() {
            return Err(XtreamFetchError::AddressNotAllowed);
        }

        let client = HttpClient::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(XTREAM_CONNECT_TIMEOUT)
            .timeout(XTREAM_MAX_REQUEST_TIMEOUT)
            .pool_max_idle_per_host(2)
            .user_agent(LIVE_TV_REMOTE_USER_AGENT)
            .resolve_to_addrs(&host, &pinned)
            .build()
            .map_err(|_| XtreamFetchError::Client)?;
        Ok(Self { base_url, client })
    }

    fn player_api_url(
        &self,
        username: &str,
        password: &str,
        action: &str,
    ) -> Result<reqwest::Url, XtreamFetchError> {
        if !valid_xtream_secret(username) || !valid_xtream_secret(password) {
            return Err(XtreamFetchError::InvalidInput);
        }
        let mut url = self.base_url.clone();
        url.set_path("player_api.php");
        url.query_pairs_mut()
            .append_pair("username", username)
            .append_pair("password", password)
            .append_pair("action", action);
        Ok(url)
    }
}

pub struct LiveTvXtreamImport {
    pub channels: Vec<serde_json::Value>,
    pub categories: Vec<serde_json::Value>,
}

pub struct XtreamMediaImport {
    pub tuner_id: String,
    pub movies: Vec<RemoteMediaItemUpsert>,
    pub series_episodes: Vec<RemoteMediaItemUpsert>,
}

/// Durable, credential-free route for an Xtream source. The provider URL and
/// credentials remain in the tuner configuration and are combined with this
/// reference only when Jellyrin opens playback or probes the item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct XtreamRemoteSourceRef {
    pub version: u8,
    pub provider: String,
    pub tuner_id: String,
    pub kind: String,
    pub remote_id: String,
    pub extension: String,
}

impl XtreamRemoteSourceRef {
    fn new(tuner_id: &str, kind: &str, remote_id: &str, extension: &str) -> Option<Self> {
        if tuner_id.trim().is_empty()
            || tuner_id.len() > 512
            || tuner_id.chars().any(char::is_control)
            || !valid_xtream_identifier(remote_id)
        {
            return None;
        }
        let extension = sanitized_xtream_extension(extension)?;
        matches!(kind, "live" | "vod" | "series-episode").then(|| Self {
            version: 1,
            provider: XTREAM_PROVIDER_TYPE.to_string(),
            tuner_id: tuner_id.trim().to_string(),
            kind: kind.to_string(),
            remote_id: remote_id.to_string(),
            extension,
        })
    }
}

pub fn xtream_remote_source_ref(value: &serde_json::Value) -> Option<XtreamRemoteSourceRef> {
    let reference = serde_json::from_value::<XtreamRemoteSourceRef>(value.clone()).ok()?;
    if reference.version != 1
        || !reference
            .provider
            .eq_ignore_ascii_case(XTREAM_PROVIDER_TYPE)
    {
        return None;
    }
    XtreamRemoteSourceRef::new(
        &reference.tuner_id,
        &reference.kind,
        &reference.remote_id,
        &reference.extension,
    )
}

pub fn resolve_remote_source_ref(
    tuner_config: &serde_json::Value,
    value: &serde_json::Value,
) -> Option<String> {
    let reference = xtream_remote_source_ref(value)?;
    let configured_tuner_id = json_string_field(tuner_config, "Id")?;
    if !configured_tuner_id.eq_ignore_ascii_case(&reference.tuner_id) {
        return None;
    }
    let base_url = validated_xtream_base_url(&json_string_field(tuner_config, "Url")?).ok()?;
    let username = json_string_field(tuner_config, "Username")
        .or_else(|| json_string_field(tuner_config, "UserName"))?;
    let password = json_string_field(tuner_config, "Password")?;
    if !valid_xtream_secret(&username) || !valid_xtream_secret(&password) {
        return None;
    }
    match reference.kind.as_str() {
        "live" => stream_url(&base_url, &username, &password, &reference.remote_id),
        "vod" => movie_url(
            &base_url,
            &username,
            &password,
            &reference.remote_id,
            &reference.extension,
        ),
        "series-episode" => series_url(
            &base_url,
            &username,
            &password,
            &reference.remote_id,
            &reference.extension,
        ),
        _ => None,
    }
}

fn encoded_live_provider_reference(reference: &XtreamRemoteSourceRef) -> Option<String> {
    let bytes = serde_json::to_vec(reference).ok()?;
    Some(format!(
        "xtream:v1:{}",
        general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn resolve_live_provider_reference(
    tuner_config: &serde_json::Value,
    provider_reference: &str,
) -> Option<String> {
    let encoded = provider_reference.trim().strip_prefix("xtream:v1:")?;
    if encoded.is_empty() || encoded.len() > 4 * 1024 {
        return None;
    }
    let bytes = general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    let reference = xtream_remote_source_ref(&value)?;
    (reference.kind == "live")
        .then(|| resolve_remote_source_ref(tuner_config, &value))
        .flatten()
}

#[derive(Default)]
pub struct LiveTvXtreamImportOptions {
    include_category_ids: HashSet<String>,
    exclude_category_ids: HashSet<String>,
    limit: Option<usize>,
}

impl LiveTvXtreamImportOptions {
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        // Prefer the live-specific key; fall back to legacy keys for compatibility.
        let include_category_ids = category_id_filter(
            payload,
            &[
                "LiveCategoryIds",
                "CategoryIds",
                "IncludeCategoryIds",
                "Categories",
            ],
        );
        let exclude_category_ids =
            category_id_filter(payload, &["ExcludeLiveCategoryIds", "ExcludeCategoryIds"]);
        let limit = live_tv_u64_field(payload, "Limit")
            .or_else(|| live_tv_u64_field(payload, "ChannelLimit"))
            .map(|value| bounded_usize(value, 1, LIVE_TV_XTREAM_MAX_IMPORT_LIMIT));
        Self {
            include_category_ids,
            exclude_category_ids,
            limit,
        }
    }

    fn allows(&self, stream: &serde_json::Value) -> bool {
        let category_id = item_category_id(stream);
        if !self.include_category_ids.is_empty()
            && category_id
                .as_ref()
                .is_none_or(|id| !self.include_category_ids.contains(id))
        {
            return false;
        }
        !category_id
            .as_ref()
            .is_some_and(|id| self.exclude_category_ids.contains(id))
    }
}

/// Import a complete live-TV snapshot while preserving the distinction between
/// an unavailable provider (`Err`), an incomplete configuration (`Ok(None)`),
/// and a successfully fetched but empty catalogue (`Ok(Some(empty))`).
pub async fn try_import_from_payload(
    payload: &serde_json::Value,
) -> Result<Option<LiveTvXtreamImport>, XtreamImportError> {
    let Some(base_url) = json_string_field(payload, "Url") else {
        return Ok(None);
    };
    let tuner_id = json_string_field(payload, "Id")
        .unwrap_or_else(|| stable_entity_id("xtream-tuner", &base_url));
    let Some(username) =
        json_string_field(payload, "Username").or_else(|| json_string_field(payload, "UserName"))
    else {
        return Ok(None);
    };
    let Some(password) = json_string_field(payload, "Password") else {
        return Ok(None);
    };
    if !valid_xtream_secret(&username) || !valid_xtream_secret(&password) {
        return Ok(None);
    }
    let client = ValidatedXtreamClient::new(&base_url)
        .await
        .map_err(|error| XtreamImportError::new("provider connection", error))?;
    let streams = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_live_streams",
        Duration::from_secs(30),
        XTREAM_MAX_CATALOG_BODY_BYTES,
        LIVE_TV_XTREAM_MAX_IMPORT_LIMIT,
    )
    .await
    .map_err(|error| XtreamImportError::new("live streams", error))?;
    let categories = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_live_categories",
        Duration::from_secs(15),
        XTREAM_MAX_CATEGORY_BODY_BYTES,
        XTREAM_MAX_CATEGORY_ITEMS,
    )
    .await
    .map_err(|error| XtreamImportError::new("live categories", error))?;
    let parsed_categories = parse_categories(&categories);
    if parsed_categories.len() != categories.len() || !unique_live_catalog(&parsed_categories) {
        return Err(XtreamImportError::new(
            "live categories",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    let options = LiveTvXtreamImportOptions::from_payload(payload);
    if !streams
        .iter()
        .filter(|stream| options.allows(stream))
        .take(options.limit.unwrap_or(usize::MAX))
        .all(|stream| {
            let stream_id = json_string_field(stream, "stream_id")
                .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()));
            direct_source_matches_reconstructed(
                stream,
                stream_id.as_deref().and_then(|stream_id| {
                    stream_url(&client.base_url, &username, &password, stream_id)
                }),
            )
        })
    {
        return Err(XtreamImportError::new(
            "live streams",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    let mut channels = parse_streams(&tuner_id, &streams, &options);
    let selected_channel_count = streams
        .iter()
        .filter(|stream| options.allows(stream))
        .take(options.limit.unwrap_or(usize::MAX))
        .count();
    if channels.len() != selected_channel_count || !unique_live_catalog(&channels) {
        return Err(XtreamImportError::new(
            "live streams",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    apply_category_names(&mut channels, &parsed_categories);
    Ok(Some(LiveTvXtreamImport {
        channels,
        categories: parsed_categories,
    }))
}

/// Compatibility wrapper used by the Live TV provider registry. A failed fetch
/// returns `None`, so its caller retains the last complete snapshot; a valid
/// empty response remains `Some(empty)` and is allowed to clear stale entries.
pub async fn import_from_payload(payload: &serde_json::Value) -> Option<LiveTvXtreamImport> {
    match try_import_from_payload(payload).await {
        Ok(import) => import,
        Err(error) => {
            tracing::warn!(operation = error.operation(), %error, "Xtream import aborted");
            None
        }
    }
}

/// Import a complete VOD/series snapshot without converting remote failures to
/// empty vectors. Persistence is only attempted after every required catalogue
/// response (including each requested series detail) has succeeded.
pub async fn try_import_media_from_payload(
    payload: &serde_json::Value,
) -> Result<Option<XtreamMediaImport>, XtreamImportError> {
    let Some(base_url) = json_string_field(payload, "Url") else {
        return Ok(None);
    };
    let tuner_id = json_string_field(payload, "Id")
        .unwrap_or_else(|| stable_entity_id("xtream-tuner", &base_url));
    let Some(username) =
        json_string_field(payload, "Username").or_else(|| json_string_field(payload, "UserName"))
    else {
        return Ok(None);
    };
    let Some(password) = json_string_field(payload, "Password") else {
        return Ok(None);
    };
    if !valid_xtream_secret(&username) || !valid_xtream_secret(&password) {
        return Ok(None);
    }
    let client = ValidatedXtreamClient::new(&base_url)
        .await
        .map_err(|error| XtreamImportError::new("provider connection", error))?;
    // VOD / movies: filter by selected VOD categories and an optional movie limit.
    let vod_selection = CategorySelection::from_payload(
        payload,
        &["VodCategoryIds", "MovieCategoryIds"],
        &["ExcludeVodCategoryIds", "ExcludeMovieCategoryIds"],
    );
    let movie_limit = live_tv_u64_field(payload, "MovieLimit")
        .or_else(|| live_tv_u64_field(payload, "VodLimit"))
        .and_then(|value| positive_bounded_usize(value, LIVE_TV_XTREAM_MAX_IMPORT_LIMIT));
    let movie_categories = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_vod_categories",
        Duration::from_secs(15),
        XTREAM_MAX_CATEGORY_BODY_BYTES,
        XTREAM_MAX_CATEGORY_ITEMS,
    )
    .await
    .map_err(|error| XtreamImportError::new("VOD categories", error))?;
    let parsed_movie_categories = parse_categories(&movie_categories);
    if parsed_movie_categories.len() != movie_categories.len()
        || !unique_live_catalog(&parsed_movie_categories)
    {
        return Err(XtreamImportError::new(
            "VOD categories",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    let movies = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_vod_streams",
        Duration::from_secs(45),
        XTREAM_MAX_CATALOG_BODY_BYTES,
        LIVE_TV_XTREAM_MAX_IMPORT_LIMIT,
    )
    .await
    .map_err(|error| XtreamImportError::new("VOD streams", error))?;
    let mut movie_streams = movies;
    movie_streams.retain(|stream| vod_selection.allows(item_category_id(stream).as_deref()));
    if let Some(limit) = movie_limit {
        movie_streams.truncate(limit);
    }
    if !movie_streams.iter().all(|stream| {
        let stream_id = json_string_field(stream, "stream_id")
            .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()));
        let Some(extension) = xtream_extension(stream, "container_extension", "mp4") else {
            return false;
        };
        direct_source_matches_reconstructed(
            stream,
            stream_id.as_deref().and_then(|stream_id| {
                movie_url(
                    &client.base_url,
                    &username,
                    &password,
                    stream_id,
                    &extension,
                )
            }),
        )
    }) {
        return Err(XtreamImportError::new(
            "VOD streams",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    let movies = parse_vod_streams(&tuner_id, &movie_streams, &parsed_movie_categories);
    if movies.len() != movie_streams.len() || !unique_remote_media_catalog(&movies) {
        return Err(XtreamImportError::new(
            "VOD streams",
            XtreamFetchError::InvalidCatalog,
        ));
    }

    // Series: filter by selected series categories before applying the series limit.
    let series_selection = CategorySelection::from_payload(
        payload,
        &["SeriesCategoryIds"],
        &["ExcludeSeriesCategoryIds"],
    );
    let series_categories = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_series_categories",
        Duration::from_secs(15),
        XTREAM_MAX_CATEGORY_BODY_BYTES,
        XTREAM_MAX_CATEGORY_ITEMS,
    )
    .await
    .map_err(|error| XtreamImportError::new("series categories", error))?;
    let parsed_series_categories = parse_categories(&series_categories);
    if parsed_series_categories.len() != series_categories.len()
        || !unique_live_catalog(&parsed_series_categories)
    {
        return Err(XtreamImportError::new(
            "series categories",
            XtreamFetchError::InvalidCatalog,
        ));
    }
    let mut series = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_series",
        Duration::from_secs(45),
        XTREAM_MAX_CATALOG_BODY_BYTES,
        LIVE_TV_XTREAM_MAX_IMPORT_LIMIT,
    )
    .await
    .map_err(|error| XtreamImportError::new("series catalogue", error))?;
    let series_limit = live_tv_u64_field(payload, "SeriesLimit")
        .or_else(|| live_tv_u64_field(payload, "XtreamSeriesLimit"))
        .and_then(|value| positive_bounded_usize(value, XTREAM_MAX_SERIES_REQUESTS));
    let episode_limit = live_tv_u64_field(payload, "SeriesEpisodeLimit")
        .or_else(|| live_tv_u64_field(payload, "XtreamSeriesEpisodeLimit"))
        .map(|value| bounded_usize(value, 1, XTREAM_MAX_EPISODES_PER_SERIES))
        .unwrap_or(XTREAM_MAX_EPISODES_PER_SERIES);
    series.retain(|item| series_selection.allows(item_category_id(item).as_deref()));
    if let Some(series_limit) = series_limit {
        series.truncate(series_limit);
    }
    let mut series_episodes = Vec::new();
    for series_item in &series {
        if series_episodes.len() >= LIVE_TV_XTREAM_MAX_IMPORT_LIMIT {
            break;
        }
        let Some(series_id) = json_string_field(series_item, "series_id")
            .or_else(|| live_tv_u64_field(series_item, "series_id").map(|id| id.to_string()))
        else {
            return Err(XtreamImportError::new(
                "series catalogue",
                XtreamFetchError::InvalidCatalog,
            ));
        };
        if !valid_xtream_identifier(&series_id) {
            return Err(XtreamImportError::new(
                "series catalogue",
                XtreamFetchError::InvalidCatalog,
            ));
        }
        let info = fetch_series_info(&client, &username, &password, &series_id)
            .await
            .map_err(|error| XtreamImportError::new("series details", error))?;
        let remaining = LIVE_TV_XTREAM_MAX_IMPORT_LIMIT - series_episodes.len();
        let series_episode_limit = episode_limit.min(remaining);
        if !valid_series_episode_prefix(&info, series_episode_limit) {
            return Err(XtreamImportError::new(
                "series details",
                XtreamFetchError::InvalidCatalog,
            ));
        }
        if !xtream_episode_values(&info)
            .into_iter()
            .take(series_episode_limit)
            .all(|(_, episode)| {
                let episode_id = json_string_field(episode, "id")
                    .or_else(|| live_tv_u64_field(episode, "id").map(|id| id.to_string()));
                let Some(extension) = xtream_extension(episode, "container_extension", "mp4")
                else {
                    return false;
                };
                let reconstructed = episode_id.as_deref().and_then(|episode_id| {
                    series_url(
                        &client.base_url,
                        &username,
                        &password,
                        episode_id,
                        &extension,
                    )
                });
                direct_source_matches_reconstructed(episode, reconstructed.clone())
                    && direct_source_matches_reconstructed(
                        episode.get("info").unwrap_or(episode),
                        reconstructed,
                    )
            })
        {
            return Err(XtreamImportError::new(
                "series details",
                XtreamFetchError::InvalidCatalog,
            ));
        }
        let parsed_episodes = parse_series_episodes(
            &tuner_id,
            series_item,
            &info,
            &parsed_series_categories,
            Some(series_episode_limit),
        );
        if parsed_episodes.len() != series_episode_count(&info).min(series_episode_limit) {
            return Err(XtreamImportError::new(
                "series details",
                XtreamFetchError::InvalidCatalog,
            ));
        }
        series_episodes.extend(parsed_episodes);
    }

    if !unique_remote_media_catalog(&series_episodes) {
        return Err(XtreamImportError::new(
            "series details",
            XtreamFetchError::InvalidCatalog,
        ));
    }

    Ok(Some(XtreamMediaImport {
        tuner_id,
        movies,
        series_episodes,
    }))
}

/// Compatibility wrapper for callers that only need an in-memory import.
/// Scheduled synchronisation uses durable incremental staging instead, so large
/// catalogues do not need to coexist as one `Vec` before publication.
pub async fn import_media_from_payload(payload: &serde_json::Value) -> Option<XtreamMediaImport> {
    match try_import_media_from_payload(payload).await {
        Ok(import) => import,
        Err(error) => {
            tracing::warn!(operation = error.operation(), %error, "Xtream media import aborted");
            None
        }
    }
}

struct XtreamArrayChunks {
    receiver: tokio::sync::mpsc::Receiver<Vec<serde_json::Value>>,
    parser: tokio::task::JoinHandle<Result<usize, XtreamFetchError>>,
}

struct XtreamArrayFetchOptions<'a> {
    action: &'a str,
    timeout: Duration,
    max_bytes: usize,
    max_items: usize,
    query: &'a [(&'a str, &'a str)],
}

impl XtreamArrayChunks {
    async fn finish(self) -> Result<usize, XtreamFetchError> {
        self.parser
            .await
            .map_err(|_| XtreamFetchError::InvalidJson)?
    }
}

struct XtreamArrayChunkSeed {
    sender: tokio::sync::mpsc::Sender<Vec<serde_json::Value>>,
    maximum_items: usize,
    overflowed: Arc<AtomicBool>,
}

impl<'de> DeserializeSeed<'de> for XtreamArrayChunkSeed {
    type Value = usize;

    fn deserialize<Deserializer>(
        self,
        deserializer: Deserializer,
    ) -> Result<Self::Value, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(XtreamArrayChunkVisitor {
            sender: self.sender,
            maximum_items: self.maximum_items,
            overflowed: self.overflowed,
        })
    }
}

struct XtreamArrayChunkVisitor {
    sender: tokio::sync::mpsc::Sender<Vec<serde_json::Value>>,
    maximum_items: usize,
    overflowed: Arc<AtomicBool>,
}

impl<'de> Visitor<'de> for XtreamArrayChunkVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an Xtream JSON array")
    }

    fn visit_seq<Access>(self, mut sequence: Access) -> Result<Self::Value, Access::Error>
    where
        Access: SeqAccess<'de>,
    {
        let mut inspected = 0usize;
        let mut chunk = Vec::with_capacity(XTREAM_ARRAY_CHUNK_ITEMS);
        while let Some(value) = sequence.next_element::<serde_json::Value>()? {
            inspected = inspected.saturating_add(1);
            if inspected > self.maximum_items {
                self.overflowed.store(true, Ordering::Relaxed);
                return Err(Access::Error::custom("Xtream array item limit exceeded"));
            }
            chunk.push(value);
            if chunk.len() == XTREAM_ARRAY_CHUNK_ITEMS {
                self.sender
                    .blocking_send(std::mem::take(&mut chunk))
                    .map_err(|_| Access::Error::custom("Xtream array consumer stopped"))?;
                chunk.reserve(XTREAM_ARRAY_CHUNK_ITEMS);
            }
        }
        if !chunk.is_empty() {
            self.sender
                .blocking_send(chunk)
                .map_err(|_| Access::Error::custom("Xtream array consumer stopped"))?;
        }
        Ok(inspected)
    }
}

fn parse_xtream_array_reader<Reader>(
    reader: Reader,
    sender: tokio::sync::mpsc::Sender<Vec<serde_json::Value>>,
    maximum_items: usize,
    overflowed: Arc<AtomicBool>,
) -> Result<usize, XtreamFetchError>
where
    Reader: std::io::Read,
{
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let parsed = XtreamArrayChunkSeed {
        sender,
        maximum_items,
        overflowed: Arc::clone(&overflowed),
    }
    .deserialize(&mut deserializer)
    .and_then(|count| {
        deserializer.end()?;
        Ok(count)
    });
    match parsed {
        Ok(count) => Ok(count),
        Err(_) if overflowed.load(Ordering::Relaxed) => Err(XtreamFetchError::TooManyItems),
        Err(_) => Err(XtreamFetchError::InvalidJson),
    }
}

async fn fetch_xtream_array_chunks(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    options: XtreamArrayFetchOptions<'_>,
) -> Result<XtreamArrayChunks, XtreamFetchError> {
    let mut url = client.player_api_url(username, password, options.action)?;
    for (key, value) in options.query {
        url.query_pairs_mut().append_pair(key, value);
    }
    let mut response = client
        .client
        .get(url)
        .timeout(options.timeout.min(XTREAM_MAX_REQUEST_TIMEOUT))
        .send()
        .await
        .map_err(|error| XtreamFetchError::Request {
            timeout: error.is_timeout(),
            connect: error.is_connect(),
        })?;
    if !response.status().is_success() {
        return Err(XtreamFetchError::Http(response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > options.max_bytes as u64)
    {
        return Err(XtreamFetchError::BodyTooLarge);
    }

    let temporary = tempfile::tempfile().map_err(|_| XtreamFetchError::Client)?;
    let mut file = tokio::fs::File::from_std(temporary);
    let mut received_bytes = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| XtreamFetchError::Request {
            timeout: error.is_timeout(),
            connect: error.is_connect(),
        })?
    {
        received_bytes = received_bytes.saturating_add(chunk.len());
        if received_bytes > options.max_bytes {
            return Err(XtreamFetchError::BodyTooLarge);
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| XtreamFetchError::Client)?;
    }
    file.flush().await.map_err(|_| XtreamFetchError::Client)?;
    file.rewind().await.map_err(|_| XtreamFetchError::Client)?;
    let file = file.into_std().await;
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let overflowed = Arc::new(AtomicBool::new(false));
    let parser_overflowed = Arc::clone(&overflowed);
    let maximum_items = options.max_items;
    let parser = tokio::task::spawn_blocking(move || {
        parse_xtream_array_reader(file, sender, maximum_items, parser_overflowed)
    });
    Ok(XtreamArrayChunks { receiver, parser })
}

async fn fetch_xtream_array(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    action: &str,
    timeout: Duration,
    max_bytes: usize,
    max_items: usize,
) -> Result<Vec<serde_json::Value>, XtreamFetchError> {
    let result = fetch_xtream_array_chunks(
        client,
        username,
        password,
        XtreamArrayFetchOptions {
            action,
            timeout,
            max_bytes,
            max_items,
            query: &[],
        },
    )
    .await;
    match result {
        Ok(mut chunks) => {
            let mut values = Vec::new();
            while let Some(chunk) = chunks.receiver.recv().await {
                values.extend(chunk);
            }
            match chunks.finish().await {
                Ok(_) => Ok(values),
                Err(error) => {
                    warn_xtream_fetch(action, error);
                    Err(error)
                }
            }
        }
        Err(error) => {
            warn_xtream_fetch(action, error);
            Err(error)
        }
    }
}

async fn fetch_series_info(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    series_id: &str,
) -> Result<serde_json::Value, XtreamFetchError> {
    if !valid_xtream_identifier(series_id) {
        return Err(XtreamFetchError::InvalidInput);
    }
    let result = fetch_xtream_json::<serde_json::Value>(
        client,
        username,
        password,
        "get_series_info",
        Duration::from_secs(20),
        XTREAM_MAX_SERIES_INFO_BODY_BYTES,
        &[("series_id", series_id)],
    )
    .await;
    match result {
        Ok(value)
            if valid_series_info_shape(&value)
                && series_episode_count(&value) <= XTREAM_MAX_EPISODES_PER_SERIES =>
        {
            Ok(value)
        }
        Ok(value) if !valid_series_info_shape(&value) => {
            warn_xtream_fetch("get_series_info", XtreamFetchError::InvalidCatalog);
            Err(XtreamFetchError::InvalidCatalog)
        }
        Ok(_) => {
            warn_xtream_fetch("get_series_info", XtreamFetchError::TooManyItems);
            Err(XtreamFetchError::TooManyItems)
        }
        Err(error) => {
            warn_xtream_fetch("get_series_info", error);
            Err(error)
        }
    }
}

async fn fetch_xtream_json<Value>(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    action: &str,
    timeout: Duration,
    max_bytes: usize,
    query: &[(&str, &str)],
) -> Result<Value, XtreamFetchError>
where
    Value: DeserializeOwned,
{
    let mut url = client.player_api_url(username, password, action)?;
    for (key, value) in query {
        url.query_pairs_mut().append_pair(key, value);
    }
    let response = client
        .client
        .get(url)
        .timeout(timeout.min(XTREAM_MAX_REQUEST_TIMEOUT))
        .send()
        .await
        .map_err(|error| XtreamFetchError::Request {
            timeout: error.is_timeout(),
            connect: error.is_connect(),
        })?;
    if !response.status().is_success() {
        return Err(XtreamFetchError::Http(response.status()));
    }
    let body = read_bounded_body(response, max_bytes).await?;
    serde_json::from_slice(&body).map_err(|_| XtreamFetchError::InvalidJson)
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, XtreamFetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(XtreamFetchError::BodyTooLarge);
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(max_bytes),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| XtreamFetchError::Request {
            timeout: error.is_timeout(),
            connect: error.is_connect(),
        })?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(XtreamFetchError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn warn_xtream_fetch(action: &str, error: XtreamFetchError) {
    match error {
        XtreamFetchError::Request { timeout, connect } => {
            tracing::warn!(action, timeout, connect, "Xtream request failed")
        }
        XtreamFetchError::Http(status) => {
            tracing::warn!(action, status = status.as_u16(), "Xtream request failed")
        }
        XtreamFetchError::InvalidInput => {
            tracing::warn!(action, reason = "invalid-input", "Xtream request rejected")
        }
        XtreamFetchError::Dns => {
            tracing::warn!(action, reason = "dns", "Xtream request rejected")
        }
        XtreamFetchError::AddressNotAllowed => tracing::warn!(
            action,
            reason = "address-not-allowed",
            "Xtream request rejected"
        ),
        XtreamFetchError::Client => {
            tracing::warn!(action, reason = "client", "Xtream request rejected")
        }
        XtreamFetchError::BodyTooLarge => tracing::warn!(
            action,
            reason = "body-too-large",
            "Xtream response rejected"
        ),
        XtreamFetchError::InvalidJson => {
            tracing::warn!(action, reason = "invalid-json", "Xtream response rejected")
        }
        XtreamFetchError::InvalidCatalog => tracing::warn!(
            action,
            reason = "invalid-catalogue",
            "Xtream response rejected"
        ),
        XtreamFetchError::TooManyItems => tracing::warn!(
            action,
            reason = "too-many-items",
            "Xtream response rejected"
        ),
    }
}

fn valid_series_info_shape(info: &serde_json::Value) -> bool {
    info.get("episodes")
        .is_some_and(|episodes| episodes.is_array() || episodes.is_object())
}

fn valid_series_episode_prefix(info: &serde_json::Value, limit: usize) -> bool {
    xtream_episode_values(info)
        .into_iter()
        .take(limit)
        .all(|(_, episode)| {
            json_string_field(episode, "id")
                .or_else(|| live_tv_u64_field(episode, "id").map(|id| id.to_string()))
                .is_some_and(|id| valid_xtream_identifier(&id))
        })
}

fn series_episode_count(info: &serde_json::Value) -> usize {
    let Some(episodes) = info.get("episodes") else {
        return 0;
    };
    if let Some(values) = episodes.as_array() {
        return values.len();
    }
    let Some(seasons) = episodes.as_object() else {
        return 0;
    };
    seasons.values().fold(0usize, |count, episodes| {
        count.saturating_add(episodes.as_array().map_or(0, Vec::len))
    })
}

fn bounded_usize(value: u64, minimum: usize, maximum: usize) -> usize {
    usize::try_from(value)
        .unwrap_or(usize::MAX)
        .clamp(minimum, maximum)
}

fn positive_bounded_usize(value: u64, maximum: usize) -> Option<usize> {
    (value > 0).then(|| bounded_usize(value, 1, maximum))
}

#[cfg(test)]
fn validate_item_count(count: usize, maximum: usize) -> Result<(), XtreamFetchError> {
    if count > maximum {
        Err(XtreamFetchError::TooManyItems)
    } else {
        Ok(())
    }
}

pub fn channel_upsert_from_json(
    tuner_id: &str,
    channel: &serde_json::Value,
) -> Option<LiveTvChannelUpsert> {
    let channel_id = json_string_field(channel, "Id")?;
    let remote_id = json_string_field(channel, "RemoteId").unwrap_or_else(|| {
        channel_id
            .strip_prefix("xtream_")
            .unwrap_or(channel_id.as_str())
            .to_string()
    });
    let name = json_string_field(channel, "Name").unwrap_or_else(|| channel_id.clone());
    let sort_name = json_string_field(channel, "SortName")
        .or_else(|| json_string_field(channel, "Number"))
        .unwrap_or_else(|| name.clone());
    let category_id = json_string_field(channel, "CategoryId")
        .map(|remote_id| category_db_id(tuner_id, &remote_id));
    let stream_url = json_string_field(channel, "Path")
        .or_else(|| json_string_field(channel, "MediaPath"))
        .unwrap_or_default();
    let has_stream_url = !stream_url.is_empty();
    let has_provider_reference = json_string_field(channel, "ProviderReference").is_some();
    if has_stream_url == has_provider_reference {
        return None;
    }
    let image_url =
        json_string_field(channel, "ImageUrl").and_then(|value| safe_xtream_image_url(&value));
    let primary_image_url = json_string_field(channel, "PrimaryImageUrl")
        .and_then(|value| safe_xtream_image_url(&value));
    let mut metadata = channel.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.retain(|key, _| {
            !key.eq_ignore_ascii_case("ImageUrl") && !key.eq_ignore_ascii_case("PrimaryImageUrl")
        });
        if let Some(image_url) = image_url.as_ref() {
            object.insert(
                "ImageUrl".to_string(),
                serde_json::Value::String(image_url.clone()),
            );
        }
        if let Some(primary_image_url) = primary_image_url.as_ref() {
            object.insert(
                "PrimaryImageUrl".to_string(),
                serde_json::Value::String(primary_image_url.clone()),
            );
        }
    }
    Some(LiveTvChannelUpsert {
        channel_id,
        tuner_id: tuner_id.to_string(),
        remote_id,
        category_id,
        name,
        sort_name,
        number: json_string_field(channel, "Number"),
        stream_url,
        logo_url: image_url,
        channel_type: json_string_field(channel, "ChannelType").unwrap_or_else(|| "TV".to_string()),
        metadata,
    })
}

pub fn category_db_id(tuner_id: &str, remote_id: &str) -> String {
    live_tv_stable_id("livetv-category", &format!("{tuner_id}-{remote_id}"))
}

/// Collect category ids from the first key in `keys` that yields any values.
///
/// Using first-match (rather than merging every key) keeps the specific key
/// authoritative when both a specific and a legacy key are present.
fn category_id_filter(payload: &serde_json::Value, keys: &[&str]) -> HashSet<String> {
    for key in keys {
        let Some(values) = json_string_list_field(payload, key) else {
            continue;
        };
        let ids = values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<HashSet<_>>();
        if !ids.is_empty() {
            return ids;
        }
    }
    HashSet::new()
}

/// A reusable include/exclude category selection.
///
/// `include` empty means "no restriction" (import every category).
#[derive(Default)]
struct CategorySelection {
    include: HashSet<String>,
    exclude: HashSet<String>,
}

impl CategorySelection {
    /// Build a selection from the payload, trying `include_keys` in order for the
    /// include set and `exclude_keys` for the exclude set.
    fn from_payload(
        payload: &serde_json::Value,
        include_keys: &[&str],
        exclude_keys: &[&str],
    ) -> Self {
        Self {
            include: category_id_filter(payload, include_keys),
            exclude: category_id_filter(payload, exclude_keys),
        }
    }

    /// Whether a stream/series with the given (optional) category id should be kept.
    fn allows(&self, category_id: Option<&str>) -> bool {
        if !self.include.is_empty() && category_id.is_none_or(|id| !self.include.contains(id)) {
            return false;
        }
        if let Some(id) = category_id
            && self.exclude.contains(id)
        {
            return false;
        }
        true
    }
}

/// Extract the category id of an Xtream stream/series item as a string.
fn item_category_id(item: &serde_json::Value) -> Option<String> {
    json_string_field(item, "category_id")
        .or_else(|| live_tv_u64_field(item, "category_id").map(|value| value.to_string()))
}

pub async fn programs_from_payload(payload: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    let base_url = json_string_field(payload, "Url")?;
    let tuner_id = json_string_field(payload, "Id")
        .unwrap_or_else(|| stable_entity_id("xtream-tuner", &base_url));
    let username = json_string_field(payload, "Username")
        .or_else(|| json_string_field(payload, "UserName"))?;
    let password = json_string_field(payload, "Password")?;
    if !valid_xtream_secret(&username) || !valid_xtream_secret(&password) {
        return None;
    }
    let stream_ids = epg_stream_ids(payload);
    if stream_ids.is_empty() {
        return None;
    }
    let limit = live_tv_u64_field(payload, "Limit")
        .or_else(|| live_tv_u64_field(payload, "EpgLimit"))
        .unwrap_or(LIVE_TV_XTREAM_DEFAULT_EPG_LIMIT as u64)
        .clamp(1, 48);
    let client = ValidatedXtreamClient::new(&base_url).await.ok()?;
    let mut programs = Vec::new();
    for stream_id in stream_ids.into_iter().take(LIVE_TV_XTREAM_MAX_EPG_CHANNELS) {
        if !valid_xtream_identifier(&stream_id) {
            continue;
        }
        let channel_id = xtream_live_channel_id(&tuner_id, &stream_id);
        let limit_string = limit.to_string();
        let epg = match fetch_xtream_json::<serde_json::Value>(
            &client,
            &username,
            &password,
            "get_short_epg",
            Duration::from_secs(15),
            XTREAM_MAX_EPG_BODY_BYTES,
            &[("stream_id", &stream_id), ("limit", &limit_string)],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                warn_xtream_fetch("get_short_epg", error);
                continue;
            }
        };
        if epg_listing_count(&epg) > XTREAM_MAX_EPG_LISTINGS {
            warn_xtream_fetch("get_short_epg", XtreamFetchError::TooManyItems);
            continue;
        }
        programs.extend(parse_epg_programs(&channel_id, &epg));
    }
    (!programs.is_empty()).then_some(programs)
}

fn epg_stream_ids(payload: &serde_json::Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut stream_ids = Vec::new();
    for key in [
        "StreamIds",
        "XtreamStreamIds",
        "ChannelIds",
        "ProbeChannelIds",
    ] {
        let Some(values) = json_string_list_field(payload, key) else {
            continue;
        };
        for value in values {
            let id = normalize_stream_id(&value);
            if !id.is_empty() && seen.insert(id.clone()) {
                stream_ids.push(id);
            }
        }
    }
    stream_ids
}

fn normalize_stream_id(value: &str) -> String {
    value
        .trim()
        .strip_prefix("xtream_")
        .unwrap_or_else(|| value.trim())
        .to_string()
}

fn epg_listing_count(epg: &serde_json::Value) -> usize {
    if let Some(values) = epg.as_array() {
        return values.len();
    }
    for key in ["epg_listings", "listings", "programs", "data"] {
        if let Some(values) = epg.get(key).and_then(serde_json::Value::as_array) {
            return values.len();
        }
    }
    0
}

fn validated_xtream_base_url(base_url: &str) -> Result<reqwest::Url, XtreamFetchError> {
    if base_url.is_empty()
        || base_url.len() > XTREAM_MAX_BASE_URL_BYTES
        || base_url.chars().any(char::is_control)
    {
        return Err(XtreamFetchError::InvalidInput);
    }
    let mut url = reqwest::Url::parse(base_url).map_err(|_| XtreamFetchError::InvalidInput)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(XtreamFetchError::InvalidInput);
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Return a durable catalogue image URL only when it cannot carry credentials
/// in userinfo, query parameters or a fragment. Providers that require a token
/// for artwork must omit the image instead of persisting the token.
fn safe_xtream_image_url(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > XTREAM_MAX_IMAGE_URL_BYTES
        || value.chars().any(char::is_control)
    {
        return None;
    }
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

fn valid_xtream_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= XTREAM_MAX_CREDENTIAL_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_xtream_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

fn private_provider_networks_allowed() -> bool {
    std::env::var("JELLYRIN_ALLOW_PRIVATE_PROVIDER_URLS")
        .ok()
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn provider_address_allowed(address: IpAddr, allow_private: bool) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            let shared_carrier_nat = first == 100 && (64..=127).contains(&second);
            let benchmarking = first == 198 && matches!(second, 18 | 19);
            let protocol_assignment = first == 192 && second == 0 && third == 0;
            let deprecated_6to4_relay = first == 192 && second == 88 && third == 99;
            if address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
                || address.is_link_local()
                || address.is_documentation()
                || first == 0
                || first >= 240
                || shared_carrier_nat
                || benchmarking
                || protocol_assignment
                || deprecated_6to4_relay
            {
                return false;
            }
            if address.is_private() || address.is_loopback() {
                return allow_private;
            }
            true
        }
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return provider_address_allowed(IpAddr::V4(address), allow_private);
            }
            let segments = address.segments();
            if segments[..6] == [0x0064, 0xff9b, 0, 0, 0, 0] {
                let address = Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    segments[6] as u8,
                    (segments[7] >> 8) as u8,
                    segments[7] as u8,
                );
                return provider_address_allowed(IpAddr::V4(address), allow_private);
            }
            let ipv4_compatible = segments[..6] == [0, 0, 0, 0, 0, 0]
                && !address.is_unspecified()
                && !address.is_loopback();
            let local_nat64 =
                segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001;
            let discard_only = segments[..4] == [0x0100, 0, 0, 0];
            let dummy_prefix = segments[..4] == [0x0100, 0, 0, 0x0001];
            let teredo = segments[0] == 0x2001 && segments[1] == 0;
            let benchmarking = segments[..3] == [0x2001, 0x0002, 0];
            let orchid = segments[0] == 0x2001 && matches!(segments[1] & 0xfff0, 0x0010 | 0x0020);
            let documentation = (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0);
            let six_to_four = segments[0] == 0x2002;
            let srv6_sid = segments[0] == 0x5f00;
            let deprecated_site_local = segments[0] & 0xffc0 == 0xfec0;
            if address.is_unspecified()
                || address.is_multicast()
                || address.is_unicast_link_local()
                || ipv4_compatible
                || local_nat64
                || discard_only
                || dummy_prefix
                || teredo
                || benchmarking
                || orchid
                || documentation
                || six_to_four
                || srv6_sid
                || deprecated_site_local
            {
                return false;
            }
            if address.is_unique_local() || address.is_loopback() {
                return allow_private;
            }
            // Accept only globally scoped unicast IPv6. The well-known NAT64
            // prefix was handled above using the embedded IPv4 policy.
            segments[0] & 0xe000 == 0x2000
        }
    }
}

fn stream_url(
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
    stream_id: &str,
) -> Option<String> {
    let mut url = base_url.clone();
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.push("live");
        segments.push(username);
        segments.push(password);
        segments.push(&format!("{stream_id}.ts"));
    }
    Some(url.to_string())
}

fn movie_url(
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
    stream_id: &str,
    extension: &str,
) -> Option<String> {
    let mut url = base_url.clone();
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.push("movie");
        segments.push(username);
        segments.push(password);
        segments.push(&format!("{stream_id}.{extension}"));
    }
    Some(url.to_string())
}

fn series_url(
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
    episode_id: &str,
    extension: &str,
) -> Option<String> {
    let mut url = base_url.clone();
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.push("series");
        segments.push(username);
        segments.push(password);
        segments.push(&format!("{episode_id}.{extension}"));
    }
    Some(url.to_string())
}

/// A provider-specific `direct_source` can only be represented by Jellyrin's credential-free
/// source reference when it resolves to the exact URL that reference reconstructs. Alternate CDN
/// URLs, query tokens, and redirects are rejected at import time: persisting them would leak a
/// bearer secret, while silently ignoring them would index an item that may not play.
fn direct_source_matches_reconstructed(
    value: &serde_json::Value,
    reconstructed: Option<String>,
) -> bool {
    let Some(direct_source) = value.as_object().and_then(|object| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("direct_source"))
            .map(|(_, value)| value)
    }) else {
        return true;
    };
    if direct_source.is_null()
        || direct_source
            .as_str()
            .is_some_and(|value| value.trim().is_empty())
    {
        return true;
    }
    let Some(direct_source) = direct_source.as_str().map(str::trim) else {
        return false;
    };
    let Some(reconstructed) = reconstructed else {
        return false;
    };
    let Ok(direct_source) = reqwest::Url::parse(direct_source) else {
        return false;
    };
    reqwest::Url::parse(&reconstructed).is_ok_and(|reconstructed| direct_source == reconstructed)
}

pub fn parse_streams(
    tuner_id: &str,
    streams: &[serde_json::Value],
    options: &LiveTvXtreamImportOptions,
) -> Vec<serde_json::Value> {
    let iter = streams.iter().filter(|stream| options.allows(stream));
    let iter: Box<dyn Iterator<Item = &serde_json::Value>> = if let Some(limit) = options.limit {
        Box::new(iter.take(limit))
    } else {
        Box::new(iter)
    };
    iter.filter_map(|stream| {
        let stream_id = json_string_field(stream, "stream_id")
            .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()))?;
        if !valid_xtream_identifier(&stream_id) {
            return None;
        }
        let name = json_string_field(stream, "name").unwrap_or_else(|| stream_id.clone());
        let provider_reference = encoded_live_provider_reference(&XtreamRemoteSourceRef::new(
            tuner_id, "live", &stream_id, "ts",
        )?)?;
        let number = json_string_field(stream, "num")
            .or_else(|| live_tv_u64_field(stream, "num").map(|value| value.to_string()));
        let epg_channel_id = json_string_field(stream, "epg_channel_id");
        let category_id = json_string_field(stream, "category_id")
            .or_else(|| live_tv_u64_field(stream, "category_id").map(|value| value.to_string()));
        let stream_icon = json_string_field(stream, "stream_icon")
            .and_then(|value| safe_xtream_image_url(&value));
        let mut channel = serde_json::json!({
            "Id": xtream_live_channel_id(tuner_id, &stream_id),
            "RemoteId": stream_id,
            "Name": name,
            "Number": number,
            "ProviderReference": provider_reference,
            "ProviderType": XTREAM_PROVIDER_TYPE,
            "ChannelType": "TV",
            "IsHD": false,
            "IsFavorite": false,
        });
        if let Some(epg_channel_id) = epg_channel_id {
            channel["GuideNumber"] = serde_json::json!(epg_channel_id);
            channel["GuideChannelId"] = serde_json::json!(epg_channel_id);
        }
        if let Some(category_id) = category_id {
            channel["CategoryId"] = serde_json::json!(category_id);
        }
        if let Some(stream_icon) = stream_icon {
            channel["ImageUrl"] = serde_json::json!(stream_icon);
        }
        Some(channel)
    })
    .collect()
}

fn unique_live_catalog(channels: &[serde_json::Value]) -> bool {
    let mut ids = HashSet::with_capacity(channels.len());
    channels
        .iter()
        .all(|channel| json_string_field(channel, "Id").is_some_and(|id| ids.insert(id)))
}

fn unique_remote_media_catalog(items: &[RemoteMediaItemUpsert]) -> bool {
    let mut ids = HashSet::with_capacity(items.len());
    let mut paths = HashSet::with_capacity(items.len());
    items.iter().all(|item| {
        !item.id.trim().is_empty()
            && !item.path.trim().is_empty()
            && ids.insert(item.id.clone())
            && paths.insert(item.path.clone())
    })
}

fn parse_vod_streams(
    tuner_id: &str,
    streams: &[serde_json::Value],
    categories: &[serde_json::Value],
) -> Vec<RemoteMediaItemUpsert> {
    let category_names = category_name_map(categories);
    streams
        .iter()
        .filter_map(|stream| {
            let stream_id = json_string_field(stream, "stream_id")
                .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()))?;
            if !valid_xtream_identifier(&stream_id) {
                return None;
            }
            let name = json_string_field(stream, "name").unwrap_or_else(|| stream_id.clone());
            let extension = xtream_extension(stream, "container_extension", "mp4")?;
            let remote_source_ref =
                XtreamRemoteSourceRef::new(tuner_id, "vod", &stream_id, &extension)?;
            let category_id = json_string_field(stream, "category_id").or_else(|| {
                live_tv_u64_field(stream, "category_id").map(|value| value.to_string())
            });
            let mut genres = Vec::new();
            if let Some(category_id) = category_id.as_deref()
                && let Some(category_name) = category_names.get(category_id)
            {
                genres.push(category_name.clone());
            }
            let runtime_ticks = duration_ticks_from_metadata(stream);
            let image_url = json_string_field(stream, "stream_icon")
                .and_then(|value| safe_xtream_image_url(&value))
                .or_else(|| {
                    json_string_field(stream, "cover")
                        .and_then(|value| safe_xtream_image_url(&value))
                });
            let id = stable_entity_id(
                "xtream-vod",
                &xtream_scoped_catalog_key(tuner_id, &stream_id),
            );
            let library_root = xtream_virtual_library_root(tuner_id, "movies");
            let path = format!(
                "{library_root}/{} [{}].{}",
                xtream_path_segment(&name),
                xtream_path_segment(&stream_id),
                extension
            );
            let mut metadata = serde_json::json!({
                "Provider": "xtream",
                "XtreamKind": "vod",
                "RemoteSourceRef": remote_source_ref,
                "XtreamStreamId": stream_id,
                "ProviderIds": { "Xtream": stream_id },
                "Name": name,
                "Genres": genres,
                "Tags": ["Xtream Codes"],
                "PrimaryImageTag": stable_entity_id("xtream-vod-image", &id),
            });
            if let Some(image_url) = image_url {
                metadata["ImageUrl"] = serde_json::json!(image_url);
                metadata["PrimaryImageUrl"] = serde_json::json!(image_url);
            }
            if let Some(year) = xtream_i32(stream, &["year", "releaseDate", "release_date"]) {
                metadata["ProductionYear"] = serde_json::json!(year);
            }
            if let Some(rating) = xtream_f64(stream, &["rating", "rating_5based"]) {
                metadata["CommunityRating"] = serde_json::json!(rating);
            }
            if let Some(overview) =
                json_string_field(stream, "plot").or_else(|| json_string_field(stream, "overview"))
            {
                metadata["Overview"] = serde_json::json!(overview);
            }

            Some(RemoteMediaItemUpsert {
                id,
                name,
                path,
                media_type: "Video".to_string(),
                collection_type: "movies".to_string(),
                runtime_ticks,
                bitrate: None,
                width: None,
                height: None,
                media_streams: default_remote_video_streams(),
                metadata,
            })
        })
        .collect()
}

fn parse_series_episodes(
    tuner_id: &str,
    series_item: &serde_json::Value,
    info: &serde_json::Value,
    categories: &[serde_json::Value],
    episode_limit: Option<usize>,
) -> Vec<RemoteMediaItemUpsert> {
    let series_id = json_string_field(series_item, "series_id")
        .or_else(|| live_tv_u64_field(series_item, "series_id").map(|id| id.to_string()))
        .unwrap_or_else(|| stable_entity_id("xtream-series-missing-id", &series_item.to_string()));
    let series_info = info.get("info").unwrap_or(series_item);
    let series_name = json_string_field(series_info, "name")
        .or_else(|| json_string_field(series_item, "name"))
        .unwrap_or_else(|| format!("Series {series_id}"));
    let category_names = category_name_map(categories);
    let category_id = json_string_field(series_item, "category_id")
        .or_else(|| live_tv_u64_field(series_item, "category_id").map(|value| value.to_string()));
    let mut genres = Vec::new();
    if let Some(category_id) = category_id.as_deref()
        && let Some(category_name) = category_names.get(category_id)
    {
        genres.push(category_name.clone());
    }
    let series_image_url = json_string_field(series_info, "cover")
        .and_then(|value| safe_xtream_image_url(&value))
        .or_else(|| {
            json_string_field(series_info, "cover_big")
                .and_then(|value| safe_xtream_image_url(&value))
        })
        .or_else(|| {
            json_string_field(series_item, "cover").and_then(|value| safe_xtream_image_url(&value))
        });
    let series_stable_id = stable_entity_id(
        "xtream-series",
        &xtream_scoped_catalog_key(tuner_id, &series_id),
    );
    let series_primary_tag = stable_entity_id("xtream-series-image", &series_stable_id);
    let mut episodes = Vec::new();

    for (season_number, episode) in xtream_episode_values(info) {
        if episode_limit.is_some_and(|limit| episodes.len() >= limit) {
            break;
        }
        let Some(episode_id) = json_string_field(episode, "id")
            .or_else(|| live_tv_u64_field(episode, "id").map(|id| id.to_string()))
        else {
            continue;
        };
        if !valid_xtream_identifier(&episode_id) {
            continue;
        }
        let episode_info = episode.get("info").unwrap_or(episode);
        let episode_number = xtream_i32(episode, &["episode_num", "episode_number", "num"])
            .or_else(|| xtream_i32(episode_info, &["episode_num", "episode_number", "num"]))
            .unwrap_or(episodes.len() as i32 + 1);
        let season_number = xtream_i32(episode, &["season", "season_number"])
            .or(season_number)
            .unwrap_or(1);
        let title = json_string_field(episode, "title")
            .or_else(|| json_string_field(episode, "name"))
            .unwrap_or_else(|| format!("Episode {episode_number}"));
        let Some(extension) = xtream_extension(episode, "container_extension", "mp4") else {
            continue;
        };
        let Some(remote_source_ref) =
            XtreamRemoteSourceRef::new(tuner_id, "series-episode", &episode_id, &extension)
        else {
            continue;
        };
        let episode_key = format!("{series_id}:{episode_id}");
        let id = stable_entity_id(
            "xtream-series-episode",
            &xtream_scoped_catalog_key(tuner_id, &episode_key),
        );
        let library_root = xtream_virtual_library_root(tuner_id, "series");
        let path = format!(
            "{library_root}/{}/Season {}/S{:02}E{:02} - {} [{}].{}",
            xtream_path_segment(&series_name),
            season_number,
            season_number,
            episode_number,
            xtream_path_segment(&title),
            xtream_path_segment(&episode_id),
            extension
        );
        let runtime_ticks = duration_ticks_from_metadata(episode_info)
            .or_else(|| duration_ticks_from_metadata(episode));
        let episode_image_url = json_string_field(episode_info, "movie_image")
            .and_then(|value| safe_xtream_image_url(&value))
            .or_else(|| {
                json_string_field(episode_info, "cover")
                    .and_then(|value| safe_xtream_image_url(&value))
            })
            .or_else(|| series_image_url.clone());
        let mut metadata = serde_json::json!({
            "Provider": "xtream",
            "XtreamKind": "series-episode",
            "RemoteSourceRef": remote_source_ref,
            "XtreamSeriesId": series_id,
            "XtreamEpisodeId": episode_id,
            "ProviderIds": { "Xtream": episode_id },
            "SeriesProviderIds": { "Xtream": series_id },
            "Name": title,
            "SeriesName": series_name,
            "SeriesId": series_stable_id,
            "SeasonId": stable_entity_id(
                "xtream-season",
                &xtream_scoped_catalog_key(tuner_id, &format!("{series_id}:{season_number}"))
            ),
            "ParentIndexNumber": season_number,
            "IndexNumber": episode_number,
            "Genres": genres,
            "Tags": ["Xtream Codes"],
            "PrimaryImageTag": stable_entity_id("xtream-episode-image", &id),
            "SeriesPrimaryImageTag": series_primary_tag,
        });
        if let Some(image_url) = episode_image_url {
            metadata["ImageUrl"] = serde_json::json!(image_url);
            metadata["PrimaryImageUrl"] = serde_json::json!(image_url);
        }
        if let Some(series_image_url) = series_image_url.as_ref() {
            metadata["SeriesImageUrl"] = serde_json::json!(series_image_url);
        }
        if let Some(overview) = json_string_field(episode_info, "plot")
            .or_else(|| json_string_field(series_info, "plot"))
        {
            metadata["Overview"] = serde_json::json!(overview);
        }
        if let Some(air_date) = json_string_field(episode_info, "releasedate")
            .or_else(|| json_string_field(episode, "air_date"))
        {
            metadata["PremiereDate"] = serde_json::json!(air_date);
        }

        episodes.push(RemoteMediaItemUpsert {
            id,
            name: title,
            path,
            media_type: "Video".to_string(),
            collection_type: "tvshows".to_string(),
            runtime_ticks,
            bitrate: None,
            width: None,
            height: None,
            media_streams: default_remote_video_streams(),
            metadata,
        });
    }

    episodes
}

fn parse_categories(categories: &[serde_json::Value]) -> Vec<serde_json::Value> {
    categories
        .iter()
        .filter_map(|category| {
            let id = json_string_field(category, "category_id")
                .or_else(|| live_tv_u64_field(category, "category_id").map(|id| id.to_string()))?;
            let name = json_string_field(category, "category_name")
                .or_else(|| json_string_field(category, "name"))
                .unwrap_or_else(|| id.clone());
            let parent_id = json_string_field(category, "parent_id")
                .or_else(|| live_tv_u64_field(category, "parent_id").map(|id| id.to_string()));
            Some(serde_json::json!({
                "Id": id,
                "Name": name,
                "ParentId": parent_id,
            }))
        })
        .collect()
}

fn category_name_map(categories: &[serde_json::Value]) -> HashMap<String, String> {
    categories
        .iter()
        .filter_map(|category| {
            Some((
                json_string_field(category, "Id")?,
                json_string_field(category, "Name")?,
            ))
        })
        .collect()
}

fn apply_category_names(channels: &mut [serde_json::Value], categories: &[serde_json::Value]) {
    let category_names = categories
        .iter()
        .filter_map(|category| {
            let id = json_string_field(category, "Id")?;
            let name = json_string_field(category, "Name")?;
            Some((id, name))
        })
        .collect::<HashMap<_, _>>();
    for channel in channels {
        let Some(category_id) = json_string_field(channel, "CategoryId") else {
            continue;
        };
        let Some(category_name) = category_names.get(&category_id) else {
            continue;
        };
        channel["Genres"] = serde_json::json!([category_name]);
        channel["Tags"] = serde_json::json!([category_name]);
        channel["GenreItems"] = serde_json::json!([{
            "Id": stable_entity_id("LiveTvGenre", category_name),
            "Name": category_name
        }]);
    }
}

fn xtream_episode_values(info: &serde_json::Value) -> Vec<(Option<i32>, &serde_json::Value)> {
    let Some(episodes) = info.get("episodes") else {
        return Vec::new();
    };
    if let Some(values) = episodes.as_array() {
        return values.iter().map(|episode| (None, episode)).collect();
    }
    let Some(object) = episodes.as_object() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for (season, season_episodes) in object {
        let season_number = season.parse::<i32>().ok();
        if let Some(season_episodes) = season_episodes.as_array() {
            values.extend(
                season_episodes
                    .iter()
                    .map(|episode| (season_number, episode)),
            );
        }
    }
    values
}

fn default_remote_video_streams() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "Codec": "h264",
            "Language": null,
            "DisplayTitle": "Video",
            "IsInterlaced": false,
            "IsDefault": true,
            "IsForced": false,
            "Type": "Video",
            "Index": 0,
            "IsExternal": false,
            "SupportsExternalStream": false,
        }),
        serde_json::json!({
            "Codec": "aac",
            "Language": null,
            "DisplayTitle": "Audio",
            "IsInterlaced": false,
            "Channels": 2,
            "IsDefault": true,
            "IsForced": false,
            "Type": "Audio",
            "Index": 1,
            "IsExternal": false,
        }),
    ]
}

fn xtream_extension(value: &serde_json::Value, key: &str, default_value: &str) -> Option<String> {
    match json_string_field(value, key) {
        Some(value) => sanitized_xtream_extension(&value),
        None => sanitized_xtream_extension(default_value),
    }
}

fn sanitized_xtream_extension(value: &str) -> Option<String> {
    let extension = value.trim().strip_prefix('.').unwrap_or(value.trim());
    if extension.is_empty()
        || extension.len() > 32
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }
    let extension = extension.to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "ts" | "m2ts"
            | "mts"
            | "mp4"
            | "m4v"
            | "mov"
            | "mkv"
            | "webm"
            | "avi"
            | "asf"
            | "wmv"
            | "flv"
            | "mpg"
            | "mpeg"
            | "mp3"
            | "flac"
            | "aac"
            | "ogg"
            | "oga"
            | "wav"
    )
    .then_some(extension)
}

fn xtream_scoped_catalog_key(tuner_id: &str, legacy_key: &str) -> String {
    if tuner_id.eq_ignore_ascii_case(XTREAM_PRIMARY_TUNER_ID) {
        legacy_key.to_string()
    } else {
        format!("{tuner_id}:{legacy_key}")
    }
}

fn xtream_live_channel_id(tuner_id: &str, remote_id: &str) -> String {
    if tuner_id.eq_ignore_ascii_case(XTREAM_PRIMARY_TUNER_ID) {
        format!("xtream_{remote_id}")
    } else {
        format!(
            "xtream_{}",
            stable_entity_id("xtream-live-channel", &format!("{tuner_id}:{remote_id}"))
        )
    }
}

fn xtream_virtual_library_root(tuner_id: &str, collection: &str) -> String {
    if tuner_id.eq_ignore_ascii_case(XTREAM_PRIMARY_TUNER_ID) {
        format!("xtream://{collection}")
    } else {
        format!("xtream://{}/{collection}", xtream_path_segment(tuner_id))
    }
}

fn xtream_path_segment(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect::<String>();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "Untitled".to_string()
    } else {
        cleaned
    }
}

fn duration_ticks_from_metadata(value: &serde_json::Value) -> Option<i64> {
    let seconds =
        xtream_i64(value, &["duration_secs", "duration_seconds", "duration"]).or_else(|| {
            json_string_field(value, "duration").and_then(|duration| {
                let parts = duration
                    .split(':')
                    .filter_map(|part| part.parse::<i64>().ok())
                    .collect::<Vec<_>>();
                match parts.as_slice() {
                    [hours, minutes, seconds] => Some(hours * 3600 + minutes * 60 + seconds),
                    [minutes, seconds] => Some(minutes * 60 + seconds),
                    [seconds] => Some(*seconds),
                    _ => None,
                }
            })
        })?;
    (seconds > 0).then_some(seconds.saturating_mul(10_000_000))
}

fn xtream_i32(value: &serde_json::Value, keys: &[&str]) -> Option<i32> {
    xtream_i64(value, keys).and_then(|value| i32::try_from(value).ok())
}

fn xtream_i64(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = value.get(*key) {
            if let Some(number) = value.as_i64() {
                return Some(number);
            }
            if let Some(number) = value.as_u64().and_then(|value| i64::try_from(value).ok()) {
                return Some(number);
            }
            if let Some(number) = value.as_str().and_then(|value| value.trim().parse().ok()) {
                return Some(number);
            }
        }
    }
    None
}

fn xtream_f64(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = value.get(*key) {
            if let Some(number) = value.as_f64() {
                return Some(number);
            }
            if let Some(number) = value.as_str().and_then(|value| value.trim().parse().ok()) {
                return Some(number);
            }
        }
    }
    None
}

pub fn parse_epg_programs(channel_id: &str, epg: &serde_json::Value) -> Vec<serde_json::Value> {
    epg_listings(epg)
        .into_iter()
        .enumerate()
        .filter_map(|(index, listing)| {
            let name = json_string_field(listing, "title")
                .map(|value| epg_text(&value))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("Program {}", index + 1));
            let overview = json_string_field(listing, "description")
                .map(|value| epg_text(&value))
                .unwrap_or_default();
            let start = epg_datetime(listing, &["start", "start_time"])
                .or_else(|| epg_timestamp(listing, &["start_timestamp"]));
            let end = epg_datetime(listing, &["end", "stop", "end_time"])
                .or_else(|| epg_timestamp(listing, &["stop_timestamp"]));
            let start = start?;
            let end = end?;
            let remote_id = json_string_field(listing, "id").unwrap_or_else(|| index.to_string());
            Some(serde_json::json!({
                "Id": live_tv_stable_id("xtream-program", &format!("{channel_id}-{remote_id}-{start}")),
                "Name": name,
                "ChannelId": channel_id,
                "StartDate": start,
                "EndDate": end,
                "Overview": overview,
                "IsLive": true,
            }))
        })
        .collect()
}

fn epg_listings(epg: &serde_json::Value) -> Vec<&serde_json::Value> {
    if let Some(values) = epg.as_array() {
        return values.iter().collect();
    }
    for key in ["epg_listings", "listings", "programs", "data"] {
        if let Some(values) = epg.get(key).and_then(serde_json::Value::as_array) {
            return values.iter().collect();
        }
    }
    Vec::new()
}

fn epg_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(bytes) = general_purpose::STANDARD.decode(trimmed.as_bytes())
        && let Ok(decoded) = String::from_utf8(bytes)
    {
        let decoded = decoded.trim().to_string();
        if !decoded.is_empty() {
            return decoded;
        }
    }
    trimmed.to_string()
}

fn epg_datetime(listing: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = json_string_field(listing, key)
            && let Some(formatted) = format_datetime(&value)
        {
            return Some(formatted);
        }
    }
    None
}

fn epg_timestamp(listing: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let timestamp = live_tv_u64_field(listing, key).or_else(|| {
            json_string_field(listing, key).and_then(|value| value.parse::<u64>().ok())
        });
        if let Some(timestamp) = timestamp
            && let Ok(value) = OffsetDateTime::from_unix_timestamp(timestamp as i64)
        {
            return Some(format_time_for_json(value));
        }
    }
    None
}

fn format_datetime(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if OffsetDateTime::parse(trimmed, &Rfc3339).is_ok() {
        return Some(trimmed.to_string());
    }
    let compact = trimmed.replace('T', " ");
    let date_time = compact
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if date_time.len() >= 19
        && date_time.as_bytes().get(4) == Some(&b'-')
        && date_time.as_bytes().get(7) == Some(&b'-')
        && date_time.as_bytes().get(13) == Some(&b':')
        && date_time.as_bytes().get(16) == Some(&b':')
    {
        return Some(format!("{}T{}Z", &date_time[0..10], &date_time[11..19]));
    }
    None
}

fn xtream_media_library_specs(tuner_id: &str) -> Vec<RemoteMediaLibraryStageSpec> {
    let primary_tuner = tuner_id.eq_ignore_ascii_case(XTREAM_PRIMARY_TUNER_ID);
    let tuner_scope = xtream_path_segment(tuner_id);
    vec![
        RemoteMediaLibraryStageSpec {
            key: "movies".to_string(),
            library_name: if primary_tuner {
                "Xtream Movies".to_string()
            } else {
                format!("Xtream Movies ({tuner_id})")
            },
            collection_type: "movies".to_string(),
            source_location: if primary_tuner {
                "xtream://movies".to_string()
            } else {
                format!("xtream://{tuner_scope}/movies")
            },
        },
        RemoteMediaLibraryStageSpec {
            key: "series".to_string(),
            library_name: if primary_tuner {
                "Xtream Series".to_string()
            } else {
                format!("Xtream Series ({tuner_id})")
            },
            collection_type: "tvshows".to_string(),
            source_location: if primary_tuner {
                "xtream://series".to_string()
            } else {
                format!("xtream://{tuner_scope}/series")
            },
        },
    ]
}

async fn append_staged_media_items<Database>(
    db: &Database,
    stage: &RemoteMediaCatalogStage,
    library_key: &str,
    items: Vec<RemoteMediaItemUpsert>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    let item_count = items.len();
    let mut items = items.into_iter();
    loop {
        let chunk = items
            .by_ref()
            .take(REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS)
            .collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        db.append_remote_media_catalog_stage(stage, library_key, chunk)
            .await?;
    }
    Ok(item_count)
}

fn valid_xtream_movie_source(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    stream: &serde_json::Value,
) -> bool {
    let stream_id = json_string_field(stream, "stream_id")
        .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()));
    let Some(extension) = xtream_extension(stream, "container_extension", "mp4") else {
        return false;
    };
    direct_source_matches_reconstructed(
        stream,
        stream_id.as_deref().and_then(|stream_id| {
            movie_url(&client.base_url, username, password, stream_id, &extension)
        }),
    )
}

fn valid_xtream_series_episode_source(
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    episode: &serde_json::Value,
) -> bool {
    let episode_id = json_string_field(episode, "id")
        .or_else(|| live_tv_u64_field(episode, "id").map(|id| id.to_string()));
    let Some(extension) = xtream_extension(episode, "container_extension", "mp4") else {
        return false;
    };
    let reconstructed = episode_id.as_deref().and_then(|episode_id| {
        series_url(&client.base_url, username, password, episode_id, &extension)
    });
    direct_source_matches_reconstructed(episode, reconstructed.clone())
        && direct_source_matches_reconstructed(
            episode.get("info").unwrap_or(episode),
            reconstructed,
        )
}

#[derive(Default)]
struct XtreamSeriesStageProgress {
    selected_series: usize,
    episode_count: usize,
    series_ids: HashSet<String>,
}

#[allow(clippy::too_many_arguments)]
async fn append_xtream_series_chunks<Database>(
    db: &Database,
    stage: &RemoteMediaCatalogStage,
    client: &ValidatedXtreamClient,
    username: &str,
    password: &str,
    tuner_id: &str,
    mut chunks: XtreamArrayChunks,
    selection: &CategorySelection,
    expected_category_id: Option<&str>,
    categories: &[serde_json::Value],
    series_limit: Option<usize>,
    episode_limit: usize,
    progress: &mut XtreamSeriesStageProgress,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    while let Some(chunk) = chunks.receiver.recv().await {
        for series_item in chunk {
            let category_id = item_category_id(&series_item);
            if expected_category_id.is_some_and(|expected| category_id.as_deref() != Some(expected))
            {
                return Err(XtreamImportError::new(
                    "series catalogue category filter",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            if !selection.allows(category_id.as_deref()) {
                continue;
            }
            if series_limit.is_some_and(|limit| progress.selected_series >= limit) {
                continue;
            }
            if progress.selected_series >= XTREAM_MAX_SERIES_REQUESTS {
                return Err(XtreamImportError::new(
                    "series catalogue",
                    XtreamFetchError::TooManyItems,
                )
                .into());
            }
            let Some(series_id) = json_string_field(&series_item, "series_id")
                .or_else(|| live_tv_u64_field(&series_item, "series_id").map(|id| id.to_string()))
            else {
                return Err(XtreamImportError::new(
                    "series catalogue",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            };
            if !valid_xtream_identifier(&series_id)
                || !progress.series_ids.insert(series_id.clone())
            {
                return Err(XtreamImportError::new(
                    "series catalogue",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            progress.selected_series = progress.selected_series.saturating_add(1);
            let info = fetch_series_info(client, username, password, &series_id)
                .await
                .map_err(|error| XtreamImportError::new("series details", error))?;
            let selected_episode_count = series_episode_count(&info).min(episode_limit);
            if progress
                .episode_count
                .saturating_add(selected_episode_count)
                > REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS
                || !valid_series_episode_prefix(&info, selected_episode_count)
                || !xtream_episode_values(&info)
                    .into_iter()
                    .take(selected_episode_count)
                    .all(|(_, episode)| {
                        valid_xtream_series_episode_source(client, username, password, episode)
                    })
            {
                return Err(XtreamImportError::new(
                    "series details",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            let items = parse_series_episodes(
                tuner_id,
                &series_item,
                &info,
                categories,
                Some(selected_episode_count),
            );
            if items.len() != selected_episode_count || !unique_remote_media_catalog(&items) {
                return Err(XtreamImportError::new(
                    "series details",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            progress.episode_count = progress
                .episode_count
                .saturating_add(append_staged_media_items(db, stage, "series", items).await?);
        }
    }
    chunks
        .finish()
        .await
        .map_err(|error| XtreamImportError::new("series catalogue", error))?;
    Ok(())
}

async fn stage_xtream_media_from_payload<Database>(
    db: &Database,
    payload: &serde_json::Value,
) -> Result<Option<(RemoteMediaCatalogStage, usize, usize)>, Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    let Some(base_url) = json_string_field(payload, "Url") else {
        return Ok(None);
    };
    let tuner_id = json_string_field(payload, "Id")
        .unwrap_or_else(|| stable_entity_id("xtream-tuner", &base_url));
    let Some(username) =
        json_string_field(payload, "Username").or_else(|| json_string_field(payload, "UserName"))
    else {
        return Ok(None);
    };
    let Some(password) = json_string_field(payload, "Password") else {
        return Ok(None);
    };
    if !valid_xtream_secret(&username) || !valid_xtream_secret(&password) {
        return Ok(None);
    }
    let client = ValidatedXtreamClient::new(&base_url)
        .await
        .map_err(|error| XtreamImportError::new("provider connection", error))?;

    let movie_categories = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_vod_categories",
        Duration::from_secs(15),
        XTREAM_MAX_CATEGORY_BODY_BYTES,
        XTREAM_MAX_CATEGORY_ITEMS,
    )
    .await
    .map_err(|error| XtreamImportError::new("VOD categories", error))?;
    let parsed_movie_categories = parse_categories(&movie_categories);
    if parsed_movie_categories.len() != movie_categories.len()
        || !unique_live_catalog(&parsed_movie_categories)
    {
        return Err(Box::new(XtreamImportError::new(
            "VOD categories",
            XtreamFetchError::InvalidCatalog,
        )));
    }
    let series_categories = fetch_xtream_array(
        &client,
        &username,
        &password,
        "get_series_categories",
        Duration::from_secs(15),
        XTREAM_MAX_CATEGORY_BODY_BYTES,
        XTREAM_MAX_CATEGORY_ITEMS,
    )
    .await
    .map_err(|error| XtreamImportError::new("series categories", error))?;
    let parsed_series_categories = parse_categories(&series_categories);
    if parsed_series_categories.len() != series_categories.len()
        || !unique_live_catalog(&parsed_series_categories)
    {
        return Err(Box::new(XtreamImportError::new(
            "series categories",
            XtreamFetchError::InvalidCatalog,
        )));
    }

    let stage = db
        .begin_remote_media_catalog_stage(xtream_media_library_specs(&tuner_id))
        .await?;
    let staged_result = async {
        let vod_selection = CategorySelection::from_payload(
            payload,
            &["VodCategoryIds", "MovieCategoryIds"],
            &["ExcludeVodCategoryIds", "ExcludeMovieCategoryIds"],
        );
        let movie_limit = live_tv_u64_field(payload, "MovieLimit")
            .or_else(|| live_tv_u64_field(payload, "VodLimit"))
            .and_then(|value| {
                positive_bounded_usize(value, REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS)
            });
        let mut movie_chunks = fetch_xtream_array_chunks(
            &client,
            &username,
            &password,
            XtreamArrayFetchOptions {
                action: "get_vod_streams",
                timeout: Duration::from_secs(45),
                max_bytes: XTREAM_MAX_CATALOG_BODY_BYTES,
                max_items: REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS,
                query: &[],
            },
        )
        .await
        .map_err(|error| XtreamImportError::new("VOD streams", error))?;
        let mut movie_count = 0usize;
        while let Some(mut chunk) = movie_chunks.receiver.recv().await {
            chunk.retain(|stream| vod_selection.allows(item_category_id(stream).as_deref()));
            if let Some(limit) = movie_limit {
                chunk.truncate(limit.saturating_sub(movie_count));
            }
            if chunk.is_empty() {
                continue;
            }
            if !chunk
                .iter()
                .all(|stream| valid_xtream_movie_source(&client, &username, &password, stream))
            {
                return Err(XtreamImportError::new(
                    "VOD streams",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            let items = parse_vod_streams(&tuner_id, &chunk, &parsed_movie_categories);
            if items.len() != chunk.len() || !unique_remote_media_catalog(&items) {
                return Err(XtreamImportError::new(
                    "VOD streams",
                    XtreamFetchError::InvalidCatalog,
                )
                .into());
            }
            movie_count = movie_count
                .saturating_add(append_staged_media_items(db, &stage, "movies", items).await?);
        }
        movie_chunks
            .finish()
            .await
            .map_err(|error| XtreamImportError::new("VOD streams", error))?;

        let series_selection = CategorySelection::from_payload(
            payload,
            &["SeriesCategoryIds"],
            &["ExcludeSeriesCategoryIds"],
        );
        let series_limit = live_tv_u64_field(payload, "SeriesLimit")
            .or_else(|| live_tv_u64_field(payload, "XtreamSeriesLimit"))
            .and_then(|value| positive_bounded_usize(value, XTREAM_MAX_SERIES_REQUESTS));
        let episode_limit = live_tv_u64_field(payload, "SeriesEpisodeLimit")
            .or_else(|| live_tv_u64_field(payload, "XtreamSeriesEpisodeLimit"))
            .map(|value| bounded_usize(value, 1, XTREAM_MAX_EPISODES_PER_SERIES))
            .unwrap_or(XTREAM_MAX_EPISODES_PER_SERIES);
        let series_chunks = fetch_xtream_array_chunks(
            &client,
            &username,
            &password,
            XtreamArrayFetchOptions {
                action: "get_series",
                timeout: Duration::from_secs(45),
                max_bytes: XTREAM_MAX_CATALOG_BODY_BYTES,
                max_items: REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS,
                query: &[],
            },
        )
        .await;
        let mut series_progress = XtreamSeriesStageProgress::default();
        match series_chunks {
            Ok(chunks) => {
                append_xtream_series_chunks(
                    db,
                    &stage,
                    &client,
                    &username,
                    &password,
                    &tuner_id,
                    chunks,
                    &series_selection,
                    None,
                    &parsed_series_categories,
                    series_limit,
                    episode_limit,
                    &mut series_progress,
                )
                .await?;
            }
            Err(XtreamFetchError::BodyTooLarge) => {
                let category_ids = parsed_series_categories
                    .iter()
                    .filter_map(|category| json_string_field(category, "Id"))
                    .filter(|category_id| series_selection.allows(Some(category_id)))
                    .collect::<Vec<_>>();
                if category_ids.is_empty() {
                    return Err(XtreamImportError::new(
                        "series catalogue",
                        XtreamFetchError::BodyTooLarge,
                    )
                    .into());
                }
                tracing::info!(
                    category_count = category_ids.len(),
                    "Xtream series catalogue is using validated category chunks"
                );
                for category_id in category_ids {
                    let query = [("category_id", category_id.as_str())];
                    let chunks = fetch_xtream_array_chunks(
                        &client,
                        &username,
                        &password,
                        XtreamArrayFetchOptions {
                            action: "get_series",
                            timeout: Duration::from_secs(45),
                            max_bytes: XTREAM_MAX_CATALOG_BODY_BYTES,
                            max_items: REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS,
                            query: &query,
                        },
                    )
                    .await
                    .map_err(|error| XtreamImportError::new("series catalogue category", error))?;
                    append_xtream_series_chunks(
                        db,
                        &stage,
                        &client,
                        &username,
                        &password,
                        &tuner_id,
                        chunks,
                        &series_selection,
                        Some(&category_id),
                        &parsed_series_categories,
                        series_limit,
                        episode_limit,
                        &mut series_progress,
                    )
                    .await?;
                }
            }
            Err(error) => {
                return Err(XtreamImportError::new("series catalogue", error).into());
            }
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
            movie_count,
            series_progress.episode_count,
        ))
    }
    .await;

    match staged_result {
        Ok((movie_count, series_episode_count)) => {
            Ok(Some((stage, movie_count, series_episode_count)))
        }
        Err(error) => {
            let _ = db.abort_remote_media_catalog_stage(&stage).await;
            Err(error)
        }
    }
}

/// Persist media import (movies + series episodes) to the database.
pub async fn persist_xtream_media_import<Database>(
    db: &Database,
    import: XtreamMediaImport,
) -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    let XtreamMediaImport {
        tuner_id,
        movies,
        series_episodes,
    } = import;
    let movie_count = movies.len();
    let series_episode_count = series_episodes.len();
    let primary_tuner = tuner_id.eq_ignore_ascii_case(XTREAM_PRIMARY_TUNER_ID);
    let tuner_scope = xtream_path_segment(&tuner_id);
    let movie_library_name = if primary_tuner {
        "Xtream Movies".to_string()
    } else {
        format!("Xtream Movies ({tuner_id})")
    };
    let series_library_name = if primary_tuner {
        "Xtream Series".to_string()
    } else {
        format!("Xtream Series ({tuner_id})")
    };
    let movie_location = if primary_tuner {
        "xtream://movies".to_string()
    } else {
        format!("xtream://{tuner_scope}/movies")
    };
    let series_location = if primary_tuner {
        "xtream://series".to_string()
    } else {
        format!("xtream://{tuner_scope}/series")
    };
    // Empty arrays are complete snapshots too. Omitting either entry would leave stale rows after
    // the provider (or an explicit category filter) legitimately becomes empty. Both entries are
    // published by the adapter in one transaction; fetch errors never reach this function.
    db.replace_remote_media_library_snapshots(vec![
        RemoteMediaLibrarySnapshot {
            library_name: movie_library_name,
            collection_type: "movies".to_string(),
            source_location: movie_location,
            items: movies,
        },
        RemoteMediaLibrarySnapshot {
            library_name: series_library_name,
            collection_type: "tvshows".to_string(),
            source_location: series_location,
            items: series_episodes,
        },
    ])
    .await?;
    Ok((movie_count, series_episode_count))
}

/// Sync media for a single tuner from its payload.
pub async fn sync_xtream_media_from_payload<Database>(
    db: &Database,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    db.cleanup_abandoned_remote_media_catalog_stages(
        OffsetDateTime::now_utc() - time::Duration::hours(24),
    )
    .await?;
    let Some((stage, movie_count, series_episode_count)) =
        stage_xtream_media_from_payload(db, payload).await?
    else {
        return Ok(None);
    };
    if let Err(error) = db.publish_remote_media_catalog_stage(&stage).await {
        let _ = db.abort_remote_media_catalog_stage(&stage).await;
        return Err(error.into());
    }
    Ok(Some(serde_json::json!({
        "MovieCount": movie_count,
        "SeriesEpisodeCount": series_episode_count,
    })))
}

/// Sync media for all configured xtream tuners.
pub async fn sync_all_configured_xtream_media<Database>(
    db: &Database,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>
where
    Database: XtreamCatalogStore + Sync,
{
    let tuners = db
        .live_tv_tuner_configurations_by_provider(XTREAM_PROVIDER_TYPE)
        .await?;
    let mut synced_tuners = 0usize;
    let mut skipped_tuners = 0usize;
    let mut movie_count = 0usize;
    let mut series_episode_count = 0usize;
    for tuner in tuners {
        match sync_xtream_media_from_payload(db, &tuner).await? {
            Some(result) => {
                synced_tuners += 1;
                movie_count += result
                    .get("MovieCount")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
                series_episode_count += result
                    .get("SeriesEpisodeCount")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
            }
            None => skipped_tuners += 1,
        }
    }
    Ok(serde_json::json!({
        "TunersSynced": synced_tuners,
        "TunersSkipped": skipped_tuners,
        "MovieCount": movie_count,
        "SeriesEpisodeCount": series_episode_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse_array_chunks_for_test(
        body: Vec<u8>,
        maximum_items: usize,
    ) -> (Vec<Vec<serde_json::Value>>, Result<usize, XtreamFetchError>) {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let overflowed = Arc::new(AtomicBool::new(false));
        let parser = tokio::task::spawn_blocking(move || {
            parse_xtream_array_reader(
                std::io::Cursor::new(body),
                sender,
                maximum_items,
                overflowed,
            )
        });
        let mut chunks = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            chunks.push(chunk);
        }
        (chunks, parser.await.unwrap())
    }

    #[derive(Clone, Default)]
    struct RecordingCatalogStore {
        snapshots: std::sync::Arc<std::sync::Mutex<Vec<(String, String, usize)>>>,
        batch_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        staged_libraries: std::sync::Arc<std::sync::Mutex<Vec<RemoteMediaLibraryStageSpec>>>,
        staged_appends: std::sync::Arc<std::sync::Mutex<Vec<(String, usize)>>>,
        stage_published: std::sync::Arc<AtomicBool>,
        stage_aborted: std::sync::Arc<AtomicBool>,
    }

    impl jellyrin_db::DatabaseBackend for RecordingCatalogStore {
        const DRIVER: jellyrin_db::DatabaseDriver = jellyrin_db::DatabaseDriver::PostgreSql;
    }

    impl XtreamCatalogStore for RecordingCatalogStore {
        fn replace_remote_media_library_snapshots(
            &self,
            batch: Vec<RemoteMediaLibrarySnapshot>,
        ) -> impl std::future::Future<Output = anyhow::Result<Vec<jellyrin_core::VirtualFolder>>> + Send
        {
            let recorded_snapshots = self.snapshots.clone();
            let batch_calls = self.batch_calls.clone();
            async move {
                batch_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let now = OffsetDateTime::now_utc();
                let mut folders = Vec::with_capacity(batch.len());
                let mut snapshots = recorded_snapshots.lock().unwrap();
                for snapshot in batch {
                    snapshots.push((
                        snapshot.library_name.clone(),
                        snapshot.collection_type.clone(),
                        snapshot.items.len(),
                    ));
                    folders.push(jellyrin_core::VirtualFolder {
                        id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
                        name: snapshot.library_name,
                        collection_type: Some(snapshot.collection_type),
                        locations: vec![snapshot.source_location],
                        created_at: now,
                        updated_at: now,
                    });
                }
                Ok(folders)
            }
        }

        fn begin_remote_media_catalog_stage(
            &self,
            libraries: Vec<RemoteMediaLibraryStageSpec>,
        ) -> impl std::future::Future<Output = anyhow::Result<RemoteMediaCatalogStage>> + Send
        {
            let staged_libraries = Arc::clone(&self.staged_libraries);
            async move {
                *staged_libraries.lock().unwrap() = libraries;
                RemoteMediaCatalogStage::try_from_id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            }
        }

        fn append_remote_media_catalog_stage<'a>(
            &'a self,
            _stage: &'a RemoteMediaCatalogStage,
            library_key: &'a str,
            items: Vec<RemoteMediaItemUpsert>,
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            let staged_appends = Arc::clone(&self.staged_appends);
            async move {
                anyhow::ensure!(
                    items.len() <= REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS,
                    "test stage append exceeded the DB contract"
                );
                staged_appends
                    .lock()
                    .unwrap()
                    .push((library_key.to_string(), items.len()));
                Ok(())
            }
        }

        fn publish_remote_media_catalog_stage<'a>(
            &'a self,
            _stage: &'a RemoteMediaCatalogStage,
        ) -> impl std::future::Future<Output = anyhow::Result<Vec<jellyrin_core::VirtualFolder>>>
        + Send
        + 'a {
            let published = Arc::clone(&self.stage_published);
            async move {
                published.store(true, Ordering::Relaxed);
                Ok(Vec::new())
            }
        }

        fn abort_remote_media_catalog_stage<'a>(
            &'a self,
            _stage: &'a RemoteMediaCatalogStage,
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            let aborted = Arc::clone(&self.stage_aborted);
            async move {
                aborted.store(true, Ordering::Relaxed);
                Ok(())
            }
        }

        async fn cleanup_abandoned_remote_media_catalog_stages(
            &self,
            _older_than: OffsetDateTime,
        ) -> anyhow::Result<u64> {
            Ok(0)
        }

        async fn live_tv_tuner_configurations_by_provider(
            &self,
            _provider_type: &str,
        ) -> anyhow::Result<Vec<serde_json::Value>> {
            Ok(Vec::new())
        }
    }

    async fn xtream_hardening_response(raw_response: &'static [u8]) -> reqwest::Response {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream.write_all(raw_response).await.unwrap();
        });
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap()
    }

    async fn xtream_single_response_client(raw_response: &'static [u8]) -> ValidatedXtreamClient {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            stream.write_all(raw_response).await.unwrap();
        });
        ValidatedXtreamClient {
            base_url: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    async fn xtream_catalog_server(
        vod_streams: Vec<u8>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 2048];
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let body: &[u8] = if request.contains("action=get_vod_categories") {
                    br#"[{"category_id":"10","category_name":"Movies"}]"#
                } else if request.contains("action=get_series_categories") {
                    br#"[{"category_id":"20","category_name":"Series"}]"#
                } else if request.contains("action=get_vod_streams") {
                    &vod_streams
                } else if request.contains("action=get_series") {
                    b"[]"
                } else {
                    panic!("unexpected Xtream request: {request}");
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
            }
        });
        (address, server)
    }

    async fn xtream_series_category_fallback_server()
    -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 2048];
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                if request.contains("action=get_series")
                    && !request.contains("category_id=")
                    && !request.contains("action=get_series_info")
                    && !request.contains("action=get_series_categories")
                {
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        XTREAM_MAX_CATALOG_BODY_BYTES + 1
                    );
                    stream.write_all(headers.as_bytes()).await.unwrap();
                    continue;
                }
                let body: &[u8] = if request.contains("action=get_vod_categories") {
                    b"[]"
                } else if request.contains("action=get_series_categories") {
                    br#"[{"category_id":"20","category_name":"Drama"},{"category_id":"21","category_name":"Comedy"}]"#
                } else if request.contains("action=get_vod_streams") {
                    b"[]"
                } else if request.contains("action=get_series_info")
                    && request.contains("series_id=100")
                {
                    br#"{"info":{"name":"Drama Show"},"episodes":{"1":[{"id":"1001","title":"Drama Episode","episode_num":1,"container_extension":"mp4"}]}}"#
                } else if request.contains("action=get_series_info")
                    && request.contains("series_id=200")
                {
                    br#"{"info":{"name":"Comedy Show"},"episodes":{"1":[{"id":"2001","title":"Comedy Episode","episode_num":1,"container_extension":"mp4"}]}}"#
                } else if request.contains("action=get_series")
                    && request.contains("category_id=20")
                {
                    br#"[{"series_id":"100","name":"Drama Show","category_id":"20"}]"#
                } else if request.contains("action=get_series")
                    && request.contains("category_id=21")
                {
                    br#"[{"series_id":"200","name":"Comedy Show","category_id":"21"}]"#
                } else {
                    panic!("unexpected Xtream fallback request: {request}");
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
            }
        });
        (address, server)
    }

    struct PrivateProviderUrlsGuard(Option<std::ffi::OsString>);

    impl PrivateProviderUrlsGuard {
        fn allow() -> Self {
            let previous = std::env::var_os("JELLYRIN_ALLOW_PRIVATE_PROVIDER_URLS");
            // SAFETY: this integration test is serialized with every test that mutates this
            // variable, and restores its original value before releasing the serial guard.
            unsafe { std::env::set_var("JELLYRIN_ALLOW_PRIVATE_PROVIDER_URLS", "true") };
            Self(previous)
        }
    }

    impl Drop for PrivateProviderUrlsGuard {
        fn drop(&mut self) {
            // SAFETY: see `allow`; the serial test guard still exists while this value drops.
            unsafe {
                if let Some(previous) = self.0.take() {
                    std::env::set_var("JELLYRIN_ALLOW_PRIVATE_PROVIDER_URLS", previous);
                } else {
                    std::env::remove_var("JELLYRIN_ALLOW_PRIVATE_PROVIDER_URLS");
                }
            }
        }
    }

    #[tokio::test]
    async fn empty_catalogue_is_success_but_invalid_response_is_an_error() {
        let empty = xtream_single_response_client(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
        )
        .await;
        assert_eq!(
            fetch_xtream_array(
                &empty,
                "account",
                "secret",
                "get_vod_streams",
                Duration::from_secs(2),
                1024,
                10,
            )
            .await,
            Ok(Vec::new())
        );

        let invalid = xtream_single_response_client(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull",
        )
        .await;
        assert_eq!(
            fetch_xtream_array(
                &invalid,
                "account",
                "secret",
                "get_vod_streams",
                Duration::from_secs(2),
                1024,
                10,
            )
            .await,
            Err(XtreamFetchError::InvalidJson)
        );
    }

    #[tokio::test]
    async fn incremental_catalog_parser_chunks_unicode_and_validates_the_complete_document() {
        let body = serde_json::to_vec(
            &(0..1_201)
                .map(|index| serde_json::json!({"id": index, "name": "Película \\ \"ñ\""}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let (chunks, result) = parse_array_chunks_for_test(body, 2_000).await;
        assert_eq!(result, Ok(1_201));
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            [500, 500, 201]
        );
        assert_eq!(chunks[2][200]["id"], 1_200);

        let malformed_after_a_complete_chunk = format!(
            "[{}",
            (0..501)
                .map(|index| format!("{{\"id\":{index}}}"))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes();
        let (chunks, result) =
            parse_array_chunks_for_test(malformed_after_a_complete_chunk, 2_000).await;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 500);
        assert_eq!(result, Err(XtreamFetchError::InvalidJson));
    }

    #[tokio::test]
    async fn incremental_catalog_parser_enforces_the_inspected_item_cap() {
        let body = format!(
            "[{}]",
            (0..1_001)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes();
        let (chunks, result) = parse_array_chunks_for_test(body, 1_000).await;
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 1_000);
        assert_eq!(result, Err(XtreamFetchError::TooManyItems));
    }

    #[tokio::test]
    async fn incremental_catalog_parser_accepts_more_than_the_legacy_100k_limit() {
        let body = format!(
            "[{}]",
            (0..100_001)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes();
        let (chunks, result) =
            parse_array_chunks_for_test(body, REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS).await;
        assert_eq!(result, Ok(100_001));
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 100_001);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= XTREAM_ARRAY_CHUNK_ITEMS)
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn staged_sync_publishes_a_catalogue_larger_than_the_legacy_100k_limit() {
        let _private_urls = PrivateProviderUrlsGuard::allow();
        let mut vod_streams = Vec::with_capacity(9_000_000);
        vod_streams.push(b'[');
        for stream_id in 1..=100_001_u64 {
            if stream_id > 1 {
                vod_streams.push(b',');
            }
            serde_json::to_writer(
                &mut vod_streams,
                &serde_json::json!({
                    "stream_id": stream_id,
                    "name": format!("Movie {stream_id}"),
                    "category_id": "10",
                    "container_extension": "mp4"
                }),
            )
            .unwrap();
        }
        vod_streams.push(b']');
        assert!(vod_streams.len() < XTREAM_MAX_CATALOG_BODY_BYTES);

        let (address, server) = xtream_catalog_server(vod_streams).await;
        let store = RecordingCatalogStore::default();
        let result = sync_xtream_media_from_payload(
            &store,
            &serde_json::json!({
                "Id": XTREAM_PRIMARY_TUNER_ID,
                "Url": format!("http://{address}"),
                "Username": "account",
                "Password": "secret",
                "MovieLimit": 0,
                "SeriesLimit": 0
            }),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();

        assert_eq!(result["MovieCount"], 100_001);
        assert_eq!(result["SeriesEpisodeCount"], 0);
        assert_eq!(
            store
                .staged_libraries
                .lock()
                .unwrap()
                .iter()
                .map(|library| library.key.as_str())
                .collect::<Vec<_>>(),
            ["movies", "series"]
        );
        let appends = store.staged_appends.lock().unwrap();
        assert!(appends.iter().all(|(_, count)| {
            *count > 0 && *count <= REMOTE_MEDIA_CATALOG_STAGE_MAX_APPEND_ITEMS
        }));
        assert_eq!(
            appends
                .iter()
                .filter(|(key, _)| key == "movies")
                .map(|(_, count)| count)
                .sum::<usize>(),
            100_001
        );
        assert!(store.stage_published.load(Ordering::Relaxed));
        assert!(!store.stage_aborted.load(Ordering::Relaxed));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn oversized_series_catalogue_falls_back_to_validated_category_chunks() {
        let _private_urls = PrivateProviderUrlsGuard::allow();
        let (address, server) = xtream_series_category_fallback_server().await;
        let store = RecordingCatalogStore::default();
        let result = sync_xtream_media_from_payload(
            &store,
            &serde_json::json!({
                "Id": XTREAM_PRIMARY_TUNER_ID,
                "Url": format!("http://{address}"),
                "Username": "account",
                "Password": "secret",
                "SeriesLimit": 0,
                "SeriesCategoryIds": ["20", "21"]
            }),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();

        assert_eq!(result["MovieCount"], 0);
        assert_eq!(result["SeriesEpisodeCount"], 2);
        let appends = store.staged_appends.lock().unwrap();
        assert_eq!(
            appends
                .iter()
                .filter(|(key, _)| key == "series")
                .map(|(_, count)| count)
                .sum::<usize>(),
            2
        );
        assert!(store.stage_published.load(Ordering::Relaxed));
        assert!(!store.stage_aborted.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn complete_empty_media_snapshot_clears_both_catalogues() {
        let store = RecordingCatalogStore::default();

        let counts = persist_xtream_media_import(
            &store,
            XtreamMediaImport {
                tuner_id: "tuner-empty".to_string(),
                movies: Vec::new(),
                series_episodes: Vec::new(),
            },
        )
        .await
        .unwrap();

        assert_eq!(counts, (0, 0));
        assert_eq!(
            store.batch_calls.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            store.snapshots.lock().unwrap().as_slice(),
            [
                (
                    "Xtream Movies (tuner-empty)".to_string(),
                    "movies".to_string(),
                    0,
                ),
                (
                    "Xtream Series (tuner-empty)".to_string(),
                    "tvshows".to_string(),
                    0,
                ),
            ]
        );
    }

    #[test]
    fn parse_vod_streams_creates_remote_movie_items() {
        let categories = parse_categories(&[serde_json::json!({
            "category_id": "10",
            "category_name": "Action"
        })]);
        let items = parse_vod_streams(
            "tuner-a",
            &[serde_json::json!({
                "stream_id": 42,
                "name": "Demo Movie",
                "container_extension": "mkv",
                "category_id": "10",
                "stream_icon": "https://images.test/movie.png",
                "duration_secs": 120
            })],
            &categories,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].collection_type, "movies");
        assert_eq!(items[0].media_type, "Video");
        assert_eq!(items[0].path, "xtream://tuner-a/movies/Demo Movie [42].mkv");
        assert_eq!(items[0].runtime_ticks, Some(1_200_000_000));
        assert!(items[0].metadata.get("RemoteSourceUrl").is_none());
        assert_eq!(items[0].metadata["RemoteSourceRef"]["TunerId"], "tuner-a");
        assert_eq!(items[0].metadata["RemoteSourceRef"]["RemoteId"], "42");
        assert_eq!(items[0].metadata["Genres"], serde_json::json!(["Action"]));
        assert_eq!(
            items[0].metadata["ImageUrl"],
            "https://images.test/movie.png"
        );
        assert_eq!(
            items[0].metadata["PrimaryImageUrl"],
            "https://images.test/movie.png"
        );
    }

    #[test]
    fn xtream_extensions_are_limited_to_the_ffmpeg_media_contract() {
        assert_eq!(sanitized_xtream_extension(".MKV"), Some("mkv".to_string()));
        assert_eq!(sanitized_xtream_extension("m3u8"), None);
        assert_eq!(sanitized_xtream_extension("exe"), None);
        assert_eq!(sanitized_xtream_extension("custom-container"), None);
        assert_eq!(sanitized_xtream_extension("m@p4"), None);
        assert_eq!(sanitized_xtream_extension("m.p4"), None);
        assert_eq!(sanitized_xtream_extension("m-p4"), None);
    }

    #[test]
    fn parse_series_episodes_creates_remote_episode_items() {
        let categories = parse_categories(&[serde_json::json!({
            "category_id": "20",
            "category_name": "Drama"
        })]);
        let series_item = serde_json::json!({
            "series_id": "99",
            "name": "Demo Series",
            "category_id": "20"
        });
        let info = serde_json::json!({
            "info": {
                "name": "Demo Series",
                "cover": "https://images.test/series.png"
            },
            "episodes": {
                "2": [{
                    "id": "abc",
                    "episode_num": 3,
                    "title": "Pilot / Start",
                    "container_extension": "mp4",
                    "info": {
                        "duration_secs": 60,
                        "plot": "Episode overview"
                    }
                }]
            }
        });

        let items = parse_series_episodes("tuner-a", &series_item, &info, &categories, None);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].collection_type, "tvshows");
        assert_eq!(
            items[0].path,
            "xtream://tuner-a/series/Demo Series/Season 2/S02E03 - Pilot Start [abc].mp4"
        );
        assert!(items[0].metadata.get("RemoteSourceUrl").is_none());
        assert_eq!(items[0].metadata["RemoteSourceRef"]["TunerId"], "tuner-a");
        assert_eq!(items[0].metadata["RemoteSourceRef"]["RemoteId"], "abc");
        assert_eq!(
            items[0].metadata["SeriesName"],
            serde_json::json!("Demo Series")
        );
        assert_eq!(items[0].metadata["ParentIndexNumber"], serde_json::json!(2));
        assert_eq!(items[0].metadata["IndexNumber"], serde_json::json!(3));
        assert_eq!(
            items[0].metadata["ImageUrl"],
            "https://images.test/series.png"
        );
        assert_eq!(
            items[0].metadata["PrimaryImageUrl"],
            "https://images.test/series.png"
        );
        assert_eq!(
            items[0].metadata["SeriesImageUrl"],
            "https://images.test/series.png"
        );
    }

    #[test]
    fn catalogue_images_omit_urls_that_can_carry_credentials() {
        assert_eq!(
            safe_xtream_image_url("https://images.test/public.png").as_deref(),
            Some("https://images.test/public.png")
        );
        for unsafe_url in [
            "https://account:secret@images.test/private.png",
            "https://images.test/private.png?token=secret",
            "https://images.test/private.png#token=secret",
        ] {
            assert_eq!(
                safe_xtream_image_url(unsafe_url),
                None,
                "unexpectedly accepted {unsafe_url:?}"
            );
        }

        let live = parse_streams(
            "tuner-a",
            &[serde_json::json!({
                "stream_id": "7",
                "name": "Private live artwork",
                "stream_icon": "https://images.test/live.png?token=secret"
            })],
            &LiveTvXtreamImportOptions::default(),
        );
        assert!(live[0].get("ImageUrl").is_none());

        let channel = serde_json::json!({
            "Id": "legacy-7",
            "Name": "Legacy channel",
            "Path": "https://provider.test/live/7.ts",
            "ImageUrl": "https://account:secret@images.test/live.png",
            "PrimaryImageUrl": "https://images.test/live.png#secret"
        });
        let channel = channel_upsert_from_json("tuner-a", &channel).unwrap();
        assert!(channel.logo_url.is_none());
        assert!(channel.metadata.get("ImageUrl").is_none());
        assert!(channel.metadata.get("PrimaryImageUrl").is_none());

        let movies = parse_vod_streams(
            "tuner-a",
            &[
                serde_json::json!({
                    "stream_id": "8",
                    "name": "Fallback artwork",
                    "stream_icon": "https://images.test/movie.png?token=secret",
                    "cover": "https://images.test/public-cover.png"
                }),
                serde_json::json!({
                    "stream_id": "9",
                    "name": "Private artwork",
                    "cover": "https://images.test/movie.png#secret"
                }),
            ],
            &[],
        );
        assert_eq!(
            movies[0].metadata["ImageUrl"],
            "https://images.test/public-cover.png"
        );
        assert!(movies[1].metadata.get("ImageUrl").is_none());
        assert!(movies[1].metadata.get("PrimaryImageUrl").is_none());

        let series = parse_series_episodes(
            "tuner-a",
            &serde_json::json!({
                "series_id": "10",
                "name": "Private series artwork"
            }),
            &serde_json::json!({
                "info": {
                    "name": "Private series artwork",
                    "cover": "https://images.test/series.png?token=secret",
                    "cover_big": "https://images.test/series.png#secret"
                },
                "episodes": [{
                    "id": "11",
                    "title": "Private episode artwork",
                    "info": {
                        "movie_image": "https://account:secret@images.test/episode.png",
                        "cover": "https://images.test/episode.png#secret"
                    }
                }]
            }),
            &[],
            None,
        );
        assert_eq!(series.len(), 1);
        assert!(series[0].metadata.get("ImageUrl").is_none());
        assert!(series[0].metadata.get("PrimaryImageUrl").is_none());
        assert!(series[0].metadata.get("SeriesImageUrl").is_none());
    }

    #[test]
    fn opaque_references_resolve_jit_without_persisting_credentials() {
        let remote_ref = XtreamRemoteSourceRef::new("tuner-a", "vod", "42", "mkv").unwrap();
        let remote_ref_json = serde_json::to_value(&remote_ref).unwrap();
        let tuner = serde_json::json!({
            "Id": "tuner-a",
            "Url": "https://provider.example/base",
            "Username": "account",
            "Password": "secret"
        });
        let url = resolve_remote_source_ref(&tuner, &remote_ref_json).unwrap();
        assert_eq!(url, "https://provider.example/movie/account/secret/42.mkv");
        assert!(!remote_ref_json.to_string().contains("account"));
        assert!(!remote_ref_json.to_string().contains("secret"));

        let live_ref = XtreamRemoteSourceRef::new("tuner-a", "live", "7", "ts").unwrap();
        let encoded = encoded_live_provider_reference(&live_ref).unwrap();
        assert_eq!(
            resolve_live_provider_reference(&tuner, &encoded).as_deref(),
            Some("https://provider.example/live/account/secret/7.ts")
        );
        assert!(!encoded.contains("account"));
        assert!(!encoded.contains("secret"));
    }

    #[test]
    fn direct_source_must_equal_the_credential_free_reference_reconstruction() {
        let reconstructed =
            Some("https://provider.example/movie/account/secret/42.mkv".to_string());
        let equivalent = serde_json::json!({
            "direct_source": "https://provider.example:443/movie/account/secret/42.mkv"
        });
        assert!(direct_source_matches_reconstructed(
            &equivalent,
            reconstructed.clone()
        ));

        let alternate_secret_url = serde_json::json!({
            "direct_source": "https://cdn.example/private/42.mkv?token=do-not-store"
        });
        assert!(!direct_source_matches_reconstructed(
            &alternate_secret_url,
            reconstructed.clone()
        ));
        assert!(!direct_source_matches_reconstructed(
            &serde_json::json!({"direct_source": "/relative/42.mkv"}),
            reconstructed.clone()
        ));
        assert!(!direct_source_matches_reconstructed(
            &serde_json::json!({"direct_source": 42}),
            reconstructed
        ));
        assert!(direct_source_matches_reconstructed(
            &serde_json::json!({}),
            None
        ));
    }

    #[test]
    fn live_channel_ids_are_namespaced_per_tuner() {
        let streams = [serde_json::json!({
            "stream_id": "42",
            "name": "Channel"
        })];
        let first = parse_streams("tuner-a", &streams, &LiveTvXtreamImportOptions::default());
        let second = parse_streams("tuner-b", &streams, &LiveTvXtreamImportOptions::default());

        assert_ne!(first[0]["Id"], second[0]["Id"]);
        assert_eq!(first[0]["RemoteId"], "42");
        assert_eq!(second[0]["RemoteId"], "42");
        assert_eq!(
            channel_upsert_from_json("tuner-a", &first[0])
                .unwrap()
                .remote_id,
            "42"
        );
    }

    #[test]
    fn primary_plugin_keeps_legacy_catalog_ids_during_jit_backfill() {
        let items = parse_vod_streams(
            XTREAM_PRIMARY_TUNER_ID,
            &[serde_json::json!({
                "stream_id": "42",
                "name": "Legacy Movie",
                "container_extension": "mkv"
            })],
            &[],
        );

        assert_eq!(items[0].id, stable_entity_id("xtream-vod", "42"));
        assert_eq!(items[0].path, "xtream://movies/Legacy Movie [42].mkv");
        assert!(items[0].metadata.get("RemoteSourceUrl").is_none());
        assert_eq!(
            items[0].metadata["RemoteSourceRef"]["TunerId"],
            XTREAM_PRIMARY_TUNER_ID
        );
    }

    #[test]
    fn category_selection_empty_include_allows_everything() {
        let sel = CategorySelection::default();
        assert!(sel.allows(Some("10")));
        assert!(sel.allows(None));
    }

    #[test]
    fn category_selection_respects_include_and_exclude() {
        let payload = serde_json::json!({
            "VodCategoryIds": ["10", "20"],
            "ExcludeVodCategoryIds": ["20"]
        });
        let sel = CategorySelection::from_payload(
            &payload,
            &["VodCategoryIds"],
            &["ExcludeVodCategoryIds"],
        );
        assert!(sel.allows(Some("10")));
        // excluded wins even though it is in include
        assert!(!sel.allows(Some("20")));
        // not in include set
        assert!(!sel.allows(Some("30")));
        // no category id and a non-empty include set => excluded
        assert!(!sel.allows(None));
    }

    #[test]
    fn live_tv_options_prefer_live_category_ids() {
        let payload = serde_json::json!({
            "LiveCategoryIds": ["5"],
            "CategoryIds": ["99"]
        });
        let opts = LiveTvXtreamImportOptions::from_payload(&payload);
        // LiveCategoryIds wins (first-match); legacy CategoryIds is ignored.
        assert!(opts.include_category_ids.contains("5"));
        assert!(!opts.include_category_ids.contains("99"));
    }

    #[test]
    fn live_tv_options_fall_back_to_legacy_category_ids() {
        let payload = serde_json::json!({ "CategoryIds": ["7"] });
        let opts = LiveTvXtreamImportOptions::from_payload(&payload);
        assert!(opts.include_category_ids.contains("7"));
    }

    #[test]
    fn opaque_provider_channel_upsert_does_not_require_or_store_a_stream_url() {
        let channel = serde_json::json!({
            "Id": "opaque-7",
            "Name": "Opaque channel",
            "ProviderType": "external-provider",
            "ProviderReference": "provider:v1:opaque.signature"
        });

        let upsert = channel_upsert_from_json("tuner-a", &channel)
            .expect("provider references should be accepted without a path");
        assert!(upsert.stream_url.is_empty());
        assert_eq!(
            upsert.metadata["ProviderReference"],
            "provider:v1:opaque.signature"
        );
        assert!(upsert.metadata.get("Path").is_none());
    }

    #[test]
    fn channel_upsert_rejects_mixed_or_missing_source_state() {
        let mixed = serde_json::json!({
            "Id": "mixed-7",
            "Name": "Mixed channel",
            "Path": "https://provider.test/live/7.ts",
            "ProviderReference": "provider:v1:opaque.signature"
        });
        assert!(channel_upsert_from_json("tuner-a", &mixed).is_none());

        let missing = serde_json::json!({
            "Id": "missing-7",
            "Name": "Missing channel"
        });
        assert!(channel_upsert_from_json("tuner-a", &missing).is_none());
    }

    #[test]
    fn xtream_hardening_base_url_and_credentials_are_strict() {
        let url = validated_xtream_base_url("https://provider.example:8443/panel/?legacy=1")
            .expect("valid provider URL");
        assert_eq!(url.as_str(), "https://provider.example:8443/");

        for invalid in [
            "ftp://provider.example/",
            "https://user:secret@provider.example/",
            "https://provider.example/#fragment",
            "https://provider.example/\nInjected: value",
            "not a URL",
        ] {
            assert_eq!(
                validated_xtream_base_url(invalid),
                Err(XtreamFetchError::InvalidInput),
                "unexpectedly accepted {invalid:?}"
            );
        }
        assert!(valid_xtream_secret("account"));
        assert!(!valid_xtream_secret(""));
        assert!(!valid_xtream_secret("secret\r\nheader"));
        assert!(!valid_xtream_secret(
            &"x".repeat(XTREAM_MAX_CREDENTIAL_BYTES + 1)
        ));
        assert!(valid_xtream_identifier("episode-42"));
        assert!(!valid_xtream_identifier("episode\n42"));
    }

    #[test]
    fn xtream_hardening_ssrf_policy_blocks_special_ranges_and_gates_private_ranges() {
        for address in [
            "0.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:203.0.113.1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
            "4000::1",
            "5f00::1",
            "fec0::1",
            "fe80::1",
            "ff02::1",
        ] {
            let address = address.parse().unwrap();
            assert!(!provider_address_allowed(address, false));
            assert!(!provider_address_allowed(address, true));
        }

        for address in ["10.0.0.1", "127.0.0.1", "fd00::1", "::1"] {
            let address = address.parse().unwrap();
            assert!(!provider_address_allowed(address, false));
            assert!(provider_address_allowed(address, true));
        }
        assert!(provider_address_allowed("8.8.8.8".parse().unwrap(), false));
        assert!(provider_address_allowed(
            "2001:4860:4860::8888".parse().unwrap(),
            false
        ));
        assert!(provider_address_allowed(
            "64:ff9b::808:808".parse().unwrap(),
            false
        ));
        assert!(!provider_address_allowed(
            "64:ff9b::a00:1".parse().unwrap(),
            false
        ));
        assert!(provider_address_allowed(
            "64:ff9b::a00:1".parse().unwrap(),
            true
        ));
        assert!(!provider_address_allowed(
            "::ffff:10.0.0.1".parse().unwrap(),
            false
        ));
        assert!(provider_address_allowed(
            "::ffff:10.0.0.1".parse().unwrap(),
            true
        ));
    }

    #[test]
    fn xtream_hardening_catalog_and_configured_limits_are_hard_bounded() {
        assert_eq!(bounded_usize(0, 1, 2_000), 1);
        assert_eq!(bounded_usize(u64::MAX, 1, 2_000), 2_000);
        assert_eq!(positive_bounded_usize(0, 2_000), None);
        assert_eq!(positive_bounded_usize(25, 2_000), Some(25));
        assert_eq!(positive_bounded_usize(u64::MAX, 2_000), Some(2_000));
        assert_eq!(validate_item_count(100_000, 100_000), Ok(()));
        assert_eq!(
            validate_item_count(100_001, 100_000),
            Err(XtreamFetchError::TooManyItems)
        );

        let options = LiveTvXtreamImportOptions::from_payload(&serde_json::json!({
            "ChannelLimit": u64::MAX
        }));
        assert_eq!(options.limit, Some(LIVE_TV_XTREAM_MAX_IMPORT_LIMIT));
        const {
            assert!(XTREAM_MAX_SERIES_REQUESTS <= LIVE_TV_XTREAM_MAX_IMPORT_LIMIT);
            assert!(XTREAM_MAX_EPISODES_PER_SERIES < LIVE_TV_XTREAM_MAX_IMPORT_LIMIT);
            assert!(XTREAM_MAX_SERIES_INFO_BODY_BYTES < XTREAM_MAX_CATALOG_BODY_BYTES);
        }
    }

    #[test]
    fn xtream_hardening_series_episode_shapes_are_counted_before_allocation() {
        let flat = serde_json::json!({ "episodes": [{}, {}, {}] });
        let seasons = serde_json::json!({
            "episodes": {
                "1": [{}, {}],
                "2": [{}, {}, {}],
                "metadata": { "ignored": true }
            }
        });
        assert_eq!(series_episode_count(&flat), 3);
        assert_eq!(series_episode_count(&seasons), 5);
        assert_eq!(series_episode_count(&serde_json::json!({})), 0);
        assert!(valid_series_info_shape(&flat));
        assert!(valid_series_info_shape(&serde_json::json!({
            "episodes": []
        })));
        assert!(!valid_series_info_shape(&serde_json::json!({
            "error": "upstream unavailable"
        })));
        assert!(!valid_series_episode_prefix(&flat, 3));
        assert!(valid_series_episode_prefix(
            &serde_json::json!({
                "episodes": [{"id": "1"}, {"id": 2}]
            }),
            2
        ));
        assert_eq!(
            epg_listing_count(&serde_json::json!({ "listings": [{}, {}] })),
            2
        );
    }

    #[tokio::test]
    async fn xtream_hardening_streaming_body_limit_checks_headers_and_chunks() {
        let declared = xtream_hardening_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
        )
        .await;
        assert_eq!(
            read_bounded_body(declared, 5).await,
            Err(XtreamFetchError::BodyTooLarge)
        );

        let chunked = xtream_hardening_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
        )
        .await;
        assert_eq!(
            read_bounded_body(chunked, 5).await,
            Err(XtreamFetchError::BodyTooLarge)
        );
    }
}
