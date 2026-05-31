use axum::{
    body::Bytes as BodyBytes,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jellyrin_core::{MediaItem, VirtualFolder};
use jellyrin_db::Database;
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket},
    path::{Path as FsPath, PathBuf},
    sync::{
        LazyLock, Mutex as StdMutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::{net::UdpSocket, task::JoinHandle, time};
use uuid::Uuid;

use crate::{
    ApiError, AppState, COMPATIBLE_PRODUCT_NAME, COMPATIBLE_SERVER_VERSION,
    default_network_configuration, dlna_hls_master_playlist_response,
    dlna_hls_media_playlist_response, dlna_hls_segment_response, stream_media_item,
    subscribe_system_lifecycle_commands,
};

const SSDP_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const SSDP_CACHE_SECONDS: u32 = 1800;
const SSDP_NOTIFY_INTERVAL_SECONDS: u64 = 60;
const SSDP_CONFIG_CHECK_SECONDS: u64 = 5;
const UPNP_EVENT_DEFAULT_TIMEOUT_SECONDS: u64 = 1800;
const UPNP_EVENT_MAX_TIMEOUT_SECONDS: u64 = 7200;
const UPNP_DEVICE_NS: &str = "urn:schemas-upnp-org:device-1-0";
const DLNA_DEVICE_NS: &str = "urn:schemas-dlna-org:device-1-0";
const UPNP_SERVICE_NS: &str = "urn:schemas-upnp-org:service-1-0";
const SOAP_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const CONTENT_DIRECTORY_SERVICE: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CONNECTION_MANAGER_SERVICE: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
const MEDIA_RECEIVER_REGISTRAR_SERVICE: &str =
    "urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1";
const CONTENT_DIRECTORY_ID: &str = "urn:upnp-org:serviceId:ContentDirectory";
const CONNECTION_MANAGER_ID: &str = "urn:upnp-org:serviceId:ConnectionManager";
const MEDIA_RECEIVER_REGISTRAR_ID: &str = "urn:microsoft.com:serviceId:X_MS_MediaReceiverRegistrar";
const UPNP_ROOT_DEVICE: &str = "upnp:rootdevice";
const MEDIA_SERVER_DEVICE: &str = "urn:schemas-upnp-org:device:MediaServer:1";
static DLNA_EVENT_SUBSCRIPTIONS: LazyLock<StdMutex<HashMap<String, DlnaEventSubscription>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
static DLNA_SYSTEM_UPDATE_ID: AtomicU32 = AtomicU32::new(0);
static DLNA_SOURCE_PROTOCOL_INFO: LazyLock<String> = LazyLock::new(dlna_source_protocol_info);
const DLNA_PROTOCOL_FLAGS: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01500000000000000000000000000000";
const DLNA_PROTOCOL_INFO_ENTRIES: &[DlnaProtocolInfoEntry] = &[
    DlnaProtocolInfoEntry::new("video/mpeg", None),
    DlnaProtocolInfoEntry::new("video/mp4", None),
    DlnaProtocolInfoEntry::new("video/vnd.dlna.mpeg-tts", None),
    DlnaProtocolInfoEntry::new("video/x-msvideo", None),
    DlnaProtocolInfoEntry::new("video/x-ms-asf", None),
    DlnaProtocolInfoEntry::new("video/x-matroska", None),
    DlnaProtocolInfoEntry::new("video/webm", None),
    DlnaProtocolInfoEntry::new("video/quicktime", None),
    DlnaProtocolInfoEntry::new("video/x-ms-wmv", None),
    DlnaProtocolInfoEntry::new("video/wtv", None),
    DlnaProtocolInfoEntry::new("application/vnd.apple.mpegurl", None),
    DlnaProtocolInfoEntry::new("audio/mpeg", Some("MP3")),
    DlnaProtocolInfoEntry::new("audio/mp4", None),
    DlnaProtocolInfoEntry::new("audio/aac", None),
    DlnaProtocolInfoEntry::new("audio/flac", None),
    DlnaProtocolInfoEntry::new("audio/ogg", None),
    DlnaProtocolInfoEntry::new("audio/x-ms-wma", None),
    DlnaProtocolInfoEntry::new("audio/wav", None),
    DlnaProtocolInfoEntry::new("audio/L16", None),
    DlnaProtocolInfoEntry::new("image/jpeg", Some("JPEG_LRG")),
    DlnaProtocolInfoEntry::new("image/png", Some("PNG_LRG")),
    DlnaProtocolInfoEntry::new("image/gif", Some("GIF_LRG")),
    DlnaProtocolInfoEntry::new("image/tiff", None),
    DlnaProtocolInfoEntry::new("image/webp", None),
    DlnaProtocolInfoEntry::new("image/bmp", None),
];
const ONE_BY_ONE_PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
    0x42, 0x60, 0x82,
];

#[derive(Clone, Copy)]
struct DlnaProtocolInfoEntry {
    mime_type: &'static str,
    profile_name: Option<&'static str>,
}

impl DlnaProtocolInfoEntry {
    const fn new(mime_type: &'static str, profile_name: Option<&'static str>) -> Self {
        Self {
            mime_type,
            profile_name,
        }
    }

    fn protocol_info(self) -> String {
        dlna_protocol_info_value(self.mime_type, self.profile_name)
    }
}

fn dlna_source_protocol_info() -> String {
    DLNA_PROTOCOL_INFO_ENTRIES
        .iter()
        .map(|entry| entry.protocol_info())
        .collect::<Vec<_>>()
        .join(",")
}

fn dlna_protocol_info() -> &'static str {
    DLNA_SOURCE_PROTOCOL_INFO.as_str()
}

fn dlna_protocol_info_value(mime_type: &str, profile_name: Option<&str>) -> String {
    match profile_name {
        Some(profile_name) => {
            format!("http-get:*:{mime_type}:DLNA.ORG_PN={profile_name};{DLNA_PROTOCOL_FLAGS}")
        }
        None => format!("http-get:*:{mime_type}:{DLNA_PROTOCOL_FLAGS}"),
    }
}

pub fn spawn_dlna_ssdp_service(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match upnp_enabled(&state.db).await {
                Ok(true) => match bind_ssdp_socket().await {
                    Ok(socket) => {
                        if let Err(error) = run_ssdp_service(state.clone(), socket).await {
                            tracing::warn!(%error, "DLNA SSDP service stopped");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to bind DLNA SSDP socket");
                    }
                },
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(?error, "failed to read DLNA SSDP configuration");
                }
            }
            time::sleep(StdDuration::from_secs(SSDP_CONFIG_CHECK_SECONDS)).await;
        }
    })
}

pub(crate) async fn description(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = dlna_context(&state, &server_id).await?;
    let server_address = request_server_address(&headers, &state);
    Ok(xml_response(root_device_xml(&context, &server_address)))
}

pub(crate) async fn content_directory_scpd(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    Ok(xml_response(content_directory_service_xml()))
}

pub(crate) async fn connection_manager_scpd(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    Ok(xml_response(connection_manager_service_xml()))
}

pub(crate) async fn media_receiver_registrar_scpd(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    Ok(xml_response(media_receiver_registrar_service_xml()))
}

pub(crate) async fn icon(
    State(state): State<AppState>,
    Path((server_id, file_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    if !file_name.eq_ignore_ascii_case("logo.png") {
        return Err(ApiError::not_found("DLNA icon not found"));
    }
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png".to_string()),
            (header::CONTENT_LENGTH, ONE_BY_ONE_PNG.len().to_string()),
        ],
        ONE_BY_ONE_PNG,
    )
        .into_response())
}

pub(crate) async fn connection_manager_control(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    body: BodyBytes,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let request = String::from_utf8_lossy(&body);
    let Some(action) = soap_action(&request) else {
        return Ok(soap_fault_response(401, "Invalid Action"));
    };
    let body = match action.as_str() {
        "getprotocolinfo" => format!(
            "<Source>{}</Source><Sink></Sink>",
            escape_xml(dlna_protocol_info())
        ),
        "getcurrentconnectionids" => "<ConnectionIDs>0</ConnectionIDs>".to_string(),
        "getcurrentconnectioninfo" => concat!(
            "<RcsID>-1</RcsID>",
            "<AVTransportID>-1</AVTransportID>",
            "<ProtocolInfo></ProtocolInfo>",
            "<PeerConnectionManager></PeerConnectionManager>",
            "<PeerConnectionID>-1</PeerConnectionID>",
            "<Direction>Output</Direction>",
            "<Status>OK</Status>"
        )
        .to_string(),
        _ => {
            return Ok(soap_fault_response(401, "Invalid Action"));
        }
    };
    Ok(soap_response(
        CONNECTION_MANAGER_SERVICE,
        action_response_name(&action),
        body,
    ))
}

pub(crate) async fn media_receiver_registrar_control(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    body: BodyBytes,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let request = String::from_utf8_lossy(&body);
    let Some(action) = soap_action(&request) else {
        return Ok(soap_fault_response(401, "Invalid Action"));
    };
    let body = match action.as_str() {
        "isauthorized" | "isvalidated" => "<Result>1</Result>".to_string(),
        "registerdevice" => "<RegistrationRespMsg></RegistrationRespMsg>".to_string(),
        _ => {
            return Ok(soap_fault_response(401, "Invalid Action"));
        }
    };
    Ok(soap_response(
        MEDIA_RECEIVER_REGISTRAR_SERVICE,
        action_response_name(&action),
        body,
    ))
}

pub(crate) async fn content_directory_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    body: BodyBytes,
) -> Result<Response, ApiError> {
    let context = dlna_context(&state, &server_id).await?;
    let server_address = request_server_address(&headers, &state);
    let request = String::from_utf8_lossy(&body);
    let Some(action) = soap_action(&request) else {
        return Ok(soap_fault_response(401, "Invalid Action"));
    };
    let response_body = match action.as_str() {
        "getsearchcapabilities" => {
            "<SearchCaps>dc:title,upnp:class,@id,res@protocolInfo</SearchCaps>".to_string()
        }
        "getsortcapabilities" => "<SortCaps>dc:title</SortCaps>".to_string(),
        "getsystemupdateid" => format!("<Id>{}</Id>", dlna_system_update_id()),
        "x_getfeaturelist" => format!("<FeatureList>{}</FeatureList>", escape_xml(feature_list())),
        "browse" => {
            match browse_response(&state.db, &request, context.server_id, &server_address).await {
                Ok(response) => response,
                Err(error) if error.status == StatusCode::NOT_FOUND => {
                    return Ok(soap_fault_response(701, "No such object"));
                }
                Err(error) => return Err(error),
            }
        }
        "search" => {
            match search_response(&state.db, &request, context.server_id, &server_address).await {
                Ok(response) => response,
                Err(error) if error.status == StatusCode::NOT_FOUND => {
                    return Ok(soap_fault_response(701, "No such object"));
                }
                Err(error) => return Err(error),
            }
        }
        _ => {
            return Ok(soap_fault_response(401, "Invalid Action"));
        }
    };
    Ok(soap_response(
        CONTENT_DIRECTORY_SERVICE,
        action_response_name(&action),
        response_body,
    ))
}

pub(crate) async fn content_directory_events(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    event_subscription_response(DlnaEventService::ContentDirectory, &method, &headers)
}

pub(crate) async fn connection_manager_events(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    event_subscription_response(DlnaEventService::ConnectionManager, &method, &headers)
}

pub(crate) async fn media_receiver_registrar_events(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    event_subscription_response(DlnaEventService::MediaReceiverRegistrar, &method, &headers)
}

pub(crate) async fn media_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, _container)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    let item = state
        .db
        .media_item_by_id(item_id)
        .await
        .map_err(|_| ApiError::not_found("DLNA media item not found"))?;
    stream_media_item(item, &headers, true).await
}

pub(crate) async fn media_stream_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, _container)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    let item = state
        .db
        .media_item_by_id(item_id)
        .await
        .map_err(|_| ApiError::not_found("DLNA media item not found"))?;
    stream_media_item(item, &headers, false).await
}

pub(crate) async fn media_hls_master_playlist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let item = dlna_media_item(&state, &server_id, &item_id).await?;
    dlna_hls_master_playlist_response(&state, &headers, item, true).await
}

pub(crate) async fn media_hls_master_playlist_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let item = dlna_media_item(&state, &server_id, &item_id).await?;
    dlna_hls_master_playlist_response(&state, &headers, item, false).await
}

pub(crate) async fn media_hls_playlist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, play_session_id)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let context = dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    dlna_hls_media_playlist_response(
        &state,
        &headers,
        context.server_id,
        item_id,
        &play_session_id,
        true,
    )
    .await
}

pub(crate) async fn media_hls_playlist_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, play_session_id)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let context = dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    dlna_hls_media_playlist_response(
        &state,
        &headers,
        context.server_id,
        item_id,
        &play_session_id,
        false,
    )
    .await
}

pub(crate) async fn media_hls_segment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, play_session_id, segment_file)): Path<(
        String,
        String,
        String,
        String,
    )>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    dlna_hls_segment_response(
        &state,
        &headers,
        item_id,
        &play_session_id,
        &segment_file,
        true,
    )
    .await
}

