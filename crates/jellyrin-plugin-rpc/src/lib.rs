use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLUGIN_RPC_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PluginRuntime {
    DotNetJellyfin,
    RustWasi,
    ExternalProcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PluginRpcMethod {
    Handshake,
    LoadPlugin,
    UnloadPlugin,
    GetManifest,
    GetConfiguration,
    UpdateConfiguration,
    ListWebPages,
    GetEmbeddedImage,
    ListCapabilities,
    InvokeCapability,
    Health,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PluginRpcErrorCode {
    InvalidRequest,
    ProtocolVersionMismatch,
    UnsupportedMethod,
    PluginNotFound,
    PluginNotLoaded,
    CapabilityNotFound,
    PermissionDenied,
    Timeout,
    HostUnavailable,
    HostFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PluginHealthStatus {
    Healthy,
    Degraded,
    NotSupported,
    Malfunctioned,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PluginLogSeverity {
    Trace,
    Debug,
    Information,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginRpcEnvelope<T> {
    pub protocol_version: u16,
    pub correlation_id: String,
    pub method: PluginRpcMethod,
    pub payload: T,
}

impl<T> PluginRpcEnvelope<T> {
    pub fn new(correlation_id: impl Into<String>, method: PluginRpcMethod, payload: T) -> Self {
        Self {
            protocol_version: PLUGIN_RPC_PROTOCOL_VERSION,
            correlation_id: correlation_id.into(),
            method,
            payload,
        }
    }

    pub fn map_payload<U>(self, payload: U) -> PluginRpcEnvelope<U> {
        PluginRpcEnvelope {
            protocol_version: self.protocol_version,
            correlation_id: self.correlation_id,
            method: self.method,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginRpcResponse<T> {
    pub protocol_version: u16,
    pub correlation_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginRpcError>,
}

impl<T> PluginRpcResponse<T> {
    pub fn success(correlation_id: impl Into<String>, result: T) -> Self {
        Self {
            protocol_version: PLUGIN_RPC_PROTOCOL_VERSION,
            correlation_id: correlation_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(correlation_id: impl Into<String>, error: PluginRpcError) -> Self {
        Self {
            protocol_version: PLUGIN_RPC_PROTOCOL_VERSION,
            correlation_id: correlation_id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginRpcError {
    pub code: PluginRpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl PluginRpcError {
    pub fn new(code: PluginRpcErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginRpcCodecError {
    #[error("plugin RPC message is larger than {limit} bytes")]
    MessageTooLarge { limit: usize },
    #[error("plugin RPC JSON codec failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn encode_json_line<T: Serialize>(
    message: &T,
    max_bytes: usize,
) -> Result<Vec<u8>, PluginRpcCodecError> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() > max_bytes {
        return Err(PluginRpcCodecError::MessageTooLarge { limit: max_bytes });
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn decode_json_line<T: for<'de> Deserialize<'de>>(
    line: &[u8],
    max_bytes: usize,
) -> Result<T, PluginRpcCodecError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    if line.len() > max_bytes {
        return Err(PluginRpcCodecError::MessageTooLarge { limit: max_bytes });
    }
    Ok(serde_json::from_slice(line)?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HandshakeRequest {
    pub runtime: PluginRuntime,
    pub runtime_version: String,
    pub host_id: String,
    pub supported_protocol_versions: Vec<u16>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HandshakeResponse {
    pub accepted_protocol_version: u16,
    pub server_name: String,
    pub server_version: String,
    pub minimum_call_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LoadPluginRequest {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub runtime: PluginRuntime,
    pub target_abi: String,
    pub install_path: String,
    pub manifest: Value,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LoadedPlugin {
    pub plugin_id: String,
    pub runtime: PluginRuntime,
    pub runtime_version: String,
    pub status: PluginHealthStatus,
    pub manifest: Value,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginIdentity {
    pub plugin_id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UpdateConfigurationRequest {
    pub plugin_id: String,
    pub configuration: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginWebPage {
    pub plugin_id: String,
    pub name: String,
    pub display_name: String,
    pub path: String,
    #[serde(default)]
    pub enable_in_main_menu: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EmbeddedImageRequest {
    pub plugin_id: String,
    pub version: String,
    pub image_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InvokeCapabilityRequest {
    pub plugin_id: String,
    pub capability: String,
    #[serde(default)]
    pub arguments: Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityResult {
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginHealth {
    pub plugin_id: String,
    pub runtime: PluginRuntime,
    pub status: PluginHealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub metrics: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginHostLogEvent {
    pub plugin_id: String,
    pub runtime: PluginRuntime,
    pub severity: PluginLogSeverity,
    pub message: String,
    #[serde(default)]
    pub fields: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MAX_MESSAGE_BYTES: usize = 4096;

    #[test]
    fn handshake_request_round_trips_with_pascal_case_fields() {
        let request = PluginRpcEnvelope::new(
            "corr-1",
            PluginRpcMethod::Handshake,
            HandshakeRequest {
                runtime: PluginRuntime::RustWasi,
                runtime_version: "0.1.0".to_string(),
                host_id: "wasi-host-a".to_string(),
                supported_protocol_versions: vec![PLUGIN_RPC_PROTOCOL_VERSION],
                capabilities: vec!["Health".to_string(), "InvokeCapability".to_string()],
            },
        );

        let encoded = encode_json_line(&request, MAX_MESSAGE_BYTES).unwrap();
        assert!(encoded.ends_with(b"\n"));
        let value: Value = serde_json::from_slice(encoded.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(value["ProtocolVersion"], PLUGIN_RPC_PROTOCOL_VERSION);
        assert_eq!(value["Method"], "Handshake");
        assert_eq!(value["Payload"]["Runtime"], "RustWasi");
        assert_eq!(
            value["Payload"]["SupportedProtocolVersions"][0],
            PLUGIN_RPC_PROTOCOL_VERSION
        );

        let decoded: PluginRpcEnvelope<HandshakeRequest> =
            decode_json_line(&encoded, MAX_MESSAGE_BYTES).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn load_plugin_request_carries_manifest_permissions_and_path() {
        let request = PluginRpcEnvelope::new(
            "corr-load",
            PluginRpcMethod::LoadPlugin,
            LoadPluginRequest {
                plugin_id: "11111111-1111-1111-1111-111111111111".to_string(),
                name: "Fixture".to_string(),
                version: "1.0.0.0".to_string(),
                runtime: PluginRuntime::DotNetJellyfin,
                target_abi: "12.0.0.0".to_string(),
                install_path: "/var/lib/jellyrin/plugins/fixture/1.0.0.0".to_string(),
                manifest: json!({ "Name": "Fixture", "Category": "Metadata" }),
                permissions: vec!["Filesystem:PluginData".to_string()],
            },
        );

        let encoded = encode_json_line(&request, MAX_MESSAGE_BYTES).unwrap();
        let decoded: PluginRpcEnvelope<LoadPluginRequest> =
            decode_json_line(&encoded, MAX_MESSAGE_BYTES).unwrap();

        assert_eq!(decoded.method, PluginRpcMethod::LoadPlugin);
        assert_eq!(decoded.payload.runtime, PluginRuntime::DotNetJellyfin);
        assert_eq!(decoded.payload.manifest["Name"], "Fixture");
        assert_eq!(decoded.payload.permissions, ["Filesystem:PluginData"]);
    }

    #[test]
    fn failure_response_omits_result_and_preserves_typed_error() {
        let response = PluginRpcResponse::<Value>::failure(
            "corr-fail",
            PluginRpcError::new(
                PluginRpcErrorCode::ProtocolVersionMismatch,
                "protocol 99 is not supported",
            ),
        );

        let encoded = encode_json_line(&response, MAX_MESSAGE_BYTES).unwrap();
        let value: Value = serde_json::from_slice(encoded.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(value["Ok"], false);
        assert!(value.get("Result").is_none());
        assert_eq!(value["Error"]["Code"], "ProtocolVersionMismatch");

        let decoded: PluginRpcResponse<Value> =
            decode_json_line(&encoded, MAX_MESSAGE_BYTES).unwrap();
        assert_eq!(
            decoded.error.unwrap().code,
            PluginRpcErrorCode::ProtocolVersionMismatch
        );
    }

    #[test]
    fn codec_rejects_oversized_messages() {
        let request = PluginRpcEnvelope::new(
            "corr-big",
            PluginRpcMethod::InvokeCapability,
            InvokeCapabilityRequest {
                plugin_id: "plugin".to_string(),
                capability: "MetadataProvider.Search".to_string(),
                arguments: json!({ "Payload": "x".repeat(128) }),
                timeout_ms: 1000,
            },
        );

        let error = encode_json_line(&request, 32).unwrap_err();
        assert!(matches!(
            error,
            PluginRpcCodecError::MessageTooLarge { limit: 32 }
        ));
    }
}
