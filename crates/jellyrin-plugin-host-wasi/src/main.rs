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
const TARGET_ABI: &str = "jellyrin-wasi-0.1";
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
    wasm_files: Vec<String>,
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
        PluginRpcMethod::InvokeCapability => handle_invoke_capability(state, envelope).await,
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
    if !request.target_abi.eq_ignore_ascii_case(TARGET_ABI) {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            format!(
                "RustWasi target ABI {} is not supported by this host.",
                request.target_abi
            ),
        );
    }
    if let Some(manifest_target_abi) = manifest_target_abi(&request.manifest)
        && !manifest_target_abi.eq_ignore_ascii_case(TARGET_ABI)
    {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            format!("RustWasi manifest target ABI {manifest_target_abi} is not supported."),
        );
    }
    if let Some(missing_permission) =
        first_missing_manifest_permission(&request.manifest, &request.permissions)
    {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PermissionDenied,
            format!("RustWasi permission {missing_permission} was not granted."),
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
        wasm_files,
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

async fn handle_invoke_capability(
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
    let value =
        if let Some(handler) = manifest_capability_handler(&plugin.manifest, &request.capability) {
            execute_manifest_capability_handler(&request, handler)
        } else {
            match execute_wasm_capability_handler(plugin, &request).await {
                Ok(Some(value)) => value,
                Ok(None) => {
                    json!({
                        "Status": "NotExecuted",
                        "Reason": "WASI execution engine is not wired for this capability yet."
                    })
                }
                Err(error) => {
                    return failure(
                        envelope.correlation_id,
                        PluginRpcErrorCode::HostFailed,
                        format!("WASM capability execution failed: {error:#}"),
                    );
                }
            }
        };
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

fn manifest_target_abi(manifest: &Value) -> Option<&str> {
    manifest.get("TargetAbi").and_then(Value::as_str)
}

fn first_missing_manifest_permission(manifest: &Value, granted: &[String]) -> Option<String> {
    manifest_permissions(manifest)
        .into_iter()
        .find(|permission| {
            !granted
                .iter()
                .any(|granted| granted.eq_ignore_ascii_case(permission))
        })
}

fn manifest_permissions(manifest: &Value) -> Vec<String> {
    manifest
        .get("Permissions")
        .and_then(Value::as_array)
        .map(|permissions| {
            permissions
                .iter()
                .filter_map(|permission| {
                    permission.as_str().or_else(|| {
                        permission
                            .get("Name")
                            .or_else(|| permission.get("Permission"))
                            .and_then(Value::as_str)
                    })
                })
                .filter(|permission| !permission.trim().is_empty())
                .map(|permission| permission.trim().to_string())
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

async fn execute_wasm_capability_handler(
    plugin: &LoadedWasiPlugin,
    request: &InvokeCapabilityRequest,
) -> Result<Option<Value>> {
    let Some(export_name) = manifest_wasm_export(&plugin.manifest, &request.capability) else {
        return Ok(None);
    };
    let Some(wasm_path) = plugin.wasm_files.first() else {
        return Ok(None);
    };
    let bytes = tokio::fs::read(wasm_path)
        .await
        .with_context(|| format!("failed to read WASM module {wasm_path}"))?;
    let (wasm_return, wasm_arguments) =
        execute_minimal_wasm_i32_export(&bytes, &export_name, &request.arguments)
            .with_context(|| format!("failed to execute WASM export {export_name}"))?;
    Ok(Some(json!({
        "Status": "Executed",
        "Capability": request.capability,
        "ExecutionMode": "WasmI32Export",
        "Export": export_name,
        "WasmArguments": wasm_arguments,
        "WasmReturn": wasm_return
    })))
}

fn manifest_wasm_export(manifest: &Value, capability: &str) -> Option<String> {
    let exports = manifest
        .get("WasmExports")
        .or_else(|| manifest.get("WasmEntryPoints"))
        .or_else(|| manifest.get("Exports"))?;
    if let Some(export_name) = exports
        .get(capability)
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
    {
        return Some(export_name.trim().to_string());
    }
    exports.as_array()?.iter().find_map(|entry| {
        let entry_capability = entry
            .get("Capability")
            .or_else(|| entry.get("Name"))
            .and_then(Value::as_str)?;
        if !entry_capability.eq_ignore_ascii_case(capability) {
            return None;
        }
        entry
            .get("Export")
            .or_else(|| entry.get("Function"))
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(|name| name.trim().to_string())
    })
}

fn execute_minimal_wasm_i32_export(
    bytes: &[u8],
    export_name: &str,
    arguments: &Value,
) -> Result<(i32, Vec<i32>)> {
    let module = MinimalWasmModule::parse(bytes)?;
    let function_index = module
        .exports
        .get(export_name)
        .copied()
        .with_context(|| format!("WASM export {export_name} was not found"))?;
    if function_index < module.imported_functions.len() as u32 {
        anyhow::bail!("WASM export {export_name} points to an imported function");
    }
    let defined_index = (function_index - module.imported_functions.len() as u32) as usize;
    let type_index = module
        .function_type_indices
        .get(defined_index)
        .copied()
        .with_context(|| format!("WASM export {export_name} has no function type"))?;
    let function_type = module
        .types
        .get(type_index as usize)
        .with_context(|| format!("WASM export {export_name} type index is invalid"))?;
    if function_type.params.iter().any(|param| *param != 0x7f)
        || function_type.results.as_slice() != [0x7f]
    {
        anyhow::bail!("WASM export {export_name} must use i32 params and return i32");
    }
    let wasm_arguments = extract_minimal_wasm_i32_arguments(arguments, function_type.params.len())?;
    let body = module
        .function_bodies
        .get(defined_index)
        .with_context(|| format!("WASM export {export_name} has no function body"))?;
    let wasm_return = execute_minimal_wasm_i32_body(&module, body, &wasm_arguments)?;
    Ok((wasm_return, wasm_arguments))
}

fn extract_minimal_wasm_i32_arguments(arguments: &Value, param_count: usize) -> Result<Vec<i32>> {
    match param_count {
        0 => Ok(Vec::new()),
        1 => Ok(vec![extract_single_i32_argument(arguments)?]),
        count => {
            anyhow::bail!("minimal WASM i32 executor supports at most one argument, got {count}")
        }
    }
}

fn extract_single_i32_argument(arguments: &Value) -> Result<i32> {
    if let Some(value) = json_number_to_i32(arguments) {
        return Ok(value);
    }
    for key in ["Value", "Input", "Argument"] {
        if let Some(value) = arguments.get(key).and_then(json_number_to_i32) {
            return Ok(value);
        }
    }
    anyhow::bail!("one-argument WASM i32 export requires numeric Value, Input or Argument")
}

fn json_number_to_i32(value: &Value) -> Option<i32> {
    let number = value.as_i64()?;
    i32::try_from(number).ok()
}

#[derive(Debug)]
struct MinimalWasmModule {
    types: Vec<MinimalWasmFunctionType>,
    imported_functions: Vec<MinimalWasmImportFunction>,
    function_type_indices: Vec<u32>,
    exports: BTreeMap<String, u32>,
    function_bodies: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct MinimalWasmFunctionType {
    params: Vec<u8>,
    results: Vec<u8>,
}

#[derive(Debug)]
struct MinimalWasmImportFunction {
    module: String,
    name: String,
    type_index: u32,
}

impl MinimalWasmModule {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[4..8] != [1, 0, 0, 0] {
            anyhow::bail!("invalid WASM module header");
        }
        let mut module = Self {
            types: Vec::new(),
            imported_functions: Vec::new(),
            function_type_indices: Vec::new(),
            exports: BTreeMap::new(),
            function_bodies: Vec::new(),
        };
        let mut cursor = WasmCursor::new(&bytes[8..]);
        while !cursor.is_empty() {
            let section_id = cursor.read_u8()?;
            let section_size = cursor.read_u32_leb()? as usize;
            let section = cursor.read_bytes(section_size)?;
            match section_id {
                1 => module.types = parse_wasm_type_section(section)?,
                2 => module.imported_functions = parse_wasm_import_section(section)?,
                3 => module.function_type_indices = parse_wasm_function_section(section)?,
                7 => module.exports = parse_wasm_export_section(section)?,
                10 => module.function_bodies = parse_wasm_code_section(section)?,
                _ => {}
            }
        }
        if module.function_bodies.len() != module.function_type_indices.len() {
            anyhow::bail!("WASM function and code section counts do not match");
        }
        Ok(module)
    }
}

fn parse_wasm_type_section(bytes: &[u8]) -> Result<Vec<MinimalWasmFunctionType>> {
    let mut cursor = WasmCursor::new(bytes);
    let count = cursor.read_u32_leb()?;
    let mut types = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if cursor.read_u8()? != 0x60 {
            anyhow::bail!("only WASM function types are supported");
        }
        let params = parse_wasm_valtypes(&mut cursor)?;
        let results = parse_wasm_valtypes(&mut cursor)?;
        types.push(MinimalWasmFunctionType { params, results });
    }
    cursor.expect_empty()?;
    Ok(types)
}

fn parse_wasm_valtypes(cursor: &mut WasmCursor<'_>) -> Result<Vec<u8>> {
    let count = cursor.read_u32_leb()?;
    let mut values = Vec::with_capacity(count as usize);
    for _ in 0..count {
        values.push(cursor.read_u8()?);
    }
    Ok(values)
}

fn parse_wasm_import_section(bytes: &[u8]) -> Result<Vec<MinimalWasmImportFunction>> {
    let mut cursor = WasmCursor::new(bytes);
    let count = cursor.read_u32_leb()?;
    let mut functions = Vec::new();
    for _ in 0..count {
        let module = cursor.read_name()?;
        let name = cursor.read_name()?;
        match cursor.read_u8()? {
            0x00 => {
                let type_index = cursor.read_u32_leb()?;
                functions.push(MinimalWasmImportFunction {
                    module,
                    name,
                    type_index,
                });
            }
            0x01 => {
                let _limits = parse_wasm_limits(&mut cursor)?;
            }
            0x02 => {
                let _limits = parse_wasm_limits(&mut cursor)?;
            }
            0x03 => {
                let _value_type = cursor.read_u8()?;
                let _mutable = cursor.read_u8()?;
            }
            tag => anyhow::bail!("unsupported WASM import tag {tag}"),
        }
    }
    cursor.expect_empty()?;
    Ok(functions)
}

fn parse_wasm_limits(cursor: &mut WasmCursor<'_>) -> Result<(u32, Option<u32>)> {
    match cursor.read_u8()? {
        0x00 => Ok((cursor.read_u32_leb()?, None)),
        0x01 => Ok((cursor.read_u32_leb()?, Some(cursor.read_u32_leb()?))),
        tag => anyhow::bail!("unsupported WASM limits tag {tag}"),
    }
}

fn parse_wasm_function_section(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut cursor = WasmCursor::new(bytes);
    let count = cursor.read_u32_leb()?;
    let mut type_indices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        type_indices.push(cursor.read_u32_leb()?);
    }
    cursor.expect_empty()?;
    Ok(type_indices)
}

fn parse_wasm_export_section(bytes: &[u8]) -> Result<BTreeMap<String, u32>> {
    let mut cursor = WasmCursor::new(bytes);
    let count = cursor.read_u32_leb()?;
    let mut exports = BTreeMap::new();
    for _ in 0..count {
        let name = cursor.read_name()?;
        let kind = cursor.read_u8()?;
        let index = cursor.read_u32_leb()?;
        if kind == 0x00 {
            exports.insert(name, index);
        }
    }
    cursor.expect_empty()?;
    Ok(exports)
}

fn parse_wasm_code_section(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut cursor = WasmCursor::new(bytes);
    let count = cursor.read_u32_leb()?;
    let mut bodies = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let body_size = cursor.read_u32_leb()? as usize;
        bodies.push(cursor.read_bytes(body_size)?.to_vec());
    }
    cursor.expect_empty()?;
    Ok(bodies)
}

fn execute_minimal_wasm_i32_body(
    module: &MinimalWasmModule,
    bytes: &[u8],
    arguments: &[i32],
) -> Result<i32> {
    let mut cursor = WasmCursor::new(bytes);
    let local_groups = cursor.read_u32_leb()?;
    let mut locals = arguments.to_vec();
    for _ in 0..local_groups {
        let count = cursor.read_u32_leb()?;
        let value_type = cursor.read_u8()?;
        if value_type != 0x7f {
            anyhow::bail!("minimal WASM executor supports only i32 locals");
        }
        locals.extend(std::iter::repeat_n(0_i32, count as usize));
    }
    let mut stack = Vec::<i32>::new();
    loop {
        match cursor.read_u8()? {
            0x0b => {
                cursor.expect_empty()?;
                return stack
                    .pop()
                    .context("WASM function ended without an i32 value");
            }
            0x41 => stack.push(cursor.read_i32_leb()?),
            0x20 => {
                let index = cursor.read_u32_leb()? as usize;
                let value = locals
                    .get(index)
                    .copied()
                    .with_context(|| format!("WASM local.get index {index} is out of range"))?;
                stack.push(value);
            }
            0x6a => {
                let rhs = stack.pop().context("WASM i32.add missing rhs")?;
                let lhs = stack.pop().context("WASM i32.add missing lhs")?;
                stack.push(lhs.wrapping_add(rhs));
            }
            0x6b => {
                let rhs = stack.pop().context("WASM i32.sub missing rhs")?;
                let lhs = stack.pop().context("WASM i32.sub missing lhs")?;
                stack.push(lhs.wrapping_sub(rhs));
            }
            0x6c => {
                let rhs = stack.pop().context("WASM i32.mul missing rhs")?;
                let lhs = stack.pop().context("WASM i32.mul missing lhs")?;
                stack.push(lhs.wrapping_mul(rhs));
            }
            0x10 => {
                let function_index = cursor.read_u32_leb()?;
                let value = execute_minimal_wasm_import_call(module, function_index, &mut stack)?;
                stack.push(value);
            }
            opcode => anyhow::bail!("unsupported WASM opcode 0x{opcode:02x}"),
        }
    }
}

fn execute_minimal_wasm_import_call(
    module: &MinimalWasmModule,
    function_index: u32,
    stack: &mut Vec<i32>,
) -> Result<i32> {
    let import = module
        .imported_functions
        .get(function_index as usize)
        .with_context(|| {
            format!(
                "minimal WASM executor can only call imported functions, got function index {function_index}"
            )
        })?;
    let function_type = module
        .types
        .get(import.type_index as usize)
        .with_context(|| {
            format!(
                "WASM import {}.{} type index is invalid",
                import.module, import.name
            )
        })?;
    if function_type.params.iter().any(|param| *param != 0x7f)
        || function_type.results.as_slice() != [0x7f]
    {
        anyhow::bail!(
            "WASM import {}.{} must use i32 params and return i32",
            import.module,
            import.name
        );
    }
    let mut args = Vec::with_capacity(function_type.params.len());
    for _ in 0..function_type.params.len() {
        args.push(stack.pop().context("WASM imported call missing argument")?);
    }
    args.reverse();
    match (import.module.as_str(), import.name.as_str()) {
        ("jellyrin:sdk", "echo_i32") => Ok(args.first().copied().unwrap_or_default()),
        ("jellyrin:sdk", "capability_argument_count") => Ok(args.len() as i32),
        _ => anyhow::bail!(
            "unsupported WASM host import {}.{}",
            import.module,
            import.name
        ),
    }
}

struct WasmCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WasmCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn is_empty(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn expect_empty(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("WASM section has trailing bytes")
        }
    }

    fn read_u8(&mut self) -> Result<u8> {
        let value = *self
            .bytes
            .get(self.position)
            .context("unexpected end of WASM bytes")?;
        self.position += 1;
        Ok(value)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .context("WASM byte range overflow")?;
        let value = self
            .bytes
            .get(self.position..end)
            .context("unexpected end of WASM bytes")?;
        self.position = end;
        Ok(value)
    }

    fn read_name(&mut self) -> Result<String> {
        let len = self.read_u32_leb()? as usize;
        let bytes = self.read_bytes(len)?;
        std::str::from_utf8(bytes)
            .context("WASM name is not UTF-8")
            .map(str::to_string)
    }

    fn read_u32_leb(&mut self) -> Result<u32> {
        let mut result = 0_u32;
        let mut shift = 0_u32;
        loop {
            let byte = self.read_u8()?;
            if shift >= 32 && (byte & 0x7f) != 0 {
                anyhow::bail!("WASM u32 LEB128 value overflowed");
            }
            result |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn read_i32_leb(&mut self) -> Result<i32> {
        let mut result = 0_i32;
        let mut shift = 0_u32;
        let mut byte;
        loop {
            byte = self.read_u8()?;
            result |= i32::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
            if shift >= 32 {
                anyhow::bail!("WASM i32 LEB128 value overflowed");
            }
        }
        if shift < 32 && (byte & 0x40) != 0 {
            result |= !0_i32 << shift;
        }
        Ok(result)
    }
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

    #[tokio::test]
    async fn wasi_host_rejects_incompatible_abi_and_ungranted_permissions() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(plugin_dir.join("fixture.wasm"), b"\0asm")
            .await
            .unwrap();

        let mut state = WasiHostState::default();
        let incompatible_abi = PluginRpcEnvelope::new(
            "corr-abi",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "abi-mismatch".to_string(),
                name: "ABI Mismatch".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: "jellyrin-wasi-99.0".to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "ABI Mismatch",
                    "TargetAbi": "jellyrin-wasi-99.0"
                }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, incompatible_abi).await;
        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            PluginRpcErrorCode::InvalidRequest
        );

        let ungranted_permission = PluginRpcEnvelope::new(
            "corr-permission",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "permission-missing".to_string(),
                name: "Permission Missing".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: TARGET_ABI.to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "Permission Missing",
                    "TargetAbi": TARGET_ABI,
                    "Permissions": [{ "Name": "Network" }]
                }),
                permissions: vec!["FileSystem".to_string()],
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, ungranted_permission).await;
        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            PluginRpcErrorCode::PermissionDenied
        );

        let granted_permission = PluginRpcEnvelope::new(
            "corr-permission-granted",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "permission-granted".to_string(),
                name: "Permission Granted".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: TARGET_ABI.to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "Permission Granted",
                    "TargetAbi": TARGET_ABI,
                    "Capabilities": ["ScheduledTask"],
                    "Permissions": [{ "Name": "Network" }]
                }),
                permissions: vec!["Network".to_string()],
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, granted_permission).await;
        assert!(response.ok);
    }

    #[tokio::test]
    async fn wasi_host_executes_declared_wasm_export_for_capability() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(
            plugin_dir.join("fixture.wasm"),
            minimal_i32_const_wasm("jellyrin_scheduled_task", 42),
        )
        .await
        .unwrap();

        let mut state = WasiHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-load-real-wasm",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "real-wasm-fixture".to_string(),
                name: "Real WASM Fixture".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: TARGET_ABI.to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "Real WASM Fixture",
                    "TargetAbi": TARGET_ABI,
                    "Capabilities": ["ScheduledTask"],
                    "WasmExports": {
                        "ScheduledTask": "jellyrin_scheduled_task"
                    }
                }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, load).await;
        assert!(response.ok);

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke-real-wasm",
            PluginRpcMethod::InvokeCapability,
            serde_json::to_value(InvokeCapabilityRequest {
                plugin_id: "real-wasm-fixture".to_string(),
                capability: "ScheduledTask".to_string(),
                arguments: json!({ "Trigger": "Manual" }),
                timeout_ms: 1000,
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, invoke).await;
        assert!(response.ok);
        let value = &response.result.as_ref().unwrap()["Value"];
        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], "ScheduledTask");
        assert_eq!(value["ExecutionMode"], "WasmI32Export");
        assert_eq!(value["Export"], "jellyrin_scheduled_task");
        assert_eq!(value["WasmArguments"], json!([]));
        assert_eq!(value["WasmReturn"], 42);
    }

    #[tokio::test]
    async fn wasi_host_invokes_declared_wasm_export_with_i32_argument() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(
            plugin_dir.join("fixture.wasm"),
            minimal_i32_param_add_const_wasm("jellyrin_scheduled_task", 2),
        )
        .await
        .unwrap();

        let mut state = WasiHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-load-i32-arg-wasm",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "i32-arg-wasm-fixture".to_string(),
                name: "I32 Arg WASM Fixture".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: TARGET_ABI.to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "I32 Arg WASM Fixture",
                    "TargetAbi": TARGET_ABI,
                    "Capabilities": ["ScheduledTask"],
                    "WasmExports": {
                        "ScheduledTask": "jellyrin_scheduled_task"
                    }
                }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, load).await;
        assert!(response.ok);

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke-i32-arg-wasm",
            PluginRpcMethod::InvokeCapability,
            serde_json::to_value(InvokeCapabilityRequest {
                plugin_id: "i32-arg-wasm-fixture".to_string(),
                capability: "ScheduledTask".to_string(),
                arguments: json!({ "Value": 40 }),
                timeout_ms: 1000,
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, invoke).await;
        assert!(response.ok);
        let value = &response.result.as_ref().unwrap()["Value"];
        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], "ScheduledTask");
        assert_eq!(value["ExecutionMode"], "WasmI32Export");
        assert_eq!(value["Export"], "jellyrin_scheduled_task");
        assert_eq!(value["WasmArguments"], json!([40]));
        assert_eq!(value["WasmReturn"], 42);
    }

    #[tokio::test]
    async fn wasi_host_invokes_declared_wasm_export_with_sdk_host_import() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(
            plugin_dir.join("fixture.wasm"),
            minimal_i32_param_sdk_echo_wasm("jellyrin_scheduled_task", 3),
        )
        .await
        .unwrap();

        let mut state = WasiHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-load-sdk-import-wasm",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "sdk-import-wasm-fixture".to_string(),
                name: "SDK Import WASM Fixture".to_string(),
                version: "1.0.0".to_string(),
                runtime: PluginRuntime::RustWasi,
                target_abi: TARGET_ABI.to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "SDK Import WASM Fixture",
                    "TargetAbi": TARGET_ABI,
                    "Capabilities": ["ScheduledTask"],
                    "WasmExports": {
                        "ScheduledTask": "jellyrin_scheduled_task"
                    }
                }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, load).await;
        assert!(response.ok);

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke-sdk-import-wasm",
            PluginRpcMethod::InvokeCapability,
            serde_json::to_value(InvokeCapabilityRequest {
                plugin_id: "sdk-import-wasm-fixture".to_string(),
                capability: "ScheduledTask".to_string(),
                arguments: json!({ "Value": 14 }),
                timeout_ms: 1000,
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, invoke).await;
        assert!(response.ok);
        let value = &response.result.as_ref().unwrap()["Value"];
        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], "ScheduledTask");
        assert_eq!(value["ExecutionMode"], "WasmI32Export");
        assert_eq!(value["Export"], "jellyrin_scheduled_task");
        assert_eq!(value["WasmArguments"], json!([14]));
        assert_eq!(value["WasmReturn"], 42);
    }

    fn minimal_i32_const_wasm(export_name: &str, value: i32) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(&mut wasm, 1, &[0x01, 0x60, 0x00, 0x01, 0x7f]);
        push_section(&mut wasm, 3, &[0x01, 0x00]);

        let mut export = Vec::new();
        push_u32_leb(&mut export, 1);
        push_name(&mut export, export_name);
        export.push(0x00);
        push_u32_leb(&mut export, 0);
        push_section(&mut wasm, 7, &export);

        let mut body = vec![0x00, 0x41];
        push_i32_leb(&mut body, value);
        body.push(0x0b);
        let mut code = Vec::new();
        push_u32_leb(&mut code, 1);
        push_u32_leb(&mut code, body.len() as u32);
        code.extend_from_slice(&body);
        push_section(&mut wasm, 10, &code);
        wasm
    }

    fn minimal_i32_param_add_const_wasm(export_name: &str, value: i32) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(&mut wasm, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);
        push_section(&mut wasm, 3, &[0x01, 0x00]);

        let mut export = Vec::new();
        push_u32_leb(&mut export, 1);
        push_name(&mut export, export_name);
        export.push(0x00);
        push_u32_leb(&mut export, 0);
        push_section(&mut wasm, 7, &export);

        let mut body = vec![0x00, 0x20, 0x00, 0x41];
        push_i32_leb(&mut body, value);
        body.push(0x6a);
        body.push(0x0b);
        let mut code = Vec::new();
        push_u32_leb(&mut code, 1);
        push_u32_leb(&mut code, body.len() as u32);
        code.extend_from_slice(&body);
        push_section(&mut wasm, 10, &code);
        wasm
    }

    fn minimal_i32_param_sdk_echo_wasm(export_name: &str, multiplier: i32) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(&mut wasm, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);

        let mut import = Vec::new();
        push_u32_leb(&mut import, 1);
        push_name(&mut import, "jellyrin:sdk");
        push_name(&mut import, "echo_i32");
        import.push(0x00);
        push_u32_leb(&mut import, 0);
        push_section(&mut wasm, 2, &import);

        push_section(&mut wasm, 3, &[0x01, 0x00]);

        let mut export = Vec::new();
        push_u32_leb(&mut export, 1);
        push_name(&mut export, export_name);
        export.push(0x00);
        push_u32_leb(&mut export, 1);
        push_section(&mut wasm, 7, &export);

        let mut body = vec![0x00, 0x20, 0x00, 0x10, 0x00, 0x41];
        push_i32_leb(&mut body, multiplier);
        body.push(0x6c);
        body.push(0x0b);
        let mut code = Vec::new();
        push_u32_leb(&mut code, 1);
        push_u32_leb(&mut code, body.len() as u32);
        code.extend_from_slice(&body);
        push_section(&mut wasm, 10, &code);
        wasm
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, content: &[u8]) {
        wasm.push(id);
        push_u32_leb(wasm, content.len() as u32);
        wasm.extend_from_slice(content);
    }

    fn push_name(wasm: &mut Vec<u8>, value: &str) {
        push_u32_leb(wasm, value.len() as u32);
        wasm.extend_from_slice(value.as_bytes());
    }

    fn push_u32_leb(wasm: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            wasm.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_i32_leb(wasm: &mut Vec<u8>, mut value: i32) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && (byte & 0x40) == 0) || (value == -1 && (byte & 0x40) != 0);
            wasm.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }
}
