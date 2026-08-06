//! MAGSTV portal request/response codec.
//!
//! The Android portal interceptor recovered from the runtime DEX performs a
//! small, independent transformation before OkHttp sends a portal-core POST:
//! common parameters are merged into the DTO, the JSON is encrypted with
//! `DESede/ECB/PKCS5Padding`, standard Base64 is applied, and every Base64
//! character is written as hexadecimal ASCII. Responses from the encrypted
//! portal methods use the inverse operation on their `data` member.
//!
//! This is the portal transport contract only. It does not implement the
//! native playback `sign2`/`sign_o3` or the public EPG MD5.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use des::{
    TdesEde3,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{env, fmt, fmt::Write as _};

use crate::{
    CodecFailureKind, CodecVerification, MAGSTV_APP_ID, MAGSTV_APP_VERSION,
    MAGSTV_PORTAL_CONTENT_TYPE, MagstvPortalEndpoint, MagstvProviderError, PortalCodec,
    PortalOperation, PortalRequest, PortalResponse, VerifiedWireRequest,
};

const PORTAL_KEY_DERIVATION_MASK: &str = "*&@!6d5d-c483-4720-bb29-785b8f321c^%";
pub const MAGSTV_PORTAL_KEY_METADATA_ENV: &str = "MAGSTV_PORTAL_KEY_METADATA";

/// SHA-256 of the sanitised static portal contract descriptor. It identifies
/// the reviewed codec revision; it is not a captured request or response.
const PORTAL_CONTRACT_REVISION: &str =
    "e649d2774be9222899e25fc9e3f25dcc482dc09a7609822761c2fd762f27562f";

const VERIFIED_OPERATIONS: [PortalOperation; 12] = [
    PortalOperation::Authenticate,
    PortalOperation::GetAuthInfo,
    PortalOperation::GetSlbInfo,
    PortalOperation::ListLiveCategories,
    PortalOperation::ListLiveChannels,
    PortalOperation::ListPrograms,
    PortalOperation::ResolvePlayback,
    PortalOperation::RefreshSession,
    PortalOperation::ListMovies,
    PortalOperation::ListSeries,
    PortalOperation::ListEpisodes,
    PortalOperation::ResolveVodPlayback,
];

/// Runtime 3DES key used by the portal interceptor. Its Debug implementation
/// is deliberately redacted because the key is derived from APK metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvPortalKey([u8; 24]);

impl fmt::Debug for MagstvPortalKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvPortalKey")
            .field("material", &"[REDACTED]")
            .finish()
    }
}

impl MagstvPortalKey {
    /// Derives the effective portal key from the APK `PORTAL_KEY` metadata
    /// value. The metadata value must be supplied by the local installation;
    /// it is intentionally not embedded in the plugin.
    pub fn from_manifest_hex(manifest_value: &str) -> Result<Self, MagstvProviderError> {
        let encoded_material = decode_hex_utf8(manifest_value)?;
        let encrypted_material = decode_app_base64(&encoded_material)?;
        let mask = decode_app_base64(PORTAL_KEY_DERIVATION_MASK)?;
        let material = des3_decrypt(&encrypted_material, first_24(&mask)?)?;
        let material = std::str::from_utf8(&material)
            .map_err(|_| codec_error(CodecFailureKind::InvalidEncoding))?;
        Self::from_key_material(material)
    }

    /// Builds a key from the intermediate Base64 key material returned by the
    /// app's metadata derivation step. This is useful when the host keeps APK
    /// metadata in a separate local secret store.
    pub fn from_key_material(material: &str) -> Result<Self, MagstvProviderError> {
        let decoded = decode_app_base64(material)?;
        Ok(Self(*first_24(&decoded)?))
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 24]) -> Self {
        Self(bytes)
    }
}

