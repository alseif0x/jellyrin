use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use jellyrin_plugin_rpc::{
    CapabilityResult, DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES, EmbeddedImageRequest, HandshakeRequest,
    HandshakeResponse, InvokeCapabilityRequest, LoadPluginRequest, LoadedPlugin,
    PLUGIN_RPC_PROTOCOL_VERSION, PluginHealth, PluginHealthStatus, PluginIdentity,
    PluginRpcEnvelope, PluginRpcError, PluginRpcErrorCode, PluginRpcMethod, PluginRpcResponse,
    PluginRuntime, PluginWebPage, UpdateConfigurationRequest, decode_json_line, encode_json_line,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const HOST_RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
const HOST_ID: &str = "jellyrin-plugin-host-wasi";
const HOST_CAPABILITIES: &[&str] = &[
    "Handshake",
    "LoadPlugin",
    "UnloadPlugin",
    "GetManifest",
    "GetConfiguration",
    "UpdateConfiguration",
    "ListWebPages",
    "GetEmbeddedImage",
    "ListCapabilities",
    "InvokeCapability",
    "Health",
    "Shutdown",
];

#[derive(Debug, Default)]
struct WasiHostState {
    loaded_plugins: BTreeMap<String, LoadedWasiPlugin>,
    shutting_down: bool,
}

#[derive(Debug, Clone)]
struct LoadedWasiPlugin {
    plugin_id: String,
    name: String,
    version: String,
    install_path: String,
    manifest: Value,
    configuration: Value,
    capabilities: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve_json_lines(BufReader::new(stdin), stdout).await
}

async fn serve_json_lines<R, W>(mut reader: R, mut writer: W) -> Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut state = WasiHostState::default();
    loop {
        let mut line = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut line).await?;
        if bytes_read == 0 {
            break;
        }
        let response = match decode_json_line::<PluginRpcEnvelope<Value>>(
            &line,
            DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES,
        ) {
            Ok(envelope) => handle_envelope(&mut state, envelope).await,
            Err(error) => PluginRpcResponse::failure(
                String::new(),
                PluginRpcError::new(PluginRpcErrorCode::InvalidRequest, error.to_string()),
            ),
        };
        write_response(&mut writer, &response).await?;
        if state.shutting_down {
            break;
        }
    }
    Ok(())
}

async fn write_response<T: Serialize, W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    response: &PluginRpcResponse<T>,
) -> Result<()> {
    let bytes = encode_json_line(response, DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn handle_envelope(
    state: &mut WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    if envelope.protocol_version != PLUGIN_RPC_PROTOCOL_VERSION {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::ProtocolVersionMismatch,
            format!(
                "Protocol version {} is not supported by {}.",
                envelope.protocol_version, HOST_ID
            ),
        );
    }

    match envelope.method {
        PluginRpcMethod::Handshake => handle_handshake(envelope),
        PluginRpcMethod::LoadPlugin => handle_load_plugin(state, envelope).await,
        PluginRpcMethod::UnloadPlugin => handle_unload_plugin(state, envelope),
        PluginRpcMethod::GetManifest => handle_get_manifest(state, envelope),
        PluginRpcMethod::GetConfiguration => handle_get_configuration(state, envelope),
        PluginRpcMethod::UpdateConfiguration => handle_update_configuration(state, envelope),
        PluginRpcMethod::ListWebPages => handle_list_web_pages(state, envelope),
        PluginRpcMethod::GetEmbeddedImage => handle_get_embedded_image(state, envelope).await,
        PluginRpcMethod::ListCapabilities => handle_list_capabilities(state, envelope),
        PluginRpcMethod::InvokeCapability => handle_invoke_capability(state, envelope),
        PluginRpcMethod::Health => handle_health(state, envelope),
        PluginRpcMethod::Shutdown => {
            state.shutting_down = true;
            success(envelope.correlation_id, json!({ "Status": "Stopped" }))
        }
    }
}

fn handle_handshake(envelope: PluginRpcEnvelope<Value>) -> PluginRpcResponse<Value> {
    let request = match serde_json::from_value::<HandshakeRequest>(envelope.payload) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    if request.runtime != PluginRuntime::RustWasi {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            "jellyrin-plugin-host-wasi only accepts RustWasi handshakes.",
        );
    }
    if !request
        .supported_protocol_versions
        .contains(&PLUGIN_RPC_PROTOCOL_VERSION)
    {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::ProtocolVersionMismatch,
            "No compatible plugin RPC protocol version was offered.",
        );
    }
    success(
        envelope.correlation_id,
        HandshakeResponse {
            accepted_protocol_version: PLUGIN_RPC_PROTOCOL_VERSION,
            server_name: HOST_ID.to_string(),
            server_version: HOST_RUNTIME_VERSION.to_string(),
            minimum_call_timeout_ms: 250,
            capabilities: HOST_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_string())
                .collect(),
        },
    )
}

