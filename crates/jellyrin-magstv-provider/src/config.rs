use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

use crate::MagstvProviderError;

/// Captured application identity. These values are protocol constants, not
/// user-configurable credentials.
pub const MAGSTV_APP_ID: &str = "com.android.msandroid";
// The portal retires older protocol versions. The update endpoint currently
// advertises 49905 for com.android.mgstv, while 49903 returns portal200001.
pub const MAGSTV_APP_VERSION: &str = "49905";
/// The installed 4.99.5 player uses its APK version code in the CDN URL.
pub const MAGSTV_PLAYBACK_APP_VERSION: &str = "49905";
pub const MAGSTV_SIGN2_METHOD: &str = "sign_o3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MagstvConfig {
    /// User-configured bootstrap URL. It is intentionally not populated from
    /// an APK resource because service endpoints rotate.
    pub bootstrap_url: String,
    /// Name of a local secret reference, never the secret value itself.
    pub secret_reference: String,
    #[serde(default)]
    pub category_ids: BTreeSet<String>,
    #[serde(default)]
    pub excluded_category_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_limit: Option<usize>,
    /// Host name of the CDN edge that serves authorised VOD bytes. It is a
    /// deployment-learned value (stable per account region), never bundled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdn_edge_host: Option<String>,
}

impl MagstvConfig {
    pub fn validates_for_connection(&self) -> Result<(), MagstvProviderError> {
        let bootstrap_url = self.bootstrap_url.trim();
        if bootstrap_url.is_empty() {
            return Err(MagstvProviderError::MissingBootstrapUrl);
        }
        let parsed =
            Url::parse(bootstrap_url).map_err(|_| MagstvProviderError::InvalidBootstrapUrl)?;
        if parsed.scheme() != "https" {
            return Err(MagstvProviderError::BootstrapMustUseHttps);
        }
        if parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(MagstvProviderError::InvalidBootstrapUrl);
        }
        if self.secret_reference.trim().is_empty() {
            return Err(MagstvProviderError::MissingSecretReference);
        }
        if let Some(host) = self.cdn_edge_host.as_deref() {
            let host = host.trim();
            let valid = !host.is_empty()
                && !host.contains('/')
                && !host.contains('?')
                && !host.contains('#')
                && !host.contains('@')
                && host.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')
                });
            if !valid {
                return Err(MagstvProviderError::InvalidPlaybackParameter { field: "host" });
            }
        }
        Ok(())
    }

    pub(crate) fn allows_category(&self, category_id: &str) -> bool {
        !self.excluded_category_ids.contains(category_id)
            && (self.category_ids.is_empty() || self.category_ids.contains(category_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str, secret_reference: &str) -> MagstvConfig {
        MagstvConfig {
            bootstrap_url: url.to_string(),
            secret_reference: secret_reference.to_string(),
            category_ids: BTreeSet::new(),
            excluded_category_ids: BTreeSet::new(),
            channel_limit: None,
            cdn_edge_host: None,
        }
    }

    #[test]
    fn connection_requires_https_and_a_secret_reference() {
        assert_eq!(
            config("http://example.invalid", "MAGSTV_ACCOUNT").validates_for_connection(),
            Err(MagstvProviderError::BootstrapMustUseHttps)
        );
        assert_eq!(
            config("https://example.invalid", "").validates_for_connection(),
            Err(MagstvProviderError::MissingSecretReference)
        );
    }

    #[test]
    fn bootstrap_rejects_embedded_credentials_and_fragments() {
        for url in [
            "https://user:password@example.invalid/bootstrap",
            "https://example.invalid/bootstrap#secret",
        ] {
            assert_eq!(
                config(url, "MAGSTV_ACCOUNT").validates_for_connection(),
                Err(MagstvProviderError::InvalidBootstrapUrl)
            );
        }
    }
}
