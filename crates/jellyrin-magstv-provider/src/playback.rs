use async_trait::async_trait;
use rand_core::{OsRng, RngCore};
use std::{collections::BTreeMap, fmt, net::IpAddr};
use time::OffsetDateTime;
use url::Url;

use crate::{
    MAGSTV_APP_ID, MAGSTV_PLAYBACK_APP_VERSION, MAGSTV_SIGN2_METHOD, MagstvProviderError,
    MagstvSession,
};

pub const MAGSTV_SIGN_O3_SECRET_HEX_ENV: &str = "JELLYRIN_MAGSTV_SIGN_O3_SECRET_HEX";
const SIGN_O3_SECRET_BYTES: usize = 21;
/// CDN URLs are generated just in time and expire about four hours later,
/// matching the lifetime observed in authorised client negotiations.
pub const MAGSTV_PLAYBACK_URL_TTL_SECONDS: i64 = 4 * 60 * 60;
/// The CDN only binds `instance` through the `sign2` digest, so a fixed value
/// is sufficient for the server-side implementation.
pub const MAGSTV_PLAYBACK_DEFAULT_INSTANCE: &str = "0";

/// All unsigned fields needed by the native `sign_o3` boundary. Keeping this
/// as a typed request makes the eventual native/oracle implementation a
/// replaceable component instead of spreading URL concatenation through API
/// handlers.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvPlaybackRequest {
    pub dev_id: String,
    pub user_id: String,
    pub trans_id: String,
    pub expired: OffsetDateTime,
    pub host: String,
    pub media_code: String,
    pub auth_id: String,
    pub client_ip: IpAddr,
    pub token: String,
    pub instance: String,
    pub start_moment: String,
}

impl fmt::Debug for MagstvPlaybackRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvPlaybackRequest")
            .field("dev_id", &self.dev_id)
            .field("user_id", &"[REDACTED]")
            .field("trans_id", &self.trans_id)
            .field("expired", &self.expired)
            .field("host", &self.host)
            .field("media_code", &self.media_code)
            .field("auth_id", &"[REDACTED]")
            .field("client_ip", &self.client_ip)
            .field("token", &"[REDACTED]")
            .field("instance", &self.instance)
            .field("start_moment", &self.start_moment)
            .finish()
    }
}

impl MagstvPlaybackRequest {
    pub fn validate(&self) -> Result<(), MagstvProviderError> {
        for (field, value) in [
            ("dev_id", self.dev_id.as_str()),
            ("user_id", self.user_id.as_str()),
            ("trans_id", self.trans_id.as_str()),
            ("host", self.host.as_str()),
            ("media_code", self.media_code.as_str()),
            ("auth_id", self.auth_id.as_str()),
            ("token", self.token.as_str()),
            ("instance", self.instance.as_str()),
            ("start_moment", self.start_moment.as_str()),
        ] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(MagstvProviderError::InvalidPlaybackParameter { field });
            }
        }
        if self.media_code.contains('/') || self.media_code.contains('\\') {
            return Err(MagstvProviderError::InvalidPlaybackParameter {
                field: "media_code",
            });
        }
        if self.host.contains('/') || self.host.contains('?') || self.host.contains('#') {
            return Err(MagstvProviderError::InvalidPlaybackParameter { field: "host" });
        }
        let host_url = Url::parse(&format!("https://{}/", self.host))
            .map_err(|_| MagstvProviderError::InvalidPlaybackParameter { field: "host" })?;
        if host_url.host_str().is_none()
            || !host_url.username().is_empty()
            || host_url.password().is_some()
        {
            return Err(MagstvProviderError::InvalidPlaybackParameter { field: "host" });
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MagstvSignedPlaybackUrl {
    pub dev_id: String,
    pub user_id: String,
    pub trans_id: String,
    pub expired: OffsetDateTime,
    pub host: String,
    pub media_code: String,
    pub auth_id: String,
    pub client_ip: IpAddr,
    pub token: String,
    pub instance: String,
    pub start_moment: String,
    pub sign2: String,
}

impl fmt::Debug for MagstvSignedPlaybackUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvSignedPlaybackUrl")
            .field("dev_id", &self.dev_id)
            .field("user_id", &"[REDACTED]")
            .field("trans_id", &self.trans_id)
            .field("expired", &self.expired)
            .field("host", &self.host)
            .field("media_code", &self.media_code)
            .field("auth_id", &"[REDACTED]")
            .field("client_ip", &self.client_ip)
            .field("token", &"[REDACTED]")
            .field("instance", &self.instance)
            .field("start_moment", &self.start_moment)
            .field("sign2", &"[REDACTED]")
            .finish()
    }
}