/// Values inserted by the app's `ld/b` interceptor before the endpoint DTO.
/// Keep this object runtime-only: several fields are device identifiers.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvCommonParams {
    pub login_type: String,
    pub app_language: String,
    pub apk_version: String,
    pub sys_version: String,
    pub app_id: String,
    pub hardware_info: String,
    pub model: String,
    pub product: String,
    pub cpu: String,
    #[serde(rename = "B29")]
    pub b29: String,
    pub reserve1: String,
    pub portal_code: String,
    pub device_token: String,
    pub sn: String,
    pub sdk_ver: i32,
}

impl fmt::Debug for MagstvCommonParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvCommonParams")
            .field("field_count", &15)
            .field("runtime_values", &"[REDACTED]")
            .finish()
    }
}

impl MagstvCommonParams {
    /// Builds the common interceptor fields from runtime-only configuration.
    ///
    /// The defaults are inert Jellyrin identity values; deployments can
    /// override every device-sensitive field without changing the provider
    /// binary. No account secret is read here.
    pub fn from_environment() -> Result<Self, MagstvProviderError> {
        let device_id = runtime_string("MAGSTV_DEVICE_ID", "jellyrin");
        let sdk_ver = match env::var("MAGSTV_SDK_VER") {
            Ok(value) => value
                .trim()
                .parse::<i32>()
                .map_err(|_| MagstvProviderError::InvalidRuntimeConfiguration)?,
            Err(env::VarError::NotPresent) => 35,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(MagstvProviderError::InvalidRuntimeConfiguration);
            }
        };

        Ok(Self {
            // The MAGSTV product adapter reports product type 2, which the
            // runtime maps to loginType "2" during application startup.
            login_type: runtime_string("MAGSTV_LOGIN_TYPE", "2"),
            app_language: runtime_string("MAGSTV_APP_LANGUAGE", "es"),
            apk_version: MAGSTV_APP_VERSION.to_string(),
            sys_version: runtime_string("MAGSTV_SYS_VERSION", "Android"),
            app_id: runtime_string("MAGSTV_APP_ID", MAGSTV_APP_ID),
            hardware_info: runtime_string("MAGSTV_HARDWARE", &device_id),
            model: runtime_string("MAGSTV_MODEL", "Jellyrin"),
            product: runtime_string("MAGSTV_PRODUCT", "Jellyrin"),
            cpu: runtime_string("MAGSTV_CPU", std::env::consts::ARCH),
            b29: runtime_string("MAGSTV_B29", ""),
            reserve1: runtime_string("MAGSTV_RESERVE1", ""),
            portal_code: runtime_string("MAGSTV_PORTAL_CODE", ""),
            device_token: runtime_string("MAGSTV_DEVICE_TOKEN", ""),
            sn: runtime_string("MAGSTV_SN", ""),
            sdk_ver,
        })
    }

    /// Builds common parameters with the version discovered from the public
    /// update service. The version is validated as a decimal protocol code so
    /// it cannot turn into an arbitrary header value.
    pub fn from_environment_with_app_version(
        app_version: impl AsRef<str>,
    ) -> Result<Self, MagstvProviderError> {
        let app_version = app_version.as_ref().trim();
        if app_version.is_empty()
            || app_version.len() > 12
            || !app_version.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(MagstvProviderError::InvalidRuntimeConfiguration);
        }
        let mut common = Self::from_environment()?;
        common.apk_version = app_version.to_string();
        Ok(common)
    }
}

/// A verified implementation of the portal-core codec recovered from the
/// runtime interceptor.
#[derive(Clone)]
pub struct MagstvPortalCodec {
    key: MagstvPortalKey,
    common: MagstvCommonParams,
    verification: CodecVerification,
}

impl fmt::Debug for MagstvPortalCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvPortalCodec")
            .field("key", &"[REDACTED]")
            .field("common", &"[REDACTED]")
            .field("contract_verified", &self.verification.is_verified())
            .finish()
    }
}

