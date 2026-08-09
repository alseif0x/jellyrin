use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use anyhow::{Context, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{Value, json};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const PROVIDER_SECRET_ENVELOPE_VERSION: u16 = 1;
const PROVIDER_SECRET_KEY_BYTES: usize = 32;
const PROVIDER_SECRET_NONCE_BYTES: usize = 12;
const PROVIDER_SECRET_MAX_PLAINTEXT_BYTES: usize = 64 * 1024;
pub const PROVIDER_SECRET_REFERENCE_FIELD: &str = "JellyrinProviderSecretRef";

#[derive(Clone)]
pub struct ProviderSecretVault {
    inner: Arc<ProviderSecretVaultInner>,
}

struct ProviderSecretVaultInner {
    active_key_id: String,
    keys: HashMap<String, Zeroizing<Vec<u8>>>,
}

impl ProviderSecretVault {
    pub fn from_base64(active_key_id: impl AsRef<str>, encoded_key: &str) -> anyhow::Result<Self> {
        let key = STANDARD
            .decode(encoded_key.trim())
            .context("provider secret key must be valid standard base64")?;
        Self::new(active_key_id, key)
    }

    pub fn new(active_key_id: impl AsRef<str>, key: Vec<u8>) -> anyhow::Result<Self> {
        let active_key_id = validate_key_id(active_key_id.as_ref())?;
        let key = Zeroizing::new(key);
        validate_key(&key)?;
        let mut keys = HashMap::new();
        keys.insert(active_key_id.clone(), key);
        Ok(Self {
            inner: Arc::new(ProviderSecretVaultInner {
                active_key_id,
                keys,
            }),
        })
    }

    /// Loads an operational keyring. The active key encrypts new writes while every other key is
    /// decryption-only, allowing online re-encryption before an old key is removed.
    pub fn from_keyring_json(payload: &str) -> anyhow::Result<Self> {
        // serde_json owns decoded strings separately from the protected input buffer. Keep the
        // parsed tree behind a zeroizing guard so every copied key value is scrubbed on success
        // and on every early error path.
        let document = ZeroizingJson(
            serde_json::from_str(payload).context("provider secret keyring must be valid JSON")?,
        );
        let active_key_id = document
            .0
            .get("active_key_id")
            .and_then(Value::as_str)
            .context("provider secret keyring active_key_id is required")?;
        let keys = document
            .0
            .get("keys")
            .and_then(Value::as_object)
            .context("provider secret keyring keys object is required")?;
        ensure!(
            !keys.is_empty() && keys.len() <= 32,
            "provider secret keyring must contain between 1 and 32 keys"
        );
        let active_encoded = keys
            .get(active_key_id)
            .and_then(Value::as_str)
            .context("provider secret keyring active key is unavailable")?;
        let mut vault = Self::from_base64(active_key_id, active_encoded)?;
        for (key_id, encoded_key) in keys {
            if key_id == active_key_id {
                continue;
            }
            let encoded_key = encoded_key
                .as_str()
                .context("provider secret keyring values must be base64 strings")?;
            let key = STANDARD
                .decode(encoded_key.trim())
                .context("provider secret keyring contains invalid base64")?;
            vault = vault.with_decryption_key(key_id, key)?;
        }
        Ok(vault)
    }

    pub fn with_decryption_key(
        mut self,
        key_id: impl AsRef<str>,
        key: Vec<u8>,
    ) -> anyhow::Result<Self> {
        let key_id = validate_key_id(key_id.as_ref())?;
        let key = Zeroizing::new(key);
        validate_key(&key)?;
        let inner = Arc::get_mut(&mut self.inner)
            .context("provider secret vault cannot be extended after it has been cloned")?;
        inner.keys.insert(key_id, key);
        Ok(self)
    }

    pub fn active_key_id(&self) -> &str {
        &self.inner.active_key_id
    }

    pub fn seal(
        &self,
        secret_id: &str,
        provider_type: &str,
        credentials: &ProviderCredentials,
    ) -> anyhow::Result<ProviderSecretEnvelope> {
        let secret_id = validate_secret_component("secret id", secret_id)?;
        let provider_type = validate_secret_component("provider type", provider_type)?;
        let key_id = self.inner.active_key_id.clone();
        let key = self
            .inner
            .keys
            .get(&key_id)
            .context("active provider secret key is unavailable")?;
        let unbound = UnboundKey::new(&AES_256_GCM, key.as_slice())
            .map_err(|_| anyhow::anyhow!("provider secret encryption key is invalid"))?;
        let key = LessSafeKey::new(unbound);

        let mut nonce_bytes = [0_u8; PROVIDER_SECRET_NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| anyhow::anyhow!("secure provider secret nonce generation failed"))?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let aad = provider_secret_aad(
            PROVIDER_SECRET_ENVELOPE_VERSION,
            &key_id,
            &secret_id,
            &provider_type,
        );
        let mut plaintext = Zeroizing::new(
            serde_json::to_vec(&json!({
                "username": credentials.username(),
                "password": credentials.password(),
            }))
            .context("failed to encode provider credentials")?,
        );
        ensure!(
            plaintext.len() <= PROVIDER_SECRET_MAX_PLAINTEXT_BYTES,
            "provider credentials exceed the maximum supported size"
        );
        key.seal_in_place_append_tag(nonce, Aad::from(aad.as_slice()), &mut *plaintext)
            .map_err(|_| anyhow::anyhow!("provider secret encryption failed"))?;

        Ok(ProviderSecretEnvelope {
            version: PROVIDER_SECRET_ENVELOPE_VERSION,
            key_id,
            nonce: nonce_bytes,
            ciphertext: plaintext.to_vec(),
        })
    }

    pub fn open(
        &self,
        secret_id: &str,
        provider_type: &str,
        envelope: &ProviderSecretEnvelope,
    ) -> anyhow::Result<ProviderCredentials> {
        ensure!(
            envelope.version == PROVIDER_SECRET_ENVELOPE_VERSION,
            "unsupported provider secret envelope version"
        );
        let secret_id = validate_secret_component("secret id", secret_id)?;
        let provider_type = validate_secret_component("provider type", provider_type)?;
        let raw_key = self
            .inner
            .keys
            .get(&envelope.key_id)
            .context("provider secret key id is unavailable")?;
        let unbound = UnboundKey::new(&AES_256_GCM, raw_key.as_slice())
            .map_err(|_| anyhow::anyhow!("provider secret decryption key is invalid"))?;
        let key = LessSafeKey::new(unbound);
        let aad = provider_secret_aad(
            envelope.version,
            &envelope.key_id,
            &secret_id,
            &provider_type,
        );
        let mut plaintext = Zeroizing::new(envelope.ciphertext.clone());
        let opened = key
            .open_in_place(
                Nonce::assume_unique_for_key(envelope.nonce),
                Aad::from(aad.as_slice()),
                &mut plaintext,
            )
            .map_err(|_| anyhow::anyhow!("provider secret authentication failed"))?;
        let payload = ZeroizingJson(
            serde_json::from_slice(opened).context("decrypted provider credentials are invalid")?,
        );
        let username = payload
            .0
            .get("username")
            .and_then(Value::as_str)
            .context("decrypted provider username is unavailable")?;
        let password = payload
            .0
            .get("password")
            .and_then(Value::as_str)
            .context("decrypted provider password is unavailable")?;
        ProviderCredentials::new(username, password)
    }

    #[cfg(any(test, feature = "sqlite"))]
    pub(crate) fn for_legacy_test_harness() -> Self {
        Self::new("test-v1", vec![0x5a; PROVIDER_SECRET_KEY_BYTES])
            .expect("test provider secret key must be valid")
    }
}

impl fmt::Debug for ProviderSecretVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSecretVault")
            .field("active_key_id", &self.inner.active_key_id)
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSecretEnvelope {
    pub version: u16,
    pub key_id: String,
    pub nonce: [u8; PROVIDER_SECRET_NONCE_BYTES],
    pub ciphertext: Vec<u8>,
}

impl fmt::Debug for ProviderSecretEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSecretEnvelope")
            .field("version", &self.version)
            .field("key_id", &self.key_id)
            .field("nonce", &"[REDACTED]")
            .field("ciphertext", &"[REDACTED]")
            .finish()
    }
}

