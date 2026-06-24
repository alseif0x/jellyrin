use std::collections::{HashMap, HashSet};

use base64::{Engine as _, engine::general_purpose};
use jellyrin_core::{
    LIVE_TV_REMOTE_USER_AGENT, LIVE_TV_XTREAM_DEFAULT_EPG_LIMIT, LIVE_TV_XTREAM_MAX_EPG_CHANNELS,
    LIVE_TV_XTREAM_MAX_IMPORT_LIMIT, format_time_for_json, json_string_field,
    json_string_list_field, live_tv_stable_id, live_tv_u64_field, stable_entity_id,
};
use jellyrin_db::{LiveTvChannelUpsert, RemoteMediaItemUpsert};
use reqwest::Client as HttpClient;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Xtream provider type identifier.
pub const XTREAM_PROVIDER_TYPE: &str = "xtream";

pub struct LiveTvXtreamImport {
    pub channels: Vec<serde_json::Value>,
    pub categories: Vec<serde_json::Value>,
}

pub struct XtreamMediaImport {
    pub movies: Vec<RemoteMediaItemUpsert>,
    pub series_episodes: Vec<RemoteMediaItemUpsert>,
}

#[derive(Default)]
pub struct LiveTvXtreamImportOptions {
    include_category_ids: HashSet<String>,
    exclude_category_ids: HashSet<String>,
    limit: Option<usize>,
}

impl LiveTvXtreamImportOptions {
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        let include_category_ids = category_id_filter(
            payload,
            &["CategoryIds", "IncludeCategoryIds", "Categories"],
        );
        let exclude_category_ids = category_id_filter(payload, &["ExcludeCategoryIds"]);
        let limit = live_tv_u64_field(payload, "Limit")
            .or_else(|| live_tv_u64_field(payload, "ChannelLimit"))
            .map(|value| (value as usize).clamp(1, LIVE_TV_XTREAM_MAX_IMPORT_LIMIT));
        Self {
            include_category_ids,
            exclude_category_ids,
            limit,
        }
    }
}