pub(crate) async fn media_hls_segment_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, item_id, play_session_id, segment_file)): Path<(
        String,
        String,
        String,
        String,
    )>,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let item_id = parse_dlna_uuid(&item_id)?;
    dlna_hls_segment_response(
        &state,
        &headers,
        item_id,
        &play_session_id,
        &segment_file,
        false,
    )
    .await
}

pub(crate) async fn item_thumbnail(
    State(state): State<AppState>,
    Path((server_id, item_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let item = dlna_media_item(&state, &server_id, &item_id).await?;
    dlna_thumbnail_response(&state, &item, true).await
}

pub(crate) async fn item_thumbnail_head(
    State(state): State<AppState>,
    Path((server_id, item_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let item = dlna_media_item(&state, &server_id, &item_id).await?;
    dlna_thumbnail_response(&state, &item, false).await
}

async fn dlna_media_item(
    state: &AppState,
    server_id: &str,
    item_id: &str,
) -> Result<MediaItem, ApiError> {
    dlna_context(state, server_id).await?;
    let item_id = parse_dlna_uuid(item_id)?;
    state
        .db
        .media_item_by_id(item_id)
        .await
        .map_err(|_| ApiError::not_found("DLNA media item not found"))
}

async fn dlna_thumbnail_response(
    state: &AppState,
    item: &MediaItem,
    include_body: bool,
) -> Result<Response, ApiError> {
    let (content_type, bytes) = match find_dlna_item_thumbnail(state, item).await? {
        Some(path) => (dlna_image_content_type(&path), tokio::fs::read(path).await?),
        None => ("image/png", ONE_BY_ONE_PNG.to_vec()),
    };
    let content_length = bytes.len().to_string();
    let body = if include_body { bytes } else { Vec::new() };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, content_length),
            (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
        ],
        body,
    )
        .into_response())
}

async fn find_dlna_item_thumbnail(
    state: &AppState,
    item: &MediaItem,
) -> Result<Option<PathBuf>, ApiError> {
    if let Some(path) = find_dlna_stored_item_thumbnail(state, item).await? {
        return Ok(Some(path));
    }
    find_dlna_local_item_thumbnail(item).await
}

async fn find_dlna_stored_item_thumbnail(
    state: &AppState,
    item: &MediaItem,
) -> Result<Option<PathBuf>, ApiError> {
    let base = state
        .log_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(&state.log_dir)
        .join("metadata")
        .join("images")
        .join("items");
    for item_id in [item.id.to_string(), item.id.simple().to_string()] {
        let dir = base.join(sanitize_dlna_image_path_segment(&item_id));
        for extension in DLNA_IMAGE_EXTENSIONS {
            let path = dir.join(format!("primary_0.{extension}"));
            if tokio::fs::metadata(&path)
                .await
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

async fn find_dlna_local_item_thumbnail(item: &MediaItem) -> Result<Option<PathBuf>, ApiError> {
    let Some(item_dir) = FsPath::new(&item.path).parent().map(FsPath::to_path_buf) else {
        return Ok(None);
    };
    let canonical_dir = match tokio::fs::canonicalize(&item_dir).await {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    for stem in ["poster", "folder", "cover", "default", "movie", "thumb"] {
        for extension in DLNA_IMAGE_EXTENSIONS {
            let path = item_dir.join(format!("{stem}.{extension}"));
            let canonical_path = match tokio::fs::canonicalize(&path).await {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !canonical_path.starts_with(&canonical_dir) {
                continue;
            }
            if tokio::fs::metadata(&canonical_path)
                .await
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return Ok(Some(canonical_path));
            }
        }
    }
    Ok(None)
}

const DLNA_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

fn dlna_image_content_type(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    }
}

fn sanitize_dlna_image_path_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "image".to_string()
    } else {
        sanitized
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DlnaEventService {
    ContentDirectory,
    ConnectionManager,
    MediaReceiverRegistrar,
}

#[derive(Clone, Debug)]
struct DlnaEventSubscription {
    service: DlnaEventService,
    expires_at: Instant,
    callback_urls: Vec<String>,
    next_seq: u32,
}

fn event_subscription_response(
    service: DlnaEventService,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    cleanup_expired_event_subscriptions()?;
    match method.as_str() {
        "SUBSCRIBE" => subscribe_event_response(service, headers),
        "UNSUBSCRIBE" => unsubscribe_event_response(service, headers),
        _ => Ok((StatusCode::METHOD_NOT_ALLOWED, BodyBytes::new()).into_response()),
    }
}

fn subscribe_event_response(
    service: DlnaEventService,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let timeout = requested_event_timeout(headers);
    if let Some(sid) = event_header(headers, "sid") {
        if event_header(headers, "callback").is_some() || event_header(headers, "nt").is_some() {
            return Err(ApiError::bad_request(
                "DLNA event renewal cannot include CALLBACK or NT",
            ));
        }
        let sid = normalize_event_sid(&sid)?;
        let mut subscriptions = dlna_event_subscriptions()?;
        let Some(subscription) = subscriptions.get_mut(&sid) else {
            return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
        };
        if subscription.service != service {
            return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
        }
        subscription.expires_at = event_expires_at(timeout);
        return Ok(event_subscription_ok_response(&sid, timeout));
    }

    let callback = event_header(headers, "callback");
    let nt = event_header(headers, "nt");
    if callback
        .as_deref()
        .is_none_or(|callback| !valid_event_callback(callback))
        || nt
            .as_deref()
            .is_none_or(|nt| !nt.eq_ignore_ascii_case("upnp:event"))
    {
        return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
    }
    let callback_urls = event_callback_urls(callback.as_deref().unwrap_or_default());
    if callback_urls.is_empty() {
        return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
    }

    let sid = format!("uuid:{}", Uuid::new_v4());
    dlna_event_subscriptions()?.insert(
        sid.clone(),
        DlnaEventSubscription {
            service,
            expires_at: event_expires_at(timeout),
            callback_urls: callback_urls.clone(),
            next_seq: 1,
        },
    );
    spawn_initial_event_notify(service, sid.clone(), callback_urls);
    Ok(event_subscription_ok_response(&sid, timeout))
}

fn unsubscribe_event_response(
    service: DlnaEventService,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let Some(sid) = event_header(headers, "sid") else {
        return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
    };
    let sid = normalize_event_sid(&sid)?;
    let mut subscriptions = dlna_event_subscriptions()?;
    let Some(subscription) = subscriptions.get(&sid) else {
        return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
    };
    if subscription.service != service {
        return Ok((StatusCode::PRECONDITION_FAILED, BodyBytes::new()).into_response());
    }
    subscriptions.remove(&sid);
    Ok((StatusCode::OK, BodyBytes::new()).into_response())
}

fn event_subscription_ok_response(sid: &str, timeout: u64) -> Response {
    (
        StatusCode::OK,
        [
            (header::HeaderName::from_static("sid"), sid.to_string()),
            (
                header::HeaderName::from_static("timeout"),
                format!("Second-{timeout}"),
            ),
            (header::CONTENT_LENGTH, "0".to_string()),
        ],
        BodyBytes::new(),
    )
        .into_response()
}

fn requested_event_timeout(headers: &HeaderMap) -> u64 {
    event_header(headers, "timeout")
        .and_then(|value| {
            value
                .strip_prefix("Second-")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .map(|seconds| seconds.clamp(1, UPNP_EVENT_MAX_TIMEOUT_SECONDS))
        .unwrap_or(UPNP_EVENT_DEFAULT_TIMEOUT_SECONDS)
}

fn event_expires_at(timeout: u64) -> Instant {
    Instant::now() + StdDuration::from_secs(timeout)
}

fn cleanup_expired_event_subscriptions() -> Result<(), ApiError> {
    let now = Instant::now();
    dlna_event_subscriptions()?.retain(|_, subscription| subscription.expires_at > now);
    Ok(())
}

fn event_header(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn valid_event_callback(callback: &str) -> bool {
    !event_callback_urls(callback).is_empty()
}

fn event_callback_urls(callback: &str) -> Vec<String> {
    callback
        .split('>')
        .filter_map(|part| part.trim().strip_prefix('<'))
        .filter_map(|url| {
            let url = url.trim();
            url.starts_with("http://").then(|| url.to_string())
        })
        .collect()
}

fn normalize_event_sid(sid: &str) -> Result<String, ApiError> {
    let sid = sid.trim();
    let uuid_value = sid
        .strip_prefix("uuid:")
        .or_else(|| sid.strip_prefix("UUID:"))
        .ok_or_else(|| ApiError::bad_request("Invalid DLNA event SID"))?;
    let uuid =
        Uuid::parse_str(uuid_value).map_err(|_| ApiError::bad_request("Invalid DLNA event SID"))?;
    Ok(format!("uuid:{uuid}"))
}

fn dlna_event_subscriptions()
-> Result<std::sync::MutexGuard<'static, HashMap<String, DlnaEventSubscription>>, ApiError> {
    DLNA_EVENT_SUBSCRIPTIONS
        .lock()
        .map_err(|_| ApiError::internal("DLNA event subscription lock poisoned"))
}

pub(crate) fn notify_dlna_content_directory_changed() {
    let update_id = DLNA_SYSTEM_UPDATE_ID
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let body =
        event_property_set_xml_from_properties(vec![("SystemUpdateID", update_id.to_string())]);
    notify_event_subscribers(DlnaEventService::ContentDirectory, body);
}

pub(crate) fn dlna_system_update_id() -> u32 {
    DLNA_SYSTEM_UPDATE_ID.load(Ordering::Relaxed)
}

fn notify_event_subscribers(service: DlnaEventService, body: String) {
    let notifications = match event_notifications_for_service(service) {
        Ok(notifications) => notifications,
        Err(error) => {
            tracing::warn!(?error, "failed to collect DLNA event subscribers");
            return;
        }
    };
    if notifications.is_empty() {
        return;
    }

    tokio::spawn(async move {
        for notification in notifications {
            for callback_url in notification.callback_urls {
                if let Err(error) = send_event_notify(
                    &callback_url,
                    &notification.sid,
                    notification.seq,
                    body.clone(),
                )
                .await
                {
                    tracing::warn!(%error, %callback_url, "DLNA change event notification failed");
                }
            }
        }
    });
}

struct DlnaEventNotification {
    sid: String,
    callback_urls: Vec<String>,
    seq: u32,
}

fn event_notifications_for_service(
    service: DlnaEventService,
) -> Result<Vec<DlnaEventNotification>, ApiError> {
    let now = Instant::now();
    let mut subscriptions = dlna_event_subscriptions()?;
    subscriptions.retain(|_, subscription| subscription.expires_at > now);
    Ok(subscriptions
        .iter_mut()
        .filter(|(_, subscription)| subscription.service == service)
        .map(|(sid, subscription)| {
            let seq = subscription.next_seq;
            subscription.next_seq = subscription.next_seq.wrapping_add(1);
            DlnaEventNotification {
                sid: sid.clone(),
                callback_urls: subscription.callback_urls.clone(),
                seq,
            }
        })
        .collect())
}

fn spawn_initial_event_notify(service: DlnaEventService, sid: String, callback_urls: Vec<String>) {
    let body = event_property_set_xml(service);
    tokio::spawn(async move {
        for callback_url in callback_urls {
            if let Err(error) = send_event_notify(&callback_url, &sid, 0, body.clone()).await {
                tracing::warn!(%error, %callback_url, "DLNA initial event notification failed");
            }
        }
    });
}

async fn send_event_notify(
    callback_url: &str,
    sid: &str,
    seq: u32,
    body: String,
) -> anyhow::Result<()> {
    let method = reqwest::Method::from_bytes(b"NOTIFY")?;
    let response = reqwest::Client::new()
        .request(method, callback_url)
        .header("NT", "upnp:event")
        .header("NTS", "upnp:propchange")
        .header("SID", sid)
        .header("SEQ", seq.to_string())
        .header(header::CONTENT_TYPE.as_str(), "text/xml; charset=\"utf-8\"")
        .header("USER-AGENT", ssdp_server_header())
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("callback returned HTTP {}", response.status());
    }
    Ok(())
}

fn event_property_set_xml(service: DlnaEventService) -> String {
    event_property_set_xml_from_properties(event_initial_properties(service))
}

fn event_property_set_xml_from_properties(properties: Vec<(&'static str, String)>) -> String {
    let mut xml = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
        "<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">"
    )
    .to_string();
    for (name, value) in properties {
        xml.push_str("<e:property><");
        xml.push_str(name);
        xml.push('>');
        xml.push_str(&escape_xml(&value));
        xml.push_str("</");
        xml.push_str(name);
        xml.push_str("></e:property>");
    }
    xml.push_str("</e:propertyset>");
    xml
}

fn event_initial_properties(service: DlnaEventService) -> Vec<(&'static str, String)> {
    match service {
        DlnaEventService::ContentDirectory => vec![(
            "SystemUpdateID",
            DLNA_SYSTEM_UPDATE_ID.load(Ordering::Relaxed).to_string(),
        )],
        DlnaEventService::ConnectionManager => vec![
            ("SourceProtocolInfo", dlna_protocol_info().to_string()),
            ("SinkProtocolInfo", String::new()),
            ("CurrentConnectionIDs", "0".to_string()),
        ],
        DlnaEventService::MediaReceiverRegistrar => vec![
            ("AuthorizationDeniedUpdateID", "0".to_string()),
            ("ValidationRevokedUpdateID", "0".to_string()),
        ],
    }
}

pub(crate) async fn upnp_enabled(db: &Database) -> Result<bool, ApiError> {
    let network = db
        .named_configuration("network")
        .await?
        .unwrap_or_else(default_network_configuration);
    Ok(network
        .get("EnableUPnP")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

async fn bind_ssdp_socket() -> anyhow::Result<UdpSocket> {
    let socket = StdUdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SSDP_PORT))?;
    socket.set_nonblocking(true)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(4)?;
    socket.join_multicast_v4(&SSDP_MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    Ok(UdpSocket::from_std(socket)?)
}

async fn run_ssdp_service(state: AppState, socket: UdpSocket) -> anyhow::Result<()> {
    let multicast = SocketAddrV4::new(SSDP_MULTICAST_ADDR, SSDP_PORT);
    let mut notify_interval = time::interval(StdDuration::from_secs(SSDP_NOTIFY_INTERVAL_SECONDS));
    let mut config_interval = time::interval(StdDuration::from_secs(SSDP_CONFIG_CHECK_SECONDS));
    let mut lifecycle = subscribe_system_lifecycle_commands();
    let mut buffer = vec![0_u8; 2048];

    send_ssdp_notify(&socket, multicast, &state, SsdpNotificationKind::Alive).await?;

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buffer) => {
                match result {
                    Ok((len, peer)) => {
                        let server = state.db.server_state().await?;
                        let base_url = ssdp_base_url_for_peer(&state.local_address, peer);
                        respond_to_ssdp_search(
                            &socket,
                            &buffer[..len],
                            peer,
                            server.server_id,
                            &base_url,
                        ).await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            _ = notify_interval.tick() => {
                send_ssdp_notify(&socket, multicast, &state, SsdpNotificationKind::Alive).await?;
            }
            _ = config_interval.tick() => {
                match upnp_enabled(&state.db).await {
                    Ok(true) => {}
                    Ok(false) => {
                        send_ssdp_notify(&socket, multicast, &state, SsdpNotificationKind::Byebye).await?;
                        return Ok(());
                    }
                    Err(error) => {
                        tracing::warn!(?error, "failed to refresh DLNA SSDP configuration");
                        send_ssdp_notify(&socket, multicast, &state, SsdpNotificationKind::Byebye).await?;
                        return Ok(());
                    }
                }
            }
            command = lifecycle.recv() => {
                if command.is_ok() {
                    send_ssdp_notify(&socket, multicast, &state, SsdpNotificationKind::Byebye).await?;
                    return Ok(());
                }
            }
        }
    }
}

async fn send_ssdp_notify(
    socket: &UdpSocket,
    multicast: SocketAddrV4,
    state: &AppState,
    kind: SsdpNotificationKind,
) -> anyhow::Result<()> {
    let server = state.db.server_state().await?;
    let base_url = ssdp_base_url_for_peer(&state.local_address, SocketAddr::V4(multicast));
    let location = ssdp_description_location(&base_url, server.server_id);
    for message in ssdp_notify_messages(server.server_id, &location, kind) {
        socket.send_to(message.as_bytes(), multicast).await?;
    }
    Ok(())
}

async fn respond_to_ssdp_search(
    socket: &UdpSocket,
    data: &[u8],
    peer: SocketAddr,
    server_id: Uuid,
    base_url: &str,
) -> anyhow::Result<usize> {
    let Some(request) = parse_ssdp_search(data) else {
        return Ok(0);
    };
    let location = ssdp_description_location(base_url, server_id);
    let mut sent = 0;
    for target in matching_ssdp_targets(server_id, &request.search_target) {
        let response = ssdp_search_response(server_id, &location, target);
        socket.send_to(response.as_bytes(), peer).await?;
        sent += 1;
    }
    Ok(sent)
}

#[derive(Clone, Copy)]
enum SsdpNotificationKind {
    Alive,
    Byebye,
}

struct SsdpSearchRequest {
    search_target: String,
}

fn parse_ssdp_search(data: &[u8]) -> Option<SsdpSearchRequest> {
    let packet = std::str::from_utf8(data).ok()?;
    let mut lines = packet.lines();
    let request_line = lines.next()?.trim();
    if !request_line.eq_ignore_ascii_case("M-SEARCH * HTTP/1.1") {
        return None;
    }

    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let man = headers.get("man")?;
    if !man.trim_matches('"').eq_ignore_ascii_case("ssdp:discover") {
        return None;
    }
    let search_target = headers.get("st")?.trim().to_string();
    if search_target.is_empty() {
        return None;
    }
    Some(SsdpSearchRequest { search_target })
}

fn matching_ssdp_targets(server_id: Uuid, search_target: &str) -> Vec<String> {
    let requested = search_target.trim();
    let targets = ssdp_advertised_targets(server_id);
    if requested.eq_ignore_ascii_case("ssdp:all") {
        return targets;
    }
    targets
        .into_iter()
        .filter(|target| target.eq_ignore_ascii_case(requested))
        .collect()
}

fn ssdp_advertised_targets(server_id: Uuid) -> Vec<String> {
    vec![
        UPNP_ROOT_DEVICE.to_string(),
        format!("uuid:{server_id}"),
        MEDIA_SERVER_DEVICE.to_string(),
        CONTENT_DIRECTORY_SERVICE.to_string(),
        CONNECTION_MANAGER_SERVICE.to_string(),
        MEDIA_RECEIVER_REGISTRAR_SERVICE.to_string(),
    ]
}

fn ssdp_search_response(server_id: Uuid, location: &str, search_target: String) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age={SSDP_CACHE_SECONDS}\r\n\
         EXT:\r\n\
         LOCATION: {location}\r\n\
         SERVER: {server_header}\r\n\
         ST: {search_target}\r\n\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: 1\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n",
        server_header = ssdp_server_header(),
        usn = ssdp_usn(server_id, &search_target),
    )
}

fn ssdp_notify_messages(
    server_id: Uuid,
    location: &str,
    kind: SsdpNotificationKind,
) -> Vec<String> {
    ssdp_advertised_targets(server_id)
        .into_iter()
        .map(|target| ssdp_notify_message(server_id, location, &target, kind))
        .collect()
}

fn ssdp_notify_message(
    server_id: Uuid,
    location: &str,
    notification_type: &str,
    kind: SsdpNotificationKind,
) -> String {
    let notification_sub_type = match kind {
        SsdpNotificationKind::Alive => "ssdp:alive",
        SsdpNotificationKind::Byebye => "ssdp:byebye",
    };
    let location = match kind {
        SsdpNotificationKind::Alive => format!("LOCATION: {location}\r\n"),
        SsdpNotificationKind::Byebye => String::new(),
    };
    let server = match kind {
        SsdpNotificationKind::Alive => format!("SERVER: {}\r\n", ssdp_server_header()),
        SsdpNotificationKind::Byebye => String::new(),
    };
    let cache_control = match kind {
        SsdpNotificationKind::Alive => {
            format!("CACHE-CONTROL: max-age={SSDP_CACHE_SECONDS}\r\n")
        }
        SsdpNotificationKind::Byebye => String::new(),
    };
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: {SSDP_MULTICAST_ADDR}:{SSDP_PORT}\r\n\
         {cache_control}\
         {location}\
         NT: {notification_type}\r\n\
         NTS: {notification_sub_type}\r\n\
         {server}\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: 1\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n",
        usn = ssdp_usn(server_id, notification_type),
    )
}

fn ssdp_usn(server_id: Uuid, target: &str) -> String {
    let uuid = format!("uuid:{server_id}");
    if target.eq_ignore_ascii_case(&uuid) {
        uuid
    } else {
        format!("{uuid}::{target}")
    }
}

fn ssdp_server_header() -> String {
    format!(
        "Jellyrin/{COMPATIBLE_SERVER_VERSION} UPnP/1.0 {COMPATIBLE_PRODUCT_NAME}/{COMPATIBLE_SERVER_VERSION}"
    )
}

fn ssdp_description_location(base_url: &str, server_id: Uuid) -> String {
    format!(
        "{}/dlna/{server_id}/description.xml",
        base_url.trim_end_matches('/')
    )
}

fn ssdp_base_url_for_peer(configured_base_url: &str, peer: SocketAddr) -> String {
    let configured_base_url = configured_base_url.trim_end_matches('/');
    if !configured_base_url.contains("://0.0.0.0:")
        && !configured_base_url.contains("://[::]:")
        && !configured_base_url.contains("://:::")
    {
        return configured_base_url.to_string();
    }

    let scheme = configured_base_url
        .split_once("://")
        .map_or("http", |(scheme, _)| scheme);
    let port = configured_base_url
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(8096);
    let ip = local_ip_for_peer(peer).unwrap_or_else(|| peer.ip());
    match ip {
        IpAddr::V4(ip) => format!("{scheme}://{ip}:{port}"),
        IpAddr::V6(ip) => format!("{scheme}://[{ip}]:{port}"),
    }
}

fn local_ip_for_peer(peer: SocketAddr) -> Option<IpAddr> {
    let bind_addr = if peer.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], 0))
    };
    let socket = StdUdpSocket::bind(bind_addr).ok()?;
    socket.connect(peer).ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