impl MagstvSignedPlaybackUrl {
    pub fn from_request(
        request: MagstvPlaybackRequest,
        sign2: impl Into<String>,
    ) -> Result<Self, MagstvProviderError> {
        request.validate()?;
        let sign2 = normalize_sign2(&sign2.into())?;
        Ok(Self {
            dev_id: request.dev_id,
            user_id: request.user_id,
            trans_id: request.trans_id,
            expired: request.expired,
            host: request.host,
            media_code: request.media_code,
            auth_id: request.auth_id,
            client_ip: request.client_ip,
            token: request.token,
            instance: request.instance,
            start_moment: request.start_moment,
            sign2,
        })
    }

    pub fn app_id(&self) -> &'static str {
        MAGSTV_APP_ID
    }

    pub fn app_version(&self) -> &'static str {
        MAGSTV_PLAYBACK_APP_VERSION
    }

    pub fn sign2_method(&self) -> &'static str {
        MAGSTV_SIGN2_METHOD
    }

    pub fn to_url(&self) -> Result<Url, MagstvProviderError> {
        let request = self.request();
        request.validate()?;
        let sign2 = normalize_sign2(&self.sign2)?;
        // The authorised client fetches VOD bytes over plain HTTP from the
        // CDN edge; authenticity comes from `sign2`, not from TLS.
        let mut url = Url::parse(&format!("http://{}/", self.host))
            .map_err(|_| MagstvProviderError::InvalidPlaybackUrl)?;
        url.set_path(&format!("/vod/{}_media.ts", self.media_code));
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("dev_id", &self.dev_id)
                .append_pair("user_id", &self.user_id)
                .append_pair("trans_id", &self.trans_id)
                .append_pair("expired", &self.expired.unix_timestamp().to_string())
                .append_pair("app_id", MAGSTV_APP_ID)
                .append_pair("app_ver", MAGSTV_PLAYBACK_APP_VERSION)
                .append_pair("host", &self.host)
                .append_pair("media_code", &self.media_code)
                .append_pair("auth_id", &self.auth_id)
                .append_pair("client_ip", &self.client_ip.to_string())
                .append_pair("token", &self.token)
                .append_pair("sign2_method", MAGSTV_SIGN2_METHOD)
                .append_pair("instance", &self.instance)
                .append_pair("start_moment", &self.start_moment)
                .append_pair("sign2", &sign2);
        }
        Ok(url)
    }

    pub fn from_url(url: &Url) -> Result<Self, MagstvProviderError> {
        if url.scheme() != "http"
            || url.username() != ""
            || url.password().is_some()
            || url.host_str().is_none()
        {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
        let segments = url
            .path_segments()
            .ok_or(MagstvProviderError::InvalidPlaybackUrl)?
            .collect::<Vec<_>>();
        if segments.len() != 2 || segments[0] != "vod" {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
        let media_code = segments[1]
            .strip_suffix("_media.ts")
            .filter(|value| !value.is_empty())
            .ok_or(MagstvProviderError::InvalidPlaybackUrl)?
            .to_string();
        if media_code.contains('/') || media_code.contains('\\') {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
        let query = unique_query_pairs(url)?;
        let host = required_query(&query, "host")?;
        if host != url_host_with_port(url)? {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
        if required_query(&query, "app_id")? != MAGSTV_APP_ID
            || required_query(&query, "app_ver")? != MAGSTV_PLAYBACK_APP_VERSION
            || required_query(&query, "sign2_method")? != MAGSTV_SIGN2_METHOD
            || required_query(&query, "media_code")? != media_code
        {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
        let expired = required_query(&query, "expired")?
            .parse::<i64>()
            .ok()
            .and_then(|value| OffsetDateTime::from_unix_timestamp(value).ok())
            .ok_or(MagstvProviderError::InvalidPlaybackUrl)?;
        let client_ip = required_query(&query, "client_ip")?
            .parse::<IpAddr>()
            .map_err(|_| MagstvProviderError::InvalidPlaybackUrl)?;
        let sign2 = normalize_sign2(&required_query(&query, "sign2")?)?;
        let result = Self {
            dev_id: required_query(&query, "dev_id")?,
            user_id: required_query(&query, "user_id")?,
            trans_id: required_query(&query, "trans_id")?,
            expired,
            host,
            media_code,
            auth_id: required_query(&query, "auth_id")?,
            client_ip,
            token: required_query(&query, "token")?,
            instance: required_query(&query, "instance")?,
            start_moment: required_query(&query, "start_moment")?,
            sign2,
        };
        result.request().validate()?;
        Ok(result)
    }

    fn request(&self) -> MagstvPlaybackRequest {
        MagstvPlaybackRequest {
            dev_id: self.dev_id.clone(),
            user_id: self.user_id.clone(),
            trans_id: self.trans_id.clone(),
            expired: self.expired,
            host: self.host.clone(),
            media_code: self.media_code.clone(),
            auth_id: self.auth_id.clone(),
            client_ip: self.client_ip,
            token: self.token.clone(),
            instance: self.instance.clone(),
            start_moment: self.start_moment.clone(),
        }
    }
}

fn normalize_sign2(sign2: &str) -> Result<String, MagstvProviderError> {
    if sign2.len() != 32 || !sign2.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MagstvProviderError::InvalidPlaybackParameter { field: "sign2" });
    }
    Ok(sign2.to_ascii_lowercase())
}

fn unique_query_pairs(url: &Url) -> Result<BTreeMap<String, String>, MagstvProviderError> {
    let mut query = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if query.insert(key.into_owned(), value.into_owned()).is_some() {
            return Err(MagstvProviderError::InvalidPlaybackUrl);
        }
    }
    Ok(query)
}

fn required_query(
    query: &BTreeMap<String, String>,
    key: &'static str,
) -> Result<String, MagstvProviderError> {
    query
        .get(key)
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .cloned()
        .ok_or(MagstvProviderError::InvalidPlaybackUrl)
}

fn url_host_with_port(url: &Url) -> Result<String, MagstvProviderError> {
    let host = url
        .host_str()
        .ok_or(MagstvProviderError::InvalidPlaybackUrl)?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(url
        .port()
        .map(|port| format!("{host}:{port}"))
        .unwrap_or(host))
}

#[async_trait]
pub trait Sign2Signer: Send + Sync {
    async fn sign(
        &self,
        session: &MagstvSession,
        request: &MagstvPlaybackRequest,
    ) -> Result<String, MagstvProviderError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableSigner;

#[async_trait]
impl Sign2Signer for UnavailableSigner {
    async fn sign(
        &self,
        _session: &MagstvSession,
        _request: &MagstvPlaybackRequest,
    ) -> Result<String, MagstvProviderError> {
        Err(MagstvProviderError::SignerUnavailable)
    }
}

/// Local implementation of the player-compatible `sign_o3` signer.
///
/// The account-specific signing material is supplied at runtime and is never
/// embedded in the binary or included in `Debug` output.
#[derive(Clone)]
pub struct SignO3Signer {
    secret: [u8; SIGN_O3_SECRET_BYTES],
}

impl fmt::Debug for SignO3Signer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignO3Signer")
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl SignO3Signer {
    pub fn from_secret_bytes(secret: [u8; SIGN_O3_SECRET_BYTES]) -> Self {
        Self { secret }
    }

    pub fn from_hex(secret: &str) -> Result<Self, MagstvProviderError> {
        let secret = secret.trim();
        if secret.len() != SIGN_O3_SECRET_BYTES * 2 {
            return Err(MagstvProviderError::InvalidPlaybackSigningSecret);
        }
        let mut decoded = [0_u8; SIGN_O3_SECRET_BYTES];
        for (index, byte) in decoded.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&secret[offset..offset + 2], 16)
                .map_err(|_| MagstvProviderError::InvalidPlaybackSigningSecret)?;
        }
        Ok(Self::from_secret_bytes(decoded))
    }

    pub fn from_environment() -> Result<Self, MagstvProviderError> {
        let secret = std::env::var(MAGSTV_SIGN_O3_SECRET_HEX_ENV)
            .map_err(|_| MagstvProviderError::MissingPlaybackSigningSecret)?;
        Self::from_hex(&secret)
    }
}

