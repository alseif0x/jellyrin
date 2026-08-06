//! Safe MAGSTV provider boundary for Jellyrin.
//!
//! This crate deliberately contains no APK code, embedded credentials,
//! proprietary keys, or concrete network transport. The binary portal codec
//! can only be enabled after sanitised fixtures verify each operation.

mod catalog;
mod client;
mod config;
mod connector;
mod epg;
mod error;
mod playback;
mod portal_codec;
mod portal_data;
mod protocol;
mod secrets;
mod session;
mod transport;
mod version;

pub use catalog::{
    JellyrinLiveTvCatalog, MagstvCategory, MagstvChannel, MagstvLiveTvImport, MagstvMediaEpisode,
    MagstvMediaImport, MagstvMediaItem, MagstvMediaKind,
};
pub use client::{MagstvAuthenticatedSession, MagstvPortalClient};
pub use config::{
    MAGSTV_APP_ID, MAGSTV_APP_VERSION, MAGSTV_PLAYBACK_APP_VERSION, MAGSTV_SIGN2_METHOD,
    MagstvConfig,
};
pub use connector::{MAX_PORTAL_REQUEST_BYTES, MAX_PORTAL_RESPONSE_BYTES, MagstvConnector};
pub use epg::{
    MAGSTV_EPG_MD5_QUERY, MagstvProgram, build_epg_url, parse_epg_programs,
    parse_portal_epg_programs, programs_from_payload, programs_from_portal_epg,
};
pub use error::{CodecFailureKind, MagstvProviderError, TransportFailureKind};
pub use playback::{
    MAGSTV_PLAYBACK_DEFAULT_INSTANCE, MAGSTV_PLAYBACK_URL_TTL_SECONDS,
    MAGSTV_SIGN_O3_SECRET_HEX_ENV, MagstvLicenseGrant, MagstvPlaybackContext,
    MagstvPlaybackRequest, MagstvSignedPlaybackUrl, Sign2Signer, SignO3Signer, UnavailableSigner,
    resolve_playback, resolve_playback_at,
};
pub use portal_codec::{
    MAGSTV_PORTAL_KEY_METADATA_ENV, MagstvCommonParams, MagstvPortalCodec, MagstvPortalKey,
};
pub use portal_data::{
    MAGSTV_PORTAL_CONTENT_TYPE, MagstvAsset, MagstvAssetData, MagstvAuthInfo, MagstvChildColumn,
    MagstvColumnContentsData, MagstvEpgData, MagstvEpgProgram, MagstvGetAuthInfoData,
    MagstvGetAuthInfoRequest, MagstvGetColumnContentsRequest, MagstvGetHomeRequest,
    MagstvGetItemDataData, MagstvGetItemDataRequest, MagstvGetLiveDataRequest,
    MagstvGetShelveRequest, MagstvLiveAddress, MagstvLiveData, MagstvLoginData, MagstvLoginRequest,
    MagstvMovieListItem, MagstvPortalChannel, MagstvPortalCode, MagstvPortalEndpoint,
    MagstvPortalIdentity, MagstvPortalResponse, MagstvPoster, MagstvProgramRequest,
    MagstvSameSeasonSeries, MagstvShelveData, MagstvSimpleProgram, MagstvSlbInfoData,
    MagstvSlbInfoRequest, MagstvStartPlayLiveData, MagstvStartPlayLiveRequest,
    MagstvStartPlayVodData, MagstvStartPlayVodItem, MagstvStartPlayVodRequest, MagstvSubtitleFile,
    MagstvSubtitleItem, MagstvTotalMovieListItem, parse_portal_response,
};
pub use protocol::{
    CodecVerification, PortalCodec, PortalOperation, PortalRequest, PortalResponse,
    UnverifiedPortalCodec, VerifiedWireRequest,
};
pub use secrets::{
    EnvironmentSecretResolver, InMemorySecretResolver, MagstvSecret, SecretResolver,
};
pub use session::MagstvSession;
pub use transport::{
    DenyNetworkTransport, MAGSTV_DEFAULT_EGRESS_PROXY, MAGSTV_EGRESS_PROXY_ENV, MagstvTransport,
    PortalTransportResponse, ReqwestEpgTransport, ReqwestMagstvTransport, build_magstv_http_client,
    configured_magstv_egress_proxy,
};
pub use version::discover_app_version;

pub const MAGSTV_PROVIDER_TYPE: &str = "magstv";

/// Compatibility guard for callers that have not supplied a verified codec.
/// It intentionally prevents guessed portal calls.
pub fn connection_is_not_ready() -> Result<(), MagstvProviderError> {
    Err(MagstvProviderError::ProtocolUnverified)
}