struct DlnaContext {
    server_id: Uuid,
    server_name: String,
}

async fn dlna_context(
    state: &AppState,
    requested_server_id: &str,
) -> Result<DlnaContext, ApiError> {
    if !upnp_enabled(&state.db).await? {
        return Err(ApiError::service_unavailable("DLNA/UPnP is disabled"));
    }
    let server = state.db.server_state().await?;
    let requested_server_id = requested_server_id.trim();
    let normalized = requested_server_id
        .strip_prefix("uuid:")
        .or_else(|| requested_server_id.strip_prefix("UUID:"))
        .unwrap_or(requested_server_id);
    if !normalized.eq_ignore_ascii_case(&server.server_id.to_string())
        && !normalized.eq_ignore_ascii_case(&server.server_id.simple().to_string())
    {
        return Err(ApiError::not_found("DLNA server not found"));
    }
    Ok(DlnaContext {
        server_id: server.server_id,
        server_name: server.server_name,
    })
}

fn request_server_address(headers: &HeaderMap, state: &AppState) -> String {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return state.local_address.trim_end_matches('/').to_string();
    };
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

fn root_device_xml(context: &DlnaContext, server_address: &str) -> String {
    let base = format!(
        "{}/dlna/{}",
        server_address.trim_end_matches('/'),
        context.server_id
    );
    format!(
        "<?xml version=\"1.0\"?>\
         <root xmlns=\"{UPNP_DEVICE_NS}\" xmlns:dlna=\"{DLNA_DEVICE_NS}\">\
         <specVersion><major>1</major><minor>0</minor></specVersion>\
         <device>\
         <dlna:X_DLNACAP/>\
         <dlna:X_DLNADOC>DMS-1.50</dlna:X_DLNADOC>\
         <dlna:X_DLNADOC>M-DMS-1.50</dlna:X_DLNADOC>\
         <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>\
         <friendlyName>Jellyfin - {}</friendlyName>\
         <manufacturer>Jellyfin</manufacturer>\
         <manufacturerURL>https://jellyfin.org/</manufacturerURL>\
         <modelDescription>UPnP/AV 1.0 Compliant Media Server</modelDescription>\
         <modelName>{}</modelName>\
         <modelNumber>12.0.0</modelNumber>\
         <modelURL>https://jellyfin.org/</modelURL>\
         <serialNumber>{}</serialNumber>\
         <UPC/>\
         <UDN>uuid:{}</UDN>\
         <iconList><icon><mimetype>image/png</mimetype><width>1</width><height>1</height><depth>24</depth><url>{}/icons/logo.png</url></icon></iconList>\
         <presentationURL>{}/web/index.html</presentationURL>\
         <serviceList>{}{}{}</serviceList>\
         </device>\
        </root>",
        escape_xml(&context.server_name),
        escape_xml(COMPATIBLE_PRODUCT_NAME),
        context.server_id,
        context.server_id,
        escape_xml(&base),
        escape_xml(server_address.trim_end_matches('/')),
        service_description(
            CONTENT_DIRECTORY_SERVICE,
            CONTENT_DIRECTORY_ID,
            &format!("{base}/contentdirectory/contentdirectory.xml"),
            &format!("{base}/contentdirectory/control"),
            &format!("{base}/contentdirectory/events"),
        ),
        service_description(
            CONNECTION_MANAGER_SERVICE,
            CONNECTION_MANAGER_ID,
            &format!("{base}/connectionmanager/connectionmanager.xml"),
            &format!("{base}/connectionmanager/control"),
            &format!("{base}/connectionmanager/events"),
        ),
        service_description(
            MEDIA_RECEIVER_REGISTRAR_SERVICE,
            MEDIA_RECEIVER_REGISTRAR_ID,
            &format!("{base}/mediareceiverregistrar/mediareceiverregistrar.xml"),
            &format!("{base}/mediareceiverregistrar/control"),
            &format!("{base}/mediareceiverregistrar/events"),
        )
    )
}