#[async_trait]
impl Sign2Signer for SignO3Signer {
    async fn sign(
        &self,
        _session: &MagstvSession,
        request: &MagstvPlaybackRequest,
    ) -> Result<String, MagstvProviderError> {
        request.validate()?;
        let mut input = Vec::with_capacity(
            6 + request.token.len()
                + 31
                + request.instance.len()
                + 14
                + request.start_moment.len()
                + self.secret.len(),
        );
        input.extend_from_slice(b"token=");
        input.extend_from_slice(request.token.as_bytes());
        input.extend_from_slice(b"&sign2_method=sign_o3&instance=");
        input.extend_from_slice(request.instance.as_bytes());
        input.extend_from_slice(b"&start_moment=");
        input.extend_from_slice(request.start_moment.as_bytes());
        input.extend_from_slice(&self.secret);
        Ok(hex_lower(&sign_o3_digest(&input)))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn sign_o3_digest(input: &[u8]) -> [u8; 16] {
    const ROTATIONS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const CONSTANTS: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd46f3085, 0x04881d05, 0xd9d4d039, 0xe6bd99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffecc47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2da7d2bb,
        0xeb86d391,
    ];
    const FIRST_ROUND_WORDS: [usize; 16] = [10, 11, 12, 13, 14, 15, 6, 7, 8, 9, 0, 1, 2, 3, 4, 5];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(input.len() + 72);
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());

