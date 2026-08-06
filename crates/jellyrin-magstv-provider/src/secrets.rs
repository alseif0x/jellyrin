use async_trait::async_trait;
use std::{collections::BTreeMap, env, fmt, sync::Arc};
use tokio::sync::RwLock;

use crate::MagstvProviderError;

/// Runtime-only credentials resolved by name. The provider never puts this
/// value into `MagstvConfig`, URLs, or ordinary debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvSecret {
    pub username: String,
    pub password: String,
    pub device_sn: Option<String>,
    pub device_user_id: Option<String>,
}

impl fmt::Debug for MagstvSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvSecret")
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("device_sn_present", &self.device_sn.is_some())
            .field("device_user_id_present", &self.device_user_id.is_some())
            .finish()
    }
}

impl MagstvSecret {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            device_sn: None,
            device_user_id: None,
        }
    }

    pub fn with_device_sn(mut self, device_sn: impl Into<String>) -> Self {
        self.device_sn = Some(device_sn.into());
        self
    }

    pub fn with_device_user_id(mut self, device_user_id: impl Into<String>) -> Self {
        self.device_user_id = Some(device_user_id.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), MagstvProviderError> {
        if self.username.trim().is_empty() || self.password.is_empty() {
            return Err(MagstvProviderError::InvalidSecret);
        }
        Ok(())
    }
}

/// Secret storage is intentionally named and asynchronous so the later DB
/// implementation can replace the in-memory test double without changing the
/// connector contract.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, reference: &str) -> Result<MagstvSecret, MagstvProviderError>;
}

/// Resolves a named MAGSTV secret from process environment variables.
///
/// The reference is only used to select variable names; values never enter
/// MagstvConfig, catalog JSON, URLs, or Debug output. This resolver is a
/// deliberately small runtime adapter until Jellyrin's encrypted secret
/// store is wired into the provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentSecretResolver;

impl EnvironmentSecretResolver {
    fn variable_prefix(reference: &str) -> Result<String, MagstvProviderError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(MagstvProviderError::MissingSecretReference);
        }

        let normalized = reference
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if normalized.trim_matches('_').is_empty() {
            return Err(MagstvProviderError::MissingSecretReference);
        }
        Ok(format!("MAGSTV_SECRET_{normalized}"))
    }

    fn required(name: &str) -> Result<String, MagstvProviderError> {
        match env::var(name) {
            Ok(value) if !value.trim().is_empty() => Ok(value),
            Ok(_) => Err(MagstvProviderError::InvalidSecret),
            Err(env::VarError::NotPresent) => Err(MagstvProviderError::SecretNotFound),
            Err(env::VarError::NotUnicode(_)) => Err(MagstvProviderError::InvalidSecret),
        }
    }

    fn optional(name: &str) -> Result<Option<String>, MagstvProviderError> {
        match env::var(name) {
            Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
            Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(MagstvProviderError::InvalidSecret),
        }
    }
}

#[async_trait]
impl SecretResolver for EnvironmentSecretResolver {
    async fn resolve(&self, reference: &str) -> Result<MagstvSecret, MagstvProviderError> {
        let prefix = Self::variable_prefix(reference)?;
        let secret = MagstvSecret {
            username: Self::required(&format!("{prefix}_USERNAME"))?,
            password: Self::required(&format!("{prefix}_PASSWORD"))?,
            device_sn: Self::optional(&format!("{prefix}_DEVICE_SN"))?,
            device_user_id: Self::optional(&format!("{prefix}_DEVICE_USER_ID"))?,
        };
        secret.validate()?;
        Ok(secret)
    }
}

#[derive(Clone, Default)]
pub struct InMemorySecretResolver {
    secrets: Arc<RwLock<BTreeMap<String, MagstvSecret>>>,
}

impl fmt::Debug for InMemorySecretResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemorySecretResolver")
            .field("secret_count", &"[REDACTED]")
            .finish()
    }
}

impl InMemorySecretResolver {
    pub async fn insert(
        &self,
        reference: impl Into<String>,
        secret: MagstvSecret,
    ) -> Result<(), MagstvProviderError> {
        secret.validate()?;
        self.secrets.write().await.insert(reference.into(), secret);
        Ok(())
    }
}

#[async_trait]
impl SecretResolver for InMemorySecretResolver {
    async fn resolve(&self, reference: &str) -> Result<MagstvSecret, MagstvProviderError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(MagstvProviderError::MissingSecretReference);
        }
        let secret = self
            .secrets
            .read()
            .await
            .get(reference)
            .cloned()
            .ok_or(MagstvProviderError::SecretNotFound)?;
        secret.validate()?;
        Ok(secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_reference_is_normalized_without_exposing_values() {
        assert_eq!(
            EnvironmentSecretResolver::variable_prefix("account-main"),
            Ok("MAGSTV_SECRET_ACCOUNT_MAIN".to_string())
        );
        assert_eq!(
            EnvironmentSecretResolver::variable_prefix("  account/main  "),
            Ok("MAGSTV_SECRET_ACCOUNT_MAIN".to_string())
        );
        assert_eq!(
            EnvironmentSecretResolver::variable_prefix("---"),
            Err(MagstvProviderError::MissingSecretReference)
        );
    }

    #[tokio::test]
    async fn resolver_round_trips_without_exposing_secret_values_in_debug() {
        let resolver = InMemorySecretResolver::default();
        let secret = MagstvSecret::new("user@example.invalid", "not-a-real-password")
            .with_device_sn("device-sn")
            .with_device_user_id("device-user");
        resolver
            .insert("MAGSTV_TEST", secret.clone())
            .await
            .expect("valid test secret");

        assert_eq!(resolver.resolve("MAGSTV_TEST").await.unwrap(), secret);
        let debug = format!("{secret:?}");
        assert!(!debug.contains("user@example.invalid"));
        assert!(!debug.contains("not-a-real-password"));
        assert!(!debug.contains("device-sn"));
    }

    #[tokio::test]
    async fn missing_and_incomplete_secrets_fail_closed() {
        let resolver = InMemorySecretResolver::default();
        assert_eq!(
            resolver.resolve("missing").await,
            Err(MagstvProviderError::SecretNotFound)
        );
        assert_eq!(
            resolver
                .insert("bad", MagstvSecret::new("", "password"))
                .await,
            Err(MagstvProviderError::InvalidSecret)
        );
    }
}