fn service_description(
    service_type: &str,
    service_id: &str,
    scpd_url: &str,
    control_url: &str,
    event_sub_url: &str,
) -> String {
    format!(
        "<service>\
         <serviceType>{}</serviceType>\
         <serviceId>{}</serviceId>\
         <SCPDURL>{}</SCPDURL>\
         <controlURL>{}</controlURL>\
         <eventSubURL>{}</eventSubURL>\
         </service>",
        escape_xml(service_type),
        escape_xml(service_id),
        escape_xml(scpd_url),
        escape_xml(control_url),
        escape_xml(event_sub_url)
    )
}

struct DlnaArgument {
    name: &'static str,
    direction: &'static str,
    related_state_variable: &'static str,
}

struct DlnaAction {
    name: &'static str,
    arguments: &'static [DlnaArgument],
}

struct DlnaStateVariable {
    name: &'static str,
    data_type: &'static str,
    sends_events: bool,
    allowed_values: &'static [&'static str],
}

fn content_directory_service_xml() -> String {
    dlna_service_xml(CONTENT_DIRECTORY_ACTIONS, CONTENT_DIRECTORY_STATE_VARIABLES)
}

fn connection_manager_service_xml() -> String {
    dlna_service_xml(
        CONNECTION_MANAGER_ACTIONS,
        CONNECTION_MANAGER_STATE_VARIABLES,
    )
}

fn media_receiver_registrar_service_xml() -> String {
    dlna_service_xml(
        MEDIA_RECEIVER_REGISTRAR_ACTIONS,
        MEDIA_RECEIVER_REGISTRAR_STATE_VARIABLES,
    )
}

fn dlna_service_xml(actions: &[DlnaAction], state_variables: &[DlnaStateVariable]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\"?><scpd xmlns=\"");
    xml.push_str(UPNP_SERVICE_NS);
    xml.push_str("\"><specVersion><major>1</major><minor>0</minor></specVersion><actionList>");
    for action in actions {
        xml.push_str("<action><name>");
        xml.push_str(action.name);
        xml.push_str("</name><argumentList>");
        for argument in action.arguments {
            xml.push_str("<argument><name>");
            xml.push_str(argument.name);
            xml.push_str("</name><direction>");
            xml.push_str(argument.direction);
            xml.push_str("</direction><relatedStateVariable>");
            xml.push_str(argument.related_state_variable);
            xml.push_str("</relatedStateVariable></argument>");
        }
        xml.push_str("</argumentList></action>");
    }
    xml.push_str("</actionList><serviceStateTable>");
    for variable in state_variables {
        xml.push_str(if variable.sends_events {
            "<stateVariable sendEvents=\"yes\">"
        } else {
            "<stateVariable sendEvents=\"no\">"
        });
        xml.push_str("<name>");
        xml.push_str(variable.name);
        xml.push_str("</name><dataType>");
        xml.push_str(variable.data_type);
        xml.push_str("</dataType>");
        if !variable.allowed_values.is_empty() {
            xml.push_str("<allowedValueList>");
            for allowed in variable.allowed_values {
                xml.push_str("<allowedValue>");
                xml.push_str(allowed);
                xml.push_str("</allowedValue>");
            }
            xml.push_str("</allowedValueList>");
        }
        xml.push_str("</stateVariable>");
    }
    xml.push_str("</serviceStateTable></scpd>");
    xml
}

async fn browse_response(
    db: &Database,
    request: &str,
    server_id: Uuid,
    server_address: &str,
) -> Result<String, ApiError> {
    let object_id = soap_param(request, "ObjectID").unwrap_or_else(|| "0".to_string());
    let browse_flag =
        soap_param(request, "BrowseFlag").unwrap_or_else(|| "BrowseDirectChildren".to_string());
    let sort_criteria = soap_param(request, "SortCriteria").unwrap_or_default();
    let browse_metadata = browse_flag.eq_ignore_ascii_case("BrowseMetadata");
    let (starting_index, requested_count) = browse_window(request);
    let payload = if is_root_object_id(&object_id) && browse_metadata {
        let child_count = db.virtual_folders().await?.len();
        BrowsePayload::metadata(didl_root_metadata(child_count))
    } else if is_root_object_id(&object_id) {
        let mut folders = db.virtual_folders().await?;
        sort_virtual_folders(&mut folders, &sort_criteria);
        let total_matches = folders.len();
        let paged_folders = paged_slice(&folders, starting_index, requested_count);
        BrowsePayload::children(
            didl_root_children(db, paged_folders).await?,
            paged_folders.len(),
            total_matches,
        )
    } else if let Some(folder_id) = parse_folder_object_id(&object_id) {
        let folder = db
            .virtual_folders()
            .await?
            .into_iter()
            .find(|folder| folder.id == folder_id)
            .ok_or_else(|| ApiError::not_found("DLNA folder not found"))?;
        let items = media_items_for_folder(db, folder.id).await?;
        if browse_metadata {
            let child_count = directory_child_count(&folder, &items, "");
            BrowsePayload::metadata(didl_folder_metadata(&folder, child_count))
        } else {
            directory_browse_payload(
                &folder,
                &items,
                "",
                (starting_index, requested_count),
                &sort_criteria,
                server_id,
                server_address,
            )?
        }
    } else if let Some((folder_id, relative_path)) = parse_directory_object_id(&object_id) {
        let folder = db
            .virtual_folders()
            .await?
            .into_iter()
            .find(|folder| folder.id == folder_id)
            .ok_or_else(|| ApiError::not_found("DLNA folder not found"))?;
        let items = media_items_for_folder(db, folder.id).await?;
        if !directory_exists(&folder, &items, &relative_path) {
            return Err(ApiError::not_found("DLNA directory not found"));
        }
        if browse_metadata {
            BrowsePayload::metadata(didl_directory_metadata(
                &folder,
                &relative_path,
                directory_child_count(&folder, &items, &relative_path),
            ))
        } else {
            directory_browse_payload(
                &folder,
                &items,
                &relative_path,
                (starting_index, requested_count),
                &sort_criteria,
                server_id,
                server_address,
            )?
        }
    } else if let Some(item_id) = parse_item_object_id(&object_id) {
        let item = db
            .media_item_by_id(item_id)
            .await
            .map_err(|_| ApiError::not_found("DLNA item not found"))?;
        if browse_metadata {
            BrowsePayload::metadata(didl_media_items(&[item], server_id, server_address))
        } else {
            BrowsePayload::empty()
        }
    } else {
        BrowsePayload::empty()
    };
    Ok(format!(
        "<Result>{}</Result><NumberReturned>{}</NumberReturned><TotalMatches>{}</TotalMatches><UpdateID>{}</UpdateID>",
        escape_xml(&payload.didl),
        payload.number_returned,
        payload.total_matches,
        dlna_system_update_id()
    ))
}

async fn media_items_for_folder(
    db: &Database,
    folder_id: Uuid,
) -> Result<Vec<MediaItem>, ApiError> {
    Ok(db
        .media_items()
        .await?
        .into_iter()
        .filter(|item| item.virtual_folder_id == folder_id)
        .collect())
}

async fn search_response(
    db: &Database,
    request: &str,
    server_id: Uuid,
    server_address: &str,
) -> Result<String, ApiError> {
    let container_id = soap_param(request, "ContainerID").unwrap_or_else(|| "0".to_string());
    let criteria = soap_param(request, "SearchCriteria").unwrap_or_else(|| "*".to_string());
    let sort_criteria = soap_param(request, "SortCriteria").unwrap_or_default();
    let (starting_index, requested_count) = browse_window(request);
    let folders = db.virtual_folders().await?;
    let mut items = search_container_items(db, &folders, &container_id).await?;
    items.retain(|item| dlna_search_criteria_matches(item, &criteria));
    sort_media_items(&mut items, &sort_criteria);
    let total_matches = items.len();
    let paged_items = paged_slice(&items, starting_index, requested_count);
    let didl = didl_search_media_items(paged_items, &folders, server_id, server_address);
    Ok(format!(
        "<Result>{}</Result><NumberReturned>{}</NumberReturned><TotalMatches>{}</TotalMatches><UpdateID>{}</UpdateID>",
        escape_xml(&didl),
        paged_items.len(),
        total_matches,
        dlna_system_update_id()
    ))
}

async fn search_container_items(
    db: &Database,
    folders: &[VirtualFolder],
    container_id: &str,
) -> Result<Vec<MediaItem>, ApiError> {
    if is_root_object_id(container_id) {
        return Ok(db.media_items().await?);
    }

    if let Some(folder_id) = parse_folder_object_id(container_id) {
        if !folders.iter().any(|folder| folder.id == folder_id) {
            return Err(ApiError::not_found("DLNA folder not found"));
        }
        return media_items_for_folder(db, folder_id).await;
    }

    if let Some((folder_id, relative_path)) = parse_directory_object_id(container_id) {
        let folder = folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .ok_or_else(|| ApiError::not_found("DLNA folder not found"))?;
        let items = media_items_for_folder(db, folder_id).await?;
        if !directory_exists(folder, &items, &relative_path) {
            return Err(ApiError::not_found("DLNA directory not found"));
        }
        return Ok(items
            .into_iter()
            .filter(|item| item_is_in_directory_subtree(folder, item, &relative_path))
            .collect());
    }

    Err(ApiError::not_found("DLNA container not found"))
}

struct BrowsePayload {
    didl: String,
    number_returned: usize,
    total_matches: usize,
}

impl BrowsePayload {
    fn metadata(didl: String) -> Self {
        Self {
            didl,
            number_returned: 1,
            total_matches: 1,
        }
    }

    fn children(didl: String, number_returned: usize, total_matches: usize) -> Self {
        Self {
            didl,
            number_returned,
            total_matches,
        }
    }

    fn empty() -> Self {
        Self {
            didl: empty_didl(),
            number_returned: 0,
            total_matches: 0,
        }
    }
}

fn browse_window(request: &str) -> (usize, usize) {
    let starting_index = soap_param(request, "StartingIndex")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let requested_count = soap_param(request, "RequestedCount")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    (starting_index, requested_count)
}

fn paged_slice<T>(items: &[T], starting_index: usize, requested_count: usize) -> &[T] {
    if starting_index >= items.len() {
        return &items[items.len()..];
    }
    let end = if requested_count == 0 {
        items.len()
    } else {
        starting_index
            .saturating_add(requested_count)
            .min(items.len())
    };
    &items[starting_index..end]
}

fn didl_root_metadata(child_count: usize) -> String {
    format!(
        "{}<container id=\"0\" parentID=\"-1\" restricted=\"1\" childCount=\"{}\">\
         <dc:title>Jellyrin</dc:title>\
         <upnp:class>object.container</upnp:class>\
         </container></DIDL-Lite>",
        didl_prefix(),
        child_count
    )
}

async fn didl_root_children(db: &Database, folders: &[VirtualFolder]) -> Result<String, ApiError> {
    let mut didl = didl_prefix();
    for folder in folders {
        let items = media_items_for_folder(db, folder.id).await?;
        let child_count = directory_child_count(folder, &items, "");
        didl.push_str("<container id=\"");
        didl.push_str(&escape_xml(&format!("folder:{}", folder.id)));
        didl.push_str("\" parentID=\"0\" restricted=\"1\" searchable=\"1\" childCount=\"");
        didl.push_str(&child_count.to_string());
        didl.push_str("\"><dc:title>");
        didl.push_str(&escape_xml(&folder.name));
        didl.push_str("</dc:title><upnp:class>");
        didl.push_str(didl_container_class(folder.collection_type.as_deref()));
        didl.push_str("</upnp:class></container>");
    }
    didl.push_str("</DIDL-Lite>");
    Ok(didl)
}

fn directory_browse_payload(
    folder: &VirtualFolder,
    items: &[MediaItem],
    relative_path: &str,
    browse_window: (usize, usize),
    sort_criteria: &str,
    server_id: Uuid,
    server_address: &str,
) -> Result<BrowsePayload, ApiError> {
    let (starting_index, requested_count) = browse_window;
    let mut entries = directory_browse_entries(folder, items, relative_path);
    sort_browse_entries(&mut entries, sort_criteria);
    let total_matches = entries.len();
    let paged_entries = paged_slice(&entries, starting_index, requested_count);
    Ok(BrowsePayload::children(
        didl_browse_entries(paged_entries, server_id, server_address),
        paged_entries.len(),
        total_matches,
    ))
}

