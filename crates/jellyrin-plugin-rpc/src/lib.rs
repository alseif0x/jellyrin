use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, io, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use zeroize::Zeroizing;

pub const PLUGIN_RPC_PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Time allowed for the host to acknowledge the protocol-level shutdown request.
pub const DEFAULT_PLUGIN_HOST_SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
/// Time allowed for the host process group to handle `SIGTERM` before `SIGKILL`.
pub const DEFAULT_PLUGIN_HOST_STOP_GRACE_PERIOD: Duration = Duration::from_millis(500);
#[cfg(unix)]
const PLUGIN_HOST_STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    #[error("plugin RPC stream ended before a complete message was read")]
    UnexpectedEof,
    #[error("plugin RPC JSON codec failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(thiserror::Error)]
pub enum PluginRpcTransportError {
    #[error(transparent)]
    Codec(#[from] PluginRpcCodecError),
    #[error("plugin RPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("plugin RPC call timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
    // Both ids cross an untrusted process boundary and may contain reflected secrets. Keep the
    // values for programmatic diagnostics, but never interpolate them into Display output.
    #[error("plugin RPC response correlation mismatch")]
    CorrelationMismatch { expected: String, actual: String },
    #[error("plugin host process did not expose {stream} pipe")]
    MissingPipe { stream: &'static str },
}

impl fmt::Debug for PluginRpcTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => formatter.debug_tuple("Codec").field(error).finish(),
            Self::Io(error) => formatter.debug_tuple("Io").field(error).finish(),
            Self::Timeout { timeout_ms } => formatter
                .debug_struct("Timeout")
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::CorrelationMismatch { .. } => {
                formatter.write_str("CorrelationMismatch([REDACTED])")
            }
            Self::MissingPipe { stream } => formatter
                .debug_struct("MissingPipe")
                .field("stream", stream)
                .finish(),
        }
    }
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

pub struct PluginRpcJsonLineTransport<R, W> {
    reader: R,
    writer: W,
    max_message_bytes: usize,
}

impl<R, W> PluginRpcJsonLineTransport<R, W>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self::with_max_message_bytes(reader, writer, DEFAULT_PLUGIN_RPC_MAX_MESSAGE_BYTES)
    }

    pub fn with_max_message_bytes(reader: R, writer: W, max_message_bytes: usize) -> Self {
        Self {
            reader,
            writer,
            max_message_bytes,
        }
    }

    pub async fn send<T: Serialize>(
        &mut self,
        envelope: &PluginRpcEnvelope<T>,
    ) -> Result<(), PluginRpcTransportError> {
        let bytes = Zeroizing::new(encode_json_line(envelope, self.max_message_bytes)?);
        self.writer.write_all(bytes.as_slice()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn read_response<T: for<'de> Deserialize<'de>>(
        &mut self,
    ) -> Result<PluginRpcResponse<T>, PluginRpcTransportError> {
        let mut line = Zeroizing::new(Vec::with_capacity(self.max_message_bytes.min(8 * 1024) + 1));
        self.read_bounded_line(&mut line).await?;
        decode_json_line(line.as_slice(), self.max_message_bytes).map_err(Into::into)
    }

    async fn read_bounded_line(
        &mut self,
        line: &mut Vec<u8>,
    ) -> Result<(), PluginRpcTransportError> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                return Err(PluginRpcCodecError::UnexpectedEof.into());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let payload_bytes = newline.unwrap_or(available.len());
            if line.len().saturating_add(payload_bytes) > self.max_message_bytes {
                return Err(PluginRpcCodecError::MessageTooLarge {
                    limit: self.max_message_bytes,
                }
                .into());
            }
            let consumed = newline.map_or(available.len(), |index| index + 1);
            line.extend_from_slice(&available[..consumed]);
            self.reader.consume(consumed);
            if newline.is_some() {
                return Ok(());
            }
        }
    }

    pub async fn call<Req, Resp>(
        &mut self,
        envelope: &PluginRpcEnvelope<Req>,
        timeout: Duration,
    ) -> Result<PluginRpcResponse<Resp>, PluginRpcTransportError>
    where
        Req: Serialize,
        Resp: for<'de> Deserialize<'de>,
    {
        let timeout_ms = timeout.as_millis().try_into().unwrap_or(u64::MAX);
        let response = tokio::time::timeout(timeout, async {
            self.send(envelope).await?;
            self.read_response::<Resp>().await
        })
        .await
        .map_err(|_| PluginRpcTransportError::Timeout { timeout_ms })??;

        if response.correlation_id != envelope.correlation_id {
            return Err(PluginRpcTransportError::CorrelationMismatch {
                expected: envelope.correlation_id.clone(),
                actual: response.correlation_id,
            });
        }
        Ok(response)
    }
}

