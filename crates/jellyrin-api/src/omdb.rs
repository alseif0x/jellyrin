use std::{net::SocketAddr, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, Url, header};
use serde::Deserialize;
use serde_json::{Value, json};
use time::{Date, Month};
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::{ApiError, remote_provider_address_allowed};

const API_ORIGIN: &str = "https://www.omdbapi.com/";
const API_HOST: &str = "www.omdbapi.com";
const API_PORT: u16 = 443;
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY_BYTES: usize = 1024 * 1024;

static HTTP_CLIENT: OnceCell<Client> = OnceCell::const_new();

pub(crate) async fn remote_search(
    item_type: &str,
    name: Option<&str>,
    year: Option<i32>,
    configuration: &Value,
) -> Result<Vec<Value>, ApiError> {
    let Some(name) = name.map(str::trim).filter(|name| {
        !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
    }) else {
        return Ok(Vec::new());
    };
    let media_type = match item_type {
        "Movie" => "movie",
        "Series" => "series",
        "Episode" => "episode",
        _ => return Ok(Vec::new()),
    };
    let api_key = environment_api_key()?;
    let mut url = api_url()?;
    {
        let year = year.map(|year| year.to_string());
        let mut query = url.query_pairs_mut();
        query
            .append_pair("apikey", api_key.as_str())
            .append_pair("t", name)
            .append_pair("type", media_type)
            .append_pair("plot", "full")
            .append_pair("r", "json");
        if let Some(year) = year.as_deref() {
            query.append_pair("y", year);
        }
    }
    let result: OmdbTitle = get_json(url).await?;
    if !result.is_success() {
        return Ok(Vec::new());
    }
    Ok(vec![result.into_remote_result(
        item_type,
        bool_option(configuration, "CastAndCrew"),
    )])
}

fn environment_api_key() -> Result<Zeroizing<String>, ApiError> {
    std::env::var("JELLYRIN_OMDB_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ApiError::service_unavailable("OMDb API key is not configured"))
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
        tracing::warn!(status = %response.status(), "OMDb request returned an error");
        return Err(ApiError::service_unavailable("OMDb request failed"));
    }
    let body = read_bounded_body(response).await?;
    serde_json::from_slice(&body)
        .map_err(|_| ApiError::service_unavailable("OMDb response is invalid"))
}

fn api_url() -> Result<Url, ApiError> {
    let url =
        Url::parse(API_ORIGIN).map_err(|_| ApiError::internal("OMDb request URL is invalid"))?;
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
        || url.path() != "/"
    {
        return Err(ApiError::internal("OMDb request URL is invalid"));
    }
    Ok(())
}