fn directory_browse_entries<'a>(
    folder: &VirtualFolder,
    items: &'a [MediaItem],
    relative_path: &str,
) -> Vec<DlnaBrowseEntry<'a>> {
    let parent_id = if relative_path.is_empty() {
        format!("folder:{}", folder.id)
    } else {
        directory_object_id(folder.id, relative_path)
    };
    let mut child_dirs = BTreeSet::new();
    let mut direct_items = Vec::new();

    for item in items {
        let item_parent = item_parent_relative_path(folder, item).unwrap_or_default();
        if item_parent == relative_path {
            direct_items.push(item);
        } else if let Some(child_name) = next_child_directory(&item_parent, relative_path) {
            child_dirs.insert(child_name);
        }
    }

    let mut entries = Vec::with_capacity(child_dirs.len() + direct_items.len());
    for child_name in child_dirs {
        let child_path = join_relative_path(relative_path, &child_name);
        entries.push(DlnaBrowseEntry::Container {
            id: directory_object_id(folder.id, &child_path),
            parent_id: parent_id.clone(),
            title: child_name,
            child_count: directory_child_count(folder, items, &child_path),
            class: directory_container_class(folder.collection_type.as_deref()),
        });
    }
    for item in direct_items {
        entries.push(DlnaBrowseEntry::Item {
            item,
            parent_id: parent_id.clone(),
        });
    }

    entries.sort_by(|left, right| {
        left.title()
            .to_ascii_lowercase()
            .cmp(&right.title().to_ascii_lowercase())
            .then_with(|| left.id().cmp(&right.id()))
    });
    entries
}

fn directory_child_count(
    folder: &VirtualFolder,
    items: &[MediaItem],
    relative_path: &str,
) -> usize {
    directory_browse_entries(folder, items, relative_path).len()
}

fn directory_exists(folder: &VirtualFolder, items: &[MediaItem], relative_path: &str) -> bool {
    !relative_path.is_empty()
        && items.iter().any(|item| {
            let item_parent = item_parent_relative_path(folder, item).unwrap_or_default();
            item_parent == relative_path
                || next_child_directory(&item_parent, relative_path).is_some()
        })
}

fn item_is_in_directory_subtree(
    folder: &VirtualFolder,
    item: &MediaItem,
    relative_path: &str,
) -> bool {
    let item_parent = item_parent_relative_path(folder, item).unwrap_or_default();
    item_parent == relative_path || next_child_directory(&item_parent, relative_path).is_some()
}

enum DlnaBrowseEntry<'a> {
    Container {
        id: String,
        parent_id: String,
        title: String,
        child_count: usize,
        class: &'static str,
    },
    Item {
        item: &'a MediaItem,
        parent_id: String,
    },
}

impl DlnaBrowseEntry<'_> {
    fn title(&self) -> &str {
        match self {
            Self::Container { title, .. } => title,
            Self::Item { item, .. } => &item.name,
        }
    }

    fn id(&self) -> String {
        match self {
            Self::Container { id, .. } => id.clone(),
            Self::Item { item, .. } => format!("item:{}", item.id),
        }
    }
}

fn didl_folder_metadata(folder: &VirtualFolder, child_count: usize) -> String {
    let mut didl = didl_prefix();
    didl.push_str("<container id=\"");
    didl.push_str(&escape_xml(&format!("folder:{}", folder.id)));
    didl.push_str("\" parentID=\"0\" restricted=\"1\" searchable=\"1\" childCount=\"");
    didl.push_str(&child_count.to_string());
    didl.push_str("\"><dc:title>");
    didl.push_str(&escape_xml(&folder.name));
    didl.push_str("</dc:title><upnp:class>");
    didl.push_str(didl_container_class(folder.collection_type.as_deref()));
    didl.push_str("</upnp:class></container></DIDL-Lite>");
    didl
}

fn didl_directory_metadata(
    folder: &VirtualFolder,
    relative_path: &str,
    child_count: usize,
) -> String {
    let mut didl = didl_prefix();
    didl.push_str("<container id=\"");
    didl.push_str(&escape_xml(&directory_object_id(folder.id, relative_path)));
    didl.push_str("\" parentID=\"");
    didl.push_str(&escape_xml(&directory_parent_object_id(
        folder.id,
        relative_path,
    )));
    didl.push_str("\" restricted=\"1\" searchable=\"1\" childCount=\"");
    didl.push_str(&child_count.to_string());
    didl.push_str("\"><dc:title>");
    didl.push_str(&escape_xml(&directory_title(relative_path)));
    didl.push_str("</dc:title><upnp:class>");
    didl.push_str(directory_container_class(folder.collection_type.as_deref()));
    didl.push_str("</upnp:class></container></DIDL-Lite>");
    didl
}

fn didl_browse_entries(
    entries: &[DlnaBrowseEntry<'_>],
    server_id: Uuid,
    server_address: &str,
) -> String {
    let mut didl = didl_prefix();
    for entry in entries {
        match entry {
            DlnaBrowseEntry::Container {
                id,
                parent_id,
                title,
                child_count,
                class,
            } => {
                append_didl_container(&mut didl, id, parent_id, title, *child_count, class);
            }
            DlnaBrowseEntry::Item { item, parent_id } => {
                append_didl_media_item(&mut didl, item, parent_id, server_id, server_address);
            }
        }
    }
    didl.push_str("</DIDL-Lite>");
    didl
}

fn didl_media_items(items: &[MediaItem], server_id: Uuid, server_address: &str) -> String {
    let mut didl = didl_prefix();
    for item in items {
        append_didl_media_item(
            &mut didl,
            item,
            &format!("folder:{}", item.virtual_folder_id),
            server_id,
            server_address,
        );
    }
    didl.push_str("</DIDL-Lite>");
    didl
}

fn didl_search_media_items(
    items: &[MediaItem],
    folders: &[VirtualFolder],
    server_id: Uuid,
    server_address: &str,
) -> String {
    let mut didl = didl_prefix();
    for item in items {
        let parent_id = folders
            .iter()
            .find(|folder| folder.id == item.virtual_folder_id)
            .map(|folder| item_parent_object_id(folder, item))
            .unwrap_or_else(|| format!("folder:{}", item.virtual_folder_id));
        append_didl_media_item(&mut didl, item, &parent_id, server_id, server_address);
    }
    didl.push_str("</DIDL-Lite>");
    didl
}

fn append_didl_container(
    didl: &mut String,
    id: &str,
    parent_id: &str,
    title: &str,
    child_count: usize,
    class: &str,
) {
    didl.push_str("<container id=\"");
    didl.push_str(&escape_xml(id));
    didl.push_str("\" parentID=\"");
    didl.push_str(&escape_xml(parent_id));
    didl.push_str("\" restricted=\"1\" searchable=\"1\" childCount=\"");
    didl.push_str(&child_count.to_string());
    didl.push_str("\"><dc:title>");
    didl.push_str(&escape_xml(title));
    didl.push_str("</dc:title><upnp:class>");
    didl.push_str(class);
    didl.push_str("</upnp:class></container>");
}

fn append_didl_media_item(
    didl: &mut String,
    item: &MediaItem,
    parent_id: &str,
    server_id: Uuid,
    server_address: &str,
) {
    didl.push_str("<item id=\"");
    didl.push_str(&escape_xml(&format!("item:{}", item.id)));
    didl.push_str("\" parentID=\"");
    didl.push_str(&escape_xml(parent_id));
    didl.push_str("\" restricted=\"1\"><dc:title>");
    didl.push_str(&escape_xml(&item.name));
    didl.push_str("</dc:title><upnp:class>");
    didl.push_str(didl_item_class(item));
    didl.push_str("</upnp:class>");
    append_didl_thumbnail(didl, item, server_id, server_address);
    if let Some(resource) = didl_resource(item, server_id, server_address) {
        didl.push_str(&resource);
    }
    append_didl_hls_transcode_resource(didl, item, server_id, server_address);
    append_didl_subtitles(didl, item, server_address);
    didl.push_str("</item>");
}

fn empty_didl() -> String {
    let mut didl = didl_prefix();
    didl.push_str("</DIDL-Lite>");
    didl
}

fn didl_prefix() -> String {
    concat!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" ",
        "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
        "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" ",
        "xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\" ",
        "xmlns:sec=\"http://www.sec.co.kr/\">"
    )
    .to_string()
}

fn didl_container_class(collection_type: Option<&str>) -> &'static str {
    match collection_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "music" => "object.container.album.musicAlbum",
        "photos" | "homevideos" => "object.container.storageFolder",
        "tvshows" => "object.container.genre.movieGenre",
        _ => "object.container.storageFolder",
    }
}

fn directory_container_class(collection_type: Option<&str>) -> &'static str {
    match collection_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "music" => "object.container.album.musicAlbum",
        "photos" | "homevideos" => "object.container.storageFolder",
        "tvshows" => "object.container.videoContainer",
        _ => "object.container.storageFolder",
    }
}

fn didl_item_class(item: &MediaItem) -> &'static str {
    match item.media_type.as_str() {
        "Video" => {
            if item
                .collection_type
                .as_deref()
                .is_some_and(|collection| collection.eq_ignore_ascii_case("movies"))
            {
                "object.item.videoItem.movie"
            } else {
                "object.item.videoItem"
            }
        }
        "Audio" => "object.item.audioItem.musicTrack",
        "Photo" => "object.item.imageItem.photo",
        _ => "object.item",
    }
}

fn didl_resource(item: &MediaItem, server_id: Uuid, server_address: &str) -> Option<String> {
    let mime_type = dlna_mime_type(item)?;
    let container = dlna_container(item).unwrap_or("bin");
    let protocol_info = dlna_protocol_info_for_item(item, mime_type);
    let mut attributes = format!("protocolInfo=\"{}\"", escape_xml(&protocol_info));
    if let Some(size) = item.file_size.filter(|size| *size >= 0) {
        attributes.push_str(" size=\"");
        attributes.push_str(&size.to_string());
        attributes.push('"');
    }
    if let Some(duration) = item.runtime_ticks.and_then(format_dlna_duration) {
        attributes.push_str(" duration=\"");
        attributes.push_str(&duration);
        attributes.push('"');
    }
    if let (Some(width), Some(height)) = (item.width, item.height)
        && width > 0
        && height > 0
    {
        attributes.push_str(" resolution=\"");
        attributes.push_str(&format!("{width}x{height}"));
        attributes.push('"');
    }
    let url = format!(
        "{}/dlna/{server_id}/items/{}/stream.{container}",
        server_address.trim_end_matches('/'),
        item.id
    );
    Some(format!("<res {attributes}>{}</res>", escape_xml(&url)))
}

fn append_didl_thumbnail(
    didl: &mut String,
    item: &MediaItem,
    server_id: Uuid,
    server_address: &str,
) {
    let url = format!(
        "{}/dlna/{server_id}/items/{}/thumbnail.png",
        server_address.trim_end_matches('/'),
        item.id
    );
    didl.push_str("<upnp:albumArtURI dlna:profileID=\"PNG_TN\">");
    didl.push_str(&escape_xml(&url));
    didl.push_str("</upnp:albumArtURI>");
}

fn append_didl_hls_transcode_resource(
    didl: &mut String,
    item: &MediaItem,
    server_id: Uuid,
    server_address: &str,
) {
    if item.media_type != "Video" {
        return;
    }
    let protocol_info = dlna_protocol_info_value("application/vnd.apple.mpegurl", None);
    let mut attributes = format!("protocolInfo=\"{}\"", escape_xml(&protocol_info));
    if let Some(duration) = item.runtime_ticks.and_then(format_dlna_duration) {
        attributes.push_str(" duration=\"");
        attributes.push_str(&duration);
        attributes.push('"');
    }
    if let (Some(width), Some(height)) = (item.width, item.height)
        && width > 0
        && height > 0
    {
        attributes.push_str(" resolution=\"");
        attributes.push_str(&format!("{width}x{height}"));
        attributes.push('"');
    }
    let url = format!(
        "{}/dlna/{server_id}/items/{}/transcode.m3u8",
        server_address.trim_end_matches('/'),
        item.id
    );
    didl.push_str("<res ");
    didl.push_str(&attributes);
    didl.push('>');
    didl.push_str(&escape_xml(&url));
    didl.push_str("</res>");
}

fn append_didl_subtitles(didl: &mut String, item: &MediaItem, server_address: &str) {
    for subtitle in dlna_subtitle_streams(item) {
        let url = format!(
            "{}/Videos/{}/{}/Subtitles/{}/Stream.{}",
            server_address.trim_end_matches('/'),
            item.id,
            item.id,
            subtitle.index,
            subtitle.format
        );
        didl.push_str("<res protocolInfo=\"http-get:*:");
        didl.push_str(subtitle.mime_type);
        didl.push_str(":*\">");
        didl.push_str(&escape_xml(&url));
        didl.push_str("</res>");
        didl.push_str("<sec:CaptionInfoEx sec:type=\"");
        didl.push_str(subtitle.format);
        didl.push_str("\">");
        didl.push_str(&escape_xml(&url));
        didl.push_str("</sec:CaptionInfoEx>");
    }
}

#[derive(Clone, Copy)]
struct DlnaSubtitleStream {
    index: i64,
    format: &'static str,
    mime_type: &'static str,
}

fn dlna_subtitle_streams(item: &MediaItem) -> Vec<DlnaSubtitleStream> {
    item.media_streams
        .iter()
        .filter(|stream| {
            json_string_case_insensitive(stream, "Type")
                .is_some_and(|stream_type| stream_type.eq_ignore_ascii_case("Subtitle"))
        })
        .filter_map(|stream| {
            let index = json_i64_case_insensitive(stream, "Index")?;
            let codec = json_string_case_insensitive(stream, "Codec").unwrap_or_default();
            let (format, mime_type) = dlna_subtitle_format(&codec)?;
            Some(DlnaSubtitleStream {
                index,
                format,
                mime_type,
            })
        })
        .collect()
}

