use std::{net::SocketAddr, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, Url, header};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::{ApiError, remote_provider_address_allowed};

const API_ORIGIN: &str = "https://api.themoviedb.org/3/";
const API_HOST: &str = "api.themoviedb.org";
const API_PORT: u16 = 443;
const IMAGE_ORIGIN: &str = "https://image.tmdb.org/t/p/original";
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULTS: usize = 10;

static HTTP_CLIENT: OnceCell<Client> = OnceCell::const_new();

pub(crate) async fn remote_search(
    item_type: &str,
    name: Option<&str>,
    year: Option<i32>,
    configuration: &Value,
) -> Result<Vec<Value>, ApiError> {
    let Some(name) = bounded_search_name(name) else {
        return Ok(Vec::new());
    };
    let endpoint = match item_type {
        "Movie" => "search/movie",
        "Series" => "search/tv",
        "Person" => "search/person",
        _ => return Ok(Vec::new()),
    };
    let api_key = environment_api_key()?;
    let mut url = api_url(endpoint)?;
    {
        let include_adult = bool_option(configuration, "IncludeAdult").to_string();
        let year = year.map(|year| year.to_string());
        let mut query = url.query_pairs_mut();
        query
            .append_pair("api_key", api_key.as_str())
            .append_pair("query", name)
            .append_pair("include_adult", &include_adult);
        if let Some(language) = string_option(configuration, "PreferredLanguage") {
            query.append_pair("language", language);
        }
        if endpoint == "search/movie" {
            if let Some(region) = string_option(configuration, "CountryCode") {
                query.append_pair("region", region);
            }
            if let Some(year) = year.as_deref() {
                query.append_pair("year", year);
            }
        } else if endpoint == "search/tv"
            && let Some(year) = year.as_deref()
        {
            query.append_pair("first_air_date_year", year);
        }
    }
    let response: SearchResponse = get_json(url).await?;
    Ok(response
        .results
        .into_iter()
        .take(MAX_RESULTS)
        .filter_map(|result| result.into_remote_result(item_type))
        .collect())
}

pub(crate) async fn details(
    item_type: &str,
    provider_id: &str,
    configuration: &Value,
) -> Result<Option<Value>, ApiError> {
    if provider_id.is_empty()
        || provider_id.len() > 20
        || !provider_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(None);
    }
    let entity = match item_type {
        "Movie" => "movie",
        "Series" => "tv",
        "Person" => "person",
        _ => return Ok(None),
    };
    let api_key = environment_api_key()?;
    let mut url = details_url(entity, provider_id)?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("api_key", api_key.as_str())
            .append_pair("append_to_response", "credits,external_ids");
        if let Some(language) = string_option(configuration, "PreferredLanguage") {
            query.append_pair("language", language);
        }
    }
    let details: Value = get_json(url).await?;
    Ok(tmdb_details_result(item_type, details, configuration))
}

fn bounded_search_name(name: Option<&str>) -> Option<&str> {
    name.map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control))
}

fn environment_api_key() -> Result<Zeroizing<String>, ApiError> {
    std::env::var("JELLYRIN_TMDB_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ApiError::service_unavailable("TheMovieDb API key is not configured"))
}

fn string_option<'a>(configuration: &'a Value, key: &str) -> Option<&'a str> {
    configuration
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bool_option(configuration: &Value, key: &str) -> bool {
    configuration
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn get_json<T: serde::de::DeserializeOwned>(url: Url) -> Result<T, ApiError> {
    validate_api_url(&url)?;
    let response = http_client()
        .await?
        .get(url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(request_error)?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "TheMovieDb request returned an error");
        return Err(ApiError::service_unavailable("TheMovieDb request failed"));
    }
    let body = read_bounded_body(response).await?;
    serde_json::from_slice(&body)
        .map_err(|_| ApiError::service_unavailable("TheMovieDb response is invalid"))
}

fn api_url(endpoint: &str) -> Result<Url, ApiError> {
    if !matches!(endpoint, "search/movie" | "search/tv" | "search/person") {
        return Err(ApiError::internal("TheMovieDb endpoint is invalid"));
    }
    let mut url = Url::parse(API_ORIGIN)
        .map_err(|_| ApiError::internal("TheMovieDb request URL is invalid"))?;
    url.set_path(&format!("/3/{endpoint}"));
    validate_api_url(&url)?;
    Ok(url)
}

fn details_url(entity: &str, provider_id: &str) -> Result<Url, ApiError> {
    if !matches!(entity, "movie" | "tv" | "person")
        || provider_id.is_empty()
        || !provider_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ApiError::internal("TheMovieDb details endpoint is invalid"));
    }
    let mut url = Url::parse(API_ORIGIN)
        .map_err(|_| ApiError::internal("TheMovieDb request URL is invalid"))?;
    url.set_path(&format!("/3/{entity}/{provider_id}"));
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
        || !url.path().starts_with("/3/")
    {
        return Err(ApiError::internal("TheMovieDb request URL is invalid"));
    }
    Ok(())
}

