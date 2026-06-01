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
    process::Stdio,
    time::Duration as StdDuration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const HOST_RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
const HOST_ID: &str = "jellyrin-plugin-host-dotnet";
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
struct DotNetHostState {
    loaded_plugins: BTreeMap<String, LoadedDotNetPlugin>,
    shutting_down: bool,
}

#[derive(Debug, Clone)]
struct LoadedDotNetPlugin {
    plugin_id: String,
    name: String,
    version: String,
    install_path: String,
    assembly_files: Vec<String>,
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
    let mut state = DotNetHostState::default();
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
    state: &mut DotNetHostState,
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
    if request.runtime != PluginRuntime::DotNetJellyfin {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            "jellyrin-plugin-host-dotnet only accepts DotNetJellyfin handshakes.",
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
    state: &mut DotNetHostState,
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
    if request.runtime != PluginRuntime::DotNetJellyfin {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::InvalidRequest,
            "DotNet host can only load DotNetJellyfin plugins.",
        );
    }
    let dll_files = match find_assembly_files(Path::new(&request.install_path)).await {
        Ok(files) => files,
        Err(error) => {
            return failure(
                envelope.correlation_id,
                PluginRpcErrorCode::PluginNotFound,
                error.to_string(),
            );
        }
    };
    if dll_files.is_empty() {
        return failure(
            envelope.correlation_id,
            PluginRpcErrorCode::PluginNotFound,
            "DotNetJellyfin plugin install path does not contain a .dll artifact.",
        );
    }
    let capabilities = manifest_capabilities(&request.manifest);
    let loaded = LoadedDotNetPlugin {
        plugin_id: request.plugin_id.clone(),
        name: request.name.clone(),
        version: request.version.clone(),
        install_path: request.install_path.clone(),
        assembly_files: dll_files,
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
            runtime: PluginRuntime::DotNetJellyfin,
            runtime_version: HOST_RUNTIME_VERSION.to_string(),
            status: PluginHealthStatus::Healthy,
            manifest: request.manifest,
            capabilities,
        },
    )
}

fn handle_unload_plugin(
    state: &mut DotNetHostState,
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
    state: &DotNetHostState,
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
    state: &DotNetHostState,
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
    state: &mut DotNetHostState,
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
    state: &DotNetHostState,
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
    state: &DotNetHostState,
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
    state: &DotNetHostState,
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
    state: &DotNetHostState,
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
            match execute_dotnet_capability_handler(plugin, &request).await {
                Ok(Some(value)) => value,
                Ok(None) => {
                    json!({
                        "Status": "NotExecuted",
                        "Reason": "DotNet assembly execution is not wired for this capability yet."
                    })
                }
                Err(error) => {
                    return failure(
                        envelope.correlation_id,
                        PluginRpcErrorCode::HostFailed,
                        format!("DotNet assembly execution failed: {error:#}"),
                    );
                }
            }
        };
    success(envelope.correlation_id, CapabilityResult { value })
}