#[derive(PartialEq, Eq)]
pub struct ProviderCredentials {
    username: String,
    password: String,
}

impl ProviderCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> anyhow::Result<Self> {
        Self::from_protected_parts(
            Zeroizing::new(username.into()),
            Zeroizing::new(password.into()),
        )
    }

    pub(crate) fn from_protected_parts(
        mut username: Zeroizing<String>,
        mut password: Zeroizing<String>,
    ) -> anyhow::Result<Self> {
        ensure!(
            !username.trim().is_empty(),
            "provider username must not be empty"
        );
        ensure!(!password.is_empty(), "provider password must not be empty");
        ensure!(
            username.len() + password.len() <= PROVIDER_SECRET_MAX_PLAINTEXT_BYTES,
            "provider credentials exceed the maximum supported size"
        );
        Ok(Self {
            username: std::mem::take(&mut *username),
            password: std::mem::take(&mut *password),
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub(crate) fn protected_username_copy(&self) -> Zeroizing<String> {
        Zeroizing::new(self.username.clone())
    }

    pub(crate) fn protected_password_copy(&self) -> Zeroizing<String> {
        Zeroizing::new(self.password.clone())
    }

    /// Transfers both allocations into another zeroizing credential container without cloning.
    pub fn into_parts(mut self) -> (String, String) {
        (
            std::mem::take(&mut self.username),
            std::mem::take(&mut self.password),
        )
    }
}

impl fmt::Debug for ProviderCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentials")
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ProviderCredentials {
    fn drop(&mut self) {
        self.username.zeroize();
        self.password.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSecretReference {
    pub id: String,
    pub provider_type: String,
    pub revision: i64,
}

impl ProviderSecretReference {
    pub fn to_json(&self) -> Value {
        json!({
            "Id": self.id,
            "Provider": self.provider_type,
            "Revision": self.revision,
        })
    }

    pub fn from_configuration(configuration: &Value) -> Option<Self> {
        let reference = configuration.get(PROVIDER_SECRET_REFERENCE_FIELD)?;
        Self::from_json(reference)
    }

    fn from_json(reference: &Value) -> Option<Self> {
        let id = reference.get("Id")?.as_str()?.trim();
        let provider_type = reference.get("Provider")?.as_str()?.trim();
        let revision = reference.get("Revision")?.as_i64()?;
        if id.is_empty() || provider_type.is_empty() || revision <= 0 {
            return None;
        }
        Some(Self {
            id: id.to_owned(),
            provider_type: provider_type.to_owned(),
            revision,
        })
    }
}

/// Returns whether a persisted configuration contains a reference to the same vault envelope.
///
/// The revision is deliberately not part of the identity check: an older persisted revision still
/// depends on the current envelope with the same id/provider pair and must prevent garbage
/// collection. Walking nested objects also covers the `TunerHosts` array in the named Live TV
/// configuration without relying on backend-specific JSON-path expressions.
pub(crate) fn configuration_references_provider_secret(
    configuration: &Value,
    reference: &ProviderSecretReference,
) -> bool {
    match configuration {
        Value::Array(values) => values
            .iter()
            .any(|value| configuration_references_provider_secret(value, reference)),
        Value::Object(object) => {
            let directly_references_envelope = object.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case(PROVIDER_SECRET_REFERENCE_FIELD)
                    && value
                        .get("Id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.trim() == reference.id)
                    && value
                        .get("Provider")
                        .and_then(Value::as_str)
                        .is_some_and(|provider| {
                            provider
                                .trim()
                                .eq_ignore_ascii_case(&reference.provider_type)
                        })
            });
            directly_references_envelope
                || object
                    .values()
                    .any(|value| configuration_references_provider_secret(value, reference))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// Collects every persisted vault envelope identity referenced by a configuration.
///
/// A malformed reference aborts reconciliation instead of being treated as absent. This is
/// intentionally stricter than point lookups: garbage collection must retain too much rather
/// than delete an envelope that an unrecognised persisted shape may still need. Revisions are
/// omitted because every revision for an id/provider pair depends on the same envelope row.
pub(crate) fn collect_provider_secret_reference_identities(
    configuration: &Value,
    references: &mut HashSet<(String, String)>,
) -> anyhow::Result<()> {
    match configuration {
        Value::Array(values) => {
            for value in values {
                collect_provider_secret_reference_identities(value, references)?;
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if key.eq_ignore_ascii_case(PROVIDER_SECRET_REFERENCE_FIELD) {
                    let reference = ProviderSecretReference::from_json(value)
                        .context("persisted provider secret reference is invalid")?;
                    references.insert((reference.id, reference.provider_type.to_ascii_lowercase()));
                }
                collect_provider_secret_reference_identities(value, references)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

type ProtectedProviderCredentialInput = (Option<Zeroizing<String>>, Option<Zeroizing<String>>);

pub(crate) fn provider_credentials_from_configuration(
    configuration: &Value,
) -> anyhow::Result<Option<ProtectedProviderCredentialInput>> {
    let Some(object) = configuration.as_object() else {
        return Ok(None);
    };
    let username = object.iter().find_map(|(key, value)| {
        (key.eq_ignore_ascii_case("Username") || key.eq_ignore_ascii_case("UserName"))
            .then(|| value.as_str())
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "********")
            .map(|value| Zeroizing::new(value.to_owned()))
    });
    let password = object.iter().find_map(|(key, value)| {
        key.eq_ignore_ascii_case("Password")
            .then(|| value.as_str())
            .flatten()
            .filter(|value| !value.is_empty() && *value != "********")
            .map(|value| Zeroizing::new(value.to_owned()))
    });
    if username.is_none() && password.is_none() {
        return Ok(None);
    }
    Ok(Some((username, password)))
}

pub(crate) fn redacted_provider_configuration(
    mut configuration: Value,
    reference: &ProviderSecretReference,
) -> anyhow::Result<Value> {
    let object = configuration
        .as_object_mut()
        .context("provider configuration must be an object")?;
    zeroize_provider_credential_fields(object);
    object.retain(|key, _| {
        !key.eq_ignore_ascii_case("Username")
            && !key.eq_ignore_ascii_case("UserName")
            && !key.eq_ignore_ascii_case("Password")
    });
    object.insert(
        PROVIDER_SECRET_REFERENCE_FIELD.to_string(),
        reference.to_json(),
    );
    object.insert("CredentialsConfigured".to_string(), Value::Bool(true));
    object.insert(
        "JellyrinConfigurationRevision".to_string(),
        Value::String(format!(
            "provider-secret:{}:{}",
            reference.id, reference.revision
        )),
    );
    Ok(configuration)
}

fn zeroize_provider_credential_fields(object: &mut serde_json::Map<String, Value>) {
    for (key, value) in object.iter_mut() {
        if key.eq_ignore_ascii_case("Username")
            || key.eq_ignore_ascii_case("UserName")
            || key.eq_ignore_ascii_case("Password")
        {
            zeroize_json_strings(value);
        }
    }
}

pub(crate) fn resolved_provider_configuration(
    mut configuration: Value,
    reference: &ProviderSecretReference,
    credentials: &ProviderCredentials,
) -> anyhow::Result<Value> {
    configuration = redacted_provider_configuration(configuration, reference)?;
    let object = configuration
        .as_object_mut()
        .context("provider configuration must be an object")?;
    object.insert(
        "Username".to_string(),
        Value::String(credentials.username().to_owned()),
    );
    object.insert(
        "Password".to_string(),
        Value::String(credentials.password().to_owned()),
    );
    Ok(configuration)
}

pub(crate) fn new_provider_secret_id() -> String {
    format!("ps_{}", Uuid::new_v4().simple())
}

pub(crate) fn normalize_provider_type(provider_type: &str) -> anyhow::Result<String> {
    let provider_type = provider_type.trim().to_ascii_lowercase();
    ensure!(
        !provider_type.is_empty()
            && provider_type.len() <= 128
            && provider_type
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_')),
        "provider type is invalid"
    );
    Ok(provider_type)
}

/// Derives the provider-secrets namespace for a Live TV tuner without changing the tuner's
/// persisted `Type`. External plugins use `plugin:<id>` (or `plugin` plus `PluginId`), while the
/// vault deliberately stores a normalized `plugin-...` namespace accepted by
/// [`normalize_provider_type`].
pub fn provider_secret_namespace_for_configuration(
    provider_type: &str,
    configuration: &Value,
) -> anyhow::Result<String> {
    let reference = ProviderSecretReference::from_configuration(configuration);
    ensure!(
        !configuration_has_provider_secret_reference_field(configuration) || reference.is_some(),
        "provider secret reference is invalid"
    );
    let namespace = derive_provider_secret_namespace(provider_type, configuration)?;
    if let Some(reference) = reference {
        ensure!(
            reference.provider_type.eq_ignore_ascii_case(&namespace),
            "provider secret reference belongs to a different provider"
        );
    }
    Ok(namespace)
}

fn derive_provider_secret_namespace(
    provider_type: &str,
    configuration: &Value,
) -> anyhow::Result<String> {
    let provider_type = provider_type.trim();
    ensure!(
        !provider_type.is_empty()
            && provider_type.len() <= 512
            && !provider_type.chars().any(char::is_control),
        "provider type is invalid"
    );

    let (kind, inline_plugin_id) = provider_type
        .split_once(':')
        .map_or((provider_type, None), |(kind, id)| (kind, Some(id)));
    if !kind.eq_ignore_ascii_case("plugin") {
        return normalize_provider_type(provider_type);
    }

    let configured_plugin_id = configuration.as_object().and_then(|object| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PluginId"))
            .map(|(_, value)| value)
    });
    let configured_plugin_id = match configured_plugin_id {
        Some(Value::String(value)) => Some(value.trim()),
        Some(_) => anyhow::bail!("plugin id must be a string"),
        None => None,
    };
    let inline_plugin_id = inline_plugin_id
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let (Some(inline), Some(configured)) = (inline_plugin_id, configured_plugin_id) {
        ensure!(
            plugin_identity_key(inline)? == plugin_identity_key(configured)?,
            "plugin type and PluginId identify different plugins"
        );
    }

    let plugin_id = inline_plugin_id
        .or(configured_plugin_id)
        .context("plugin id is required for provider credentials")?;
    let identity = plugin_identity_key(plugin_id)?;
    let namespace = if identity.len() <= 121
        && identity
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
    {
        format!("plugin-{identity}")
    } else {
        let hash = digest(&SHA256, identity.as_bytes());
        let mut encoded = String::with_capacity(hash.as_ref().len() * 2);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in hash.as_ref() {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        format!("plugin-sha256-{encoded}")
    };
    normalize_provider_type(&namespace)
}

fn plugin_identity_key(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
        "plugin id is invalid"
    );
    Ok(value.to_lowercase())
}

pub(crate) fn inherit_provider_secret_reference(
    configuration: &mut Value,
    existing: Option<&Value>,
) {
    // A client-supplied malformed field must fail validation in the protection path. Replacing it
    // with the existing reference here would turn malformed/untrusted input into a valid write.
    if configuration_has_provider_secret_reference_field(configuration) {
        return;
    }
    let Some(reference) = existing
        .and_then(|value| value.get(PROVIDER_SECRET_REFERENCE_FIELD))
        .cloned()
    else {
        return;
    };
    if let Some(object) = configuration.as_object_mut() {
        object.insert(PROVIDER_SECRET_REFERENCE_FIELD.to_string(), reference);
    }
}

/// Inherits a Live TV reference only when the submitted provider identity still resolves to the
/// same vault namespace. A tuner may move to another plugin only with a complete new credential
/// pair, in which case the old reference is intentionally not inherited.
pub(crate) fn inherit_provider_secret_reference_for_configuration(
    configuration: &mut Value,
    existing: Option<&Value>,
    provider_type: &str,
) -> anyhow::Result<()> {
    if configuration_has_provider_secret_reference_field(configuration) {
        return Ok(());
    }
    let Some(existing) = existing else {
        return Ok(());
    };
    let existing_has_reference = configuration_has_provider_secret_reference_field(existing);
    let existing_reference = ProviderSecretReference::from_configuration(existing);
    ensure!(
        !existing_has_reference || existing_reference.is_some(),
        "persisted provider secret reference is invalid"
    );
    let Some(existing_reference) = existing_reference else {
        return Ok(());
    };

    let mut identity_configuration = configuration.clone();
    set_provider_secret_reference(&mut identity_configuration, &existing_reference)?;
    let expected_namespace =
        derive_provider_secret_namespace(provider_type, &identity_configuration)?;
    if existing_reference
        .provider_type
        .eq_ignore_ascii_case(&expected_namespace)
    {
        set_provider_secret_reference(configuration, &existing_reference)?;
        return Ok(());
    }

    let has_complete_replacement = matches!(
        provider_credentials_from_configuration(configuration)?,
        Some((Some(_), Some(_)))
    );
    ensure!(
        has_complete_replacement,
        "provider identity changed; complete provider credentials are required"
    );
    Ok(())
}

pub(crate) fn set_provider_secret_reference(
    configuration: &mut Value,
    reference: &ProviderSecretReference,
) -> anyhow::Result<()> {
    let object = configuration
        .as_object_mut()
        .context("provider configuration must be an object")?;
    object.insert(
        PROVIDER_SECRET_REFERENCE_FIELD.to_string(),
        reference.to_json(),
    );
    Ok(())
}

pub(crate) fn configuration_has_provider_secret_material(configuration: &Value) -> bool {
    configuration_has_provider_secret_reference_field(configuration)
        || provider_credentials_from_configuration(configuration)
            .ok()
            .flatten()
            .is_some()
}

pub(crate) fn configuration_has_provider_secret_input_field(configuration: &Value) -> bool {
    configuration.as_object().is_some_and(|object| {
        object.keys().any(|key| {
            key.eq_ignore_ascii_case(PROVIDER_SECRET_REFERENCE_FIELD)
                || key.eq_ignore_ascii_case("Username")
                || key.eq_ignore_ascii_case("UserName")
                || key.eq_ignore_ascii_case("Password")
        })
    })
}

pub(crate) fn configuration_has_provider_secret_reference_field(configuration: &Value) -> bool {
    configuration.as_object().is_some_and(|object| {
        object
            .keys()
            .any(|key| key.eq_ignore_ascii_case(PROVIDER_SECRET_REFERENCE_FIELD))
    })
}

fn provider_secret_aad(
    version: u16,
    key_id: &str,
    secret_id: &str,
    provider_type: &str,
) -> Vec<u8> {
    format!("jellyrin:provider-secret:{version}:{key_id}:{provider_type}:{secret_id}").into_bytes()
}

fn validate_key_id(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "provider secret key id is invalid"
    );
    Ok(value.to_owned())
}

fn validate_key(key: &[u8]) -> anyhow::Result<()> {
    ensure!(
        key.len() == PROVIDER_SECRET_KEY_BYTES,
        "provider secret key must decode to exactly {PROVIDER_SECRET_KEY_BYTES} bytes"
    );
    Ok(())
}

fn validate_secret_component(name: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(
        !value.is_empty()
            && value.len() <= 512
            && !value.chars().any(char::is_control)
            && !value.contains(':'),
        "provider {name} is invalid"
    );
    Ok(value.to_ascii_lowercase())
}

struct ZeroizingJson(Value);

impl Drop for ZeroizingJson {
    fn drop(&mut self) {
        zeroize_json_strings(&mut self.0);
    }
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                zeroize_json_strings(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                zeroize_json_strings(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ProviderCredentials, ProviderSecretReference, ProviderSecretVault,
        collect_provider_secret_reference_identities, provider_credentials_from_configuration,
        provider_secret_namespace_for_configuration, redacted_provider_configuration,
        zeroize_provider_credential_fields,
    };

    #[test]
    fn reference_collection_is_nested_exact_and_fails_closed() {
        let mut references = HashSet::new();
        collect_provider_secret_reference_identities(
            &json!({
                "nested": [{
                    "JellyrinProviderSecretRef": {
                        "Id": "ps_one",
                        "Provider": "MAGSTV",
                        "Revision": 1
                    }
                }],
                "jellyrinprovidersecretref": {
                    "Id": "ps_one",
                    "Provider": "magstv",
                    "Revision": 9
                }
            }),
            &mut references,
        )
        .unwrap();
        assert_eq!(references.len(), 1);
        assert!(references.contains(&("ps_one".to_owned(), "magstv".to_owned())));

        let error = collect_provider_secret_reference_identities(
            &json!({
                "JellyrinProviderSecretRef": {
                    "Id": "ps_unknown",
                    "Provider": "xtream"
                }
            }),
            &mut references,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("reference is invalid"));
    }

    #[test]
    fn ciphertext_does_not_contain_credentials_and_wrong_key_fails_closed() {
        let credentials =
            ProviderCredentials::new("alice", "correct horse battery staple").unwrap();
        let vault = ProviderSecretVault::new("primary", vec![0x11; 32]).unwrap();
        let envelope = vault.seal("secret-1", "xtream", &credentials).unwrap();
        let ciphertext = String::from_utf8_lossy(&envelope.ciphertext);
        assert!(!ciphertext.contains("alice"));
        assert!(!ciphertext.contains("correct horse battery staple"));

        let opened = vault.open("secret-1", "xtream", &envelope).unwrap();
        assert_eq!(opened.username(), "alice");
        assert_eq!(opened.password(), "correct horse battery staple");

        let wrong = ProviderSecretVault::new("primary", vec![0x22; 32]).unwrap();
        let error = wrong
            .open("secret-1", "xtream", &envelope)
            .unwrap_err()
            .to_string();
        assert!(error.contains("authentication failed"));
        assert!(!error.contains("alice"));
        assert!(!error.contains("correct horse battery staple"));
    }

    #[test]
    fn provider_credentials_can_transfer_allocations_without_cloning() {
        let credentials = ProviderCredentials::new("alice", "secret").unwrap();
        let (mut username, mut password) = credentials.into_parts();
        assert_eq!(username, "alice");
        assert_eq!(password, "secret");
        use zeroize::Zeroize as _;
        username.zeroize();
        password.zeroize();
        assert!(username.is_empty());
        assert!(password.is_empty());
    }

    #[test]
    fn submitted_provider_credentials_stay_protected_and_transfer_without_cloning() {
        let submitted = provider_credentials_from_configuration(&json!({
            "uSeRnAmE": "  alice  ",
            "pAsSwOrD": "correct horse battery staple"
        }))
        .unwrap()
        .unwrap();
        let (username, password) = submitted;
        let username = username.unwrap();
        let password = password.unwrap();
        let username_pointer = username.as_ptr();
        let password_pointer = password.as_ptr();

        let credentials = ProviderCredentials::from_protected_parts(username, password).unwrap();

        assert_eq!(credentials.username(), "alice");
        assert_eq!(credentials.password(), "correct horse battery staple");
        assert_eq!(credentials.username().as_ptr(), username_pointer);
        assert_eq!(credentials.password().as_ptr(), password_pointer);
    }

    #[test]
    fn provider_redaction_zeroizes_every_removed_credential_value() {
        let mut configuration = json!({
            "Username": {"nested": "alice"},
            "UserName": ["alias", {"nested": "second-alias"}],
            "PASSWORD": "correct horse battery staple",
            "Url": "https://provider.example"
        });
        zeroize_provider_credential_fields(configuration.as_object_mut().unwrap());
        assert_eq!(configuration["Username"]["nested"], "");
        assert_eq!(configuration["UserName"][0], "");
        assert_eq!(configuration["UserName"][1]["nested"], "");
        assert_eq!(configuration["PASSWORD"], "");
        assert_eq!(configuration["Url"], "https://provider.example");

        let reference = ProviderSecretReference {
            id: "ps_test".to_string(),
            provider_type: "xtream".to_string(),
            revision: 1,
        };
        let redacted = redacted_provider_configuration(configuration, &reference).unwrap();
        assert!(redacted.get("Username").is_none());
        assert!(redacted.get("UserName").is_none());
        assert!(redacted.get("PASSWORD").is_none());
        assert_eq!(redacted["Url"], "https://provider.example");
    }

    #[test]
    fn aad_binds_ciphertext_to_secret_identity_and_provider() {
        let credentials = ProviderCredentials::new("alice", "secret").unwrap();
        let vault = ProviderSecretVault::new("primary", vec![0x33; 32]).unwrap();
        let envelope = vault.seal("secret-1", "xtream", &credentials).unwrap();

        assert!(vault.open("secret-2", "xtream", &envelope).is_err());
        assert!(
            vault
                .open("secret-1", "another-provider", &envelope)
                .is_err()
        );
    }

    #[test]
    fn keyring_keeps_old_keys_for_decryption_and_redacts_debug_output() {
        let old_key = vec![0x44; 32];
        let new_key = vec![0x55; 32];
        let old_vault = ProviderSecretVault::new("old", old_key.clone()).unwrap();
        let credentials = ProviderCredentials::new("alice", "secret").unwrap();
        let envelope = old_vault
            .seal("rotation-secret", "xtream", &credentials)
            .unwrap();
        let keyring = serde_json::json!({
            "active_key_id": "new",
            "keys": {
                "new": STANDARD.encode(&new_key),
                "old": STANDARD.encode(&old_key),
            }
        });
        let vault = ProviderSecretVault::from_keyring_json(&keyring.to_string()).unwrap();

        assert_eq!(vault.active_key_id(), "new");
        assert_eq!(
            vault
                .open("rotation-secret", "xtream", &envelope)
                .unwrap()
                .password(),
            "secret"
        );
        let debug = format!("{vault:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&STANDARD.encode(old_key)));
        assert!(!debug.contains(&STANDARD.encode(new_key)));
    }

    #[test]
    fn plugin_secret_namespace_is_stable_bounded_and_rejects_ambiguous_identity() {
        let plugin_id = Uuid::new_v4();
        let upper = plugin_id.to_string().to_ascii_uppercase();
        let expected = format!("plugin-{plugin_id}");
        assert_eq!(
            provider_secret_namespace_for_configuration(
                &format!("PLUGIN:{upper}"),
                &json!({"PluginId": plugin_id.to_string()}),
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            provider_secret_namespace_for_configuration("plugin", &json!({"PluginId": plugin_id}),)
                .unwrap(),
            expected
        );

        let hashed_upper = provider_secret_namespace_for_configuration(
            "plugin",
            &json!({"PluginId": "Vendor.Plugin/Á"}),
        )
        .unwrap();
        let hashed_lower = provider_secret_namespace_for_configuration(
            "PLUGIN",
            &json!({"PluginId": "vendor.plugin/á"}),
        )
        .unwrap();
        assert_eq!(hashed_upper, hashed_lower);
        assert!(hashed_upper.starts_with("plugin-sha256-"));
        assert!(hashed_upper.len() <= 128);

        assert!(
            provider_secret_namespace_for_configuration(
                &format!("plugin:{plugin_id}"),
                &json!({"PluginId": Uuid::new_v4()}),
            )
            .unwrap_err()
            .to_string()
            .contains("different plugins")
        );
        assert!(
            provider_secret_namespace_for_configuration(
                &format!("plugin:{plugin_id}"),
                &json!({
                    "PluginId": plugin_id,
                    "JellyrinProviderSecretRef": {
                        "Id": "ps_foreign",
                        "Provider": format!("plugin-{}", Uuid::new_v4()),
                        "Revision": 1
                    }
                }),
            )
            .unwrap_err()
            .to_string()
            .contains("different provider")
        );
        assert!(
            provider_secret_namespace_for_configuration(
                "plugin",
                &json!({"PluginId": "bad\nplugin"}),
            )
            .is_err()
        );
    }
}