    let mut state = [0x67452301_u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in padded.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().expect("four-byte word"));
        }
        let [mut a, mut b, mut c, mut d] = state;
        for index in 0..64 {
            let (function, word_index) = if index < 16 {
                ((b & c) | (!b & d), FIRST_ROUND_WORDS[index])
            } else if index < 32 {
                ((d & b) | (!d & c), (5 * index + 1) % 16)
            } else if index < 48 {
                (b ^ c ^ d, (3 * index + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * index) % 16)
            };
            let mixed = a
                .wrapping_add(function)
                .wrapping_add(CONSTANTS[index])
                .wrapping_add(words[word_index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(mixed.rotate_left(ROTATIONS[index]));
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut digest = [0_u8; 16];
    for (destination, word) in digest.chunks_exact_mut(4).zip(state) {
        destination.copy_from_slice(&word.to_le_bytes());
    }
    digest
}

/// Resolves CDN playback just in time. The provider intentionally does not
/// persist a signed URL: `expired` and `client_ip` make it session/egress
/// bound, and a refresh must go through this function again.
pub async fn resolve_playback<S: Sign2Signer + ?Sized>(
    session: &MagstvSession,
    request: &MagstvPlaybackRequest,
    signer: &S,
) -> Result<MagstvSignedPlaybackUrl, MagstvProviderError> {
    resolve_playback_at(session, request, signer, OffsetDateTime::now_utc()).await
}

pub async fn resolve_playback_at<S: Sign2Signer + ?Sized>(
    session: &MagstvSession,
    request: &MagstvPlaybackRequest,
    signer: &S,
    now: OffsetDateTime,
) -> Result<MagstvSignedPlaybackUrl, MagstvProviderError> {
    session.validate_at(now)?;
    request.validate()?;
    if let Some(bound_ip) = session.bound_client_ip()
        && bound_ip.parse::<IpAddr>().ok() != Some(request.client_ip)
    {
        return Err(MagstvProviderError::PlaybackClientIpMismatch);
    }
    let sign2 = signer.sign(session, request).await?;
    MagstvSignedPlaybackUrl::from_request(request.clone(), sign2)
}

/// Typed view of one per-variant license entry returned by `startPlayVOD`.
/// The license itself is a query string; only field provenance verified
/// against authorised client negotiations is accepted here. The 32-character
/// license token is retained only in memory and redacted from `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvLicenseGrant {
    pub app_id: String,
    pub tag: String,
    pub scheme: String,
    pub media_code: String,
    pub expires_at: OffsetDateTime,
    token: String,
}

impl fmt::Debug for MagstvLicenseGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvLicenseGrant")
            .field("app_id", &self.app_id)
            .field("tag", &self.tag)
            .field("scheme", &self.scheme)
            .field("media_code", &self.media_code)
            .field("expires_at", &self.expires_at)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl MagstvLicenseGrant {
    /// Parses a license query string, rejecting duplicate keys, unexpected
    /// field sets and malformed values instead of guessing.
    pub fn parse(license: &str) -> Result<Self, MagstvProviderError> {
        let query = license.split_once('?').map_or(license, |(_, tail)| tail);
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(MagstvProviderError::InvalidLicenseGrant { field: "format" });
            };
            if fields.insert(key.to_string(), value.to_string()).is_some() {
                return Err(MagstvProviderError::InvalidLicenseGrant { field: "duplicate" });
            }
        }
        let mut take = |key: &'static str| -> Result<String, MagstvProviderError> {
            fields
                .remove(key)
                .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
                .ok_or(MagstvProviderError::InvalidLicenseGrant { field: key })
        };
        let grant = Self {
            app_id: take("app_id")?,
            tag: take("tag")?,
            scheme: take("scheme")?,
            media_code: take("media_code")?,
            expires_at: take("expired")?
                .parse::<i64>()
                .ok()
                .and_then(|value| OffsetDateTime::from_unix_timestamp(value).ok())
                .ok_or(MagstvProviderError::InvalidLicenseGrant { field: "expired" })?,
            token: take("token")?,
        };
        if !fields.is_empty() {
            return Err(MagstvProviderError::InvalidLicenseGrant { field: "unexpected" });
        }
        if grant.token.len() != 32 || !grant.token.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(MagstvProviderError::InvalidLicenseGrant { field: "token" });
        }
        if grant.media_code.contains('/') || grant.media_code.contains('\\') {
            return Err(MagstvProviderError::InvalidLicenseGrant { field: "media_code" });
        }
        Ok(grant)
    }

    pub fn validate_at(&self, now: OffsetDateTime) -> Result<(), MagstvProviderError> {
        if now < self.expires_at {
            Ok(())
        } else {
            Err(MagstvProviderError::LicenseGrantExpired)
        }
    }
}