pub struct PluginHostStdioClient {
    child: Option<Child>,
    #[cfg(unix)]
    process_group_id: Option<libc::pid_t>,
    transport: PluginRpcJsonLineTransport<BufReader<ChildStdout>, ChildStdin>,
}

impl PluginHostStdioClient {
    pub fn spawn(command: &mut Command) -> Result<Self, PluginRpcTransportError> {
        configure_plugin_host_command(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Runtime stderr is intentionally discarded: an untrusted plugin could otherwise
            // block on a full pipe or disclose resolved provider URLs through server logs.
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or(PluginRpcTransportError::MissingPipe { stream: "stdin" })?;
        let stdout = child
            .stdout
            .take()
            .ok_or(PluginRpcTransportError::MissingPipe { stream: "stdout" })?;
        #[cfg(unix)]
        let process_group_id = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
        Ok(Self {
            child: Some(child),
            #[cfg(unix)]
            process_group_id,
            transport: PluginRpcJsonLineTransport::new(BufReader::new(stdout), stdin),
        })
    }

    pub async fn call<Req, Resp>(
        &mut self,
        envelope: &PluginRpcEnvelope<Req>,
        timeout: Duration,
    ) -> Result<PluginRpcResponse<Resp>, PluginRpcTransportError>
    where
        Req: Serialize,
        Resp: for<'de> Deserialize<'de>,
    {
        if self.child.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "plugin host process is no longer available",
            )
            .into());
        }

        let result = self.transport.call(envelope, timeout).await;
        // A timed-out JSON-lines exchange cannot be resumed safely: its eventual response would
        // be mistaken for the next call. Tear down the complete process group immediately so a
        // wedged helper cannot survive while the client is retained by a persistent host lane.
        if matches!(result, Err(PluginRpcTransportError::Timeout { .. })) {
            let _ = self
                .terminate_and_reap(DEFAULT_PLUGIN_HOST_STOP_GRACE_PERIOD)
                .await;
        }
        result
    }

    pub async fn shutdown(mut self) -> Result<(), PluginRpcTransportError> {
        let should_request_shutdown = match self.child.as_mut() {
            Some(child) => child.try_wait()?.is_none(),
            None => false,
        };
        if should_request_shutdown {
            let request = PluginRpcEnvelope::new(
                "jellyrin-host-shutdown",
                PluginRpcMethod::Shutdown,
                Value::Object(Default::default()),
            );
            let _ = self
                .transport
                .call::<_, Value>(&request, DEFAULT_PLUGIN_HOST_SHUTDOWN_REQUEST_TIMEOUT)
                .await;
        }
        self.terminate_and_reap(DEFAULT_PLUGIN_HOST_STOP_GRACE_PERIOD)
            .await?;
        Ok(())
    }

    async fn terminate_and_reap(&mut self, grace_period: Duration) -> io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            #[cfg(unix)]
            {
                self.process_group_id = None;
            }
            return Ok(());
        };

        #[cfg(unix)]
        terminate_process_group(child, self.process_group_id, grace_period).await?;

        #[cfg(not(unix))]
        {
            let _ = grace_period;
            force_kill_and_reap(child).await?;
        }

        #[cfg(unix)]
        {
            self.process_group_id = None;
        }
        self.child = None;
        Ok(())
    }
}