pub async fn import_from_payload(payload: &serde_json::Value) -> Option<LiveTvXtreamImport> {
    let base_url = json_string_field(payload, "Url")?;
    let username = json_string_field(payload, "Username")
        .or_else(|| json_string_field(payload, "UserName"))?;
    let password = json_string_field(payload, "Password")?;
    let client = HttpClient::new();
    let streams_url = player_api_url(&base_url, &username, &password, Some("get_live_streams"))?;
    let streams: Vec<serde_json::Value> = client
        .get(streams_url)
        .header("User-Agent", LIVE_TV_REMOTE_USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let categories = if let Some(categories_url) =
        player_api_url(&base_url, &username, &password, Some("get_live_categories"))
    {
        match client
            .get(categories_url)
            .header("User-Agent", LIVE_TV_REMOTE_USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response
                .json::<Vec<serde_json::Value>>()
                .await
                .map(|values| parse_categories(&values))
                .unwrap_or_default(),
            Ok(response) => {
                tracing::warn!("Xtream categories returned HTTP {}", response.status());
                Vec::new()
            }
            Err(error) => {
                tracing::warn!("failed to fetch Xtream categories: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let base = base_url_root(&base_url)?;
    let options = LiveTvXtreamImportOptions::from_payload(payload);
    let mut channels = parse_streams(&base, &username, &password, &streams, &options);
    apply_category_names(&mut channels, &categories);
    (!channels.is_empty()).then_some(LiveTvXtreamImport {
        channels,
        categories,
    })
}

pub async fn import_media_from_payload(
    payload: &serde_json::Value,
) -> Option<XtreamMediaImport> {
    let base_url = json_string_field(payload, "Url")?;
    let username = json_string_field(payload, "Username")
        .or_else(|| json_string_field(payload, "UserName"))?;
    let password = json_string_field(payload, "Password")?;
    let client = HttpClient::new();
    let base = base_url_root(&base_url)?;

    let movie_categories = fetch_xtream_array(
        &client,
        &base_url,
        &username,
        &password,
        "get_vod_categories",
        15,
    )
    .await
    .map(|values| parse_categories(&values))
    .unwrap_or_default();
    let movies = fetch_xtream_array(
        &client,
        &base_url,
        &username,
        &password,
        "get_vod_streams",
        45,
    )
    .await
    .map(|streams| parse_vod_streams(&base, &username, &password, &streams, &movie_categories))
    .unwrap_or_default();

    let series_categories = fetch_xtream_array(
        &client,
        &base_url,
        &username,
        &password,
        "get_series_categories",
        15,
    )
    .await
    .map(|values| parse_categories(&values))
    .unwrap_or_default();
    let series = fetch_xtream_array(&client, &base_url, &username, &password, "get_series", 45)
        .await
        .unwrap_or_default();
    let series_limit = live_tv_u64_field(payload, "SeriesLimit")
        .or_else(|| live_tv_u64_field(payload, "XtreamSeriesLimit"))
        .map(|value| value as usize)
        .unwrap_or(250);
    let episode_limit = live_tv_u64_field(payload, "SeriesEpisodeLimit")
        .or_else(|| live_tv_u64_field(payload, "XtreamSeriesEpisodeLimit"))
        .map(|value| value as usize);
    let mut series_episodes = Vec::new();
    for series_item in series.iter().take(series_limit) {
        let Some(series_id) = json_string_field(series_item, "series_id")
            .or_else(|| live_tv_u64_field(series_item, "series_id").map(|id| id.to_string()))
        else {
            continue;
        };
        let Some(info) =
            fetch_series_info(&client, &base_url, &username, &password, &series_id).await
        else {
            continue;
        };
        series_episodes.extend(parse_series_episodes(
            &base,
            &username,
            &password,
            series_item,
            &info,
            &series_categories,
            episode_limit,
        ));
    }

    (!movies.is_empty() || !series_episodes.is_empty()).then_some(XtreamMediaImport {
        movies,
        series_episodes,
    })
}

async fn fetch_xtream_array(
    client: &HttpClient,
    base_url: &str,
    username: &str,
    password: &str,
    action: &str,
    timeout_secs: u64,
) -> Option<Vec<serde_json::Value>> {
    let url = player_api_url(base_url, username, password, Some(action))?;
    let response = client
        .get(url)
        .header("User-Agent", LIVE_TV_REMOTE_USER_AGENT)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::warn!("Xtream {action} returned HTTP {}", response.status());
        return None;
    }
    response.json::<Vec<serde_json::Value>>().await.ok()
}

async fn fetch_series_info(
    client: &HttpClient,
    base_url: &str,
    username: &str,
    password: &str,
    series_id: &str,
) -> Option<serde_json::Value> {
    let mut url = player_api_url(base_url, username, password, Some("get_series_info"))?;
    url.query_pairs_mut().append_pair("series_id", series_id);
    let response = client
        .get(url)
        .header("User-Agent", LIVE_TV_REMOTE_USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::warn!(
            "Xtream series info for {series_id} returned HTTP {}",
            response.status()
        );
        return None;
    }
    response.json().await.ok()
}

pub fn channel_upsert_from_json(
    tuner_id: &str,
    channel: &serde_json::Value,
) -> Option<LiveTvChannelUpsert> {
    let channel_id = json_string_field(channel, "Id")?;
    let remote_id = channel_id
        .strip_prefix("xtream_")
        .unwrap_or(channel_id.as_str())
        .to_string();
    let name = json_string_field(channel, "Name").unwrap_or_else(|| channel_id.clone());
    let sort_name = json_string_field(channel, "SortName")
        .or_else(|| json_string_field(channel, "Number"))
        .unwrap_or_else(|| name.clone());
    let category_id = json_string_field(channel, "CategoryId")
        .map(|remote_id| category_db_id(tuner_id, &remote_id));
    let stream_url =
        json_string_field(channel, "Path").or_else(|| json_string_field(channel, "MediaPath"))?;
    Some(LiveTvChannelUpsert {
        channel_id,
        tuner_id: tuner_id.to_string(),
        remote_id,
        category_id,
        name,
        sort_name,
        number: json_string_field(channel, "Number"),
        stream_url,
        logo_url: json_string_field(channel, "ImageUrl"),
        channel_type: json_string_field(channel, "ChannelType").unwrap_or_else(|| "TV".to_string()),
        metadata: channel.clone(),
    })
}

pub fn category_db_id(tuner_id: &str, remote_id: &str) -> String {
    live_tv_stable_id("livetv-category", &format!("{tuner_id}-{remote_id}"))
}

fn category_id_filter(payload: &serde_json::Value, keys: &[&str]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for key in keys {
        let Some(values) = json_string_list_field(payload, key) else {
            continue;
        };
        ids.extend(
            values
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        );
    }
    ids
}

pub async fn programs_from_payload(
    payload: &serde_json::Value,
) -> Option<Vec<serde_json::Value>> {
    let base_url = json_string_field(payload, "Url")?;
    let username = json_string_field(payload, "Username")
        .or_else(|| json_string_field(payload, "UserName"))?;
    let password = json_string_field(payload, "Password")?;
    let stream_ids = epg_stream_ids(payload);
    if stream_ids.is_empty() {
        return None;
    }
    let limit = live_tv_u64_field(payload, "Limit")
        .or_else(|| live_tv_u64_field(payload, "EpgLimit"))
        .unwrap_or(LIVE_TV_XTREAM_DEFAULT_EPG_LIMIT as u64)
        .clamp(1, 48);
    let client = HttpClient::new();
    let mut programs = Vec::new();
    for stream_id in stream_ids.into_iter().take(LIVE_TV_XTREAM_MAX_EPG_CHANNELS) {
        let channel_id = format!("xtream_{stream_id}");
        let mut epg_url = player_api_url(&base_url, &username, &password, Some("get_short_epg"))?;
        epg_url
            .query_pairs_mut()
            .append_pair("stream_id", &stream_id)
            .append_pair("limit", &limit.to_string());
        let response = match client
            .get(epg_url)
            .header("User-Agent", LIVE_TV_REMOTE_USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!("failed to fetch Xtream EPG for stream {stream_id}: {error}");
                continue;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(
                "Xtream EPG for stream {stream_id} returned HTTP {}",
                response.status()
            );
            continue;
        }
        let epg: serde_json::Value = match response.json().await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("failed to decode Xtream EPG for stream {stream_id}: {error}");
                continue;
            }
        };
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

fn base_url_root(base_url: &str) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url).ok()?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn player_api_url(
    base_url: &str,
    username: &str,
    password: &str,
    action: Option<&str>,
) -> Option<reqwest::Url> {
    let mut url = base_url_root(base_url)?;
    url.set_path("player_api.php");
    url.query_pairs_mut()
        .append_pair("username", username)
        .append_pair("password", password);
    if let Some(action) = action {
        url.query_pairs_mut().append_pair("action", action);
    }
    Some(url)
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

pub fn parse_streams(
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
    streams: &[serde_json::Value],
    options: &LiveTvXtreamImportOptions,
) -> Vec<serde_json::Value> {
    let iter = streams.iter().filter(|stream| {
        let category_id = json_string_field(stream, "category_id")
            .or_else(|| live_tv_u64_field(stream, "category_id").map(|value| value.to_string()));
        if !options.include_category_ids.is_empty()
            && category_id
                .as_ref()
                .is_none_or(|id| !options.include_category_ids.contains(id))
        {
            return false;
        }
        if category_id
            .as_ref()
            .is_some_and(|id| options.exclude_category_ids.contains(id))
        {
            return false;
        }
        true
    });
    let iter: Box<dyn Iterator<Item = &serde_json::Value>> = if let Some(limit) = options.limit {
        Box::new(iter.take(limit))
    } else {
        Box::new(iter)
    };
    iter.filter_map(|stream| {
        let stream_id = json_string_field(stream, "stream_id")
            .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()))?;
        let name = json_string_field(stream, "name").unwrap_or_else(|| stream_id.clone());
        let path = json_string_field(stream, "direct_source")
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
            .or_else(|| stream_url(base_url, username, password, &stream_id))?;
        let number = json_string_field(stream, "num")
            .or_else(|| live_tv_u64_field(stream, "num").map(|value| value.to_string()));
        let epg_channel_id = json_string_field(stream, "epg_channel_id");
        let category_id = json_string_field(stream, "category_id")
            .or_else(|| live_tv_u64_field(stream, "category_id").map(|value| value.to_string()));
        let stream_icon = json_string_field(stream, "stream_icon")
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
        let mut channel = serde_json::json!({
            "Id": format!("xtream_{stream_id}"),
            "Name": name,
            "Number": number,
            "Path": path,
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

fn parse_vod_streams(
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
    streams: &[serde_json::Value],
    categories: &[serde_json::Value],
) -> Vec<RemoteMediaItemUpsert> {
    let category_names = category_name_map(categories);
    streams
        .iter()
        .filter_map(|stream| {
            let stream_id = json_string_field(stream, "stream_id")
                .or_else(|| live_tv_u64_field(stream, "stream_id").map(|id| id.to_string()))?;
            let name = json_string_field(stream, "name").unwrap_or_else(|| stream_id.clone());
            let extension = xtream_extension(stream, "container_extension", "mp4");
            let remote_url = json_string_field(stream, "direct_source")
                .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
                .or_else(|| movie_url(base_url, username, password, &stream_id, &extension))?;
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
                .or_else(|| json_string_field(stream, "cover"))
                .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
            let id = stable_entity_id("xtream-vod", &stream_id);
            let path = format!(
                "xtream://movies/{} [{}].{}",
                xtream_path_segment(&name),
                xtream_path_segment(&stream_id),
                extension
            );
            let mut metadata = serde_json::json!({
                "Provider": "xtream",
                "XtreamKind": "vod",
                "RemoteSourceUrl": remote_url,
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
    base_url: &reqwest::Url,
    username: &str,
    password: &str,
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
        .or_else(|| json_string_field(series_info, "cover_big"))
        .or_else(|| json_string_field(series_item, "cover"))
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
    let series_stable_id = stable_entity_id("xtream-series", &series_id);
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
        let extension = xtream_extension(episode, "container_extension", "mp4");
        let Some(remote_url) = series_url(base_url, username, password, &episode_id, &extension)
        else {
            continue;
        };
        let id = stable_entity_id(
            "xtream-series-episode",
            &format!("{series_id}:{episode_id}"),
        );
        let path = format!(
            "xtream://series/{}/Season {}/S{:02}E{:02} - {} [{}].{}",
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
            .or_else(|| json_string_field(episode_info, "cover"))
            .or_else(|| series_image_url.clone())
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
        let mut metadata = serde_json::json!({
            "Provider": "xtream",
            "XtreamKind": "series-episode",
            "RemoteSourceUrl": remote_url,
            "XtreamSeriesId": series_id,
            "XtreamEpisodeId": episode_id,
            "ProviderIds": { "Xtream": episode_id },
            "SeriesProviderIds": { "Xtream": series_id },
            "Name": title,
            "SeriesName": series_name,
            "SeriesId": series_stable_id,
            "SeasonId": stable_entity_id("xtream-season", &format!("{series_id}:{season_number}")),
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

fn xtream_extension(value: &serde_json::Value, key: &str, default_value: &str) -> String {
    json_string_field(value, key)
        .map(|value| {
            value
                .trim()
                .trim_start_matches('.')
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_value.to_string())
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

pub fn parse_epg_programs(
    channel_id: &str,
    epg: &serde_json::Value,
) -> Vec<serde_json::Value> {
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

/// Persist media import (movies + series episodes) to the database.
pub async fn persist_xtream_media_import(
    db: &jellyrin_db::Database,
    import: XtreamMediaImport,
) -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    let movie_count = import.movies.len();
    let series_episode_count = import.series_episodes.len();
    if movie_count > 0 {
        db.replace_remote_media_library_snapshot(
            "Xtream Movies",
            "movies",
            "xtream://movies",
            import.movies,
        )
        .await?;
    }
    if series_episode_count > 0 {
        db.replace_remote_media_library_snapshot(
            "Xtream Series",
            "tvshows",
            "xtream://series",
            import.series_episodes,
        )
        .await?;
    }
    Ok((movie_count, series_episode_count))
}

/// Sync media for a single tuner from its payload.
pub async fn sync_xtream_media_from_payload(
    db: &jellyrin_db::Database,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(media_import) = import_media_from_payload(payload).await else {
        return Ok(None);
    };
    let (movie_count, series_episode_count) = persist_xtream_media_import(db, media_import).await?;
    Ok(Some(serde_json::json!({
        "MovieCount": movie_count,
        "SeriesEpisodeCount": series_episode_count,
    })))
}

/// Sync media for all configured xtream tuners.
pub async fn sync_all_configured_xtream_media(
    db: &jellyrin_db::Database,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
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

    #[test]
    fn parse_vod_streams_creates_remote_movie_items() {
        let base_url = reqwest::Url::parse("http://example.test/").unwrap();
        let categories = parse_categories(&[serde_json::json!({
            "category_id": "10",
            "category_name": "Action"
        })]);
        let items = parse_vod_streams(
            &base_url,
            "user",
            "pass",
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
        assert_eq!(items[0].path, "xtream://movies/Demo Movie [42].mkv");
        assert_eq!(items[0].runtime_ticks, Some(1_200_000_000));
        assert_eq!(
            items[0].metadata["RemoteSourceUrl"],
            serde_json::json!("http://example.test/movie/user/pass/42.mkv")
        );
        assert_eq!(items[0].metadata["Genres"], serde_json::json!(["Action"]));
    }

    #[test]
    fn parse_series_episodes_creates_remote_episode_items() {
        let base_url = reqwest::Url::parse("http://example.test/").unwrap();
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

        let items = parse_series_episodes(
            &base_url,
            "user",
            "pass",
            &series_item,
            &info,
            &categories,
            None,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].collection_type, "tvshows");
        assert_eq!(
            items[0].path,
            "xtream://series/Demo Series/Season 2/S02E03 - Pilot Start [abc].mp4"
        );
        assert_eq!(
            items[0].metadata["RemoteSourceUrl"],
            serde_json::json!("http://example.test/series/user/pass/abc.mp4")
        );
        assert_eq!(
            items[0].metadata["SeriesName"],
            serde_json::json!("Demo Series")
        );
        assert_eq!(items[0].metadata["ParentIndexNumber"], serde_json::json!(2));
        assert_eq!(items[0].metadata["IndexNumber"], serde_json::json!(3));
    }
}
