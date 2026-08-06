use crate::PortalOperation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailureKind {
    Timeout,
    Dns,
    Tls,
    RedirectRejected,
    HttpStatus(u16),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecFailureKind {
    MalformedMessage,
    IntegrityCheckFailed,
    UnsupportedRevision,
    UnexpectedPayload,
    InvalidKey,
    InvalidEncoding,
    InvalidPadding,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MagstvProviderError {
    #[error("MAGSTV bootstrap URL is required")]
    MissingBootstrapUrl,
    #[error("MAGSTV bootstrap URL must use HTTPS")]
    BootstrapMustUseHttps,
    #[error("MAGSTV bootstrap URL is invalid")]
    InvalidBootstrapUrl,
    #[error("MAGSTV secret reference is required")]
    MissingSecretReference,
    #[error("MAGSTV secret reference was not found")]
    SecretNotFound,
    #[error("MAGSTV secret is incomplete")]
    InvalidSecret,
    #[error("MAGSTV runtime configuration is invalid")]
    InvalidRuntimeConfiguration,
    #[error("MAGSTV operation requires a valid session")]
    SessionRequired,
    #[error("MAGSTV session has expired or is not yet valid")]
    SessionExpired,
    #[error("MAGSTV protocol codec is not verified yet")]
    ProtocolUnverified,
    #[error("MAGSTV portal payload is not valid JSON")]
    InvalidPortalPayload,
    #[error(
        "MAGSTV login response omitted required identity fields (user_id={user_id_present}, token={token_present})"
    )]
    MissingPortalIdentity {
        user_id_present: bool,
        token_present: bool,
    },
    #[error("MAGSTV portal response has an unexpected {data_type} data member")]
    UnexpectedPortalDataType { data_type: &'static str },
    #[error("MAGSTV portal rejected the request with code {return_code}")]
    PortalRejected { return_code: String },
    #[error("MAGSTV operation {operation:?} has no verified protocol fixture")]
    OperationUnverified { operation: PortalOperation },
    #[error("MAGSTV protocol evidence is invalid")]
    InvalidProtocolEvidence,
    #[error("MAGSTV encoded request endpoint is invalid")]
    InvalidEncodedEndpoint,
    #[error("MAGSTV encoded request content type is invalid")]
    InvalidContentType,
    #[error("MAGSTV encoded request header is invalid")]
    InvalidHeader,
    #[error("MAGSTV request exceeds the configured safety limit")]
    RequestTooLarge,
    #[error("MAGSTV response exceeds the configured safety limit")]
    ResponseTooLarge,
    #[error("MAGSTV transport failed: {0:?}")]
    Transport(TransportFailureKind),
    #[error("MAGSTV codec failed: {0:?}")]
    Codec(CodecFailureKind),
    #[error("MAGSTV playback signer is unavailable")]
    SignerUnavailable,
    #[error("MAGSTV playback signing secret is not configured")]
    MissingPlaybackSigningSecret,
    #[error("MAGSTV playback signing secret is invalid")]
    InvalidPlaybackSigningSecret,
    #[error("MAGSTV playback parameter is invalid: {field}")]
    InvalidPlaybackParameter { field: &'static str },
    #[error("MAGSTV playback license grant is invalid: {field}")]
    InvalidLicenseGrant { field: &'static str },
    #[error("MAGSTV playback license grant has expired")]
    LicenseGrantExpired,
    #[error("MAGSTV playback client IP does not match the authorised session")]
    PlaybackClientIpMismatch,
    #[error("MAGSTV signed playback URL is invalid")]
    InvalidPlaybackUrl,
    #[error("MAGSTV EPG request is invalid")]
    InvalidEpgRequest,
}