fn handle_health(
    state: &DotNetHostState,
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
            runtime: PluginRuntime::DotNetJellyfin,
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

async fn find_assembly_files(path: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    find_assembly_files_recursive(path, &mut files)
        .await
        .with_context(|| {
            format!(
                "failed to inspect DotNetJellyfin plugin path {}",
                path.display()
            )
        })?;
    Ok(files)
}

async fn find_assembly_files_recursive(path: &Path, files: &mut Vec<String>) -> Result<()> {
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
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
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

async fn execute_dotnet_capability_handler(
    plugin: &LoadedDotNetPlugin,
    request: &InvokeCapabilityRequest,
) -> Result<Option<Value>> {
    let Some(handler) = manifest_dotnet_capability_handler(&plugin.manifest, &request.capability)
    else {
        return Ok(None);
    };
    let Some(relative_assembly) = handler
        .assembly
        .as_deref()
        .and_then(safe_relative_path)
        .or_else(|| default_plugin_assembly_path(plugin))
    else {
        return Ok(None);
    };
    let assembly_path = Path::new(&plugin.install_path).join(relative_assembly);
    if !assembly_path.is_file() {
        anyhow::bail!("DotNet assembly {} does not exist", assembly_path.display());
    }
    let arguments_json = serde_json::to_string(&request.arguments)?;
    let mut command = Command::new(dotnet_command());
    command
        .arg(&assembly_path)
        .args(expand_dotnet_handler_arguments(
            &handler.arguments,
            request,
            &arguments_json,
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let timeout = StdDuration::from_millis(request.timeout_ms.clamp(250, 30_000));
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .context("DotNet assembly execution timed out")?
        .context("failed to spawn dotnet assembly")?;
    if !output.status.success() {
        anyhow::bail!(
            "DotNet assembly exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout =
        String::from_utf8(output.stdout).context("DotNet assembly stdout was not UTF-8")?;
    let mut value: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("DotNet assembly stdout was not JSON: {}", stdout.trim()))?;
    if !value.is_object() {
        value = json!({ "Value": value });
    }
    value["Status"] = value
        .get("Status")
        .cloned()
        .unwrap_or_else(|| json!("Executed"));
    value["Capability"] = value
        .get("Capability")
        .cloned()
        .unwrap_or_else(|| json!(request.capability));
    value["ExecutionMode"] = value
        .get("ExecutionMode")
        .cloned()
        .unwrap_or_else(|| json!("DotNetAssembly"));
    Ok(Some(value))
}

#[derive(Debug, Default)]
struct DotNetCapabilityHandler {
    assembly: Option<String>,
    arguments: Vec<String>,
}

fn manifest_dotnet_capability_handler(
    manifest: &Value,
    capability: &str,
) -> Option<DotNetCapabilityHandler> {
    let handlers = manifest
        .get("DotNetExports")
        .or_else(|| manifest.get("DotNetEntryPoints"))
        .or_else(|| manifest.get("AssemblyHandlers"))?;
    if let Some(handler) = handlers.get(capability) {
        return dotnet_capability_handler_from_value(handler);
    }
    handlers.as_array()?.iter().find_map(|handler| {
        let entry_capability = handler
            .get("Capability")
            .or_else(|| handler.get("Name"))
            .and_then(Value::as_str)?;
        entry_capability
            .eq_ignore_ascii_case(capability)
            .then(|| dotnet_capability_handler_from_value(handler))
            .flatten()
    })
}

fn dotnet_capability_handler_from_value(value: &Value) -> Option<DotNetCapabilityHandler> {
    if let Some(assembly) = value.as_str().filter(|value| !value.trim().is_empty()) {
        return Some(DotNetCapabilityHandler {
            assembly: Some(assembly.trim().to_string()),
            arguments: default_dotnet_handler_arguments(),
        });
    }
    let assembly = value
        .get("Assembly")
        .or_else(|| value.get("Path"))
        .or_else(|| value.get("Dll"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    let arguments = value
        .get("Arguments")
        .and_then(Value::as_array)
        .map(|arguments| {
            arguments
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|arguments| !arguments.is_empty())
        .unwrap_or_else(default_dotnet_handler_arguments);
    Some(DotNetCapabilityHandler {
        assembly,
        arguments,
    })
}

fn default_dotnet_handler_arguments() -> Vec<String> {
    vec![
        "--capability".to_string(),
        "{Capability}".to_string(),
        "--arguments".to_string(),
        "{ArgumentsJson}".to_string(),
    ]
}

fn expand_dotnet_handler_arguments(
    arguments: &[String],
    request: &InvokeCapabilityRequest,
    arguments_json: &str,
) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| {
            argument
                .replace("{Capability}", &request.capability)
                .replace("{ArgumentsJson}", arguments_json)
                .replace("{PluginId}", &request.plugin_id)
        })
        .collect()
}

fn default_plugin_assembly_path(plugin: &LoadedDotNetPlugin) -> Option<PathBuf> {
    let path = Path::new(plugin.assembly_files.first()?);
    path.file_name().map(PathBuf::from)
}

fn dotnet_command() -> String {
    std::env::var("JELLYRIN_DOTNET_HOST_DOTNET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "dotnet".to_string())
}

fn manifest_configuration(manifest: &Value) -> Value {
    manifest
        .get("Configuration")
        .or_else(|| manifest.get("DefaultConfiguration"))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn manifest_web_pages(plugin: &LoadedDotNetPlugin) -> Vec<PluginWebPage> {
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

fn manifest_web_page(plugin: &LoadedDotNetPlugin, page: &Value) -> Option<PluginWebPage> {
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
    use tempfile::tempdir;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn dotnet_host_handshake_load_health_and_shutdown() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(plugin_dir.join("Jellyfin.Plugin.Fixture.dll"), b"dll")
            .await
            .unwrap();
        tokio::fs::write(plugin_dir.join("logo.png"), b"dotnet-image")
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
                runtime: PluginRuntime::DotNetJellyfin,
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
                plugin_id: "dotnet-fixture".to_string(),
                name: "DotNet Fixture".to_string(),
                version: "1.0.0.0".to_string(),
                runtime: PluginRuntime::DotNetJellyfin,
                target_abi: "12.0.0.0".to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "DotNet Fixture",
                    "Capabilities": ["MetadataProvider"],
                    "Configuration": { "Enabled": true },
                    "WebPages": [{
                        "Name": "dotnet-config",
                        "DisplayName": "DotNet Config",
                        "Path": "configuration.html",
                        "EnableInMainMenu": true
                    }],
                    "Images": [{
                        "ImageType": "Primary",
                        "Path": "logo.png",
                        "MimeType": "image/png"
                    }],
                    "CapabilityHandlers": {
                        "MetadataProvider": {
                            "EchoArguments": true,
                            "Result": {
                                "Provider": "DotNet Fixture"
                            }
                        }
                    }
                }),
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
            "MetadataProvider"
        );

        let identity = PluginIdentity {
            plugin_id: "dotnet-fixture".to_string(),
            version: "1.0.0.0".to_string(),
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
        assert_eq!(response.result.as_ref().unwrap()["Enabled"], true);

        let update_configuration = PluginRpcEnvelope::new(
            "corr-update-config",
            PluginRpcMethod::UpdateConfiguration,
            UpdateConfigurationRequest {
                plugin_id: "dotnet-fixture".to_string(),
                configuration: json!({ "Enabled": false }),
            },
        );
        let response = client
            .call::<_, Value>(&update_configuration, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["Enabled"], false);

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
        assert_eq!(
            response.result.as_ref().unwrap()[0]["Name"],
            "dotnet-config"
        );
        assert_eq!(
            response.result.as_ref().unwrap()[0]["EnableInMainMenu"],
            true
        );

        let image = PluginRpcEnvelope::new(
            "corr-image",
            PluginRpcMethod::GetEmbeddedImage,
            EmbeddedImageRequest {
                plugin_id: "dotnet-fixture".to_string(),
                version: "1.0.0.0".to_string(),
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
            general_purpose::STANDARD.encode(b"dotnet-image")
        );

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke",
            PluginRpcMethod::InvokeCapability,
            InvokeCapabilityRequest {
                plugin_id: "dotnet-fixture".to_string(),
                capability: "MetadataProvider".to_string(),
                arguments: json!({ "ItemId": "movie-1" }),
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
        assert_eq!(value["Capability"], "MetadataProvider");
        assert_eq!(value["Provider"], "DotNet Fixture");
        assert_eq!(value["Arguments"]["ItemId"], "movie-1");

        let health = PluginRpcEnvelope::new("corr-health", PluginRpcMethod::Health, identity);
        let response = client
            .call::<_, Value>(&health, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.result.as_ref().unwrap()["Status"], "Healthy");

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
    async fn dotnet_host_rejects_missing_dll_and_unloaded_configuration() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();

        let mut state = DotNetHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-missing",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "missing".to_string(),
                name: "Missing".to_string(),
                version: "1.0.0.0".to_string(),
                runtime: PluginRuntime::DotNetJellyfin,
                target_abi: "12.0.0.0".to_string(),
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
                version: "1.0.0.0".to_string(),
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
    async fn dotnet_host_executes_declared_assembly_for_capability() {
        let temp = tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        compile_dotnet_fixture(&plugin_dir.join("Jellyfin.Plugin.RealFixture.dll")).await;

        let mut state = DotNetHostState::default();
        let load = PluginRpcEnvelope::new(
            "corr-load-real-dotnet",
            PluginRpcMethod::LoadPlugin,
            serde_json::to_value(LoadPluginRequest {
                plugin_id: "real-dotnet-fixture".to_string(),
                name: "Real DotNet Fixture".to_string(),
                version: "1.0.0.0".to_string(),
                runtime: PluginRuntime::DotNetJellyfin,
                target_abi: "12.0.0.0".to_string(),
                install_path: plugin_dir.to_string_lossy().to_string(),
                manifest: json!({
                    "Name": "Real DotNet Fixture",
                    "Capabilities": ["MetadataProvider"],
                    "DotNetExports": {
                        "MetadataProvider": {
                            "Assembly": "Jellyfin.Plugin.RealFixture.dll"
                        }
                    }
                }),
                permissions: Vec::new(),
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, load).await;
        assert!(response.ok);

        let invoke = PluginRpcEnvelope::new(
            "corr-invoke-real-dotnet",
            PluginRpcMethod::InvokeCapability,
            serde_json::to_value(InvokeCapabilityRequest {
                plugin_id: "real-dotnet-fixture".to_string(),
                capability: "MetadataProvider".to_string(),
                arguments: json!({ "ItemId": "movie-1" }),
                timeout_ms: 5000,
            })
            .unwrap(),
        );
        let response = handle_envelope(&mut state, invoke).await;
        assert!(response.ok, "{:?}", response.error);
        let value = &response.result.as_ref().unwrap()["Value"];
        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], "MetadataProvider");
        assert_eq!(value["ExecutionMode"], "DotNetAssembly");
        assert_eq!(value["Provider"], "Compiled DotNet Fixture");
        assert_eq!(value["SawCapability"], "MetadataProvider");
        assert_eq!(value["SawArguments"], "{\"ItemId\":\"movie-1\"}");
    }

    async fn compile_dotnet_fixture(output_path: &Path) {
        let source_path = output_path.with_extension("cs");
        tokio::fs::write(
            &source_path,
            r#"using System;

public static class Program
{
    public static int Main(string[] args)
    {
        string capability = "";
        string arguments = "{}";
        for (int i = 0; i < args.Length; i++)
        {
            if (args[i] == "--capability" && i + 1 < args.Length)
            {
                capability = args[++i];
            }
            else if (args[i] == "--arguments" && i + 1 < args.Length)
            {
                arguments = args[++i];
            }
        }

        Console.WriteLine("{\"Status\":\"Executed\",\"Provider\":\"Compiled DotNet Fixture\",\"SawCapability\":\"" + Escape(capability) + "\",\"SawArguments\":\"" + Escape(arguments) + "\"}");
        return 0;
    }

    private static string Escape(string value)
    {
        return value.Replace("\\", "\\\\").Replace("\"", "\\\"");
    }
}
"#,
        )
        .await
        .unwrap();
        let csc_path = dotnet_sdk_root().join("Roslyn/bincore/csc.dll");
        assert!(
            csc_path.is_file(),
            "missing csc.dll at {}",
            csc_path.display()
        );
        let ref_dir = dotnet_ref_dir();
        let mut command = Command::new(dotnet_command());
        command
            .arg(csc_path)
            .arg("-nologo")
            .arg("-target:exe")
            .arg(format!("-out:{}", output_path.display()))
            .arg("-langversion:latest");
        for reference in std::fs::read_dir(&ref_dir).unwrap() {
            let path = reference.unwrap().path();
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
            {
                command.arg(format!("-reference:{}", path.display()));
            }
        }
        command.arg(&source_path);
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "csc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::fs::write(
            output_path.with_file_name("Jellyfin.Plugin.RealFixture.runtimeconfig.json"),
            r#"{"runtimeOptions":{"tfm":"net10.0","framework":{"name":"Microsoft.NETCore.App","version":"10.0.0"}}}"#,
        )
        .await
        .unwrap();
    }

    fn dotnet_sdk_root() -> PathBuf {
        let output = std::process::Command::new(dotnet_command())
            .arg("--info")
            .output()
            .unwrap();
        assert!(output.status.success(), "dotnet --info failed");
        let text = String::from_utf8(output.stdout).unwrap();
        for line in text.lines() {
            if let Some(path) = line.trim().strip_prefix("Base Path:") {
                return PathBuf::from(path.trim());
            }
        }
        panic!("dotnet --info did not include SDK base path");
    }

    fn dotnet_ref_dir() -> PathBuf {
        let dotnet_root = std::env::var_os("DOTNET_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/local/share/dotnet"));
        let pack_root = dotnet_root.join("packs").join("Microsoft.NETCore.App.Ref");
        let mut versions = std::fs::read_dir(&pack_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.join("ref/net10.0").is_dir())
            .collect::<Vec<_>>();
        versions.sort();
        versions
            .pop()
            .expect("missing Microsoft.NETCore.App.Ref net10.0 pack")
            .join("ref/net10.0")
    }
}