async fn handle_load_plugin(
    state: &mut WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let request = match serde_json::from_value::<LoadPluginRequest>(envelope.payload) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    if request.runtime != PluginRuntime::RustWasi {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            "WASI host can only load RustWasi plugins.",
        );
    }
    let wasm_files = match find_wasm_files(Path::new(&request.install_path)).await {
        Ok(files) => files,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::PluginNotFound,
                error.to_string(),
            );
        }
    };
    if wasm_files.is_empty() {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotFound,
            "RustWasi plugin install path does not contain a .wasm artifact.",
        );
    }
    let capabilities = manifest_capabilities(&request.manifest);
    let loaded = LoadedWasiPlugin {
        plugin_id: request.plugin_id.clone(),
        name: request.name.clone(),
        version: request.version.clone(),
        install_path: request.install_path.clone(),
        manifest: request.manifest.clone(),
        configuration: manifest_configuration(&request.manifest),
        capabilities: capabilities.clone(),
    };
    state
        .loaded_plugins
        .insert(request.plugin_id.clone(), loaded);
    success(
        envelope.correlation_id,
        LoadedPlugin {
            plugin_id: request.plugin_id,
            runtime: PluginRuntime::RustWasi,
            runtime_version: HOST_RUNTIME_VERSION.to_string(),
            status: PluginHealthStatus::Healthy,
            manifest: request.manifest,
            capabilities,
        },
    )
}

fn handle_unload_plugin(
    state: &mut WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    if state.loaded_plugins.remove(&identity.plugin_id).is_none() {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    }
    success(envelope.correlation_id, json!({ "Status": "Unloaded" }))
}

fn handle_get_manifest(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&identity.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    success(envelope.correlation_id, plugin.manifest.clone())
}

fn handle_get_configuration(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&identity.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    success(envelope.correlation_id, plugin.configuration.clone())
}

fn handle_update_configuration(
    state: &mut WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let request = match serde_json::from_value::<UpdateConfigurationRequest>(envelope.payload) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get_mut(&request.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    plugin.configuration = request.configuration;
    success(envelope.correlation_id, plugin.configuration.clone())
}

fn handle_list_web_pages(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&identity.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    success(envelope.correlation_id, manifest_web_pages(plugin))
}

async fn handle_get_embedded_image(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let request = match serde_json::from_value::<EmbeddedImageRequest>(envelope.payload) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&request.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    let Some(image) = manifest_embedded_image(&plugin.manifest, &request.image_type) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotFound,
            format!("Embedded image {} is not registered.", request.image_type),
        );
    };
    let Some(relative_path) = image.path.and_then(|path| safe_relative_path(&path)) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            "Embedded image path is invalid.",
        );
    };
    let image_path = Path::new(&plugin.install_path).join(relative_path);
    let bytes = match tokio::fs::read(&image_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::PluginNotFound,
                format!("Embedded image could not be read: {error}"),
            );
        }
    };
    success(
        envelope.correlation_id,
        json!({
            "PluginId": plugin.plugin_id,
            "ImageType": request.image_type,
            "MimeType": image.mime_type.unwrap_or_else(|| mime_type_for_path(&image_path).to_string()),
            "Base64Data": general_purpose::STANDARD.encode(bytes)
        }),
    )
}

fn handle_list_capabilities(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&identity.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    success(envelope.correlation_id, json!(plugin.capabilities))
}