impl MagstvPortalCodec {
    pub fn new(
        key: MagstvPortalKey,
        common: MagstvCommonParams,
    ) -> Result<Self, MagstvProviderError> {
        let verification =
            CodecVerification::verified_contract(PORTAL_CONTRACT_REVISION, VERIFIED_OPERATIONS)?;
        Ok(Self {
            key,
            common,
            verification,
        })
    }

    pub fn from_manifest_hex(
        manifest_value: &str,
        common: MagstvCommonParams,
    ) -> Result<Self, MagstvProviderError> {
        Self::new(MagstvPortalKey::from_manifest_hex(manifest_value)?, common)
    }

    /// Enables the reviewed codec from local runtime configuration. The APK
    /// metadata value is intentionally supplied by the operator and is never
    /// stored in the plugin or catalog database.
    pub fn from_environment() -> Result<Self, MagstvProviderError> {
        let manifest_value = env::var(MAGSTV_PORTAL_KEY_METADATA_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(MagstvProviderError::ProtocolUnverified)?;
        Self::from_manifest_hex(&manifest_value, MagstvCommonParams::from_environment()?)
    }

    pub fn key(&self) -> &MagstvPortalKey {
        &self.key
    }

    pub fn common_params(&self) -> &MagstvCommonParams {
        &self.common
    }

    fn endpoint(operation: PortalOperation) -> Result<MagstvPortalEndpoint, MagstvProviderError> {
        match operation {
            PortalOperation::Authenticate => Ok(MagstvPortalEndpoint::Login),
            PortalOperation::GetAuthInfo => Ok(MagstvPortalEndpoint::GetAuthInfo),
            PortalOperation::GetSlbInfo => Ok(MagstvPortalEndpoint::GetSlbInfo),
            PortalOperation::ListLiveCategories => Ok(MagstvPortalEndpoint::GetColumnContents),
            PortalOperation::ListLiveChannels => Ok(MagstvPortalEndpoint::GetLiveData),
            PortalOperation::ListPrograms => Ok(MagstvPortalEndpoint::GetProgram),
            PortalOperation::ResolvePlayback => Ok(MagstvPortalEndpoint::StartPlayLive),
            PortalOperation::ResolveVodPlayback => Ok(MagstvPortalEndpoint::StartPlayVod),
            PortalOperation::RefreshSession => Ok(MagstvPortalEndpoint::TerminalAuth),
            PortalOperation::ListMovies | PortalOperation::ListSeries => {
                Ok(MagstvPortalEndpoint::GetShelveData)
            }
            PortalOperation::ListEpisodes => Ok(MagstvPortalEndpoint::GetItemData),
            PortalOperation::Bootstrap => {
                Err(MagstvProviderError::OperationUnverified { operation })
            }
        }
    }

    fn merged_arguments(&self, arguments: &Value) -> Result<Value, MagstvProviderError> {
        let common = serde_json::to_value(&self.common)
            .map_err(|_| codec_error(CodecFailureKind::UnexpectedPayload))?;
        let common = common
            .as_object()
            .ok_or_else(|| codec_error(CodecFailureKind::UnexpectedPayload))?;
        let specific = arguments
            .as_object()
            .ok_or_else(|| codec_error(CodecFailureKind::UnexpectedPayload))?;

        let mut merged = Map::new();
        for (name, value) in common {
            merged.insert(name.clone(), value.clone());
        }
        // The Android serializer writes common fields first and the endpoint
        // DTO second, so an endpoint field wins if a name ever collides.
        for (name, value) in specific {
            merged.insert(name.clone(), value.clone());
        }
        Ok(Value::Object(merged))
    }

    fn encrypt_json(&self, value: &Value) -> Result<Vec<u8>, MagstvProviderError> {
        let plaintext = serde_json::to_vec(value)
            .map_err(|_| codec_error(CodecFailureKind::UnexpectedPayload))?;
        let ciphertext = des3_encrypt(&plaintext, &self.key.0)?;
        Ok(hex_encode_ascii(&BASE64.encode(ciphertext)))
    }

    fn decrypt_wire_text(&self, wire: &str) -> Result<Value, MagstvProviderError> {
        let encoded = decode_hex_utf8(wire)?;
        let ciphertext = decode_app_base64(&encoded)?;
        let plaintext = des3_decrypt(&ciphertext, &self.key.0)?;
        serde_json::from_slice(&plaintext)
            .map_err(|_| codec_error(CodecFailureKind::MalformedMessage))
    }

    #[cfg(test)]
    pub(crate) fn decode_wire_for_test(&self, body: &[u8]) -> Result<Value, MagstvProviderError> {
        let wire = std::str::from_utf8(body)
            .map_err(|_| codec_error(CodecFailureKind::InvalidEncoding))?;
        self.decrypt_wire_text(wire)
    }
}

impl PortalCodec for MagstvPortalCodec {
    fn verification(&self) -> CodecVerification {
        self.verification.clone()
    }

