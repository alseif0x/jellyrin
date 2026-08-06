//! Runtime discovery of the portal protocol version.
//!
//! MAGSTV retires old protocol versions independently of the APK package that
//! is installed locally. The public MarketServer update response advertises
//! the version accepted by the portal, so the provider can refresh this value
//! without sending account credentials or session material.

use reqwest::{Client, Url};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

use crate::{
    MAGSTV_APP_VERSION, MagstvProviderError, TransportFailureKind, build_magstv_http_client,
};

const MAGSTV_PACKAGE_NAME: &str = "com.android.mgstv";
const MAGSTV_DEFAULT_UPDATE_URL: &str = "https://iyut.xgw3sdzoac.com/MarketServer/update";
const MAGSTV_UPDATE_URL_ENV: &str = "MAGSTV_UPDATE_URL";
const MAGSTV_APP_VERSION_ENV: &str = "MAGSTV_APP_VERSION";
const VERSION_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_UPDATE_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
struct CachedVersion {
    value: String,
    expires_at: Instant,
}

static VERSION_CACHE: OnceLock<RwLock<Option<CachedVersion>>> = OnceLock::new();
static VERSION_CLIENT: OnceLock<Result<Client, ()>> = OnceLock::new();

fn version_cache() -> &'static RwLock<Option<CachedVersion>> {
    VERSION_CACHE.get_or_init(|| RwLock::new(None))
}

fn version_client() -> Result<&'static Client, MagstvProviderError> {
    VERSION_CLIENT
        .get_or_init(|| build_magstv_http_client(Duration::from_secs(20)).map_err(|_| ()))
        .as_ref()
        .map_err(|_| MagstvProviderError::Transport(TransportFailureKind::Unavailable))
}

/// Returns the current protocol version, using the public update endpoint.
///
/// A deployment may override the update endpoint or pin a version through
/// environment variables for an offline/test installation. The response is
/// cached briefly to avoid hitting the update service on every catalogue
/// refresh.
pub async fn discover_app_version() -> Result<String, MagstvProviderError> {
    if let Some(version) = runtime_version_override()? {
        return Ok(version);
    }

    if let Some(version) = version_cache()
        .read()
        .await
        .as_ref()
        .filter(|cached| cached.expires_at > Instant::now())
        .map(|cached| cached.value.clone())
    {
        return Ok(version);
    }

    let update_url = std::env::var(MAGSTV_UPDATE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| MAGSTV_DEFAULT_UPDATE_URL.to_string());
    let mut url = Url::parse(update_url.trim())
        .map_err(|_| MagstvProviderError::InvalidRuntimeConfiguration)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(MagstvProviderError::InvalidRuntimeConfiguration);
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("action", "checkUpdate");
        query.append_pair(
            "packagenamesAndVersioncodes",
            &format!("{MAGSTV_PACKAGE_NAME},{MAGSTV_APP_VERSION}"),
        );
        query.append_pair("language", "es");
    }

    let response = version_client()?
        .get(url)
        .send()
        .await
        .map_err(map_version_transport_error)?;
    if response.status().is_redirection() {
        return Err(MagstvProviderError::Transport(
            TransportFailureKind::RedirectRejected,
        ));
    }
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(MagstvProviderError::Transport(
            TransportFailureKind::HttpStatus(status),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_UPDATE_RESPONSE_BYTES as u64)
    {
        return Err(MagstvProviderError::ResponseTooLarge);
    }
    let body = response
        .bytes()
        .await
        .map_err(map_version_transport_error)?;
    if body.len() > MAX_UPDATE_RESPONSE_BYTES {
        return Err(MagstvProviderError::ResponseTooLarge);
    }
    let version = parse_update_version(&body)?;
    version_cache().write().await.replace(CachedVersion {
        value: version.clone(),
        expires_at: Instant::now() + VERSION_CACHE_TTL,
    });
    Ok(version)
}

fn runtime_version_override() -> Result<Option<String>, MagstvProviderError> {
    let Some(value) = std::env::var(MAGSTV_APP_VERSION_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(validate_version(value.trim())?))
}

fn parse_update_version(body: &[u8]) -> Result<String, MagstvProviderError> {
    let body =
        std::str::from_utf8(body).map_err(|_| MagstvProviderError::InvalidRuntimeConfiguration)?;
    let start_tag = "<versionCode>";
    let end_tag = "</versionCode>";
    let Some(start) = body.find(start_tag).map(|offset| offset + start_tag.len()) else {
        // MarketServer returns rows="0" when the queried version is already
        // current. In that case the queried fallback is the accepted version.
        if body.contains("<list rows=\"0\"") {
            return Ok(MAGSTV_APP_VERSION.to_string());
        }
        return Err(MagstvProviderError::InvalidRuntimeConfiguration);
    };
    let end = body[start..]
        .find(end_tag)
        .map(|offset| start + offset)
        .ok_or(MagstvProviderError::InvalidRuntimeConfiguration)?;
    validate_version(body[start..end].trim())
}

fn validate_version(value: &str) -> Result<String, MagstvProviderError> {
    if value.is_empty() || value.len() > 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MagstvProviderError::InvalidRuntimeConfiguration);
    }
    Ok(value.to_string())
}

fn map_version_transport_error(error: reqwest::Error) -> MagstvProviderError {
    let kind = if error.is_timeout() {
        TransportFailureKind::Timeout
    } else if error.is_connect() {
        TransportFailureKind::Dns
    } else {
        TransportFailureKind::Unavailable
    };
    MagstvProviderError::Transport(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_xml_extracts_numeric_version_only() {
        assert_eq!(
            parse_update_version(
                br#"<?xml version=\"1.0\"?><App><versionCode>49905</versionCode></App>"#
            )
            .unwrap(),
            "49905"
        );
    }

    #[test]
    fn update_xml_rejects_missing_or_non_numeric_version() {
        assert!(parse_update_version(br#"<App></App>"#).is_err());
        assert!(parse_update_version(br#"<versionCode>49905x</versionCode>"#).is_err());
    }

    #[test]
    fn update_xml_accepts_no_update_as_the_current_fallback() {
        assert_eq!(
            parse_update_version(br#"<AppInfo><list rows="0"/></AppInfo>"#).unwrap(),
            MAGSTV_APP_VERSION
        );
    }
}
