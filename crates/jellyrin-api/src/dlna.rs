use axum::{
    body::Bytes as BodyBytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use jellyrin_core::VirtualFolder;
use jellyrin_db::Database;
use serde_json::Value;
use uuid::Uuid;

use crate::{ApiError, AppState, COMPATIBLE_PRODUCT_NAME, default_network_configuration};

const UPNP_DEVICE_NS: &str = "urn:schemas-upnp-org:device-1-0";
const DLNA_DEVICE_NS: &str = "urn:schemas-dlna-org:device-1-0";
const UPNP_SERVICE_NS: &str = "urn:schemas-upnp-org:service-1-0";
const SOAP_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const CONTENT_DIRECTORY_SERVICE: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const CONNECTION_MANAGER_SERVICE: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
const CONTENT_DIRECTORY_ID: &str = "urn:upnp-org:serviceId:ContentDirectory";
const CONNECTION_MANAGER_ID: &str = "urn:upnp-org:serviceId:ConnectionManager";
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