async fn http_client() -> Result<&'static Client, ApiError> {
    HTTP_CLIENT
        .get_or_try_init(|| async {
            let resolved =
                tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((API_HOST, API_PORT)))
                    .await
                    .map_err(|_| ApiError::service_unavailable("OMDb DNS resolution failed"))?
                    .map_err(|_| ApiError::service_unavailable("OMDb DNS resolution failed"))?;
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
        return Err(ApiError::service_unavailable("OMDb address is not allowed"));
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(2)
        .user_agent("Jellyrin/0.1.0 omdb")
        .resolve_to_addrs(API_HOST, resolved)
        .build()
        .map_err(|_| ApiError::internal("OMDb HTTP client initialization failed"))
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(ApiError::service_unavailable(
            "OMDb response exceeds size limit",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(request_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(ApiError::service_unavailable(
                "OMDb response exceeds size limit",
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
        "OMDb request failed"
    );
    ApiError::service_unavailable("OMDb request failed")
}

fn optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "N/A")
}

fn split_list(value: Option<String>) -> Vec<String> {
    optional_text(value)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct OmdbTitle {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    released: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    genre: Option<String>,
    #[serde(default)]
    director: Option<String>,
    #[serde(default)]
    writer: Option<String>,
    #[serde(default)]
    actors: Option<String>,
    #[serde(default)]
    plot: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    awards: Option<String>,
    #[serde(default)]
    poster: Option<String>,
    #[serde(default)]
    metascore: Option<String>,
    #[serde(default)]
    #[serde(rename = "imdbRating")]
    imdb_rating: Option<String>,
    #[serde(default)]
    #[serde(rename = "imdbVotes")]
    imdb_votes: Option<String>,
    #[serde(default)]
    #[serde(rename = "imdbID")]
    imdb_id: Option<String>,
    #[serde(default)]
    rated: Option<String>,
    #[serde(default)]
    response: Option<String>,
}

impl OmdbTitle {
    fn is_success(&self) -> bool {
        self.response
            .as_deref()
            .is_none_or(|value| value.eq_ignore_ascii_case("true"))
            && optional_text(self.title.clone()).is_some()
    }

    fn into_remote_result(self, item_type: &str, cast_and_crew: bool) -> Value {
        let year = optional_text(self.year)
            .and_then(|value| value.get(0..4).and_then(|year| year.parse::<i32>().ok()));
        let poster = optional_text(self.poster).filter(|url| url.starts_with("https://"));
        let provider_ids = optional_text(self.imdb_id)
            .map(|id| json!({ "Imdb": id }))
            .unwrap_or_else(|| json!({}));
        let runtime_ticks = optional_text(self.runtime.clone())
            .and_then(|runtime| {
                runtime
                    .strip_suffix(" min")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .and_then(|minutes| minutes.parse::<i64>().ok())
            .and_then(|minutes| minutes.checked_mul(60)?.checked_mul(10_000_000));
        let mut people = Vec::new();
        if cast_and_crew {
            people.extend(
                split_list(self.actors)
                    .into_iter()
                    .map(|name| json!({"Name": name, "Type": "Actor"})),
            );
            people.extend(
                split_list(self.director)
                    .into_iter()
                    .map(|name| json!({"Name": name, "Type": "Director"})),
            );
            people.extend(
                split_list(self.writer)
                    .into_iter()
                    .map(|name| json!({"Name": name, "Type": "Writer"})),
            );
        }
        json!({
            "Name": optional_text(self.title).unwrap_or_default(),
            "ProviderIds": provider_ids,
            "ProductionYear": year,
            "PremiereDate": optional_text(self.released).and_then(|date| omdb_date_to_utc(&date)),
            "ImageUrl": poster,
            "Overview": optional_text(self.plot).unwrap_or_default(),
            "OfficialRating": optional_text(self.rated),
            "CommunityRating": optional_text(self.imdb_rating).and_then(|value| value.parse::<f64>().ok()),
            "CriticRating": optional_text(self.metascore).and_then(|value| value.parse::<f64>().ok()),
            "Genres": split_list(self.genre),
            "People": people,
            "Languages": split_list(self.language),
            "Countries": split_list(self.country),
            "Awards": optional_text(self.awards),
            "RunTimeTicks": runtime_ticks,
            "VoteCount": optional_text(self.imdb_votes),
            "SearchProviderName": "The Open Movie Database",
            "Type": item_type
        })
    }
}

fn omdb_date_to_utc(value: &str) -> Option<String> {
    let mut parts = value.split_whitespace();
    let day = parts.next()?.parse::<u8>().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parts.next()?.parse::<i32>().ok()?;
    if parts.next().is_some() || !(1..=9999).contains(&year) {
        return None;
    }
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    Some(format!("{year:04}-{month:02}-{day:02}T00:00:00.0000000Z"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_fixture_maps_and_filters_na_values() {
        let title: OmdbTitle = serde_json::from_value(json!({
            "Title": "Blade Runner",
            "Year": "1982",
            "Released": "25 Jun 1982",
            "Genre": "Action, Drama",
            "Actors": "Harrison Ford, Rutger Hauer",
            "Director": "Ridley Scott",
            "Writer": "N/A",
            "Plot": "A plot",
            "Poster": "https://m.media-amazon.com/poster.jpg",
            "imdbRating": "8.1",
            "imdbID": "tt0083658",
            "Response": "True"
        }))
        .unwrap();
        let result = title.into_remote_result("Movie", true);
        assert_eq!(result["ProviderIds"]["Imdb"], "tt0083658");
        assert_eq!(result["ProductionYear"], 1982);
        assert_eq!(result["Genres"].as_array().unwrap().len(), 2);
        assert_eq!(result["People"].as_array().unwrap().len(), 3);
        assert_eq!(result["PremiereDate"], "1982-06-25T00:00:00.0000000Z");
    }

    #[test]
    fn unsuccessful_response_is_not_a_result() {
        let title: OmdbTitle = serde_json::from_value(json!({
            "Response": "False",
            "Error": "Movie not found!"
        }))
        .unwrap();
        assert!(!title.is_success());
    }

    #[test]
    fn url_validation_rejects_foreign_hosts_and_paths() {
        assert!(api_url().is_ok());
        assert!(validate_api_url(&Url::parse("https://example.invalid/").unwrap()).is_err());
        assert!(validate_api_url(&Url::parse("https://www.omdbapi.com/private").unwrap()).is_err());
    }
}
