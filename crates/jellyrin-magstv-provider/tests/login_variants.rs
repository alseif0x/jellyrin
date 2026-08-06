//! Temporary opt-in diagnostic for the operator-owned portal session.
//!
//! It reports only return-code shapes.  Never enable this outside the isolated
//! MX sidecar because it performs a small number of real login attempts.

use jellyrin_magstv_provider::{
    MAGSTV_PORTAL_CONTENT_TYPE, MAGSTV_PORTAL_KEY_METADATA_ENV, MagstvCommonParams,
    MagstvConfig, MagstvConnector, MagstvLoginData, MagstvLoginRequest, MagstvPortalCodec,
    MagstvPortalResponse, MagstvProviderError, MagstvSecret, MagstvTransport, PortalOperation,
    PortalRequest, ReqwestMagstvTransport,
};
use serde_json::Value;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required runtime variable {name}"))
}

fn code_from_payload(payload: &Value) -> String {
    serde_json::from_value::<MagstvPortalResponse<MagstvLoginData>>(payload.clone())
        .ok()
        .and_then(|response| response.return_code)
        .unwrap_or_else(|| "unparsed".to_string())
}

async fn try_variant(
    bootstrap_url: &str,
    metadata: &str,
    common: MagstvCommonParams,
    secret: &MagstvSecret,
    account_type: &str,
) -> Result<String, MagstvProviderError> {
    let codec = MagstvPortalCodec::from_manifest_hex(metadata, common)?;
    let transport = ReqwestMagstvTransport::new()?;
    let connector = MagstvConnector::new(transport, codec);
    let mut login = MagstvLoginRequest::from_secret(secret, "02:00:00:00:00:01")?;
    login.account_type = account_type.to_string();
    let request = PortalRequest::new(
        PortalOperation::Authenticate,
        serde_json::to_value(login).map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
    );
    let config = MagstvConfig {
        bootstrap_url: bootstrap_url.to_string(),
        secret_reference: "MAGSTV_VARIANT_PROBE".to_string(),
        category_ids: Default::default(),
        excluded_category_ids: Default::default(),
        channel_limit: None,
        cdn_edge_host: None,
    };
    let response = connector.execute(&config, &request).await?;
    Ok(code_from_payload(&response.payload))
}

#[tokio::test]
#[ignore = "requires the operator-owned account and MX egress"]
async fn login_variants_report_only_codes() {
    let metadata = required(MAGSTV_PORTAL_KEY_METADATA_ENV);
    let bootstrap_url = required("MAGSTV_BOOTSTRAP_URL");
    let secret = MagstvSecret::new(
        required("MAGSTV_LIVE_PROBE_USERNAME"),
        required("MAGSTV_LIVE_PROBE_PASSWORD"),
    );
    let _ = MAGSTV_PORTAL_CONTENT_TYPE;

    for version in ["49903", "49904", "49905"] {
        for account_type in ["1", "2"] {
            let common = MagstvCommonParams::from_environment_with_app_version(version)
                .expect("valid common params");
            let result = try_variant(
                &bootstrap_url,
                &metadata,
                common,
                &secret,
                account_type,
            )
            .await;
            match result {
                Ok(code) => eprintln!("version={version} account_type={account_type} return_code={code}"),
                Err(error) => eprintln!(
                    "version={version} account_type={account_type} transport_or_codec={error:?}"
                ),
            }
        }
    }
}