    fn encode(&self, request: &PortalRequest) -> Result<VerifiedWireRequest, MagstvProviderError> {
        let endpoint = Self::endpoint(request.operation)?;
        let arguments = self.merged_arguments(&request.arguments)?;
        let body = self.encrypt_json(&arguments)?;
        let mut headers = vec![
            ("apk".to_string(), self.common.app_id.clone()),
            ("apkVer".to_string(), self.common.apk_version.clone()),
            ("spkgVer".to_string(), self.common.sys_version.clone()),
        ];
        // These are endpoint annotations in the APK Retrofit service, not
        // optional client hints. In particular startPlayVOD is explicitly
        // marked no-store so an intermediary cannot replay a stale playback
        // negotiation.
        match request.operation {
            PortalOperation::Authenticate
            | PortalOperation::GetAuthInfo
            | PortalOperation::ListLiveChannels => {
                headers.push(("ProcessResult".to_string(), "false".to_string()));
            }
            PortalOperation::ResolveVodPlayback => {
                headers.push(("Cache-Control".to_string(), "no-store".to_string()));
            }
            _ => {}
        }
        VerifiedWireRequest::from_verified_contract(
            endpoint.path(),
            MAGSTV_PORTAL_CONTENT_TYPE,
            body,
            headers,
        )
    }

    fn decode(
        &self,
        operation: PortalOperation,
        status: u16,
        body: &[u8],
    ) -> Result<PortalResponse, MagstvProviderError> {
        if !(200..300).contains(&status) {
            return Err(MagstvProviderError::Transport(
                crate::TransportFailureKind::HttpStatus(status),
            ));
        }
        Self::endpoint(operation)?;
        let mut payload: Value = serde_json::from_slice(body)
            .map_err(|_| codec_error(CodecFailureKind::MalformedMessage))?;
        // `ProcessResult:false` controls a higher-level app callback, not the
        // wire cipher. Successful login/live responses observed at runtime
        // still carry encrypted string data, while fixture/direct responses
        // may already contain an object. Decrypt exactly the string shape.
        if let Some(data) = payload.get("data").and_then(Value::as_str) {
            let decrypted = self.decrypt_wire_text(data)?;
            payload["data"] = decrypted;
        }
        Ok(PortalResponse { payload })
    }
}

fn codec_error(kind: CodecFailureKind) -> MagstvProviderError {
    MagstvProviderError::Codec(kind)
}

fn runtime_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn first_24(bytes: &[u8]) -> Result<&[u8; 24], MagstvProviderError> {
    bytes
        .get(..24)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| codec_error(CodecFailureKind::InvalidKey))
}

fn decode_hex_utf8(input: &str) -> Result<String, MagstvProviderError> {
    let input = input.trim();
    if input.is_empty() || input.len() % 2 != 0 {
        return Err(codec_error(CodecFailureKind::InvalidEncoding));
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    let chars = input.as_bytes().chunks_exact(2);
    for pair in chars {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| codec_error(CodecFailureKind::InvalidEncoding))
}

fn hex_value(byte: u8) -> Result<u8, MagstvProviderError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(codec_error(CodecFailureKind::InvalidEncoding)),
    }
}

