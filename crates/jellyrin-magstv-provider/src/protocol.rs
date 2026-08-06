use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::MagstvProviderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PortalOperation {
    Bootstrap,
    Authenticate,
    GetAuthInfo,
    GetSlbInfo,
    ListLiveCategories,
    ListLiveChannels,
    ListPrograms,
    ListMovies,
    ListSeries,
    ListEpisodes,
    ResolvePlayback,
    ResolveVodPlayback,
    RefreshSession,
}

impl PortalOperation {
    /// Portal operations that are authenticated by the app session. Plain GET
    /// resources such as EPG/notice/update are intentionally outside this
    /// enum and must not be smuggled through the encrypted portal codec.
    pub const fn requires_session(self) -> bool {
        !matches!(self, Self::Bootstrap | Self::Authenticate)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PortalRequest {
    pub operation: PortalOperation,
    #[serde(default)]
    pub arguments: Value,
}

impl PortalRequest {
    pub fn new(operation: PortalOperation, arguments: Value) -> Self {
        Self {
            operation,
            arguments,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PortalResponse {
    pub payload: Value,
}

/// Encoded requests deliberately hide endpoint details and bytes from Debug so
/// a future auth codec cannot leak tokens through structured logs.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedWireRequest {
    relative_path: String,
    content_type: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

impl VerifiedWireRequest {
    /// Test-only until a real codec and its sanitised fixtures are committed in
    /// the same reviewed change.
    #[cfg(test)]
    pub(crate) fn from_verified_fixture(
        relative_path: impl Into<String>,
        content_type: impl Into<String>,
        body: Vec<u8>,
    ) -> Result<Self, MagstvProviderError> {
        let relative_path = relative_path.into();
        if !relative_path.starts_with('/')
            || relative_path.starts_with("//")
            || relative_path.contains("://")
            || relative_path.split('/').any(|segment| segment == "..")
            || relative_path.chars().any(char::is_control)
        {
            return Err(MagstvProviderError::InvalidEncodedEndpoint);
        }
        let content_type = content_type.into();
        if content_type.trim().is_empty() || content_type.chars().any(char::is_control) {
            return Err(MagstvProviderError::InvalidContentType);
        }
        Ok(Self {
            relative_path,
            content_type,
            body,
            headers: BTreeMap::new(),
        })
    }

    /// Constructs a wire request from a codec whose contract is verified in
    /// this crate. Keeping this constructor crate-visible prevents callers
    /// from turning arbitrary plaintext into a network request.
    pub(crate) fn from_verified_contract(
        relative_path: impl Into<String>,
        content_type: impl Into<String>,
        body: Vec<u8>,
        headers: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, MagstvProviderError> {
        let relative_path = relative_path.into();
        validate_relative_endpoint(&relative_path)?;
        let content_type = content_type.into();
        validate_content_type(&content_type)?;
        let headers = headers.into_iter().try_fold(
            BTreeMap::new(),
            |mut headers, (name, value)| -> Result<_, MagstvProviderError> {
                if name.is_empty()
                    || name.bytes().any(|byte| {
                        !(byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
                    })
                    || value.chars().any(char::is_control)
                {
                    return Err(MagstvProviderError::InvalidHeader);
                }
                headers.insert(name, value);
                Ok(headers)
            },
        )?;
        Ok(Self {
            relative_path,
            content_type,
            body,
            headers,
        })
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub(crate) fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }
}

impl fmt::Debug for VerifiedWireRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedWireRequest")
            .field("endpoint", &"[REDACTED]")
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .field("header_count", &self.headers.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecVerification {
    fixture_set_sha256: Option<String>,
    operations: BTreeSet<PortalOperation>,
}

impl CodecVerification {
    pub const fn unverified() -> Self {
        Self {
            fixture_set_sha256: None,
            operations: BTreeSet::new(),
        }
    }

    /// Evidence for a statically recovered contract. The string is an
    /// identifier for the reviewed contract revision, not a captured account
    /// payload or secret.
    pub(crate) fn verified_contract(
        contract_revision: impl Into<String>,
        operations: impl IntoIterator<Item = PortalOperation>,
    ) -> Result<Self, MagstvProviderError> {
        let contract_revision = contract_revision.into();
        let operations = operations.into_iter().collect::<BTreeSet<_>>();
        if contract_revision.len() != 64
            || !contract_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || operations.is_empty()
        {
            return Err(MagstvProviderError::InvalidProtocolEvidence);
        }
        Ok(Self {
            fixture_set_sha256: Some(contract_revision.to_ascii_lowercase()),
            operations,
        })
    }

    /// Test-only helper for fixture-backed connector tests.
    #[cfg(test)]
    pub(crate) fn verified(
        fixture_set_sha256: impl Into<String>,
        operations: impl IntoIterator<Item = PortalOperation>,
    ) -> Result<Self, MagstvProviderError> {
        let fixture_set_sha256 = fixture_set_sha256.into();
        let operations = operations.into_iter().collect::<BTreeSet<_>>();
        if fixture_set_sha256.len() != 64
            || !fixture_set_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || operations.is_empty()
        {
            return Err(MagstvProviderError::InvalidProtocolEvidence);
        }
        Ok(Self {
            fixture_set_sha256: Some(fixture_set_sha256.to_ascii_lowercase()),
            operations,
        })
    }

    pub fn is_verified(&self) -> bool {
        self.fixture_set_sha256.is_some()
    }

    pub fn verifies(&self, operation: PortalOperation) -> bool {
        self.is_verified() && self.operations.contains(&operation)
    }
}

fn validate_relative_endpoint(relative_path: &str) -> Result<(), MagstvProviderError> {
    if !relative_path.starts_with('/')
        || relative_path.starts_with("//")
        || relative_path.contains("://")
        || relative_path.split('/').any(|segment| segment == "..")
        || relative_path.chars().any(char::is_control)
    {
        return Err(MagstvProviderError::InvalidEncodedEndpoint);
    }
    Ok(())
}

fn validate_content_type(content_type: &str) -> Result<(), MagstvProviderError> {
    if content_type.trim().is_empty() || content_type.chars().any(char::is_control) {
        return Err(MagstvProviderError::InvalidContentType);
    }
    Ok(())
}

pub trait PortalCodec: Send + Sync {
    fn verification(&self) -> CodecVerification;

    fn encode(&self, request: &PortalRequest) -> Result<VerifiedWireRequest, MagstvProviderError>;

    fn decode(
        &self,
        operation: PortalOperation,
        status: u16,
        body: &[u8],
    ) -> Result<PortalResponse, MagstvProviderError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnverifiedPortalCodec;

impl PortalCodec for UnverifiedPortalCodec {
    fn verification(&self) -> CodecVerification {
        CodecVerification::unverified()
    }

    fn encode(&self, _request: &PortalRequest) -> Result<VerifiedWireRequest, MagstvProviderError> {
        Err(MagstvProviderError::ProtocolUnverified)
    }

    fn decode(
        &self,
        _operation: PortalOperation,
        _status: u16,
        _body: &[u8],
    ) -> Result<PortalResponse, MagstvProviderError> {
        Err(MagstvProviderError::ProtocolUnverified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_requires_a_sha256_and_at_least_one_operation() {
        assert_eq!(
            CodecVerification::verified("not-a-sha256", [PortalOperation::Bootstrap]),
            Err(MagstvProviderError::InvalidProtocolEvidence)
        );
        assert_eq!(
            CodecVerification::verified("a".repeat(64), []),
            Err(MagstvProviderError::InvalidProtocolEvidence)
        );
    }

    #[test]
    fn encoded_endpoint_must_remain_relative_and_cannot_traverse() {
        for endpoint in [
            "https://example.invalid/api",
            "//example.invalid/api",
            "/api/../secret",
            "/api\r\nHeader: value",
        ] {
            assert_eq!(
                VerifiedWireRequest::from_verified_fixture(
                    endpoint,
                    "application/octet-stream",
                    vec![]
                ),
                Err(MagstvProviderError::InvalidEncodedEndpoint)
            );
        }
    }

    #[test]
    fn debug_output_redacts_endpoint_and_body() {
        let encoded = VerifiedWireRequest::from_verified_fixture(
            "/api/private?token=do-not-log",
            "application/octet-stream",
            b"secret-body".to_vec(),
        )
        .expect("valid fixture request");
        let debug = format!("{encoded:?}");
        assert!(!debug.contains("do-not-log"));
        assert!(!debug.contains("secret-body"));
        assert!(debug.contains("body_len"));
    }
}