impl Drop for PluginHostStdioClient {
    fn drop(&mut self) {
        // Drop cannot wait, but killing the dedicated group synchronously closes the cancellation
        // path used by one-shot grants. The direct child is then handed to Tokio for reaping when
        // a runtime is available; `kill_on_drop` remains the portable last line of defence.
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id.take() {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
        }

        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

fn configure_plugin_host_command(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Isolate every host before exec so shutdown, revocation, timeout and cancellation can
        // target wrappers and all helpers without signalling Jellyrin's own process group.
        command.as_std_mut().process_group(0);
    }

    #[cfg(not(unix))]
    let _ = command;
}

async fn force_kill_and_reap(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    if let Err(error) = child.start_kill() {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        return Err(error);
    }
    child.wait().await
}

#[cfg(unix)]
async fn terminate_process_group(
    child: &mut Child,
    process_group_id: Option<libc::pid_t>,
    grace_period: Duration,
) -> io::Result<std::process::ExitStatus> {
    let Some(process_group_id) = process_group_id.filter(|id| *id > 0) else {
        return force_kill_and_reap(child).await;
    };

    let mut status = child.try_wait()?;
    if process_group_exists(process_group_id)? {
        if let Err(error) = signal_process_group(process_group_id, libc::SIGTERM) {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
            if status.is_none() {
                return force_kill_and_reap(child).await;
            }
            return Err(error);
        }

        let deadline = tokio::time::Instant::now() + grace_period;
        loop {
            if status.is_none() {
                status = child.try_wait()?;
            }
            if !process_group_exists(process_group_id).unwrap_or(true) {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                let _ = signal_process_group(process_group_id, libc::SIGKILL);
                break;
            }
            tokio::time::sleep(
                PLUGIN_HOST_STOP_POLL_INTERVAL.min(deadline.saturating_duration_since(now)),
            )
            .await;
        }
    }

    match status {
        Some(status) => Ok(status),
        None => force_kill_and_reap(child).await,
    }
}

#[cfg(unix)]
fn signal_process_group(process_group_id: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if process_group_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be positive",
        ));
    }

    // SAFETY: this positive id was captured from a child that was configured as its own group
    // leader, so negation targets only that dedicated process group.
    if unsafe { libc::kill(-process_group_id, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn process_group_exists(process_group_id: libc::pid_t) -> io::Result<bool> {
    if process_group_id <= 0 {
        return Ok(false);
    }

    // SAFETY: signal zero only checks the dedicated group captured at spawn time.
    if unsafe { libc::kill(-process_group_id, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
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
    #[serde(default)]
    pub capabilities: Vec<String>,
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UpdateConfigurationRequest {
    pub plugin_id: String,
    pub configuration: Value,
}

impl std::fmt::Debug for UpdateConfigurationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateConfigurationRequest")
            .field("plugin_id", &self.plugin_id)
            .field("configuration", &"[REDACTED]")
            .finish()
    }
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InvokeCapabilityRequest {
    pub plugin_id: String,
    pub capability: String,
    #[serde(default)]
    pub arguments: Value,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for InvokeCapabilityRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InvokeCapabilityRequest")
            .field("plugin_id", &self.plugin_id)
            .field("capability", &self.capability)
            .field("arguments", &"[REDACTED]")
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityResult {
    #[serde(default)]
    pub value: Value,
}

impl std::fmt::Debug for CapabilityResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityResult")
            .field("value", &"[REDACTED]")
            .finish()
    }
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
    use tokio::io::{AsyncWriteExt, BufReader};

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
    fn capability_result_debug_redacts_arbitrary_plugin_values() {
        let result = CapabilityResult {
            value: json!({
                "SourceUrl": "https://provider.invalid/live?token=must-not-leak"
            }),
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("must-not-leak"));
        assert_eq!(
            serde_json::to_value(result).unwrap()["Value"]["SourceUrl"],
            "https://provider.invalid/live?token=must-not-leak"
        );
    }

    #[test]
    fn invoke_capability_request_round_trips_but_redacts_arguments_in_debug() {
        let request = InvokeCapabilityRequest {
            plugin_id: "plugin-a".to_string(),
            capability: "LiveTvProvider".to_string(),
            arguments: json!({
                "SecretGrant": {
                    "Password": "provider-password"
                }
            }),
            timeout_ms: 2_500,
        };

        let debug = format!("{request:?}");
        assert!(debug.contains("plugin-a"));
        assert!(debug.contains("LiveTvProvider"));
        assert!(debug.contains("2500"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("provider-password"));

        let encoded = encode_json_line(&request, MAX_MESSAGE_BYTES).unwrap();
        let decoded: InvokeCapabilityRequest =
            decode_json_line(&encoded, MAX_MESSAGE_BYTES).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(
            decoded.arguments["SecretGrant"]["Password"],
            "provider-password"
        );
    }

    #[test]
    fn update_configuration_request_round_trips_but_redacts_configuration_in_debug() {
        let request = UpdateConfigurationRequest {
            plugin_id: "plugin-a".to_string(),
            configuration: json!({ "Password": "provider-password" }),
        };

        let debug = format!("{request:?}");
        assert!(debug.contains("plugin-a"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("provider-password"));

        let encoded = encode_json_line(&request, MAX_MESSAGE_BYTES).unwrap();
        let decoded: UpdateConfigurationRequest =
            decode_json_line(&encoded, MAX_MESSAGE_BYTES).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(decoded.configuration["Password"], "provider-password");
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

    #[tokio::test]
    async fn json_line_transport_bounds_a_message_before_newline() {
        let (client_stream, mut host_stream) = tokio::io::duplex(64);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let writer = tokio::spawn(async move {
            host_stream.write_all(&[b'x'; 33]).await.unwrap();
            host_stream.flush().await.unwrap();
        });
        let mut transport = PluginRpcJsonLineTransport::with_max_message_bytes(
            BufReader::new(client_read),
            client_write,
            32,
        );

        let error = transport.read_response::<Value>().await.unwrap_err();
        writer.await.unwrap();
        assert!(matches!(
            error,
            PluginRpcTransportError::Codec(PluginRpcCodecError::MessageTooLarge { limit: 32 })
        ));
    }

    #[tokio::test]
    async fn json_line_transport_calls_host_and_checks_correlation() {
        let (client_stream, host_stream) = tokio::io::duplex(MAX_MESSAGE_BYTES);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (host_read, mut host_write) = tokio::io::split(host_stream);
        let host = tokio::spawn(async move {
            let mut host_reader = BufReader::new(host_read);
            let mut line = Vec::new();
            host_reader.read_until(b'\n', &mut line).await.unwrap();
            let request: PluginRpcEnvelope<HandshakeRequest> =
                decode_json_line(&line, MAX_MESSAGE_BYTES).unwrap();
            assert_eq!(request.correlation_id, "corr-transport");
            let response = PluginRpcResponse::success(
                request.correlation_id,
                HandshakeResponse {
                    accepted_protocol_version: PLUGIN_RPC_PROTOCOL_VERSION,
                    server_name: "Jellyrin".to_string(),
                    server_version: "12.0.0".to_string(),
                    minimum_call_timeout_ms: 250,
                    capabilities: vec!["Health".to_string()],
                },
            );
            let bytes = encode_json_line(&response, MAX_MESSAGE_BYTES).unwrap();
            host_write.write_all(&bytes).await.unwrap();
            host_write.flush().await.unwrap();
        });

        let mut transport =
            PluginRpcJsonLineTransport::new(BufReader::new(client_read), client_write);
        let request = PluginRpcEnvelope::new(
            "corr-transport",
            PluginRpcMethod::Handshake,
            HandshakeRequest {
                runtime: PluginRuntime::RustWasi,
                runtime_version: "0.1.0".to_string(),
                host_id: "host-a".to_string(),
                supported_protocol_versions: vec![PLUGIN_RPC_PROTOCOL_VERSION],
                capabilities: Vec::new(),
            },
        );
        let response: PluginRpcResponse<HandshakeResponse> = transport
            .call(&request, Duration::from_secs(1))
            .await
            .unwrap();

        host.await.unwrap();
        assert!(response.ok);
        assert_eq!(
            response.result.unwrap().accepted_protocol_version,
            PLUGIN_RPC_PROTOCOL_VERSION
        );
    }

    #[tokio::test]
    async fn json_line_transport_rejects_correlation_mismatch() {
        let (client_stream, host_stream) = tokio::io::duplex(MAX_MESSAGE_BYTES);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (host_read, mut host_write) = tokio::io::split(host_stream);
        let host = tokio::spawn(async move {
            let mut host_reader = BufReader::new(host_read);
            let mut line = Vec::new();
            host_reader.read_until(b'\n', &mut line).await.unwrap();
            let response =
                PluginRpcResponse::success("wrong-correlation", json!({ "Status": "Healthy" }));
            let bytes = encode_json_line(&response, MAX_MESSAGE_BYTES).unwrap();
            host_write.write_all(&bytes).await.unwrap();
            host_write.flush().await.unwrap();
        });

        let mut transport =
            PluginRpcJsonLineTransport::new(BufReader::new(client_read), client_write);
        let request = PluginRpcEnvelope::new(
            "corr-expected",
            PluginRpcMethod::Health,
            PluginIdentity {
                plugin_id: "plugin".to_string(),
                version: "1.0.0.0".to_string(),
            },
        );

        let error = transport
            .call::<_, Value>(&request, Duration::from_secs(1))
            .await
            .unwrap_err();
        host.await.unwrap();
        let display = format!("{error}");
        let debug = format!("{error:?}");
        for untrusted_id in ["corr-expected", "wrong-correlation"] {
            assert!(!display.contains(untrusted_id));
            assert!(!debug.contains(untrusted_id));
        }
        assert!(matches!(
            error,
            PluginRpcTransportError::CorrelationMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn json_line_transport_times_out_waiting_for_host() {
        let (client_stream, _host_stream) = tokio::io::duplex(MAX_MESSAGE_BYTES);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let mut transport =
            PluginRpcJsonLineTransport::new(BufReader::new(client_read), client_write);
        let request = PluginRpcEnvelope::new(
            "corr-timeout",
            PluginRpcMethod::Health,
            PluginIdentity {
                plugin_id: "plugin".to_string(),
                version: "1.0.0.0".to_string(),
            },
        );

        let error = transport
            .call::<_, Value>(&request, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(matches!(error, PluginRpcTransportError::Timeout { .. }));
    }

    #[cfg(unix)]
    async fn wait_for_fixture_pid(path: &std::path::Path) -> libc::pid_t {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(path).await
                    && let Ok(process_id) = value.trim().parse::<libc::pid_t>()
                {
                    return process_id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("plugin host fixture did not publish its helper pid")
    }

    #[cfg(unix)]
    fn process_exists(process_id: libc::pid_t) -> bool {
        // SAFETY: signal zero does not deliver a signal and the fixture pid is positive.
        if process_id > 0 && unsafe { libc::kill(process_id, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(unix)]
    async fn wait_for_process_exit(process_id: libc::pid_t) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while process_exists(process_id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process {process_id} survived plugin host cleanup"));
    }

    #[cfg(unix)]
    fn spawn_shell_host(script: &str, pid_path: &std::path::Path) -> PluginHostStdioClient {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .arg("jellyrin-plugin-rpc-fixture")
            .arg(pid_path);
        PluginHostStdioClient::spawn(&mut command).unwrap()
    }

    #[cfg(unix)]
    const RESPONSIVE_GROUP_HOST: &str = r#"
trap 'kill "$helper" 2>/dev/null || true; wait "$helper" 2>/dev/null || true; exit 0' TERM
sleep 300 &
helper=$!
printf '%s' "$helper" > "$1"
while IFS= read -r line; do
  printf '%s\n' '{"ProtocolVersion":1,"CorrelationId":"jellyrin-host-shutdown","Ok":true,"Result":{}}'
done
"#;

    #[cfg(unix)]
    const UNRESPONSIVE_GROUP_HOST: &str = r#"
trap 'kill "$helper" 2>/dev/null || true; wait "$helper" 2>/dev/null || true; exit 0' TERM
sleep 300 &
helper=$!
printf '%s' "$helper" > "$1"
while IFS= read -r line; do :; done
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_host_leads_a_dedicated_process_group() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("helper.pid");
        let client = spawn_shell_host(UNRESPONSIVE_GROUP_HOST, &pid_path);
        let leader_id = libc::pid_t::try_from(
            client
                .child
                .as_ref()
                .and_then(Child::id)
                .expect("plugin host child id"),
        )
        .unwrap();
        let helper_id = wait_for_fixture_pid(&pid_path).await;

        // SAFETY: getpgid only inspects the positive fixture process ids.
        assert_eq!(unsafe { libc::getpgid(leader_id) }, leader_id);
        // SAFETY: getpgid only inspects the positive fixture process ids.
        assert_eq!(unsafe { libc::getpgid(helper_id) }, leader_id);

        drop(client);
        wait_for_process_exit(helper_id).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_terminates_and_reaps_the_host_process_group() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("helper.pid");
        let client = spawn_shell_host(RESPONSIVE_GROUP_HOST, &pid_path);
        let helper_id = wait_for_fixture_pid(&pid_path).await;
        assert!(process_exists(helper_id));

        client.shutdown().await.unwrap();

        wait_for_process_exit(helper_id).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drop_kills_the_complete_host_process_group() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("helper.pid");
        let client = spawn_shell_host(UNRESPONSIVE_GROUP_HOST, &pid_path);
        let helper_id = wait_for_fixture_pid(&pid_path).await;
        assert!(process_exists(helper_id));

        drop(client);

        wait_for_process_exit(helper_id).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rpc_timeout_terminates_the_complete_host_process_group() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("helper.pid");
        let mut client = spawn_shell_host(UNRESPONSIVE_GROUP_HOST, &pid_path);
        let helper_id = wait_for_fixture_pid(&pid_path).await;
        let request = PluginRpcEnvelope::new(
            "corr-timeout-process",
            PluginRpcMethod::Health,
            PluginIdentity {
                plugin_id: "plugin".to_string(),
                version: "1.0.0.0".to_string(),
            },
        );

        let error = client
            .call::<_, Value>(&request, Duration::from_millis(20))
            .await
            .unwrap_err();

        assert!(matches!(error, PluginRpcTransportError::Timeout { .. }));
        assert!(client.child.is_none());
        wait_for_process_exit(helper_id).await;
    }
}
