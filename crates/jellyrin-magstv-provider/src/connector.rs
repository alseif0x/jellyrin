use crate::{
    MagstvConfig, MagstvProviderError, MagstvSession, MagstvTransport, PortalCodec, PortalRequest,
    PortalResponse, TransportFailureKind,
};
use time::OffsetDateTime;

pub const MAX_PORTAL_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PORTAL_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub struct MagstvConnector<T, C> {
    transport: T,
    codec: C,
}

impl<T, C> MagstvConnector<T, C>
where
    T: MagstvTransport,
    C: PortalCodec,
{
    pub fn new(transport: T, codec: C) -> Self {
        Self { transport, codec }
    }

    /// Executes only operations backed by a sanitised fixture set. The guard
    /// runs before encoding or transport, so an unverified codec cannot make a
    /// network call even if a concrete transport is supplied later.
    pub async fn execute(
        &self,
        config: &MagstvConfig,
        request: &PortalRequest,
    ) -> Result<PortalResponse, MagstvProviderError> {
        self.execute_at(config, request, None, OffsetDateTime::now_utc())
            .await
    }

    /// Executes an authenticated portal operation with a session supplied by
    /// the caller. The session is never inferred from config or logs.
    pub async fn execute_with_session(
        &self,
        config: &MagstvConfig,
        request: &PortalRequest,
        session: &MagstvSession,
    ) -> Result<PortalResponse, MagstvProviderError> {
        self.execute_at(config, request, Some(session), OffsetDateTime::now_utc())
            .await
    }

    /// Deterministic form used by callers/tests that own the clock. The
    /// session gate deliberately runs before codec verification and transport.
    pub async fn execute_at(
        &self,
        config: &MagstvConfig,
        request: &PortalRequest,
        session: Option<&MagstvSession>,
        now: OffsetDateTime,
    ) -> Result<PortalResponse, MagstvProviderError> {
        config.validates_for_connection()?;
        if request.operation.requires_session() {
            let session = session.ok_or(MagstvProviderError::SessionRequired)?;
            if !session.is_valid_at(now) {
                return Err(MagstvProviderError::SessionExpired);
            }
        }
        let verification = self.codec.verification();
        if !verification.is_verified() {
            return Err(MagstvProviderError::ProtocolUnverified);
        }
        if !verification.verifies(request.operation) {
            return Err(MagstvProviderError::OperationUnverified {
                operation: request.operation,
            });
        }

        let encoded = self.codec.encode(request)?;
        if encoded.body().len() > MAX_PORTAL_REQUEST_BYTES {
            return Err(MagstvProviderError::RequestTooLarge);
        }
        let response = self
            .transport
            .exchange(config.bootstrap_url.trim(), &encoded)
            .await?;
        if response.body.len() > MAX_PORTAL_RESPONSE_BYTES {
            return Err(MagstvProviderError::ResponseTooLarge);
        }
        if !(200..300).contains(&response.status) {
            return Err(MagstvProviderError::Transport(
                TransportFailureKind::HttpStatus(response.status),
            ));
        }
        self.codec
            .decode(request.operation, response.status, &response.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CodecVerification, PortalOperation, PortalTransportResponse, UnverifiedPortalCodec,
        VerifiedWireRequest,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingTransport {
        calls: AtomicUsize,
        response: PortalTransportResponse,
    }

    #[async_trait]
    impl MagstvTransport for CountingTransport {
        async fn exchange(
            &self,
            _bootstrap_url: &str,
            _request: &VerifiedWireRequest,
        ) -> Result<PortalTransportResponse, MagstvProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    struct FixtureCodec {
        verified_operations: BTreeSet<PortalOperation>,
    }

    impl PortalCodec for FixtureCodec {
        fn verification(&self) -> CodecVerification {
            CodecVerification::verified("a".repeat(64), self.verified_operations.iter().copied())
                .expect("valid fixture evidence")
        }

        fn encode(
            &self,
            request: &PortalRequest,
        ) -> Result<VerifiedWireRequest, MagstvProviderError> {
            VerifiedWireRequest::from_verified_fixture(
                "/fixture",
                "application/octet-stream",
                serde_json::to_vec(&request.arguments)
                    .map_err(|_| MagstvProviderError::InvalidProtocolEvidence)?,
            )
        }

        fn decode(
            &self,
            _operation: PortalOperation,
            _status: u16,
            body: &[u8],
        ) -> Result<PortalResponse, MagstvProviderError> {
            let payload = serde_json::from_slice(body).map_err(|_| {
                MagstvProviderError::Codec(crate::CodecFailureKind::MalformedMessage)
            })?;
            Ok(PortalResponse { payload })
        }
    }

    fn config() -> MagstvConfig {
        MagstvConfig {
            bootstrap_url: "https://portal.example.invalid".to_string(),
            secret_reference: "MAGSTV_ACCOUNT".to_string(),
            category_ids: BTreeSet::new(),
            excluded_category_ids: BTreeSet::new(),
            channel_limit: None,
            cdn_edge_host: None,
        }
    }

    #[tokio::test]
    async fn unverified_codec_cannot_reach_transport() {
        let transport = CountingTransport {
            calls: AtomicUsize::new(0),
            response: PortalTransportResponse {
                status: 200,
                content_type: None,
                body: b"{}".to_vec(),
            },
        };
        let connector = MagstvConnector::new(transport, UnverifiedPortalCodec);
        let result = connector
            .execute(
                &config(),
                &PortalRequest::new(PortalOperation::Bootstrap, json!({})),
            )
            .await;
        assert!(matches!(
            result,
            Err(MagstvProviderError::ProtocolUnverified)
        ));
        assert_eq!(connector.transport.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_fixture_verified_operations_can_reach_transport() {
        let transport = CountingTransport {
            calls: AtomicUsize::new(0),
            response: PortalTransportResponse {
                status: 200,
                content_type: Some("application/octet-stream".to_string()),
                body: br#"{"categories":["sports"]}"#.to_vec(),
            },
        };
        let codec = FixtureCodec {
            verified_operations: BTreeSet::from([PortalOperation::ListLiveCategories]),
        };
        let connector = MagstvConnector::new(transport, codec);
        let session = MagstvSession::new(
            "fixture-session",
            time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        );

        let rejected = connector
            .execute_at(
                &config(),
                &PortalRequest::new(PortalOperation::Authenticate, json!({})),
                Some(&session),
                time::OffsetDateTime::from_unix_timestamp(1_700_000_001).unwrap(),
            )
            .await;
        assert!(matches!(
            rejected,
            Err(MagstvProviderError::OperationUnverified {
                operation: PortalOperation::Authenticate
            })
        ));
        assert_eq!(connector.transport.calls.load(Ordering::SeqCst), 0);

        let accepted = connector
            .execute_at(
                &config(),
                &PortalRequest::new(PortalOperation::ListLiveCategories, json!({})),
                Some(&session),
                time::OffsetDateTime::from_unix_timestamp(1_700_000_001).unwrap(),
            )
            .await
            .expect("verified fixture exchange");
        assert_eq!(accepted.payload["categories"], json!(["sports"]));
        assert_eq!(connector.transport.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn session_gate_runs_before_codec_verification() {
        let transport = CountingTransport {
            calls: AtomicUsize::new(0),
            response: PortalTransportResponse {
                status: 200,
                content_type: None,
                body: b"{}".to_vec(),
            },
        };
        let connector = MagstvConnector::new(transport, UnverifiedPortalCodec);
        let result = connector
            .execute_at(
                &config(),
                &PortalRequest::new(PortalOperation::ListLiveChannels, json!({})),
                None,
                time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            )
            .await;
        assert!(matches!(result, Err(MagstvProviderError::SessionRequired)));
        assert_eq!(connector.transport.calls.load(Ordering::SeqCst), 0);
    }
}
