use std::{net::SocketAddr, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, Url, header};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};

use crate::{ApiError, remote_provider_address_allowed};

const API_ORIGIN: &str = "https://musicbrainz.org/ws/2/";
const API_HOST: &str = "musicbrainz.org";
const API_PORT: u16 = 443;
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULTS: usize = 10;
const USER_AGENT: &str = "Jellyrin/0.1.0 (https://github.com/alseif0x/jellyrin)";

static HTTP_CLIENT: OnceCell<Client> = OnceCell::const_new();
static LAST_REQUEST_STARTED: Mutex<Option<tokio::time::Instant>> = Mutex::const_new(None);

pub(crate) async fn remote_search(
    item_type: &str,
    name: Option<&str>,
    year: Option<i32>,
    configuration: &Value,
) -> Result<Vec<Value>, ApiError> {
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(Vec::new());
    };
    let request_interval = configured_request_interval(configuration);
    match item_type {
        "MusicArtist" => search_artists(name, request_interval).await,
        "MusicAlbum" => search_release_groups(name, year, request_interval).await,
        _ => Ok(Vec::new()),
    }
}

async fn search_artists(name: &str, request_interval: Duration) -> Result<Vec<Value>, ApiError> {
    let response = get_search_response::<ArtistSearchResponse>(
        "artist",
        &format!("artist:\"{}\"", escape_lucene_phrase(name)),
        request_interval,
    )
    .await?;
    Ok(response
        .artists
        .into_iter()
        .take(MAX_RESULTS)
        .map(|artist| {
            let overview = [artist.disambiguation, artist.country]
                .into_iter()
                .flatten()
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            json!({
                "Name": artist.name,
                "ProviderIds": { "MusicBrainz": artist.id },
                "SearchProviderName": "MusicBrainz",
                "Overview": overview,
                "Type": "MusicArtist"
            })
        })
        .collect())
}

async fn search_release_groups(
    name: &str,
    year: Option<i32>,
    request_interval: Duration,
) -> Result<Vec<Value>, ApiError> {
    let response = get_search_response::<ReleaseGroupSearchResponse>(
        "release-group",
        &format!("releasegroup:\"{}\"", escape_lucene_phrase(name)),
        request_interval,
    )
    .await?;
    Ok(response
        .release_groups
        .into_iter()
        .filter(|release_group| {
            year.is_none_or(|expected| release_group_year(release_group) == Some(expected))
        })
        .take(MAX_RESULTS)
        .map(|release_group| {
            let production_year = release_group_year(&release_group);
            let artists = release_group
                .artist_credit
                .into_iter()
                .map(|credit| credit.name)
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>();
            json!({
                "Name": release_group.title,
                "ProviderIds": { "MusicBrainz": release_group.id },
                "ProductionYear": production_year,
                "PremiereDate": release_group.first_release_date,
                "Artists": artists,
                "SearchProviderName": "MusicBrainz",
                "Type": "MusicAlbum"
            })
        })
        .collect())
}

async fn get_search_response<T>(
    entity: &str,
    query: &str,
    request_interval: Duration,
) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let mut url = api_url(entity)?;
    url.query_pairs_mut()
        .append_pair("query", query)
        .append_pair("limit", &MAX_RESULTS.to_string())
        .append_pair("fmt", "json");
    validate_api_url(&url)?;
    wait_for_rate_limit(request_interval).await;
    let response = http_client()
        .await?
        .get(url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(request_error)?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "MusicBrainz request returned an error");
        return Ok(T::default());
    }
    let body = read_bounded_body(response).await?;
    serde_json::from_slice(&body)
        .map_err(|_| ApiError::service_unavailable("MusicBrainz response is invalid"))
}

async fn wait_for_rate_limit(request_interval: Duration) {
    let mut last_started = LAST_REQUEST_STARTED.lock().await;
    if let Some(last_started_at) = *last_started {
        let elapsed = last_started_at.elapsed();
        if elapsed < request_interval {
            tokio::time::sleep(request_interval - elapsed).await;
        }
    }
    *last_started = Some(tokio::time::Instant::now());
}

fn configured_request_interval(configuration: &Value) -> Duration {
    let requests_per_second = configuration
        .get("RateLimitPerSecond")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 10);
    Duration::from_secs_f64(1.0 / requests_per_second as f64)
        .max(Duration::from_millis(100))
        .min(DEFAULT_REQUEST_INTERVAL)
}

fn api_url(entity: &str) -> Result<Url, ApiError> {
    if !matches!(entity, "artist" | "release-group") {
        return Err(ApiError::internal("MusicBrainz entity is invalid"));
    }
    let mut url = Url::parse(API_ORIGIN)
        .map_err(|_| ApiError::internal("MusicBrainz request URL is invalid"))?;
    url.set_path(&format!("/ws/2/{entity}"));
    validate_api_url(&url)?;
    Ok(url)
}

