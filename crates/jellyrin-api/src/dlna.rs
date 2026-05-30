use axum::{
    body::Bytes as BodyBytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use jellyrin_core::VirtualFolder;
use jellyrin_db::Database;
use serde_json::Value;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket},
    time::Duration as StdDuration,
};
use tokio::{net::UdpSocket, task::JoinHandle, time};
use uuid::Uuid;

use crate::{
    ApiError, AppState, COMPATIBLE_PRODUCT_NAME, COMPATIBLE_SERVER_VERSION,
    default_network_configuration, subscribe_system_lifecycle_commands,
};

const SSDP_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const SSDP_CACHE_SECONDS: u32 = 1800;
const SSDP_NOTIFY_INTERVAL_SECONDS: u64 = 60;
const SSDP_CONFIG_CHECK_SECONDS: u64 = 5;
const UPNP_DEVICE_NS: &str = "urn:schemas-upnp-org:device-1-0";
const DLNA_DEVICE_NS: &str = "urn:schemas-dlna-org:device-1-0";
const UPNP_SERVICE_NS: &str = "urn:schemas-upnp-org:service-1-0";
const SOAP_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const CONTENT_DIRECTORY_SERVICE: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CONNECTION_MANAGER_SERVICE: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
const CONTENT_DIRECTORY_ID: &str = "urn:upnp-org:serviceId:ContentDirectory";
const CONNECTION_MANAGER_ID: &str = "urn:upnp-org:serviceId:ConnectionManager";
const UPNP_ROOT_DEVICE: &str = "upnp:rootdevice";
const MEDIA_SERVER_DEVICE: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const DLNA_PROTOCOL_INFO: &str = concat!(
    "http-get:*:video/mpeg:*,",
    "http-get:*:video/mp4:*,",
    "http-get:*:video/vnd.dlna.mpeg-tts:*,",
    "http-get:*:video/avi:*,",
    "http-get:*:video/x-matroska:*,",
    "http-get:*:video/x-ms-wmv:*,",
    "http-get:*:video/wtv:*,",
    "http-get:*:audio/mpeg:*,",
    "http-get:*:audio/mp3:*,",
    "http-get:*:audio/mp4:*,",
    "http-get:*:audio/x-ms-wma:*,",
    "http-get:*:audio/wav:*,",
    "http-get:*:audio/L16:*,",
    "http-get:*:image/jpeg:*,",
    "http-get:*:image/png:*,",
    "http-get:*:image/gif:*,",
    "http-get:*:image/tiff:*"
);
const ONE_BY_ONE_PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
    0x42, 0x60, 0x82,
];

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
    let action =
        soap_action(&request).ok_or_else(|| ApiError::bad_request("Invalid SOAP action"))?;
    let body = match action.as_str() {
        "getprotocolinfo" => format!(
            "<Source>{}</Source><Sink></Sink>",
            escape_xml(DLNA_PROTOCOL_INFO)
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
            return Err(ApiError::not_found(
                "Unsupported ConnectionManager SOAP action",
            ));
        }
    };
    Ok(soap_response(
        CONNECTION_MANAGER_SERVICE,
        action_response_name(&action),
        body,
    ))
}

pub(crate) async fn content_directory_control(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    body: BodyBytes,
) -> Result<Response, ApiError> {
    dlna_context(&state, &server_id).await?;
    let request = String::from_utf8_lossy(&body);
    let action =
        soap_action(&request).ok_or_else(|| ApiError::bad_request("Invalid SOAP action"))?;
    let response_body = match action.as_str() {
        "getsearchcapabilities" => "<SearchCaps></SearchCaps>".to_string(),
        "getsortcapabilities" => "<SortCaps>dc:title</SortCaps>".to_string(),
        "getsystemupdateid" => "<Id>0</Id>".to_string(),
        "x_getfeaturelist" => format!("<FeatureList>{}</FeatureList>", escape_xml(feature_list())),
        "browse" => browse_response(&state.db, &request).await?,
        "search" => empty_browse_like_response(),
        _ => {
            return Err(ApiError::not_found(
                "Unsupported ContentDirectory SOAP action",
            ));
        }
    };
    Ok(soap_response(
        CONTENT_DIRECTORY_SERVICE,
        action_response_name(&action),
        response_body,
    ))
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
         <serviceList>{}{}</serviceList>\
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

async fn browse_response(db: &Database, request: &str) -> Result<String, ApiError> {
    let object_id = soap_param(request, "ObjectID").unwrap_or_else(|| "0".to_string());
    let browse_flag =
        soap_param(request, "BrowseFlag").unwrap_or_else(|| "BrowseDirectChildren".to_string());
    let didl = if object_id == "0" && browse_flag.eq_ignore_ascii_case("BrowseMetadata") {
        didl_root_metadata()
    } else if object_id == "0" {
        didl_root_children(db.virtual_folders().await?)
    } else {
        empty_didl()
    };
    let count = didl.matches("<container ").count() + didl.matches("<item ").count();
    Ok(format!(
        "<Result>{}</Result><NumberReturned>{count}</NumberReturned><TotalMatches>{count}</TotalMatches><UpdateID>0</UpdateID>",
        escape_xml(&didl)
    ))
}

fn empty_browse_like_response() -> String {
    format!(
        "<Result>{}</Result><NumberReturned>0</NumberReturned><TotalMatches>0</TotalMatches><UpdateID>0</UpdateID>",
        escape_xml(&empty_didl())
    )
}

fn didl_root_metadata() -> String {
    concat!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" ",
        "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
        "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
        "<container id=\"0\" parentID=\"-1\" restricted=\"1\" childCount=\"0\">",
        "<dc:title>Jellyrin</dc:title>",
        "<upnp:class>object.container</upnp:class>",
        "</container></DIDL-Lite>"
    )
    .to_string()
}

fn didl_root_children(folders: Vec<VirtualFolder>) -> String {
    let mut didl = didl_prefix();
    for folder in folders {
        didl.push_str("<container id=\"");
        didl.push_str(&escape_xml(&format!("folder:{}", folder.id)));
        didl.push_str("\" parentID=\"0\" restricted=\"1\" childCount=\"0\"><dc:title>");
        didl.push_str(&escape_xml(&folder.name));
        didl.push_str("</dc:title><upnp:class>");
        didl.push_str(didl_container_class(folder.collection_type.as_deref()));
        didl.push_str("</upnp:class></container>");
    }
    didl.push_str("</DIDL-Lite>");
    didl
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
        "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">"
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
    ]
    .into_iter()
    .find(|action| {
        xml.contains(&format!(":{action}"))
            || xml.contains(&format!("<{action}"))
            || xml.contains(&format!("\"{action}\""))
    })
    .map(|action| action.to_ascii_lowercase())
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
        assert_eq!(all.len(), 5);
        assert!(all.contains(&UPNP_ROOT_DEVICE.to_string()));
        assert!(all.contains(&format!("uuid:{server_id}")));
        assert!(all.contains(&MEDIA_SERVER_DEVICE.to_string()));
        assert!(all.contains(&CONTENT_DIRECTORY_SERVICE.to_string()));
        assert!(all.contains(&CONNECTION_MANAGER_SERVICE.to_string()));

        let service = matching_ssdp_targets(server_id, CONTENT_DIRECTORY_SERVICE);
        assert_eq!(service, vec![CONTENT_DIRECTORY_SERVICE.to_string()]);
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