async fn http_client() -> Result<&'static Client, ApiError> {
    HTTP_CLIENT
        .get_or_try_init(|| async {
            let resolved =
                tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((API_HOST, API_PORT)))
                    .await
                    .map_err(|_| ApiError::service_unavailable("TheMovieDb DNS resolution failed"))?
                    .map_err(|_| {
                        ApiError::service_unavailable("TheMovieDb DNS resolution failed")
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
            "TheMovieDb address is not allowed",
        ));
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(2)
        .user_agent("Jellyrin/0.1.0 tmdb")
        .resolve_to_addrs(API_HOST, resolved)
        .build()
        .map_err(|_| ApiError::internal("TheMovieDb HTTP client initialization failed"))
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(ApiError::service_unavailable(
            "TheMovieDb response exceeds size limit",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(request_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(ApiError::service_unavailable(
                "TheMovieDb response exceeds size limit",
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
        "TheMovieDb request failed"
    );
    ApiError::service_unavailable("TheMovieDb request failed")
}

#[derive(Debug, Default, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    original_title: Option<String>,
    #[serde(default)]
    original_name: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    profile_path: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
}

impl SearchResult {
    fn into_remote_result(self, item_type: &str) -> Option<Value> {
        let name = self.title.or(self.name)?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let premiere_date = self
            .release_date
            .or(self.first_air_date)
            .filter(|date| !date.is_empty());
        let production_year = premiere_date
            .as_deref()
            .and_then(|date| date.get(0..4))
            .and_then(|year| year.parse::<i32>().ok());
        let image_url = self
            .poster_path
            .or(self.profile_path)
            .and_then(tmdb_image_url);
        Some(json!({
            "Name": name,
            "OriginalTitle": self.original_title.or(self.original_name),
            "ProviderIds": { "Tmdb": self.id.to_string() },
            "ProductionYear": production_year,
            "PremiereDate": premiere_date,
            "ImageUrl": image_url,
            "CommunityRating": self.vote_average,
            "SearchProviderName": "TheMovieDb",
            "Overview": self.overview.unwrap_or_default(),
            "Type": item_type
        }))
    }
}

fn tmdb_image_url(path: String) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') || path.len() > 512 || path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("{IMAGE_ORIGIN}{path}"))
}

fn tmdb_details_result(item_type: &str, details: Value, configuration: &Value) -> Option<Value> {
    let id = details.get("id")?.as_i64()?;
    let name = details
        .get("title")
        .or_else(|| details.get("name"))
        .and_then(Value::as_str)?
        .trim();
    if name.is_empty() {
        return None;
    }
    let original_title = details
        .get("original_title")
        .or_else(|| details.get("original_name"))
        .and_then(Value::as_str);
    let premiere_date = details
        .get("release_date")
        .or_else(|| details.get("first_air_date"))
        .or_else(|| details.get("birthday"))
        .and_then(Value::as_str)
        .filter(|date| !date.is_empty());
    let production_year = premiere_date
        .and_then(|date| date.get(0..4))
        .and_then(|year| year.parse::<i32>().ok());
    let image_url = details
        .get("poster_path")
        .or_else(|| details.get("profile_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .and_then(tmdb_image_url);
    let backdrop_image_url = details
        .get("backdrop_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .and_then(tmdb_image_url);
    let genres = named_values(details.get("genres"));
    let studios = named_object_values(
        details
            .get("production_companies")
            .or_else(|| details.get("networks")),
    );
    let mut provider_ids =
        serde_json::Map::from_iter([("Tmdb".to_string(), Value::String(id.to_string()))]);
    if let Some(imdb_id) = details
        .get("imdb_id")
        .or_else(|| details.pointer("/external_ids/imdb_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        provider_ids.insert("Imdb".to_string(), Value::String(imdb_id.to_string()));
    }
    if let Some(tvdb_id) = details
        .pointer("/external_ids/tvdb_id")
        .and_then(Value::as_i64)
    {
        provider_ids.insert("Tvdb".to_string(), Value::String(tvdb_id.to_string()));
    }
    let people = tmdb_people(
        &details,
        usize_option(configuration, "MaxCastMembers", 15),
        usize_option(configuration, "MaxCrewMembers", 15),
    );
    Some(json!({
        "Name": name,
        "OriginalTitle": original_title,
        "ProviderIds": provider_ids,
        "ProductionYear": production_year,
        "PremiereDate": premiere_date,
        "ImageUrl": image_url,
        "PrimaryImageUrl": image_url,
        "BackdropImageUrl": backdrop_image_url,
        "CommunityRating": details.get("vote_average").cloned().unwrap_or(Value::Null),
        "Overview": details.get("overview").or_else(|| details.get("biography")).and_then(Value::as_str).unwrap_or_default(),
        "Genres": genres,
        "Studios": studios,
        "People": people,
        "SearchProviderName": "TheMovieDb",
        "Type": item_type
    }))
}

fn named_values(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn named_object_values(value: Option<&Value>) -> Vec<Value> {
    named_values(value)
        .into_iter()
        .map(|name| json!({ "Name": name }))
        .collect()
}

fn usize_option(configuration: &Value, key: &str, default: usize) -> usize {
    configuration
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn tmdb_people(details: &Value, max_cast: usize, max_crew: usize) -> Vec<Value> {
    let mut people = details
        .pointer("/credits/cast")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(max_cast)
        .filter_map(|credit| {
            let name = credit.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(json!({
                "Name": name,
                "Role": credit.get("character").and_then(Value::as_str).unwrap_or_default(),
                "Type": "Actor",
                "ProviderIds": credit.get("id").and_then(Value::as_i64).map(|id| json!({ "Tmdb": id.to_string() })).unwrap_or_else(|| json!({}))
            }))
        })
        .collect::<Vec<_>>();
    people.extend(
        details
            .pointer("/credits/crew")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(max_crew)
            .filter_map(|credit| {
                let name = credit.get("name")?.as_str()?.trim();
                let job = credit.get("job")?.as_str()?.trim();
                if name.is_empty() || job.is_empty() {
                    return None;
                }
                Some(json!({
                    "Name": name,
                    "Role": job,
                    "Type": job,
                    "ProviderIds": credit.get("id").and_then(Value::as_i64).map(|id| json!({ "Tmdb": id.to_string() })).unwrap_or_else(|| json!({}))
                }))
            }),
    );
    people
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_fixture_maps_to_jellyfin_contract() {
        let response: SearchResponse = serde_json::from_value(json!({"results": [{
            "id": 550,
            "title": "Fight Club",
            "original_title": "Fight Club",
            "overview": "Overview",
            "release_date": "1999-10-15",
            "poster_path": "/poster.jpg",
            "vote_average": 8.4
        }]}))
        .unwrap();
        let result = response
            .results
            .into_iter()
            .next()
            .unwrap()
            .into_remote_result("Movie")
            .unwrap();
        assert_eq!(result["ProviderIds"]["Tmdb"], "550");
        assert_eq!(result["ProductionYear"], 1999);
        assert_eq!(
            result["ImageUrl"],
            "https://image.tmdb.org/t/p/original/poster.jpg"
        );
    }

    #[test]
    fn url_validation_rejects_foreign_hosts_and_non_search_paths() {
        assert!(api_url("search/movie").is_ok());
        assert!(api_url("movie/550").is_err());
        assert!(
            validate_api_url(&Url::parse("https://example.invalid/3/search/movie").unwrap())
                .is_err()
        );
    }

    #[test]
    fn image_paths_are_strictly_bounded() {
        assert!(tmdb_image_url("poster.jpg".to_string()).is_none());
        assert!(tmdb_image_url("/bad\0.jpg".to_string()).is_none());
    }

    #[test]
    fn details_fixture_maps_studios_people_images_and_external_ids() {
        let result = tmdb_details_result(
            "Movie",
            json!({
                "id": 550,
                "title": "Fight Club",
                "original_title": "Fight Club",
                "overview": "Overview",
                "release_date": "1999-10-15",
                "poster_path": "/poster.jpg",
                "backdrop_path": "/backdrop.jpg",
                "vote_average": 8.4,
                "imdb_id": "tt0137523",
                "genres": [{"name": "Drama"}],
                "production_companies": [{"name": "Fox 2000 Pictures"}],
                "credits": {
                    "cast": [{"id": 287, "name": "Brad Pitt", "character": "Tyler"}],
                    "crew": [{"id": 7467, "name": "David Fincher", "job": "Director"}]
                }
            }),
            &json!({ "MaxCastMembers": 1, "MaxCrewMembers": 1 }),
        )
        .unwrap();
        assert_eq!(result["ProviderIds"]["Imdb"], "tt0137523");
        assert_eq!(result["Studios"][0]["Name"], "Fox 2000 Pictures");
        assert_eq!(result["People"].as_array().unwrap().len(), 2);
        assert_eq!(
            result["BackdropImageUrl"],
            "https://image.tmdb.org/t/p/original/backdrop.jpg"
        );
    }
}