fn validate_api_url(url: &Url) -> Result<(), ApiError> {
    if url.scheme() != "https"
        || url.host_str() != Some(API_HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/ws/2/")
    {
        return Err(ApiError::internal("MusicBrainz request URL is invalid"));
    }
    Ok(())
}

async fn http_client() -> Result<&'static Client, ApiError> {
    HTTP_CLIENT
        .get_or_try_init(|| async {
            let resolved =
                tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((API_HOST, API_PORT)))
                    .await
                    .map_err(|_| {
                        ApiError::service_unavailable("MusicBrainz DNS resolution failed")
                    })?
                    .map_err(|_| {
                        ApiError::service_unavailable("MusicBrainz DNS resolution failed")
                    })?;
            let mut resolved = resolved
                .filter(|address| remote_provider_address_allowed(address.ip(), false))
                .collect::<Vec<_>>();
            resolved.sort_unstable();
            resolved.dedup();
            build_http_client(&resolved)
        })
        .await
}

fn build_http_client(resolved: &[SocketAddr]) -> Result<Client, ApiError> {
    if resolved.is_empty()
        || resolved.iter().any(|address| {
            address.port() != API_PORT || !remote_provider_address_allowed(address.ip(), false)
        })
    {
        return Err(ApiError::service_unavailable(
            "MusicBrainz address is not allowed",
        ));
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(1)
        .user_agent(USER_AGENT)
        .resolve_to_addrs(API_HOST, resolved)
        .build()
        .map_err(|_| ApiError::internal("MusicBrainz HTTP client initialization failed"))
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(ApiError::service_unavailable(
            "MusicBrainz response exceeds size limit",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(request_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(ApiError::service_unavailable(
                "MusicBrainz response exceeds size limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn request_error(error: reqwest::Error) -> ApiError {
    tracing::warn!(
        timeout = error.is_timeout(),
        connect = error.is_connect(),
        "MusicBrainz request failed"
    );
    ApiError::service_unavailable("MusicBrainz request failed")
}

fn escape_lucene_phrase(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars().take(256) {
        if matches!(
            character,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            escaped.push('\\');
        }
        if !character.is_control() {
            escaped.push(character);
        }
    }
    escaped
}

fn release_group_year(release_group: &ReleaseGroup) -> Option<i32> {
    release_group
        .first_release_date
        .as_deref()
        .and_then(|date| date.get(0..4))
        .and_then(|year| year.parse().ok())
}

#[derive(Debug, Default, Deserialize)]
struct ArtistSearchResponse {
    #[serde(default)]
    artists: Vec<Artist>,
}

#[derive(Debug, Deserialize)]
struct Artist {
    id: String,
    name: String,
    #[serde(default)]
    disambiguation: Option<String>,
    #[serde(default)]
    country: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ReleaseGroupSearchResponse {
    #[serde(default, rename = "release-groups")]
    release_groups: Vec<ReleaseGroup>,
}

#[derive(Debug, Deserialize)]
struct ReleaseGroup {
    id: String,
    title: String,
    #[serde(default, rename = "first-release-date")]
    first_release_date: Option<String>,
    #[serde(default, rename = "artist-credit")]
    artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Deserialize)]
struct ArtistCredit {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_fixture_maps_to_jellyfin_remote_search_shape() {
        let response: ArtistSearchResponse = serde_json::from_value(json!({
            "artists": [{
                "id": "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d",
                "name": "The Beatles",
                "country": "GB",
                "disambiguation": "UK rock band"
            }]
        }))
        .unwrap();
        let artist = &response.artists[0];
        assert_eq!(artist.name, "The Beatles");
        assert_eq!(artist.country.as_deref(), Some("GB"));
    }

    #[test]
    fn release_group_fixture_maps_year_and_artist_credit() {
        let response: ReleaseGroupSearchResponse = serde_json::from_value(json!({
            "release-groups": [{
                "id": "f5093c06-23e3-404f-aeaa-40f72885ee3a",
                "title": "Abbey Road",
                "first-release-date": "1969-09-26",
                "artist-credit": [{ "name": "The Beatles" }]
            }]
        }))
        .unwrap();
        assert_eq!(release_group_year(&response.release_groups[0]), Some(1969));
        assert_eq!(
            response.release_groups[0].artist_credit[0].name,
            "The Beatles"
        );
    }

    #[test]
    fn lucene_phrase_escapes_operators_and_bounds_input() {
        let escaped = escape_lucene_phrase("AC/DC: Live?*");
        assert_eq!(escaped, "AC\\/DC\\: Live\\?\\*");
        assert!(escape_lucene_phrase(&"a".repeat(300)).len() <= 256);
    }

    #[test]
    fn configured_rate_limit_controls_request_interval_with_safe_bounds() {
        assert_eq!(
            configured_request_interval(&json!({})),
            Duration::from_secs(1)
        );
        assert_eq!(
            configured_request_interval(&json!({ "RateLimitPerSecond": 2 })),
            Duration::from_millis(500)
        );
        assert_eq!(
            configured_request_interval(&json!({ "RateLimitPerSecond": 50 })),
            Duration::from_millis(100)
        );
        assert_eq!(
            configured_request_interval(&json!({ "RateLimitPerSecond": 0 })),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn url_validation_rejects_credentials_ports_and_foreign_hosts() {
        assert!(api_url("artist").is_ok());
        for url in [
            "https://user:pass@musicbrainz.org/ws/2/artist",
            "https://musicbrainz.org:444/ws/2/artist",
            "https://example.invalid/ws/2/artist",
            "http://musicbrainz.org/ws/2/artist",
        ] {
            assert!(validate_api_url(&Url::parse(url).unwrap()).is_err());
        }
    }
}
