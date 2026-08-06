use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{
    Client, Proxy, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};
use std::{fmt, time::Duration};

use crate::{MagstvProviderError, TransportFailureKind, VerifiedWireRequest};

/// The MAGSTV sidecar owns the Mexico WireGuard tunnel. Keeping the default
/// local makes the egress boundary explicit and prevents a provider request
/// from silently falling back to Jellyrin's ordinary host network.
pub const MAGSTV_EGRESS_PROXY_ENV: &str = "MAGSTV_EGRESS_PROXY";
pub const MAGSTV_DEFAULT_EGRESS_PROXY: &str = "http://127.0.0.1:18080";
/// Explicit operator opt-out of the local sidecar: the process then relies on
/// the host already routing through the authorised Mexican egress, which is
/// how the TV client reaches the service as well.
pub const MAGSTV_DIRECT_EGRESS_VALUE: &str = "direct";

/// Raw transport responses hide their body from Debug to keep decrypted or
/// token-bearing protocol material out of logs.
#[derive(Clone, PartialEq, Eq)]
pub struct PortalTransportResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for PortalTransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PortalTransportResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .finish()
    }
}

#[async_trait]
pub trait MagstvTransport: Send + Sync {
    async fn exchange(
        &self,
        bootstrap_url: &str,
        request: &VerifiedWireRequest,
    ) -> Result<PortalTransportResponse, MagstvProviderError>;
}

/// Bounded HTTPS transport for a verified portal codec.
///
/// The transport is codec-agnostic: it posts only an already verified
/// `VerifiedWireRequest`. Adding a real HTTP client therefore does not make
/// guessed plaintext requests to the service.
#[derive(Clone)]
pub struct ReqwestMagstvTransport {
    client: Client,
    max_response_bytes: usize,
}

impl fmt::Debug for ReqwestMagstvTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestMagstvTransport")
            .field("max_response_bytes", &self.max_response_bytes)
            .field("client", &"[CONFIGURED]")
            .finish()
    }
}

impl ReqwestMagstvTransport {
    pub fn new() -> Result<Self, MagstvProviderError> {
        let client = build_magstv_http_client(Duration::from_secs(30))?;
        Ok(Self {
            client,
            max_response_bytes: crate::MAX_PORTAL_RESPONSE_BYTES,
        })
    }

    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            max_response_bytes: crate::MAX_PORTAL_RESPONSE_BYTES,
        }
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }
}

#[async_trait]
impl MagstvTransport for ReqwestMagstvTransport {
    async fn exchange(
        &self,
        bootstrap_url: &str,
        request: &VerifiedWireRequest,
    ) -> Result<PortalTransportResponse, MagstvProviderError> {
        let url = portal_url(bootstrap_url, request.relative_path())?;
        let mut headers = HeaderMap::new();
        for (name, value) in request.headers() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| MagstvProviderError::InvalidHeader)?;
            let value = HeaderValue::try_from(value.as_str())
                .map_err(|_| MagstvProviderError::InvalidHeader)?;
            headers.insert(name, value);
        }
        let response = self
            .client
            .post(url)
            .headers(headers)
            .header(CONTENT_TYPE, request.content_type())
            .body(request.body().to_vec())
            .send()
            .await
            .map_err(map_reqwest_error)?;
        read_bounded_response(response, self.max_response_bytes).await
    }
}

/// Bounded HTTPS reader for the captured public EPG GET. The `md5` value is
/// supplied by the caller because its native derivation is still unverified.
/// This helper returns non-2xx responses (for example the observed 403) so the
/// caller can distinguish an unavailable public feed from a transport error.
#[derive(Clone)]
pub struct ReqwestEpgTransport {
    client: Client,
    max_response_bytes: usize,
}

impl fmt::Debug for ReqwestEpgTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestEpgTransport")
            .field("max_response_bytes", &self.max_response_bytes)
            .field("client", &"[CONFIGURED]")
            .finish()
    }
}

impl ReqwestEpgTransport {
    pub fn new() -> Result<Self, MagstvProviderError> {
        let client = build_magstv_http_client(Duration::from_secs(30))?;
        Ok(Self {
            client,
            max_response_bytes: crate::MAX_PORTAL_RESPONSE_BYTES,
        })
    }

    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            max_response_bytes: crate::MAX_PORTAL_RESPONSE_BYTES,
        }
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub async fn get(
        &self,
        base_url: &str,
        epg_path: &str,
        md5: &str,
    ) -> Result<PortalTransportResponse, MagstvProviderError> {
        let url = crate::build_epg_url(base_url, epg_path, md5)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        read_bounded_response(response, self.max_response_bytes).await
    }
}

/// Build the HTTP client used for MAGSTV control-plane traffic.
///
/// MAGSTV requires the same Mexican egress used by the Android application.
/// The VPN itself is deliberately external to this crate: a local sidecar
/// brings up WireGuard and exposes an HTTP CONNECT proxy. The proxy URL can be
/// changed for container deployments, but an explicit proxy is always used.
pub fn build_magstv_http_client(timeout: Duration) -> Result<Client, MagstvProviderError> {
    let builder = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout);
    let builder = match configured_magstv_egress_proxy()? {
        Some(proxy_url) => {
            let proxy = Proxy::all(proxy_url.as_str())
                .map_err(|_| MagstvProviderError::InvalidRuntimeConfiguration)?;
            builder.proxy(proxy)
        }
        None => builder,
    };
    builder
        .build()
        .map_err(|_| MagstvProviderError::Transport(TransportFailureKind::Unavailable))
}

