use std::{collections::HashMap, net::SocketAddr, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, Url, header};
use tokio::sync::{OnceCell, RwLock};

use crate::{ApiError, remote_provider_address_allowed};

const REPOSITORY_ORIGIN: &str =
    "https://raw.githubusercontent.com/jellyfin/emby-artwork/master/studios/";
const REPOSITORY_HOST: &str = "raw.githubusercontent.com";
const REPOSITORY_PORT: u16 = 443;
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const LIST_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const LIST_MAX_BYTES: usize = 2 * 1024 * 1024;
const LIST_MAX_ENTRIES: usize = 50_000;
const ENTRY_MAX_BYTES: usize = 256;

static HTTP_CLIENT: OnceCell<Client> = OnceCell::const_new();
static LIST_CACHE: RwLock<Option<StudioListCache>> = RwLock::const_new(None);

struct StudioListCache {
    loaded_at: tokio::time::Instant,
    entries: HashMap<String, String>,
}

pub(crate) async fn image_url_for_studio(name: &str) -> Result<Option<String>, ApiError> {
    let comparable = comparable_name(name);
    if comparable.is_empty() {
        return Ok(None);
    }
    {
        let cache = LIST_CACHE.read().await;
        if let Some(cache) = cache.as_ref()
            && cache.loaded_at.elapsed() < LIST_CACHE_TTL
        {
            return cache
                .entries
                .get(&comparable)
                .map(|image_name| image_url(image_name))
                .transpose();
        }
    }
    let entries = fetch_studio_list().await?;
    let image_name = entries.get(&comparable).cloned();
    *LIST_CACHE.write().await = Some(StudioListCache {
        loaded_at: tokio::time::Instant::now(),
        entries,
    });
    image_name.as_deref().map(image_url).transpose()
}

async fn fetch_studio_list() -> Result<HashMap<String, String>, ApiError> {
    let url = repository_url(&["thumbs.txt"])?;
    let response = http_client()
        .await?
        .get(url)
        .header(header::ACCEPT, "text/plain")
        .send()
        .await
        .map_err(request_error)?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "Studio Images list request returned an error");
        return Err(ApiError::service_unavailable(
            "Studio Images list is unavailable",
        ));
    }
    let body = read_bounded_body(response).await?;
    decode_studio_list(&body)
}

fn decode_studio_list(body: &[u8]) -> Result<HashMap<String, String>, ApiError> {
    let body = std::str::from_utf8(body)
        .map_err(|_| ApiError::service_unavailable("Studio Images list is invalid"))?;
    let mut entries = HashMap::new();
    for line in body.lines() {
        let image_name = line.trim();
        if image_name.is_empty() {
            continue;
        }
        if image_name.len() > ENTRY_MAX_BYTES
            || image_name.chars().any(char::is_control)
            || image_name == "."
            || image_name == ".."
        {
            return Err(ApiError::service_unavailable(
                "Studio Images list contains an invalid entry",
            ));
        }
        entries
            .entry(comparable_name(image_name))
            .or_insert_with(|| image_name.to_string());
        if entries.len() > LIST_MAX_ENTRIES {
            return Err(ApiError::service_unavailable(
                "Studio Images list exceeds item limit",
            ));
        }
    }
    Ok(entries)
}

fn comparable_name(name: &str) -> String {
    name.chars()
        .filter(|character| {
            !character.is_whitespace()
                && !matches!(character, '.' | '&' | '!' | ',' | '/' | '\\')
                && !character.is_control()
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn image_url(image_name: &str) -> Result<String, ApiError> {
    repository_url(&["images", image_name, "thumb.jpg"]).map(Url::into)
}

fn repository_url(segments: &[&str]) -> Result<Url, ApiError> {
    let mut url = Url::parse(REPOSITORY_ORIGIN)
        .map_err(|_| ApiError::internal("Studio Images URL is invalid"))?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| ApiError::internal("Studio Images URL is invalid"))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    validate_repository_url(&url)?;
    Ok(url)
}

fn validate_repository_url(url: &Url) -> Result<(), ApiError> {
    if url.scheme() != "https"
        || url.host_str() != Some(REPOSITORY_HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !url
            .path()
            .starts_with("/jellyfin/emby-artwork/master/studios/")
    {
        return Err(ApiError::internal("Studio Images URL is invalid"));
    }
    Ok(())
}

async fn http_client() -> Result<&'static Client, ApiError> {
    HTTP_CLIENT
        .get_or_try_init(|| async {
            let resolved = tokio::time::timeout(
                DNS_TIMEOUT,
                tokio::net::lookup_host((REPOSITORY_HOST, REPOSITORY_PORT)),
            )
            .await
            .map_err(|_| ApiError::service_unavailable("Studio Images DNS resolution failed"))?
            .map_err(|_| ApiError::service_unavailable("Studio Images DNS resolution failed"))?;
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
            address.port() != REPOSITORY_PORT
                || !remote_provider_address_allowed(address.ip(), false)
        })
    {
        return Err(ApiError::service_unavailable(
            "Studio Images address is not allowed",
        ));
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(1)
        .user_agent("Jellyrin/0.1.0 studio-images")
        .resolve_to_addrs(REPOSITORY_HOST, resolved)
        .build()
        .map_err(|_| ApiError::internal("Studio Images HTTP client initialization failed"))
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > LIST_MAX_BYTES as u64)
    {
        return Err(ApiError::service_unavailable(
            "Studio Images list exceeds size limit",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(request_error)?;
        if body.len().saturating_add(chunk.len()) > LIST_MAX_BYTES {
            return Err(ApiError::service_unavailable(
                "Studio Images list exceeds size limit",
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
        "Studio Images request failed"
    );
    ApiError::service_unavailable("Studio Images request failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparable_names_match_jellyfin_classic_behavior() {
        assert_eq!(comparable_name("A&E Studios, Inc."), "aestudiosinc");
        assert_eq!(comparable_name("A / E Studios!"), "aestudios");
    }

    #[test]
    fn repository_url_encodes_studio_name_as_one_path_segment() {
        let url = image_url("A/B Studios").unwrap();
        assert_eq!(
            url,
            "https://raw.githubusercontent.com/jellyfin/emby-artwork/master/studios/images/A%2FB%20Studios/thumb.jpg"
        );
    }

    #[test]
    fn list_decoder_rejects_unbounded_or_control_entries() {
        assert!(decode_studio_list(b"HBO\nNetflix\n").is_ok());
        assert!(decode_studio_list(b"HBO\nBad\0Name\n").is_err());
        let oversized = format!("{}\n", "a".repeat(ENTRY_MAX_BYTES + 1));
        assert!(decode_studio_list(oversized.as_bytes()).is_err());
    }

    #[test]
    fn repository_validation_rejects_foreign_or_unsafe_urls() {
        assert!(repository_url(&["thumbs.txt"]).is_ok());
        for url in [
            "http://raw.githubusercontent.com/jellyfin/emby-artwork/master/studios/thumbs.txt",
            "https://example.invalid/jellyfin/emby-artwork/master/studios/thumbs.txt",
            "https://raw.githubusercontent.com:444/jellyfin/emby-artwork/master/studios/thumbs.txt",
            "https://user:pass@raw.githubusercontent.com/jellyfin/emby-artwork/master/studios/thumbs.txt",
        ] {
            assert!(validate_repository_url(&Url::parse(url).unwrap()).is_err());
        }
    }
}
