use jellyrin_plugin_rpc::{
    HandshakeRequest, HandshakeResponse, LoadPluginRequest, LoadedPlugin,
    PLUGIN_RPC_PROTOCOL_VERSION, PluginHealth, PluginHostStdioClient, PluginIdentity,
    PluginRpcEnvelope, PluginRpcMethod, PluginRuntime,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use tempfile::tempdir;
use tokio::process::Command;

#[tokio::test]
async fn wasi_host_binary_round_trips_over_stdio() {
    let mut client = PluginHostStdioClient::spawn(&mut Command::new(host_binary_path())).unwrap();
    let temp = tempdir().unwrap();
    let plugin_dir = temp.path().join("plugin");
    tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
    tokio::fs::write(plugin_dir.join("fixture.wasm"), b"\0asm")
        .await
        .unwrap();

    let handshake = PluginRpcEnvelope::new(
        "smoke-handshake",
        PluginRpcMethod::Handshake,
        HandshakeRequest {
            runtime: PluginRuntime::RustWasi,
            runtime_version: "0.1.0".to_string(),
            host_id: "stdio-smoke".to_string(),
            supported_protocol_versions: vec![PLUGIN_RPC_PROTOCOL_VERSION],
            capabilities: Vec::new(),
        },
    );
    let handshake_response = client
        .call::<_, HandshakeResponse>(&handshake, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(handshake_response.ok);
    let handshake_result = handshake_response.result.unwrap();
    assert_eq!(
        handshake_result.accepted_protocol_version,
        PLUGIN_RPC_PROTOCOL_VERSION
    );
    assert!(
        handshake_result
            .capabilities
            .iter()
            .any(|capability| capability == "LoadPlugin")
    );

    let load = PluginRpcEnvelope::new(
        "smoke-load",
        PluginRpcMethod::LoadPlugin,
        LoadPluginRequest {
            plugin_id: "stdio-wasi-fixture".to_string(),
            name: "Stdio WASI Fixture".to_string(),
            version: "1.0.0".to_string(),
            runtime: PluginRuntime::RustWasi,
            target_abi: "jellyrin-wasi-0.1".to_string(),
            install_path: plugin_dir.to_string_lossy().to_string(),
            manifest: json!({
                "Name": "Stdio WASI Fixture",
                "Capabilities": ["ScheduledTask", "MetadataProvider"]
            }),
            permissions: Vec::new(),
        },
    );
    let load_response = client
        .call::<_, LoadedPlugin>(&load, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(load_response.ok);
    let loaded = load_response.result.unwrap();
    assert_eq!(loaded.plugin_id, "stdio-wasi-fixture");
    assert_eq!(loaded.runtime, PluginRuntime::RustWasi);
    assert_eq!(loaded.capabilities, ["ScheduledTask", "MetadataProvider"]);

    let health = PluginRpcEnvelope::new(
        "smoke-health",
        PluginRpcMethod::Health,
        PluginIdentity {
            plugin_id: "stdio-wasi-fixture".to_string(),
            version: "1.0.0".to_string(),
        },
    );
    let health_response = client
        .call::<_, PluginHealth>(&health, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(health_response.ok);
    let health = health_response.result.unwrap();
    assert_eq!(health.plugin_id, "stdio-wasi-fixture");
    assert_eq!(health.runtime, PluginRuntime::RustWasi);
    assert_eq!(health.metrics["CapabilityCount"], 2);

    let shutdown = PluginRpcEnvelope::new("smoke-shutdown", PluginRpcMethod::Shutdown, json!({}));
    let shutdown_response = client
        .call::<_, serde_json::Value>(&shutdown, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(shutdown_response.ok);
    client.shutdown().await.unwrap();
}

fn host_binary_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jellyrin-plugin-host-wasi")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut path = std::env::current_exe().unwrap();
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path.push(format!(
                "jellyrin-plugin-host-wasi{}",
                std::env::consts::EXE_SUFFIX
            ));
            path
        })
}