fn handle_invoke_capability(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let request = match serde_json::from_value::<InvokeCapabilityRequest>(envelope.payload) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&request.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    if !plugin
        .capabilities
        .iter()
        .any(|capability| capability == &request.capability)
    {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::CapabilityNotFound,
            format!("Capability {} is not registered.", request.capability),
        );
    }
    let value = manifest_capability_handler(&plugin.manifest, &request.capability)
        .map(|handler| execute_manifest_capability_handler(&request, handler))
        .unwrap_or_else(|| {
            json!({
                "Status": "NotExecuted",
                "Reason": "WASI execution engine is not wired for this capability yet."
            })
        });
    success(envelope.correlation_id, CapabilityResult { value })
}

fn handle_health(
    state: &WasiHostState,
    envelope: PluginRpcEnvelope<Value>,
) -> PluginRpcResponse<Value> {
    let identity = match serde_json::from_value::<PluginIdentity>(envelope.payload) {
        Ok(identity) => identity,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::InvalidRequest,
                error.to_string(),
            );
        }
    };
    let Some(plugin) = state.loaded_plugins.get(&identity.plugin_id) else {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotLoaded,
            "Plugin is not loaded.",
        );
    };
    success(
        envelope.correlation_id,
        PluginHealth {
            plugin_id: plugin.plugin_id.clone(),
            runtime: PluginRuntime::RustWasi,
            status: PluginHealthStatus::Healthy,
            last_error: None,
            metrics: json!({
                "Name": plugin.name,
                "Version": plugin.version,
                "InstallPath": plugin.install_path,
                "CapabilityCount": plugin.capabilities.len()
            }),
        },
    )
}

async fn find_wasm_files(path: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    find_wasm_files_recursive(path, &mut files)
        .await
        .with_context(|| format!("failed to inspect RustWasi plugin path {}", path.display()))?;
    Ok(files)
}

async fn find_wasm_files_recursive(path: &Path, files: &mut Vec<String>) -> Result<()> {
    let mut stack = vec![path.to_path_buf()];
    while let Some(path) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if file_type.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
            {
                files.push(entry.path().to_string_lossy().to_string());
            }
        }
    }
    Ok(())
}

fn manifest_capabilities(manifest: &Value) -> Vec<String> {
    manifest
        .get("Capabilities")
        .and_then(Value::as_array)
        .map(|capabilities| {
            capabilities
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn manifest_capability_handler<'a>(manifest: &'a Value, capability: &str) -> Option<&'a Value> {
    let handlers = manifest
        .get("CapabilityHandlers")
        .or_else(|| manifest.get("Handlers"))?;
    if let Some(handler) = handlers.get(capability) {
        return Some(handler);
    }
    handlers.as_array()?.iter().find(|handler| {
        handler
            .get("Capability")
            .or_else(|| handler.get("Name"))
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(capability))
    })
}

fn execute_manifest_capability_handler(
    request: &InvokeCapabilityRequest,
    handler: &Value,
) -> Value {
    let mut result = handler
        .get("Result")
        .or_else(|| handler.get("Response"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        result = json!({ "Value": result });
    }
    result["Status"] = result
        .get("Status")
        .cloned()
        .unwrap_or_else(|| json!("Executed"));
    result["Capability"] = json!(request.capability);
    if handler
        .get("EchoArguments")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        result["Arguments"] = request.arguments.clone();
    }
    result
}