/// Per-installation values the CDN accepts verbatim. Field provenance was
/// established empirically: the CDN binds only `token`, `instance` and
/// `start_moment` through `sign2`, accepts any well-formed `dev_id`,
/// `trans_id` and `instance`, requires `client_ip` to match the fetching
/// egress, and expects `host` to name the serving edge.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvPlaybackContext {
    pub dev_id: String,
    pub user_id: String,
    pub host: String,
    pub client_ip: IpAddr,
    pub instance: String,
}

impl fmt::Debug for MagstvPlaybackContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvPlaybackContext")
            .field("dev_id", &self.dev_id)
            .field("user_id", &"[REDACTED]")
            .field("host", &self.host)
            .field("client_ip", &self.client_ip)
            .field("instance", &self.instance)
            .finish()
    }
}

fn random_alphanumeric(rng: &mut dyn RngCore, length: usize) -> String {
    const ALPHABET: &[u8; 62] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..length)
        .map(|_| {
            let byte = (rng.next_u32() & 0xff) as usize;
            char::from(ALPHABET[byte % ALPHABET.len()])
        })
        .collect()
}

impl MagstvPlaybackRequest {
    /// Builds a just-in-time playback request from a verified license grant.
    /// `token` and `trans_id` are fresh per negotiation, `start_moment` is the
    /// resolution instant and `expired` is the resolution instant plus
    /// [`MAGSTV_PLAYBACK_URL_TTL_SECONDS`].
    pub fn from_grant(
        grant: &MagstvLicenseGrant,
        context: &MagstvPlaybackContext,
        now: OffsetDateTime,
    ) -> Result<Self, MagstvProviderError> {
        Self::from_grant_with_rng(grant, context, now, &mut OsRng)
    }