fn hex_encode_ascii(input: &str) -> Vec<u8> {
    let mut output = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        // All standard Base64 characters are at least 0x2b, so Java's
        // Integer.toHexString(byte) and this two-digit form agree here.
        let _ = write!(output, "{byte:02x}");
    }
    output.into_bytes()
}

/// Decodes the app's Base64 variant. It uses the standard alphabet, ignores
/// CR/LF, and maps non-alphabet bytes to -1 before applying the same bit masks
/// as the Android decoder. The latter matters for the app's obfuscated key
/// derivation mask, which is intentionally not a conventional Base64 string.
fn decode_app_base64(input: &str) -> Result<Vec<u8>, MagstvProviderError> {
    let bytes = input
        .bytes()
        .filter(|byte| *byte != b'\r' && *byte != b'\n')
        .collect::<Vec<_>>();
    if bytes.len() < 2 || bytes.len() % 4 == 1 {
        return Err(codec_error(CodecFailureKind::InvalidEncoding));
    }

    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    for group in bytes.chunks(4) {
        if group.len() < 2 {
            return Err(codec_error(CodecFailureKind::InvalidEncoding));
        }
        let c0 = base64_value(group[0]);
        let c1 = base64_value(group[1]);
        let c2 = group.get(2).copied().unwrap_or(b'=');
        let c3 = group.get(3).copied().unwrap_or(b'=');
        let v2 = base64_value(c2);
        let v3 = base64_value(c3);

        output.push(((c0 << 2) & 0xfc | ((c1 >> 4) & 0x03)) as u8);
        if c2 != b'=' {
            output.push(((c1 << 4) & 0xf0 | ((v2 >> 2) & 0x0f)) as u8);
        }
        if c3 != b'=' {
            output.push(((v2 << 6) & 0xc0 | (v3 & 0x3f)) as u8);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> i32 {
    match byte {
        b'A'..=b'Z' => (byte - b'A') as i32,
        b'a'..=b'z' => (byte - b'a' + 26) as i32,
        b'0'..=b'9' => (byte - b'0' + 52) as i32,
        b'+' => 62,
        b'/' => 63,
        _ => -1,
    }
}

fn des3_encrypt(input: &[u8], key: &[u8; 24]) -> Result<Vec<u8>, MagstvProviderError> {
    let padding = 8 - (input.len() % 8);
    let mut padded = Vec::with_capacity(input.len() + padding);
    padded.extend_from_slice(input);
    padded.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher =
        TdesEde3::new_from_slice(key).map_err(|_| codec_error(CodecFailureKind::InvalidKey))?;
    for chunk in padded.chunks_exact_mut(8) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Ok(padded)
}

fn des3_decrypt(input: &[u8], key: &[u8; 24]) -> Result<Vec<u8>, MagstvProviderError> {
    if input.is_empty() || input.len() % 8 != 0 {
        return Err(codec_error(CodecFailureKind::InvalidPadding));
    }
    let cipher =
        TdesEde3::new_from_slice(key).map_err(|_| codec_error(CodecFailureKind::InvalidKey))?;
    let mut plaintext = input.to_vec();
    for chunk in plaintext.chunks_exact_mut(8) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    let padding = *plaintext
        .last()
        .ok_or_else(|| codec_error(CodecFailureKind::InvalidPadding))? as usize;
    if !(1..=8).contains(&padding)
        || plaintext.len() < padding
        || !plaintext[plaintext.len() - padding..]
            .iter()
            .all(|byte| *byte as usize == padding)
    {
        return Err(codec_error(CodecFailureKind::InvalidPadding));
    }
    plaintext.truncate(plaintext.len() - padding);
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn common() -> MagstvCommonParams {
        MagstvCommonParams {
            login_type: "1".to_string(),
            app_language: "es".to_string(),
            apk_version: "49905".to_string(),
            sys_version: "test-system".to_string(),
            app_id: "test-app".to_string(),
            hardware_info: "test-hardware".to_string(),
            model: "test-model".to_string(),
            product: "test-product".to_string(),
            cpu: "x86_64".to_string(),
            b29: "serial".to_string(),
            reserve1: "mac".to_string(),
            portal_code: "live".to_string(),
            device_token: "device-token".to_string(),
            sn: "sn".to_string(),
            sdk_ver: 35,
        }
    }

    fn codec() -> MagstvPortalCodec {
        MagstvPortalCodec::new(MagstvPortalKey::from_bytes([0x24; 24]), common()).unwrap()
    }

    #[test]
    fn portal_request_matches_hex_wrapped_three_des_contract() {
        let codec = codec();
        let request = PortalRequest::new(
            PortalOperation::Authenticate,
            json!({"userName": "user", "password": "pass"}),
        );
        let wire = codec.encode(&request).unwrap();
        assert_eq!(wire.relative_path(), MagstvPortalEndpoint::Login.path());
        assert!(wire.body().iter().all(u8::is_ascii_hexdigit));
        assert_eq!(wire.content_type(), MAGSTV_PORTAL_CONTENT_TYPE);

        let plaintext = codec.decrypt_wire_text(&String::from_utf8(wire.body().to_vec()).unwrap());
        let plaintext = plaintext.unwrap();
        assert_eq!(plaintext["userName"], "user");
        assert_eq!(plaintext["password"], "pass");
        assert_eq!(plaintext["apkVersion"], "49905");
        assert_eq!(plaintext["B29"], "serial");
        assert!(plaintext.get("b29").is_none());
    }

    #[test]
    fn encrypted_response_data_is_replaced_with_json() {
        let codec = codec();
        let inner = json!({"channelList": [], "dataVersion": "v1"});
        let encrypted = codec.encrypt_json(&inner).unwrap();
        let outer = json!({"returnCode": "0", "data": String::from_utf8(encrypted).unwrap()});
        let decoded = codec
            .decode(
                PortalOperation::ListPrograms,
                200,
                &serde_json::to_vec(&outer).unwrap(),
            )
            .unwrap();
        assert_eq!(decoded.payload["data"]["dataVersion"], "v1");
        assert!(decoded.payload["data"]["channelList"].is_array());
    }

    #[test]
    fn direct_process_result_response_is_not_decrypted() {
        let codec = codec();
        let outer = json!({
            "returnCode": "0",
            "data": {"userId": "runtime-user", "userToken": "runtime-token"}
        });
        let decoded = codec
            .decode(
                PortalOperation::Authenticate,
                200,
                &serde_json::to_vec(&outer).unwrap(),
            )
            .unwrap();
        assert_eq!(decoded.payload["data"]["userId"], "runtime-user");
        assert!(!format!("{codec:?}").contains("runtime-token"));
    }

    #[test]
    fn encrypted_login_data_is_replaced_with_json() {
        let codec = codec();
        let inner = json!({"userId": "runtime-user", "userToken": "runtime-token"});
        let encrypted = codec.encrypt_json(&inner).unwrap();
        let outer = json!({"returnCode": "0", "data": String::from_utf8(encrypted).unwrap()});
        let decoded = codec
            .decode(
                PortalOperation::Authenticate,
                200,
                &serde_json::to_vec(&outer).unwrap(),
            )
            .unwrap();
        assert_eq!(decoded.payload["data"]["userId"], "runtime-user");
        assert!(!format!("{codec:?}").contains("runtime-token"));
    }

    #[test]
    fn invalid_padding_is_rejected() {
        let codec = codec();
        let result = codec.decrypt_wire_text("00");
        assert!(matches!(
            result,
            Err(MagstvProviderError::Codec(
                CodecFailureKind::InvalidEncoding
            ))
        ));
    }
}