fn manifest_configuration(manifest: &Value) -> Value {
    manifest
        .get("Configuration")
        .or_else(|| manifest.get("DefaultConfiguration"))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn manifest_web_pages(plugin: &LoadedWasiPlugin) -> Vec<PluginWebPage> {
    plugin
        .manifest
        .get("WebPages")
        .or_else(|| plugin.manifest.get("ConfigurationPages"))
        .and_then(Value::as_array)
        .map(|pages| {
            pages
                .iter()
                .filter_map(|page| manifest_web_page(plugin, page))
                .collect()
        })
        .unwrap_or_default()
}

fn manifest_web_page(plugin: &LoadedWasiPlugin, page: &Value) -> Option<PluginWebPage> {
    let name = page.get("Name").and_then(Value::as_str)?.to_string();
    let display_name = page
        .get("DisplayName")
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_string();
    let path = page
        .get("Path")
        .or_else(|| page.get("EmbeddedResourcePath"))
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_string();
    Some(PluginWebPage {
        plugin_id: plugin.plugin_id.clone(),
        name,
        display_name,
        path,
        enable_in_main_menu: page
            .get("EnableInMainMenu")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

struct ManifestEmbeddedImage {
    path: Option<String>,
    mime_type: Option<String>,
}

fn manifest_embedded_image(
    manifest: &Value,
    requested_type: &str,
) -> Option<ManifestEmbeddedImage> {
    let images = manifest
        .get("Images")
        .or_else(|| manifest.get("EmbeddedImages"))?;
    if let Some(array) = images.as_array() {
        return array
            .iter()
            .find(|image| {
                image
                    .get("ImageType")
                    .or_else(|| image.get("Type"))
                    .and_then(Value::as_str)
                    .is_some_and(|image_type| image_type.eq_ignore_ascii_case(requested_type))
            })
            .map(|image| ManifestEmbeddedImage {
                path: image
                    .get("Path")
                    .or_else(|| image.get("File"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                mime_type: image
                    .get("MimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
    }
    images
        .get(requested_type)
        .and_then(Value::as_str)
        .map(|path| ManifestEmbeddedImage {
            path: Some(path.to_string()),
            mime_type: None,
        })
}

fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() {
        return None;
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => safe.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!safe.as_os_str().is_empty()).then_some(safe)
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn success<T: Serialize>(correlation_id: String, result: T) -> PluginRpcResponse<Value> {
    match serde_json::to_value(result) {
        Ok(value) => PluginRpcResponse::success(correlation_id, value),
        Err(error) => failure(
            correlation_id,
            PluginRpcErrorCode::HostFailed,
            error.to_string(),
        ),
    }
}

fn failure(
    correlation_id: String,
    code: PluginRpcErrorCode,
    message: impl Into<String>,
) -> PluginRpcResponse<Value> {
    PluginRpcResponse::failure(correlation_id, PluginRpcError::new(code, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jellyrin_plugin_rpc::{PluginRpcJsonLineTransport, UpdateConfigurationRequest};
    use jellyrin_plugin_sdk::{
        CAPABILITY_SCHEDULED_TASK, CapabilityHandler, CapabilityResponse as SdkCapabilityResponse,
        PluginEmbeddedImage as SdkPluginEmbeddedImage, PluginManifest as SdkPluginManifest,
        PluginWebPage as SdkPluginWebPage, ScheduledTaskResult,
    };
    use tempfile::tempdir;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn wasi_host_handshake_load_health_and_shutdown() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(plugin_dir.join("fixture.wasm"), b"\0asm")
            .await
            .unwrap();
        tokio::fs::write(plugin_dir.join("logo.png"), b"wasi-image")
            .await
            .unwrap();

        let (client_stream, host_stream) = tokio::io::duplex(DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (host_read, host_write) = tokio::io::split(host_stream);
        let host_task = tokio::spawn(serve_json_lines(BufReader::new(host_read), host_write));
        let mut client = PluginRpcJsonLineTransport::new(BufReader::new(client_read), client_write);

        let handshake = PluginRpcEnvelope::new(
            "corr-handshake",
            PluginRpcMethod::Handshake,
            HandshakeRequest {
                runtime: PluginRuntime::RustWasi,
                runtime_version: "0.1.0".to_string(),
                host_id: "test".to_string(),
                supported_protocol_versions: vec![PLUGIN_RPC_PROTOCOL_VERSION],
                capabilities: Vec::new(),
            },
        );
        let response = client
            .call::<_, Value>(&handshake, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(
            response.result.unwrap()["AcceptedProtocolVersion"],
            PLUGIN_RPC_PROTOCOL_VERSION
        );

        let load = PluginRpcEnvelope::new(
            "corr-load",
            PluginRpcMethod::LoadPlugin,
            LoadPluginRequest {
                plugin_id: "wasi-fixture".to_string(),
                name: "WASI Fixture".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: "jellyrin-wasi-0.1".to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: SdkPluginManifest::builder("wasi-fixture", "WASI Fixture", "1.0.0")
                    .configuration(json!({ "IntervalMinutes": 15 }))
                    .web_page(
                        SdkPluginWebPage::new("wasi-config", "configuration.html")
                            .display_name("WASI Config")
                            .enable_in_main_menu(),
                    )
                    .embedded_image(
                        SdkPluginEmbeddedImage::new("Primary", "logo.png").mime_type("image/png"),
                    )
                    .capability_handler(
                        CAPABILITY_SCHEDULED_TASK,
                        CapabilityHandler::new(
                            SdkCapabilityResponse::scheduled_task(ScheduledTaskResult::completed(
                                "WASI Fixture Task",
                            ))
                            .into_host_value(),
                        )
                        .echo_arguments(),
                    )
                    .build()
                    .into_json(),
                permissions: Vec::new(),
            },
        );
        let response = client
            .call::<_, Value>(&load, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["Status"], "Healthy");
        assert_eq!(
            response.result.as_ref().unwrap()["Capabilities"][0],
            "ScheduledTask"
        );

        let identity = PluginIdentity {
            plugin_id: "wasi-fixture".to_string(),
            version: "1.0.0".to_string(),
        };
        let configuration = PluginRpcEnvelope::new(
            "corr-config",
            PluginRpcMethod::GetConfiguration,
            identity.clone(),
        );
        let response = client
            .call::<_, Value>(&configuration, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["IntervalMinutes"], 15);

        let update_configuration = PluginRpcEnvelope::new(
            "corr-update-config",
            PluginRpcMethod::UpdateConfiguration,
            UpdateConfigurationRequest {
                plugin_id: "wasi-fixture".to_string(),
                configuration: json!({ "IntervalMinutes": 30 }),
            },
        );
        let response = client
            .call::<_, Value>(&update_configuration, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["IntervalMinutes"], 30);

        let pages = PluginRpcEnvelope::new(
            "corr-pages",
            PluginRpcMethod::ListWebPages,
            identity.clone(),
        );
        let response = client
            .call::<_, Value>(&pages, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()[0]["Name"], "wasi-config");
        assert_eq!(
            response.result.as_ref().unwrap()[0]["EnableInMainMenu"],
            true
        );

        let image = PluginRpcEnvelope::new(
            "corr-image",
            PluginRpcMethod::GetEmbeddedImage,
            EmbeddedImageRequest {
                plugin_id: "wasi-fixture".to_string(),
                version: "1.0.0".to_string(),
                image_type: "Primary".to_string(),
            },
        );
        let response = client
            .call::<_, Value>(&image, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["MimeType"], "image/png");
        assert_eq!(
            response.result.as_ref().unwrap()["Base64Data"],
            general_purpose::STANDARD.encode(b"wasi-image")
        );

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke",
            PluginRpcMethod::InvokeCapability,
            InvokeCapabilityRequest {
                plugin_id: "wasi-fixture".to_string(),
                capability: "ScheduledTask".to_string(),
                arguments: json!({ "Trigger": "Manual" }),
                timeout_ms: 1000,
            },
        );
        let response = client
            .call::<_, Value>(&invoke, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        let value = &response.result.as_ref().unwrap()["Value"];
        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], "ScheduledTask");
        assert_eq!(value["TaskName"], "WASI Fixture Task");
        assert_eq!(value["Arguments"]["Trigger"], "Manual");

        let health = PluginRpcEnvelope::new("corr-health", PluginRpcMethod::Health, identity);
        let response = client
            .call::<_, Value>(&health, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["Status"], "Healthy");
        assert_eq!(
            response.result.as_ref().unwrap()["Metrics"]["CapabilityCount"],
            1
        );

        let shutdown =
            PluginRpcEnvelope::new("corr-shutdown", PluginRpcMethod::Shutdown, json!({}));
        let response = client
            .call::<_, Value>(&shutdown, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        host_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn wasi_host_rejects_missing_wasm_and_unloaded_configuration() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();

        let mut state = WasiHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-missing",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "missing".to_string(),
                name: "Missing".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: "jellyrin-wasi-0.1".to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({ "Name": "Missing" }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, load).await;
        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            PluginRpcErrorCode::PluginNotFound
        );

        let unloaded_config = PluginRpcEnvelope::new(
            "corr-config",
            PluginRpcMethod::GetConfiguration,
            serde_json::to_value(PluginIdentity {
                plugin_id: "missing".to_string(),
                version: "1.0.0".to_string(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, unloaded_config).await;
        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            PluginRpcErrorCode::PluginNotLoaded
        );
    }
}