    pub fn from_grant_with_rng(
        grant: &MagstvLicenseGrant,
        context: &MagstvPlaybackContext,
        now: OffsetDateTime,
        rng: &mut dyn RngCore,
    ) -> Result<Self, MagstvProviderError> {
        grant.validate_at(now)?;
        if grant.app_id != MAGSTV_APP_ID {
            return Err(MagstvProviderError::InvalidLicenseGrant { field: "app_id" });
        }
        let start_moment = (now.unix_timestamp_nanos() / 1_000_000).to_string();
        let expired = now + time::Duration::seconds(MAGSTV_PLAYBACK_URL_TTL_SECONDS);
        let request = Self {
            dev_id: context.dev_id.clone(),
            user_id: context.user_id.clone(),
            trans_id: format!(
                "{}_{}",
                random_alphanumeric(rng, 12),
                random_alphanumeric(rng, 12)
            ),
            expired,
            host: context.host.clone(),
            media_code: grant.media_code.clone(),
            auth_id: format!(
                "{}_{}__{}",
                context.user_id, MAGSTV_APP_ID, context.instance
            ),
            client_ip: context.client_ip,
            token: random_alphanumeric(rng, 8),
            instance: context.instance.clone(),
            start_moment,
        };
        request.validate()?;
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGN2: &str = "0123456789abcdef0123456789abcdef";

    fn instant(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(seconds).unwrap()
    }

    fn request() -> MagstvPlaybackRequest {
        MagstvPlaybackRequest {
            dev_id: "device-1".to_string(),
            user_id: "user-1".to_string(),
            trans_id: "transaction-1".to_string(),
            expired: instant(2_000_000_000),
            host: "cdn.example.invalid".to_string(),
            media_code: "movie-1".to_string(),
            auth_id: "user-1_com.android.msandroid__0".to_string(),
            client_ip: "203.0.113.7".parse().unwrap(),
            token: "runtime-token".to_string(),
            instance: "instance-1".to_string(),
            start_moment: "0".to_string(),
        }
    }

    struct StaticSigner;

    #[test]
    fn sign_o3_digest_matches_independent_synthetic_vectors() {
        assert_eq!(
            hex_lower(&sign_o3_digest(b"abc")),
            "1391f058d231d7608e5ce9194d0b8d25"
        );
    }

    #[tokio::test]
    async fn sign_o3_signer_uses_the_verified_canonical_input() {
        let signer = SignO3Signer::from_secret_bytes([0xa5; SIGN_O3_SECRET_BYTES]);
        let mut request = request();
        request.token = "test1234".to_string();
        request.instance = "instance-1".to_string();
        request.start_moment = "0".to_string();
        let session = MagstvSession::new("session", instant(1_900_000_000));
        assert_eq!(
            signer.sign(&session, &request).await.unwrap(),
            "a989df89d07fca4ad15e9d965cfabfef"
        );
        assert!(!format!("{signer:?}").contains("a5a5"));
    }

    #[test]
    fn sign_o3_signer_rejects_malformed_secret_material() {
        assert_eq!(
            SignO3Signer::from_hex("not-a-secret").unwrap_err(),
            MagstvProviderError::InvalidPlaybackSigningSecret
        );
    }

    #[async_trait]
    impl Sign2Signer for StaticSigner {
        async fn sign(
            &self,
            _session: &MagstvSession,
            _request: &MagstvPlaybackRequest,
        ) -> Result<String, MagstvProviderError> {
            Ok(SIGN2.to_string())
        }
    }

    #[tokio::test]
    async fn static_signer_round_trips_the_captured_url_shape() {
        let session = MagstvSession::new("session", instant(1_900_000_000))
            .with_expires_at(instant(2_100_000_000))
            .with_bound_client_ip("203.0.113.7");
        let signed =
            resolve_playback_at(&session, &request(), &StaticSigner, instant(1_950_000_000))
                .await
                .expect("fixture signer succeeds");
        let url = signed.to_url().expect("valid playback URL");
        assert_eq!(url.path(), "/vod/movie-1_media.ts");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "sign2")
                .map(|(_, value)| value.into_owned()),
            Some(SIGN2.to_string())
        );
        assert_eq!(MagstvSignedPlaybackUrl::from_url(&url).unwrap(), signed);
    }

    #[tokio::test]
    async fn unavailable_signer_fails_closed() {
        let session = MagstvSession::new("session", instant(1_900_000_000));
        let result = resolve_playback_at(
            &session,
            &request(),
            &UnavailableSigner,
            instant(1_950_000_000),
        )
        .await;
        assert_eq!(result, Err(MagstvProviderError::SignerUnavailable));
    }

    #[tokio::test]
    async fn invalid_signature_and_ip_binding_are_rejected() {
        let invalid = MagstvSignedPlaybackUrl::from_request(request(), "not-md5");
        assert_eq!(
            invalid,
            Err(MagstvProviderError::InvalidPlaybackParameter { field: "sign2" })
        );

        let session = MagstvSession::new("session", instant(1_900_000_000))
            .with_bound_client_ip("198.51.100.8");
        let result =
            resolve_playback_at(&session, &request(), &StaticSigner, instant(1_950_000_000)).await;
        assert_eq!(result, Err(MagstvProviderError::PlaybackClientIpMismatch));
        let debug = format!("{:?}", request());
        assert!(!debug.contains("runtime-token"));
    }

    struct FixedRng(u64);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 32) as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            self.0
        }
        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for chunk in destination.chunks_mut(8) {
                chunk.copy_from_slice(&self.next_u64().to_le_bytes()[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    fn license() -> String {
        format!(
            "app_id={MAGSTV_APP_ID}&tag=vod1&scheme=https&media_code=MEDIACODE123&\
             expired=2000000000&token=0123456789abcdef0123456789abcdef"
        )
    }

    fn context() -> MagstvPlaybackContext {
        MagstvPlaybackContext {
            dev_id: "device-1".to_string(),
            user_id: "user-1".to_string(),
            host: "cdn.example.invalid".to_string(),
            client_ip: "203.0.113.7".parse().unwrap(),
            instance: MAGSTV_PLAYBACK_DEFAULT_INSTANCE.to_string(),
        }
    }

    #[test]
    fn license_grant_parses_the_verified_field_set() {
        let grant = MagstvLicenseGrant::parse(&license()).unwrap();
        assert_eq!(grant.media_code, "MEDIACODE123");
        assert_eq!(grant.expires_at, instant(2_000_000_000));
        assert!(!format!("{grant:?}").contains("0123456789abcdef0123456789abcdef"));
        assert!(MagstvLicenseGrant::parse("app_id=x&app_id=y").is_err());
        assert!(MagstvLicenseGrant::parse(&license().replace("tag=vod1&", "")).is_err());
        assert!(MagstvLicenseGrant::parse(&(license() + "&extra=1")).is_err());
        assert!(MagstvLicenseGrant::parse(&license().replace("expired=2000000000", "expired=soon")).is_err());
        assert!(MagstvLicenseGrant::parse(&license().replace(
            "token=0123456789abcdef0123456789abcdef",
            "token=short"
        ))
        .is_err());
    }

    #[test]
    fn playback_request_from_grant_uses_only_verified_provenance() {
        let grant = MagstvLicenseGrant::parse(&license()).unwrap();
        let mut rng = FixedRng(7);
        let built = MagstvPlaybackRequest::from_grant_with_rng(
            &grant,
            &context(),
            instant(1_900_000_000),
            &mut rng,
        )
        .unwrap();
        assert_eq!(built.media_code, grant.media_code);
        assert_eq!(built.auth_id, format!("user-1_{MAGSTV_APP_ID}__0"));
        assert_eq!(built.start_moment, "1900000000000");
        assert_eq!(
            built.expired,
            instant(1_900_000_000 + MAGSTV_PLAYBACK_URL_TTL_SECONDS)
        );
        assert_eq!(built.token.len(), 8);
        let trans_parts: Vec<&str> = built.trans_id.split('_').collect();
        assert_eq!(trans_parts.len(), 2);
        assert!(trans_parts.iter().all(|part| part.len() == 12));
        assert!(built.validate().is_ok());
        let debug = format!("{built:?}");
        assert!(!debug.contains(&built.token));
    }

    #[test]
    fn playback_request_rejects_expired_or_mismatched_grants() {
        let expired_license = license().replace("expired=2000000000", "expired=100");
        let expired_grant = MagstvLicenseGrant::parse(&expired_license).unwrap();
        let mut rng = FixedRng(7);
        assert_eq!(
            MagstvPlaybackRequest::from_grant_with_rng(
                &expired_grant,
                &context(),
                instant(1_900_000_000),
                &mut rng,
            ),
            Err(MagstvProviderError::LicenseGrantExpired)
        );

        let foreign_license = license().replace(MAGSTV_APP_ID, "com.example.other");
        let foreign_grant = MagstvLicenseGrant::parse(&foreign_license).unwrap();
        assert_eq!(
            MagstvPlaybackRequest::from_grant_with_rng(
                &foreign_grant,
                &context(),
                instant(1_900_000_000),
                &mut rng,
            ),
            Err(MagstvProviderError::InvalidLicenseGrant { field: "app_id" })
        );
    }

    #[tokio::test]
    async fn grant_built_request_round_trips_through_sign_o3() {
        let grant = MagstvLicenseGrant::parse(&license()).unwrap();
        let mut rng = FixedRng(11);
        let built = MagstvPlaybackRequest::from_grant_with_rng(
            &grant,
            &context(),
            instant(1_900_000_000),
            &mut rng,
        )
        .unwrap();
        let signer = SignO3Signer::from_secret_bytes([0xa5; SIGN_O3_SECRET_BYTES]);
        let session = MagstvSession::new("session", instant(1_900_000_000));
        let signed = resolve_playback_at(&session, &built, &signer, instant(1_900_000_100))
            .await
            .unwrap();
        let url = signed.to_url().unwrap();
        assert_eq!(url.path(), "/vod/MEDIACODE123_media.ts");
        assert_eq!(MagstvSignedPlaybackUrl::from_url(&url).unwrap(), signed);
    }
}