fn dlna_subtitle_format(codec: &str) -> Option<(&'static str, &'static str)> {
    match codec.to_ascii_lowercase().as_str() {
        "srt" | "subrip" | "ass" | "ssa" | "webvtt" | "vtt" => Some(("vtt", "text/vtt")),
        _ => None,
    }
}

fn json_field_case_insensitive<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    value.as_object()?.iter().find_map(|(key, value)| {
        if key.eq_ignore_ascii_case(field) {
            Some(value)
        } else {
            None
        }
    })
}

fn json_string_case_insensitive(value: &Value, field: &str) -> Option<String> {
    let value = json_field_case_insensitive(value, field)?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn json_i64_case_insensitive(value: &Value, field: &str) -> Option<i64> {
    let value = json_field_case_insensitive(value, field)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn dlna_container(item: &MediaItem) -> Option<&str> {
    FsPath::new(&item.path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
}

fn dlna_mime_type(item: &MediaItem) -> Option<&'static str> {
    match dlna_container(item)?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "mkv" => Some("video/x-matroska"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        "avi" => Some("video/x-msvideo"),
        "asf" => Some("video/x-ms-asf"),
        "wmv" => Some("video/x-ms-wmv"),
        "ts" | "mpegts" | "m2ts" | "mts" => Some("video/vnd.dlna.mpeg-tts"),
        "mpeg" | "mpg" => Some("video/mpeg"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "flac" => Some("audio/flac"),
        "ogg" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "tif" | "tiff" => Some("image/tiff"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn dlna_protocol_info_for_item(item: &MediaItem, mime_type: &str) -> String {
    dlna_protocol_info_value(mime_type, dlna_profile_name_for_item(item, mime_type))
}

fn dlna_profile_name_for_item(_item: &MediaItem, mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "audio/mpeg" => Some("MP3"),
        "image/jpeg" => Some("JPEG_LRG"),
        "image/png" => Some("PNG_LRG"),
        "image/gif" => Some("GIF_LRG"),
        _ => None,
    }
}

fn is_root_object_id(object_id: &str) -> bool {
    matches!(object_id.trim(), "" | "0" | "1")
}

fn format_dlna_duration(runtime_ticks: i64) -> Option<String> {
    if runtime_ticks <= 0 {
        return None;
    }
    let total_millis = runtime_ticks / 10_000;
    let millis = total_millis % 1000;
    let total_seconds = total_millis / 1000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    Some(format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}"))
}

fn parse_folder_object_id(object_id: &str) -> Option<Uuid> {
    parse_prefixed_object_uuid(object_id, "folder:")
}

fn parse_item_object_id(object_id: &str) -> Option<Uuid> {
    parse_prefixed_object_uuid(object_id, "item:")
}

fn parse_directory_object_id(object_id: &str) -> Option<(Uuid, String)> {
    let value = object_id.trim().strip_prefix("dir:")?;
    let (folder_id, encoded_path) = value.split_once(':')?;
    let folder_id = Uuid::parse_str(folder_id).ok()?;
    let decoded = URL_SAFE_NO_PAD.decode(encoded_path).ok()?;
    let relative_path = String::from_utf8(decoded).ok()?;
    if relative_path.is_empty() {
        return None;
    }
    Some((folder_id, normalize_relative_path(&relative_path)))
}

fn parse_prefixed_object_uuid(object_id: &str, prefix: &str) -> Option<Uuid> {
    object_id
        .trim()
        .strip_prefix(prefix)
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn directory_object_id(folder_id: Uuid, relative_path: &str) -> String {
    format!(
        "dir:{folder_id}:{}",
        URL_SAFE_NO_PAD.encode(normalize_relative_path(relative_path).as_bytes())
    )
}

fn directory_parent_object_id(folder_id: Uuid, relative_path: &str) -> String {
    let mut components = relative_components(relative_path);
    if components.len() <= 1 {
        format!("folder:{folder_id}")
    } else {
        components.pop();
        directory_object_id(folder_id, &components.join("/"))
    }
}

fn directory_title(relative_path: &str) -> String {
    relative_components(relative_path)
        .pop()
        .unwrap_or_else(|| relative_path.to_string())
}

fn item_parent_relative_path(folder: &VirtualFolder, item: &MediaItem) -> Option<String> {
    let item_path = FsPath::new(&item.path);
    folder
        .locations
        .iter()
        .find_map(|location| {
            item_path
                .strip_prefix(FsPath::new(location))
                .ok()
                .map(relative_path_from_file_path)
        })
        .or_else(|| Some(String::new()))
}

fn item_parent_object_id(folder: &VirtualFolder, item: &MediaItem) -> String {
    let relative_path = item_parent_relative_path(folder, item).unwrap_or_default();
    if relative_path.is_empty() {
        format!("folder:{}", folder.id)
    } else {
        directory_object_id(folder.id, &relative_path)
    }
}

fn relative_path_from_file_path(relative_file_path: &FsPath) -> String {
    let mut components = relative_file_path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    components.pop();
    components.join("/")
}

fn relative_components(relative_path: &str) -> Vec<String> {
    relative_path
        .split('/')
        .map(str::trim)
        .filter(|component| !component.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_relative_path(relative_path: &str) -> String {
    relative_components(relative_path).join("/")
}

fn next_child_directory(item_parent: &str, current_relative_path: &str) -> Option<String> {
    let parent_components = relative_components(item_parent);
    let current_components = relative_components(current_relative_path);
    if parent_components.len() <= current_components.len() {
        return None;
    }
    if parent_components
        .iter()
        .zip(current_components.iter())
        .any(|(parent, current)| parent != current)
    {
        return None;
    }
    parent_components.get(current_components.len()).cloned()
}

fn join_relative_path(parent: &str, child: &str) -> String {
    let parent = normalize_relative_path(parent);
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

#[derive(Clone, Copy)]
enum DlnaSortDirection {
    Ascending,
    Descending,
}

fn dlna_title_sort_direction(sort_criteria: &str) -> Option<DlnaSortDirection> {
    sort_criteria
        .split(',')
        .map(str::trim)
        .filter(|criterion| !criterion.is_empty())
        .find_map(|criterion| {
            let (direction, field) = match criterion.as_bytes().first() {
                Some(b'-') => (DlnaSortDirection::Descending, &criterion[1..]),
                Some(b'+') => (DlnaSortDirection::Ascending, &criterion[1..]),
                _ => (DlnaSortDirection::Ascending, criterion),
            };
            field
                .trim()
                .eq_ignore_ascii_case("dc:title")
                .then_some(direction)
        })
}

fn sort_virtual_folders(folders: &mut [VirtualFolder], sort_criteria: &str) {
    if let Some(direction) = dlna_title_sort_direction(sort_criteria) {
        folders.sort_by(|left, right| compare_dlna_titles(&left.name, &right.name, direction));
    }
}

fn sort_media_items(items: &mut [MediaItem], sort_criteria: &str) {
    if let Some(direction) = dlna_title_sort_direction(sort_criteria) {
        items.sort_by(|left, right| {
            compare_dlna_titles(&left.name, &right.name, direction)
                .then_with(|| left.id.cmp(&right.id))
        });
    }
}

fn sort_browse_entries(entries: &mut [DlnaBrowseEntry<'_>], sort_criteria: &str) {
    if let Some(direction) = dlna_title_sort_direction(sort_criteria) {
        entries.sort_by(|left, right| {
            compare_dlna_titles(left.title(), right.title(), direction)
                .then_with(|| left.id().cmp(&right.id()))
        });
    }
}

fn compare_dlna_titles(
    left: &str,
    right: &str,
    direction: DlnaSortDirection,
) -> std::cmp::Ordering {
    let ordering = left
        .to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right));
    match direction {
        DlnaSortDirection::Ascending => ordering,
        DlnaSortDirection::Descending => ordering.reverse(),
    }
}

fn dlna_search_criteria_matches(item: &MediaItem, criteria: &str) -> bool {
    let criteria = unescape_xml(criteria.trim());
    if criteria.is_empty() || criteria == "*" {
        return true;
    }

    search_criteria_expression_matches(item, &criteria).unwrap_or(false)
}

fn search_criteria_expression_matches(item: &MediaItem, criteria: &str) -> Option<bool> {
    let criteria = strip_wrapping_search_parentheses(criteria);
    if criteria.is_empty() || criteria == "*" {
        return Some(true);
    }

    let or_terms = split_search_criteria_logical(criteria, "or");
    if or_terms.len() > 1 {
        let mut recognized = false;
        for term in or_terms {
            if let Some(matches) = search_criteria_expression_matches(item, term) {
                recognized = true;
                if matches {
                    return Some(true);
                }
            }
        }
        return recognized.then_some(false);
    }

    let and_terms = split_search_criteria_logical(criteria, "and");
    if and_terms.len() > 1 {
        let mut recognized = false;
        for term in and_terms {
            if let Some(matches) = search_criteria_expression_matches(item, term) {
                recognized = true;
                if !matches {
                    return Some(false);
                }
            } else {
                return None;
            }
        }
        return recognized.then_some(true);
    }

    search_criteria_condition_matches(item, criteria)
}

fn search_criteria_condition_matches(item: &MediaItem, criteria: &str) -> Option<bool> {
    if let Some(value) = search_criteria_value(criteria, "dc:title doesnotcontain") {
        return Some(!search_text_contains(&item.name, &value));
    }
    if let Some(value) = search_criteria_value(criteria, "dc:title contains") {
        return Some(search_text_contains(&item.name, &value));
    }
    if let Some(value) = search_criteria_value(criteria, "dc:title !=") {
        return Some(!item.name.eq_ignore_ascii_case(&value));
    }
    if let Some(value) = search_criteria_value(criteria, "dc:title =") {
        return Some(item.name.eq_ignore_ascii_case(&value));
    }
    if let Some(value) = search_criteria_value(criteria, "upnp:class derivedfrom") {
        let class = didl_item_class(item).to_ascii_lowercase();
        return Some(class.starts_with(&value.to_ascii_lowercase()));
    }
    if let Some(value) = search_criteria_value(criteria, "upnp:class !=") {
        let class = didl_item_class(item).to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        return Some(class != value && !class.starts_with(&format!("{value}.")));
    }
    if let Some(value) = search_criteria_value(criteria, "upnp:class =") {
        let class = didl_item_class(item).to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        return Some(class == value || class.starts_with(&format!("{value}.")));
    }
    if let Some(value) = search_criteria_value(criteria, "@id !=") {
        return Some(!search_item_id_matches(item, &value));
    }
    if let Some(value) = search_criteria_value(criteria, "@id =") {
        return Some(search_item_id_matches(item, &value));
    }
    for property in ["res@protocolinfo", "@protocolinfo", "protocolinfo"] {
        if let Some(value) = search_criteria_value(criteria, &format!("{property} contains")) {
            let protocol_info = item_protocol_info_for_search(item)?;
            return Some(search_text_contains(&protocol_info, &value));
        }
        if let Some(value) = search_criteria_value(criteria, &format!("{property} !=")) {
            let protocol_info = item_protocol_info_for_search(item)?;
            return Some(!protocol_info.eq_ignore_ascii_case(&value));
        }
        if let Some(value) = search_criteria_value(criteria, &format!("{property} =")) {
            let protocol_info = item_protocol_info_for_search(item)?;
            return Some(protocol_info.eq_ignore_ascii_case(&value));
        }
        if let Some(value) = search_criteria_value(criteria, &format!("{property} exists")) {
            return Some(search_exists_result(
                item_protocol_info_for_search(item).is_some(),
                &value,
            ));
        }
    }
    for property in ["dc:title", "upnp:class", "@id"] {
        if let Some(value) = search_criteria_value(criteria, &format!("{property} exists")) {
            return Some(search_exists_result(
                search_property_exists(item, property)?,
                &value,
            ));
        }
    }

    None
}

fn search_text_contains(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn search_item_id_matches(item: &MediaItem, value: &str) -> bool {
    let item_id = format!("item:{}", item.id);
    item_id.eq_ignore_ascii_case(value) || item.id.to_string().eq_ignore_ascii_case(value)
}

fn item_protocol_info_for_search(item: &MediaItem) -> Option<String> {
    let mime_type = dlna_mime_type(item)?;
    Some(dlna_protocol_info_for_item(item, mime_type))
}

fn search_property_exists(item: &MediaItem, property: &str) -> Option<bool> {
    match property {
        "dc:title" => Some(!item.name.trim().is_empty()),
        "upnp:class" | "@id" => Some(true),
        _ => None,
    }
}

fn search_exists_result(exists: bool, expected: &str) -> bool {
    match expected.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => exists,
        "false" | "0" | "no" => !exists,
        _ => false,
    }
}

fn split_search_criteria_logical<'a>(criteria: &'a str, operator: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut depth = 0_usize;

    for (index, ch) in criteria.char_indices() {
        if let Some(quote_char) = quote {
            if ch == quote_char {
                quote = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }

        if depth == 0 && logical_operator_at(criteria, index, operator) {
            let part = criteria[start..index].trim();
            if !part.is_empty() {
                parts.push(part);
            }
            start = index + operator.len();
        }
    }

    if parts.is_empty() {
        return vec![criteria.trim()];
    }
    let tail = criteria[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn logical_operator_at(criteria: &str, index: usize, operator: &str) -> bool {
    let Some(candidate) = criteria.get(index..index.saturating_add(operator.len())) else {
        return false;
    };
    if !candidate.eq_ignore_ascii_case(operator) {
        return false;
    }
    let before = criteria[..index].chars().next_back();
    let after = criteria[index + operator.len()..].chars().next();
    before.is_some_and(char::is_whitespace) && after.is_some_and(char::is_whitespace)
}

fn strip_wrapping_search_parentheses(value: &str) -> &str {
    let mut value = value.trim();
    loop {
        if !(value.starts_with('(') && value.ends_with(')')) {
            return value;
        }
        if !outer_search_parentheses_wrap(value) {
            return value;
        }
        value = value[1..value.len() - 1].trim();
    }
}

fn outer_search_parentheses_wrap(value: &str) -> bool {
    let last_index = value.len() - 1;
    let mut quote = None;
    let mut depth = 0_usize;
    for (index, ch) in value.char_indices() {
        if let Some(quote_char) = quote {
            if ch == quote_char {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && index != last_index {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn search_criteria_value(criteria: &str, token: &str) -> Option<String> {
    let lower = criteria.to_ascii_lowercase();
    let token_lower = token.to_ascii_lowercase();
    let start = lower.find(&token_lower)? + token.len();
    let rest = criteria.get(start..)?.trim_start();
    quoted_or_bare_search_value(rest)
}

fn quoted_or_bare_search_value(value: &str) -> Option<String> {
    let mut chars = value.chars();
    match chars.next()? {
        '"' => chars
            .as_str()
            .split_once('"')
            .map(|(quoted, _)| quoted.to_string()),
        '\'' => chars
            .as_str()
            .split_once('\'')
            .map(|(quoted, _)| quoted.to_string()),
        _ => value
            .split_whitespace()
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    }
}

fn parse_dlna_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value.trim()).map_err(|_| ApiError::bad_request("Invalid DLNA item id"))
}

fn soap_response(service_type: &str, action_response: String, body: String) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <SOAP-ENV:Envelope xmlns:SOAP-ENV=\"{SOAP_ENV_NS}\" SOAP-ENV:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <SOAP-ENV:Body><u:{action_response} xmlns:u=\"{service_type}\">{body}</u:{action_response}></SOAP-ENV:Body>\
         </SOAP-ENV:Envelope>"
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/xml; charset=utf-8".to_string()),
            (header::CONTENT_LENGTH, xml.len().to_string()),
            (header::HeaderName::from_static("ext"), String::new()),
        ],
        xml,
    )
        .into_response()
}

fn soap_fault_response(error_code: u16, description: &str) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <SOAP-ENV:Envelope xmlns:SOAP-ENV=\"{SOAP_ENV_NS}\" SOAP-ENV:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <SOAP-ENV:Body><SOAP-ENV:Fault>\
         <faultcode>SOAP-ENV:Client</faultcode>\
         <faultstring>UPnPError</faultstring>\
         <detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
         <errorCode>{error_code}</errorCode>\
         <errorDescription>{}</errorDescription>\
         </UPnPError></detail>\
         </SOAP-ENV:Fault></SOAP-ENV:Body>\
         </SOAP-ENV:Envelope>",
        escape_xml(description)
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [
            (header::CONTENT_TYPE, "text/xml; charset=utf-8".to_string()),
            (header::CONTENT_LENGTH, xml.len().to_string()),
            (header::HeaderName::from_static("ext"), String::new()),
        ],
        xml,
    )
        .into_response()
}

fn xml_response(xml: String) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/xml; charset=utf-8".to_string()),
            (header::CONTENT_LENGTH, xml.len().to_string()),
        ],
        xml,
    )
        .into_response()
}

fn soap_action(xml: &str) -> Option<String> {
    [
        "GetProtocolInfo",
        "GetCurrentConnectionIDs",
        "GetCurrentConnectionInfo",
        "GetSearchCapabilities",
        "GetSortCapabilities",
        "GetSystemUpdateID",
        "X_GetFeatureList",
        "Browse",
        "Search",
        "IsAuthorized",
        "IsValidated",
        "RegisterDevice",
    ]
    .into_iter()
    .find(|action| {
        xml.contains(&format!(":{action}"))
            || xml.contains(&format!("<{action}"))
            || xml.contains(&format!("\"{action}\""))
    })
    .map(|action| action.to_ascii_lowercase())
    .or_else(|| soap_body_action(xml))
}

fn soap_body_action(xml: &str) -> Option<String> {
    let mut offset = 0;
    while let Some(open_rel) = xml[offset..].find('<') {
        let open_start = offset + open_rel;
        let Some(open_end_rel) = xml[open_start..].find('>') else {
            break;
        };
        let open_end = open_start + open_end_rel;
        let tag = xml[open_start + 1..open_end].trim();
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            offset = open_end + 1;
            continue;
        }
        let tag_name = tag.split_whitespace().next().unwrap_or_default();
        let tag_name = tag_name.trim_end_matches('/');
        if !tag_local_name(tag_name).eq_ignore_ascii_case("Body") {
            offset = open_end + 1;
            continue;
        }

        let mut body_offset = open_end + 1;
        while let Some(body_open_rel) = xml[body_offset..].find('<') {
            let body_open = body_offset + body_open_rel;
            let Some(body_open_end_rel) = xml[body_open..].find('>') else {
                break;
            };
            let body_open_end = body_open + body_open_end_rel;
            let body_tag = xml[body_open + 1..body_open_end].trim();
            if body_tag.starts_with('/') {
                return None;
            }
            if body_tag.starts_with('?') || body_tag.starts_with('!') {
                body_offset = body_open_end + 1;
                continue;
            }
            let body_tag_name = body_tag
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_end_matches('/');
            let local_name = tag_local_name(body_tag_name);
            if local_name.is_empty() {
                return None;
            }
            return Some(local_name.to_ascii_lowercase());
        }
        return None;
    }
    None
}

fn action_response_name(action: &str) -> String {
    match action {
        "getprotocolinfo" => "GetProtocolInfoResponse",
        "getcurrentconnectionids" => "GetCurrentConnectionIDsResponse",
        "getcurrentconnectioninfo" => "GetCurrentConnectionInfoResponse",
        "getsearchcapabilities" => "GetSearchCapabilitiesResponse",
        "getsortcapabilities" => "GetSortCapabilitiesResponse",
        "getsystemupdateid" => "GetSystemUpdateIDResponse",
        "x_getfeaturelist" => "X_GetFeatureListResponse",
        "browse" => "BrowseResponse",
        "search" => "SearchResponse",
        "isauthorized" => "IsAuthorizedResponse",
        "isvalidated" => "IsValidatedResponse",
        "registerdevice" => "RegisterDeviceResponse",
        _ => "UnknownResponse",
    }
    .to_string()
}

fn soap_param(xml: &str, name: &str) -> Option<String> {
    let mut offset = 0;
    while let Some(open_rel) = xml[offset..].find('<') {
        let open_start = offset + open_rel;
        let Some(open_end_rel) = xml[open_start..].find('>') else {
            break;
        };
        let open_end = open_start + open_end_rel;
        let tag = xml[open_start + 1..open_end].trim();
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            offset = open_end + 1;
            continue;
        }
        let tag_name = tag.split_whitespace().next().unwrap_or_default();
        let tag_name = tag_name.trim_end_matches('/');
        if tag_local_name(tag_name) != name {
            offset = open_end + 1;
            continue;
        }

        let value_start = open_end + 1;
        let close = format!("</{tag_name}>");
        if let Some(close_rel) = xml[value_start..].find(&close) {
            let value_end = value_start + close_rel;
            return Some(unescape_xml(xml[value_start..value_end].trim()));
        }

        let mut close_offset = value_start;
        while let Some(close_rel) = xml[close_offset..].find("</") {
            let close_start = close_offset + close_rel;
            let Some(close_end_rel) = xml[close_start..].find('>') else {
                break;
            };
            let close_end = close_start + close_end_rel;
            let close_tag = xml[close_start + 2..close_end].trim();
            if tag_local_name(close_tag) == name {
                return Some(unescape_xml(xml[value_start..close_start].trim()));
            }
            close_offset = close_end + 1;
        }

        offset = value_start;
    }
    None
}

fn tag_local_name(tag_name: &str) -> &str {
    tag_name
        .rsplit_once(':')
        .map_or(tag_name, |(_, local_name)| local_name)
}

fn feature_list() -> &'static str {
    concat!(
        "<Features xmlns=\"urn:schemas-upnp-org:av:avs\" ",
        "xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
        "xsi:schemaLocation=\"urn:schemas-upnp-org:av:avs http://www.upnp.org/schemas/av/avs.xsd\">",
        "<Feature name=\"samsung.com_BASICVIEW\" version=\"1\"><container id=\"0\" type=\"object.item\"/>",
        "</Feature></Features>"
    )
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

const CONTENT_DIRECTORY_ACTIONS: &[DlnaAction] = &[
    DlnaAction {
        name: "GetSearchCapabilities",
        arguments: &[DlnaArgument {
            name: "SearchCaps",
            direction: "out",
            related_state_variable: "SearchCapabilities",
        }],
    },
    DlnaAction {
        name: "GetSortCapabilities",
        arguments: &[DlnaArgument {
            name: "SortCaps",
            direction: "out",
            related_state_variable: "SortCapabilities",
        }],
    },
    DlnaAction {
        name: "GetSystemUpdateID",
        arguments: &[DlnaArgument {
            name: "Id",
            direction: "out",
            related_state_variable: "SystemUpdateID",
        }],
    },
    DlnaAction {
        name: "Browse",
        arguments: &[
            DlnaArgument {
                name: "ObjectID",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_ObjectID",
            },
            DlnaArgument {
                name: "BrowseFlag",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_BrowseFlag",
            },
            DlnaArgument {
                name: "Filter",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Filter",
            },
            DlnaArgument {
                name: "StartingIndex",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Index",
            },
            DlnaArgument {
                name: "RequestedCount",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "SortCriteria",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_SortCriteria",
            },
            DlnaArgument {
                name: "Result",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Result",
            },
            DlnaArgument {
                name: "NumberReturned",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "TotalMatches",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "UpdateID",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_UpdateID",
            },
        ],
    },
    DlnaAction {
        name: "Search",
        arguments: &[
            DlnaArgument {
                name: "ContainerID",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_ObjectID",
            },
            DlnaArgument {
                name: "SearchCriteria",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_SearchCriteria",
            },
            DlnaArgument {
                name: "Filter",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Filter",
            },
            DlnaArgument {
                name: "StartingIndex",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Index",
            },
            DlnaArgument {
                name: "RequestedCount",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "SortCriteria",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_SortCriteria",
            },
            DlnaArgument {
                name: "Result",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Result",
            },
            DlnaArgument {
                name: "NumberReturned",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "TotalMatches",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Count",
            },
            DlnaArgument {
                name: "UpdateID",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_UpdateID",
            },
        ],
    },
    DlnaAction {
        name: "X_GetFeatureList",
        arguments: &[DlnaArgument {
            name: "FeatureList",
            direction: "out",
            related_state_variable: "A_ARG_TYPE_Featurelist",
        }],
    },
];

const CONNECTION_MANAGER_ACTIONS: &[DlnaAction] = &[
    DlnaAction {
        name: "GetProtocolInfo",
        arguments: &[
            DlnaArgument {
                name: "Source",
                direction: "out",
                related_state_variable: "SourceProtocolInfo",
            },
            DlnaArgument {
                name: "Sink",
                direction: "out",
                related_state_variable: "SinkProtocolInfo",
            },
        ],
    },
    DlnaAction {
        name: "GetCurrentConnectionIDs",
        arguments: &[DlnaArgument {
            name: "ConnectionIDs",
            direction: "out",
            related_state_variable: "CurrentConnectionIDs",
        }],
    },
    DlnaAction {
        name: "GetCurrentConnectionInfo",
        arguments: &[
            DlnaArgument {
                name: "ConnectionID",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_ConnectionID",
            },
            DlnaArgument {
                name: "RcsID",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_RcsID",
            },
            DlnaArgument {
                name: "AVTransportID",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_AVTransportID",
            },
            DlnaArgument {
                name: "ProtocolInfo",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_ProtocolInfo",
            },
            DlnaArgument {
                name: "PeerConnectionManager",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_ConnectionManager",
            },
            DlnaArgument {
                name: "PeerConnectionID",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_ConnectionID",
            },
            DlnaArgument {
                name: "Direction",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Direction",
            },
            DlnaArgument {
                name: "Status",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_ConnectionStatus",
            },
        ],
    },
];

const MEDIA_RECEIVER_REGISTRAR_ACTIONS: &[DlnaAction] = &[
    DlnaAction {
        name: "IsAuthorized",
        arguments: &[
            DlnaArgument {
                name: "DeviceID",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_DeviceID",
            },
            DlnaArgument {
                name: "Result",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Result",
            },
        ],
    },
    DlnaAction {
        name: "IsValidated",
        arguments: &[
            DlnaArgument {
                name: "DeviceID",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_DeviceID",
            },
            DlnaArgument {
                name: "Result",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_Result",
            },
        ],
    },
    DlnaAction {
        name: "RegisterDevice",
        arguments: &[
            DlnaArgument {
                name: "RegistrationReqMsg",
                direction: "in",
                related_state_variable: "A_ARG_TYPE_RegistrationReqMsg",
            },
            DlnaArgument {
                name: "RegistrationRespMsg",
                direction: "out",
                related_state_variable: "A_ARG_TYPE_RegistrationRespMsg",
            },
        ],
    },
];

const CONTENT_DIRECTORY_STATE_VARIABLES: &[DlnaStateVariable] = &[
    DlnaStateVariable {
        name: "SearchCapabilities",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "SortCapabilities",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "SystemUpdateID",
        data_type: "ui4",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Result",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_ObjectID",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_BrowseFlag",
        data_type: "string",
        sends_events: false,
        allowed_values: &["BrowseMetadata", "BrowseDirectChildren"],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Filter",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Index",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Count",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_UpdateID",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_SortCriteria",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_SearchCriteria",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Featurelist",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
];

const CONNECTION_MANAGER_STATE_VARIABLES: &[DlnaStateVariable] = &[
    DlnaStateVariable {
        name: "SourceProtocolInfo",
        data_type: "string",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "SinkProtocolInfo",
        data_type: "string",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "CurrentConnectionIDs",
        data_type: "string",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_ConnectionStatus",
        data_type: "string",
        sends_events: false,
        allowed_values: &[
            "OK",
            "ContentFormatMismatch",
            "InsufficientBandwidth",
            "UnreliableChannel",
            "Unknown",
        ],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_ProtocolInfo",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_ConnectionManager",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Direction",
        data_type: "string",
        sends_events: false,
        allowed_values: &["Input", "Output"],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_ConnectionID",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_AVTransportID",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_RcsID",
        data_type: "ui4",
        sends_events: false,
        allowed_values: &[],
    },
];

const MEDIA_RECEIVER_REGISTRAR_STATE_VARIABLES: &[DlnaStateVariable] = &[
    DlnaStateVariable {
        name: "AuthorizationDeniedUpdateID",
        data_type: "ui4",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "ValidationRevokedUpdateID",
        data_type: "ui4",
        sends_events: true,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_DeviceID",
        data_type: "string",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_Result",
        data_type: "int",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_RegistrationReqMsg",
        data_type: "bin.base64",
        sends_events: false,
        allowed_values: &[],
    },
    DlnaStateVariable {
        name: "A_ARG_TYPE_RegistrationRespMsg",
        data_type: "bin.base64",
        sends_events: false,
        allowed_values: &[],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    fn test_server_id() -> Uuid {
        Uuid::parse_str("58deb718-f9ee-4ac5-a1d4-05286d64cf42").unwrap()
    }

    #[test]
    fn ssdp_search_response_matches_upnp_contract() {
        let server_id = test_server_id();
        let response = ssdp_search_response(
            server_id,
            "http://192.168.1.46:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml",
            MEDIA_SERVER_DEVICE.to_string(),
        );

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("CACHE-CONTROL: max-age=1800\r\n"));
        assert!(response.contains("EXT:\r\n"));
        assert!(response.contains("LOCATION: http://192.168.1.46:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml\r\n"));
        assert!(response.contains("ST: urn:schemas-upnp-org:device:MediaServer:1\r\n"));
        assert!(response.contains("USN: uuid:58deb718-f9ee-4ac5-a1d4-05286d64cf42::urn:schemas-upnp-org:device:MediaServer:1\r\n"));
        assert!(response.contains("BOOTID.UPNP.ORG: 1\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
    }

    #[test]
    fn ssdp_matching_targets_cover_root_uuid_device_and_services() {
        let server_id = test_server_id();
        let all = matching_ssdp_targets(server_id, "ssdp:all");
        assert_eq!(all.len(), 6);
        assert!(all.contains(&UPNP_ROOT_DEVICE.to_string()));
        assert!(all.contains(&format!("uuid:{server_id}")));
        assert!(all.contains(&MEDIA_SERVER_DEVICE.to_string()));
        assert!(all.contains(&CONTENT_DIRECTORY_SERVICE.to_string()));
        assert!(all.contains(&CONNECTION_MANAGER_SERVICE.to_string()));
        assert!(all.contains(&MEDIA_RECEIVER_REGISTRAR_SERVICE.to_string()));

        let service = matching_ssdp_targets(server_id, CONTENT_DIRECTORY_SERVICE);
        assert_eq!(service, vec![CONTENT_DIRECTORY_SERVICE.to_string()]);
        let registrar = matching_ssdp_targets(server_id, MEDIA_RECEIVER_REGISTRAR_SERVICE);
        assert_eq!(
            registrar,
            vec![MEDIA_RECEIVER_REGISTRAR_SERVICE.to_string()]
        );
        assert!(matching_ssdp_targets(server_id, "urn:example:unknown").is_empty());
    }

    #[test]
    fn ssdp_notify_alive_and_byebye_use_expected_headers() {
        let server_id = test_server_id();
        let alive = ssdp_notify_message(
            server_id,
            "http://127.0.0.1:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml",
            UPNP_ROOT_DEVICE,
            SsdpNotificationKind::Alive,
        );
        assert!(alive.starts_with("NOTIFY * HTTP/1.1\r\n"));
        assert!(alive.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(alive.contains("NTS: ssdp:alive\r\n"));
        assert!(alive.contains("LOCATION: http://127.0.0.1:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml\r\n"));
        assert!(
            alive.contains("USN: uuid:58deb718-f9ee-4ac5-a1d4-05286d64cf42::upnp:rootdevice\r\n")
        );

        let byebye = ssdp_notify_message(
            server_id,
            "http://127.0.0.1:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml",
            UPNP_ROOT_DEVICE,
            SsdpNotificationKind::Byebye,
        );
        assert!(byebye.contains("NTS: ssdp:byebye\r\n"));
        assert!(!byebye.contains("LOCATION:"));
        assert!(
            byebye.contains("USN: uuid:58deb718-f9ee-4ac5-a1d4-05286d64cf42::upnp:rootdevice\r\n")
        );
    }

    #[test]
    fn ssdp_base_url_replaces_unspecified_bind_address_for_peer() {
        let peer: SocketAddr = "127.0.0.1:55321".parse().unwrap();
        assert_eq!(
            ssdp_base_url_for_peer("http://0.0.0.0:8097", peer),
            "http://127.0.0.1:8097"
        );
        assert_eq!(
            ssdp_base_url_for_peer("http://192.168.1.46:8097", peer),
            "http://192.168.1.46:8097"
        );
    }

    #[test]
    fn dlna_protocol_info_adds_profile_hints_and_seek_flags() {
        let source = dlna_protocol_info();
        assert!(source.contains("http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_OP=01"));
        assert!(source.contains("http-get:*:image/jpeg:DLNA.ORG_PN=JPEG_LRG;DLNA.ORG_OP=01"));
        assert!(source.contains("http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0"));
        assert!(source.contains("http-get:*:video/vnd.dlna.mpeg-tts:DLNA.ORG_OP=01"));
        assert!(source.contains("http-get:*:video/x-ms-asf:DLNA.ORG_OP=01"));
        assert!(source.contains("DLNA.ORG_FLAGS=01500000000000000000000000000000"));
        assert!(!dlna_protocol_info_value("video/mp4", None).contains("DLNA.ORG_PN="));
    }

    #[test]
    fn dlna_video_mime_matrix_covers_direct_play_containers_without_false_pn() {
        let mp4 = test_media_item("/media/movie.mp4", "Video");
        assert_eq!(dlna_mime_type(&mp4), Some("video/mp4"));
        assert_eq!(dlna_profile_name_for_item(&mp4, "video/mp4"), None);
        assert!(!dlna_protocol_info_for_item(&mp4, "video/mp4").contains("DLNA.ORG_PN="));

        let mpeg_ps = test_media_item("/media/movie.mpg", "Video");
        assert_eq!(dlna_mime_type(&mpeg_ps), Some("video/mpeg"));
        assert_eq!(dlna_profile_name_for_item(&mpeg_ps, "video/mpeg"), None);

        let mpeg_ts = test_media_item("/media/movie.ts", "Video");
        assert_eq!(dlna_mime_type(&mpeg_ts), Some("video/vnd.dlna.mpeg-tts"));
        assert_eq!(
            dlna_profile_name_for_item(&mpeg_ts, "video/vnd.dlna.mpeg-tts"),
            None
        );

        let m2ts = test_media_item("/media/movie.m2ts", "Video");
        assert_eq!(dlna_mime_type(&m2ts), Some("video/vnd.dlna.mpeg-tts"));

        let asf = test_media_item("/media/movie.asf", "Video");
        assert_eq!(dlna_mime_type(&asf), Some("video/x-ms-asf"));
        assert_eq!(dlna_profile_name_for_item(&asf, "video/x-ms-asf"), None);
    }

    #[test]
    fn dlna_search_criteria_supports_boolean_protocol_and_exists() {
        let mut video = test_media_item("/media/Match Movie.mp4", "Video");
        video.name = "Match Movie".to_string();
        video.collection_type = Some("movies".to_string());

        assert!(dlna_search_criteria_matches(
            &video,
            r#"(upnp:class derivedfrom "object.item.videoItem" and dc:title doesNotContain "Other") and res@protocolInfo contains "video/mp4""#
        ));
        assert!(dlna_search_criteria_matches(
            &video,
            &format!(r#"@id = "item:{}" and dc:title exists true"#, video.id)
        ));
        assert!(dlna_search_criteria_matches(
            &video,
            r#"dc:title contains "Song" or dc:title = "Match Movie""#
        ));
        assert!(!dlna_search_criteria_matches(
            &video,
            r#"upnp:class != "object.item.videoItem""#
        ));
        assert!(!dlna_search_criteria_matches(
            &video,
            r#"res@protocolInfo exists false"#
        ));

        let mut audio = test_media_item("/media/Match Song.mp3", "Audio");
        audio.name = "Match Song".to_string();
        assert!(dlna_search_criteria_matches(
            &audio,
            r#"upnp:class != "object.item.videoItem" and res@protocolInfo contains "audio/mpeg""#
        ));
    }

    #[test]
    fn dlna_sort_criteria_orders_items_by_title_direction() {
        let mut items = vec![
            {
                let mut item = test_media_item("/media/b.mp4", "Video");
                item.name = "Bravo".to_string();
                item
            },
            {
                let mut item = test_media_item("/media/a.mp4", "Video");
                item.name = "Alpha".to_string();
                item
            },
        ];

        sort_media_items(&mut items, "+dc:title");
        assert_eq!(items[0].name, "Alpha");
        assert_eq!(items[1].name, "Bravo");

        sort_media_items(&mut items, "-dc:title");
        assert_eq!(items[0].name, "Bravo");
        assert_eq!(items[1].name, "Alpha");
    }

    fn test_media_item(path: &str, media_type: &str) -> MediaItem {
        MediaItem {
            id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            virtual_folder_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            name: "Test".to_string(),
            path: path.to_string(),
            media_type: media_type.to_string(),
            collection_type: Some("movies".to_string()),
            file_size: None,
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            created_at: ::time::OffsetDateTime::UNIX_EPOCH,
            updated_at: ::time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    #[ignore = "requires UDP bind permission; run explicitly for local/device validation"]
    async fn ssdp_udp_handler_responds_to_msearch() {
        let server_id = test_server_id();
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let request = concat!(
            "M-SEARCH * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "MAN: \"ssdp:discover\"\r\n",
            "MX: 1\r\n",
            "ST: urn:schemas-upnp-org:device:MediaServer:1\r\n",
            "\r\n"
        );

        client
            .send_to(request.as_bytes(), server.local_addr().unwrap())
            .await
            .unwrap();
        let mut request_buffer = vec![0_u8; 1024];
        let (len, peer) = server.recv_from(&mut request_buffer).await.unwrap();

        let sent = respond_to_ssdp_search(
            &server,
            &request_buffer[..len],
            peer,
            server_id,
            "http://127.0.0.1:8097",
        )
        .await
        .unwrap();
        assert_eq!(sent, 1);

        let mut response_buffer = vec![0_u8; 2048];
        let (len, _) = timeout(
            Duration::from_secs(2),
            client.recv_from(&mut response_buffer),
        )
        .await
        .unwrap()
        .unwrap();
        let response = std::str::from_utf8(&response_buffer[..len]).unwrap();
        assert!(response.contains("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("ST: urn:schemas-upnp-org:device:MediaServer:1\r\n"));
        assert!(response.contains("USN: uuid:58deb718-f9ee-4ac5-a1d4-05286d64cf42::urn:schemas-upnp-org:device:MediaServer:1\r\n"));
        assert!(response.contains("LOCATION: http://127.0.0.1:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml\r\n"));
    }
}