/// Resolve and validate the egress proxy without exposing its value in
/// diagnostics. Credentials in a proxy URL are rejected so a secret cannot
/// accidentally enter process arguments, traces, or a Debug representation.
/// Returns `None` when the operator explicitly selects the host's own MX
/// route with [`MAGSTV_DIRECT_EGRESS_VALUE`].
pub fn configured_magstv_egress_proxy() -> Result<Option<Url>, MagstvProviderError> {
    let value = std::env::var(MAGSTV_EGRESS_PROXY_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| MAGSTV_DEFAULT_EGRESS_PROXY.to_string());
    resolve_magstv_egress_proxy(&value)
}

fn resolve_magstv_egress_proxy(value: &str) -> Result<Option<Url>, MagstvProviderError> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case(MAGSTV_DIRECT_EGRESS_VALUE)
        || trimmed.eq_ignore_ascii_case("none")
    {
        return Ok(None);
    }
    parse_magstv_egress_proxy(trimmed).map(Some)
}

fn parse_magstv_egress_proxy(value: &str) -> Result<Url, MagstvProviderError> {
    let url =
        Url::parse(value.trim()).map_err(|_| MagstvProviderError::InvalidRuntimeConfiguration)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().is_empty() && url.path() != "/"
    {
        return Err(MagstvProviderError::InvalidRuntimeConfiguration);
    }
    Ok(url)
}

async fn read_bounded_response(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<PortalTransportResponse, MagstvProviderError> {
    if response.status().is_redirection() {
        return Err(MagstvProviderError::Transport(
            TransportFailureKind::RedirectRejected,
        ));
    }
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(MagstvProviderError::ResponseTooLarge);
    }

    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        if body.len().saturating_add(chunk.len()) > max_response_bytes {
            return Err(MagstvProviderError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(PortalTransportResponse {
        status,
        content_type,
        body,
    })
}

fn portal_url(bootstrap_url: &str, relative_path: &str) -> Result<Url, MagstvProviderError> {
    let base =
        Url::parse(bootstrap_url.trim()).map_err(|_| MagstvProviderError::InvalidBootstrapUrl)?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(MagstvProviderError::InvalidBootstrapUrl);
    }
    if !relative_path.starts_with('/')
        || relative_path.starts_with("//")
        || relative_path.contains("://")
        || relative_path.split('/').any(|segment| segment == "..")
        || relative_path.chars().any(char::is_control)
    {
        return Err(MagstvProviderError::InvalidEncodedEndpoint);
    }
    base.join(relative_path)
        .map_err(|_| MagstvProviderError::InvalidEncodedEndpoint)
}

fn map_reqwest_error(error: reqwest::Error) -> MagstvProviderError {
    let kind = if error.is_timeout() {
        TransportFailureKind::Timeout
    } else if error.is_connect() {
        TransportFailureKind::Dns
    } else {
        TransportFailureKind::Unavailable
    };
    MagstvProviderError::Transport(kind)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DenyNetworkTransport;

#[async_trait]
impl MagstvTransport for DenyNetworkTransport {
    async fn exchange(
        &self,
        _bootstrap_url: &str,
        _request: &VerifiedWireRequest,
    ) -> Result<PortalTransportResponse, MagstvProviderError> {
        Err(MagstvProviderError::Transport(
            TransportFailureKind::Unavailable,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_happens_before_network() {
        assert_eq!(
            portal_url("https://portal.example.invalid", "/api/../secret"),
            Err(MagstvProviderError::InvalidEncodedEndpoint)
        );
        assert_eq!(
            portal_url("http://portal.example.invalid", "/api"),
            Err(MagstvProviderError::InvalidBootstrapUrl)
        );
        assert_eq!(
            portal_url("https://portal.example.invalid?token=secret", "/api"),
            Err(MagstvProviderError::InvalidBootstrapUrl)
        );
    }

    #[test]
    fn transport_debug_does_not_include_client_details() {
        let transport = ReqwestMagstvTransport::new().expect("reqwest client");
        let debug = format!("{transport:?}");
        assert!(debug.contains("[CONFIGURED]"));
        assert!(!debug.contains("example.invalid"));

        let epg = ReqwestEpgTransport::new().expect("EPG reqwest client");
        assert!(format!("{epg:?}").contains("[CONFIGURED]"));
    }

    #[test]
    fn egress_proxy_defaults_to_local_sidecar() {
        let proxy = configured_magstv_egress_proxy().expect("default proxy");
        let proxy = proxy.expect("sidecar proxy by default");
        assert_eq!(proxy.scheme(), "http");
        assert_eq!(proxy.host_str(), Some("127.0.0.1"));
        assert_eq!(proxy.port(), Some(18080));
    }

    #[test]
    fn egress_proxy_direct_opt_out_is_explicit() {
        assert_eq!(resolve_magstv_egress_proxy(MAGSTV_DIRECT_EGRESS_VALUE), Ok(None));
        assert_eq!(resolve_magstv_egress_proxy("none"), Ok(None));
        assert_eq!(resolve_magstv_egress_proxy(" DIRECT "), Ok(None));
        assert!(matches!(
            resolve_magstv_egress_proxy("http://127.0.0.1:9999"),
            Ok(Some(_))
        ));
    }

    #[test]
    fn egress_proxy_rejects_credentials_and_non_http_schemes() {
        for value in [
            "http://user:password@127.0.0.1:18080",
            "socks5://127.0.0.1:18080",
            "http://127.0.0.1:18080/path",
        ] {
            assert_eq!(
                parse_magstv_egress_proxy(value),
                Err(MagstvProviderError::InvalidRuntimeConfiguration)
            );
        }
    }
}
