use std::{fs, path::PathBuf};

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
use jellyrin_compat::{
    AuthenticateUserByNameDto, AuthenticationResultDto, CountryDto, CultureDto, HealthResponse,
    LocalizationOptionDto, PublicSystemInfo, SessionInfoDto, StartupConfigurationDto,
    StartupRemoteAccessDto, StartupUserDto, UserDto, UserPolicyDto,
};
use jellyrin_core::{DeviceToken, StartupConfig, User, VirtualFolder};
use jellyrin_db::Database;
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tower_http::{services::ServeDir, trace::TraceLayer};
use uuid::Uuid;

const COMPATIBLE_SERVER_VERSION: &str = "12.0.0";
const COMPATIBLE_PRODUCT_NAME: &str = "Jellyfin Server";
const ISO_639_2_DATA: &str = include_str!("localization/iso6392.txt");
const COUNTRIES_DATA: &str = include_str!("localization/countries.json");

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub web_dir: PathBuf,
    pub local_address: String,
}

pub fn router(state: AppState) -> Router {
    let web_dir = state.web_dir.clone();

    Router::new()
        .route("/", get(|| async { Redirect::temporary("/web/") }))
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/System/Info/Public", get(system_info_public))
        .route("/System/Info", get(system_info))
        .route("/Startup/Configuration", get(get_startup_configuration))
        .route("/Startup/Configuration", post(post_startup_configuration))
        .route("/Startup/RemoteAccess", post(post_startup_remote_access))
        .route("/Startup/User", get(get_startup_user))
        .route("/Startup/FirstUser", get(get_startup_user))
        .route("/Startup/User", post(post_startup_user))
        .route("/Startup/Complete", post(post_startup_complete))
        .route("/Users/Public", get(get_public_users))
        .route("/users/public", get(get_public_users))
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
        .route("/Users/authenticatebyname", post(authenticate_by_name))
        .route("/users/authenticatebyname", post(authenticate_by_name))
        .route("/users/AuthenticateByName", post(authenticate_by_name))
        .route("/Users/Me", get(get_current_user))
        .route("/users/me", get(get_current_user))
        .route("/Users/{user_id}/Views", get(empty_items_result))
        .route("/users/{user_id}/views", get(empty_items_result))
        .route("/Users/{user_id}", get(get_user_by_id))
        .route("/users/{user_id}", get(get_user_by_id))
        .route("/Sessions/Logout", post(logout))
        .route("/sessions/logout", post(logout))
        .route("/Sessions/Capabilities", post(no_content))
        .route("/Sessions/Capabilities/Full", post(no_content))
        .route("/sessions/capabilities", post(no_content))
        .route("/sessions/capabilities/full", post(no_content))
        .route("/QuickConnect/Enabled", get(quick_connect_enabled))
        .route("/quickconnect/enabled", get(quick_connect_enabled))
        .route("/Branding/Configuration", get(branding_configuration))
        .route("/branding/configuration", get(branding_configuration))
        .route("/Branding/Css", get(empty_text))
        .route("/Branding/Css.css", get(empty_text))
        .route("/Branding/Splashscreen", get(empty_text))
        .route("/branding/splashscreen", get(empty_text))
        .route("/Library/VirtualFolders", get(get_virtual_folders))
        .route("/Library/VirtualFolders", post(add_virtual_folder))
        .route(
            "/Library/VirtualFolders/Paths",
            post(add_virtual_folder_path),
        )
        .route("/library/virtualfolders", get(get_virtual_folders))
        .route("/library/virtualfolders", post(add_virtual_folder))
        .route(
            "/library/virtualfolders/paths",
            post(add_virtual_folder_path),
        )
        .route("/Environment/Drives", get(environment_drives))
        .route("/environment/drives", get(environment_drives))
        .route(
            "/Environment/DirectoryContents",
            get(environment_directory_contents),
        )
        .route(
            "/environment/directorycontents",
            get(environment_directory_contents),
        )
        .route("/Environment/ParentPath", get(environment_parent_path))
        .route("/environment/parentpath", get(environment_parent_path))
        .route("/Environment/ValidatePath", post(environment_validate_path))
        .route("/environment/validatepath", post(environment_validate_path))
        .route("/DisplayPreferences/usersettings", get(display_preferences))
        .route("/displaypreferences/usersettings", get(display_preferences))
        .route("/System/Endpoint", get(system_endpoint))
        .route("/system/endpoint", get(system_endpoint))
        .route("/Playback/BitrateTest", get(bitrate_test))
        .route("/playback/bitratetest", get(bitrate_test))
        .route("/UserViews", get(empty_items_result))
        .route("/userviews", get(empty_items_result))
        .route("/Items", get(empty_items_result))
        .route("/items", get(empty_items_result))
        .route("/Items/Latest", get(empty_list))
        .route("/items/latest", get(empty_list))
        .route("/Items/{item_id}/Images/{image_type}", get(placeholder_png))
        .route("/items/{item_id}/images/{image_type}", get(placeholder_png))
        .route("/Users/{user_id}/Images/{image_type}", get(placeholder_png))
        .route("/users/{user_id}/images/{image_type}", get(placeholder_png))
        .route("/Shows/NextUp", get(empty_items_result))
        .route("/shows/nextup", get(empty_items_result))
        .route("/UserItems/Resume", get(empty_items_result))
        .route("/useritems/resume", get(empty_items_result))
        .route("/Localization/Options", get(localization_options))
        .route("/Localization/Cultures", get(localization_cultures))
        .route("/Localization/cultures", get(localization_cultures))
        .route("/Localization/Countries", get(localization_countries))
        .route("/Localization/countries", get(localization_countries))
        .route("/socket", get(websocket))
        .nest_service(
            "/web",
            ServeDir::new(web_dir).append_index_html_on_directories(true),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "Healthy" })
}

async fn ready(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    sqlx::query("SELECT 1").execute(state.db.pool()).await?;
    Ok(Json(HealthResponse { status: "Ready" }))
}

async fn system_info_public(
    State(state): State<AppState>,
) -> Result<Json<PublicSystemInfo>, ApiError> {
    let server = state.db.server_state().await?;
    Ok(Json(PublicSystemInfo {
        id: server.server_id,
        server_name: server.server_name,
        version: COMPATIBLE_SERVER_VERSION.to_string(),
        product_name: COMPATIBLE_PRODUCT_NAME.to_string(),
        operating_system: "Linux".to_string(),
        local_address: state.local_address,
        startup_wizard_completed: server.startup_wizard_completed,
    }))
}

async fn system_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<PublicSystemInfo>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    system_info_public(State(state)).await
}

async fn get_startup_configuration(
    State(state): State<AppState>,
) -> Result<Json<StartupConfigurationDto>, ApiError> {
    Ok(Json(startup_config_to_dto(
        state.db.startup_config().await?,
    )))
}

async fn post_startup_configuration(
    State(state): State<AppState>,
    Json(payload): Json<StartupConfigurationDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    let current = state.db.startup_config().await?;
    state
        .db
        .update_startup_config(StartupConfig {
            server_name: payload.server_name,
            ui_culture: payload.ui_culture,
            metadata_country_code: payload.metadata_country_code,
            preferred_metadata_language: payload.preferred_metadata_language,
            enable_remote_access: current.enable_remote_access,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_startup_remote_access(
    State(state): State<AppState>,
    Json(payload): Json<StartupRemoteAccessDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    state
        .db
        .set_remote_access(payload.enable_remote_access)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_startup_user(State(state): State<AppState>) -> Result<Json<StartupUserDto>, ApiError> {
    let user = state.db.first_user().await?;
    Ok(Json(StartupUserDto {
        name: Some(user.name),
        password: None,
    }))
}

async fn post_startup_user(
    State(state): State<AppState>,
    Json(payload): Json<StartupUserDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    let name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("admin")
        .to_string();
    let password = payload
        .password
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("Password must not be empty"))?;

    state.db.update_first_user(name, password).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_startup_complete(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    state.db.complete_startup_wizard().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_public_users(State(state): State<AppState>) -> Result<Json<Vec<UserDto>>, ApiError> {
    let server = state.db.server_state().await?;
    let user = state.db.first_user().await?;
    Ok(Json(vec![user_to_dto(&user, server.server_id)]))
}

async fn authenticate_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AuthenticateUserByNameDto>,
) -> Result<Json<AuthenticationResultDto>, ApiError> {
    let auth = client_auth_from_headers(&headers);
    let username = payload
        .username
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("Username must not be empty"))?;
    let password = payload.pw.as_deref().unwrap_or("");
    let (user, token) = state
        .db
        .authenticate_user_by_name(
            username,
            password,
            &auth.device_id,
            &auth.device,
            &auth.client,
            &auth.version,
        )
        .await
        .map_err(|_| ApiError::unauthorized("Invalid username or password"))?;
    let server = state.db.server_state().await?;

    Ok(Json(authentication_result_to_dto(
        &user,
        &token,
        server.server_id,
    )))
}

async fn get_current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<UserDto>, ApiError> {
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    Ok(Json(user_to_dto(&user, server.server_id)))
}

async fn get_user_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<UserDto>, ApiError> {
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if user.id != user_id && !user.is_administrator {
        return Err(ApiError::forbidden("User access denied"));
    }
    let server = state.db.server_state().await?;
    Ok(Json(user_to_dto(&user, server.server_id)))
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let token = bearer_token(&headers)
        .or_else(|| query.api_key.clone())
        .ok_or_else(|| ApiError::unauthorized("Missing token"))?;
    state.db.revoke_token(&token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn quick_connect_enabled() -> Json<bool> {
    Json(false)
}

async fn branding_configuration() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "LoginDisclaimer": null,
        "CustomCss": null,
        "SplashscreenEnabled": true
    }))
}

async fn empty_text() -> &'static str {
    ""
}

async fn empty_list() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn get_virtual_folders(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let folders = state.db.virtual_folders().await?;
    Ok(Json(folders.iter().map(virtual_folder_to_json).collect()))
}

#[derive(Debug, Deserialize)]
struct AddVirtualFolderQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
    #[serde(alias = "CollectionType", alias = "collectionType")]
    collection_type: Option<String>,
    #[serde(alias = "Paths", alias = "paths")]
    paths: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddVirtualFolderBody {
    #[serde(alias = "LibraryOptions")]
    library_options: Option<LibraryOptionsBody>,
}

#[derive(Debug, Deserialize)]
struct LibraryOptionsBody {
    #[serde(alias = "PathInfos")]
    path_infos: Option<Vec<MediaPathInfoBody>>,
}

#[derive(Debug, Deserialize)]
struct MediaPathInfoBody {
    #[serde(alias = "Path")]
    path: Option<String>,
}

async fn add_virtual_folder(
    State(state): State<AppState>,
    Query(query): Query<AddVirtualFolderQuery>,
    body: Option<Json<AddVirtualFolderBody>>,
) -> Result<StatusCode, ApiError> {
    let mut locations = comma_delimited_paths(query.paths.as_deref());
    if let Some(Json(body)) = body
        && let Some(library_options) = body.library_options
        && let Some(path_infos) = library_options.path_infos
    {
        locations.extend(
            path_infos
                .into_iter()
                .filter_map(|path_info| path_info.path),
        );
    }

    state
        .db
        .upsert_virtual_folder(&query.name, query.collection_type.as_deref(), locations)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct AddMediaPathBody {
    #[serde(alias = "Name")]
    name: String,
    #[serde(alias = "Path")]
    path: Option<String>,
    #[serde(alias = "PathInfo")]
    path_info: Option<MediaPathInfoBody>,
}

async fn add_virtual_folder_path(
    State(state): State<AppState>,
    Json(payload): Json<AddMediaPathBody>,
) -> Result<StatusCode, ApiError> {
    let path = payload
        .path_info
        .and_then(|path_info| path_info.path)
        .or(payload.path)
        .ok_or_else(|| ApiError::bad_request("PathInfo and Path can't both be null"))?;
    state
        .db
        .add_virtual_folder_path(&payload.name, &path)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn comma_delimited_paths(paths: Option<&str>) -> Vec<String> {
    paths
        .into_iter()
        .flat_map(|paths| paths.split(','))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn virtual_folder_to_json(folder: &VirtualFolder) -> serde_json::Value {
    serde_json::json!({
        "Name": folder.name,
        "Locations": folder.locations,
        "CollectionType": folder.collection_type,
        "LibraryOptions": {
            "Enabled": true,
            "EnablePhotos": true,
            "EnableRealtimeMonitor": false,
            "EnableLUFSScan": false,
            "EnableChapterImageExtraction": false,
            "ExtractChapterImagesDuringLibraryScan": false,
            "EnableTrickplayImageExtraction": false,
            "ExtractTrickplayImagesDuringLibraryScan": false,
            "PathInfos": folder.locations.iter().map(|path| {
                serde_json::json!({ "Path": path })
            }).collect::<Vec<_>>(),
            "SaveLocalMetadata": false,
            "EnableInternetProviders": false,
            "EnableAutomaticSeriesGrouping": true,
            "EnableEmbeddedTitles": false,
            "EnableEmbeddedExtrasTitles": false,
            "EnableEmbeddedEpisodeInfos": false,
            "AutomaticRefreshIntervalDays": 0,
            "PreferredMetadataLanguage": null,
            "MetadataCountryCode": null,
            "SeasonZeroDisplayName": "Specials"
        },
        "ItemId": folder.id.simple().to_string(),
        "PrimaryImageItemId": null,
        "RefreshProgress": null,
        "RefreshStatus": "Idle"
    })
}

async fn environment_drives() -> Json<Vec<serde_json::Value>> {
    Json(vec![
        serde_json::json!({
            "Name": "/",
            "Path": "/",
            "Type": "Network",
        }),
        serde_json::json!({
            "Name": "Home",
            "Path": "/home/cdmonio",
            "Type": "Network",
        }),
    ])
}

#[derive(Debug, Deserialize)]
struct DirectoryQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
    #[serde(alias = "IncludeFiles")]
    include_files: Option<bool>,
}

async fn environment_directory_contents(
    Query(query): Query<DirectoryQuery>,
) -> Json<Vec<serde_json::Value>> {
    let path = query.path.unwrap_or_else(|| PathBuf::from("/"));
    let include_files = query.include_files.unwrap_or(false);

    let entries = fs::read_dir(path)
        .map(|read_dir| {
            read_dir
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let metadata = entry.metadata().ok()?;
                    if metadata.is_file() && !include_files {
                        return None;
                    }

                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    Some(serde_json::json!({
                        "Name": name,
                        "Path": path,
                        "Type": if metadata.is_dir() { "Directory" } else { "File" },
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Json(entries)
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_parent_path(Query(query): Query<PathQuery>) -> Json<serde_json::Value> {
    let parent = query
        .path
        .as_deref()
        .and_then(|path| path.parent())
        .map(|path| path.to_string_lossy().into_owned());
    Json(serde_json::json!({ "Path": parent }))
}

#[derive(Debug, Deserialize)]
struct ValidatePathRequest {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_validate_path(Json(payload): Json<ValidatePathRequest>) -> StatusCode {
    if payload.path.as_deref().is_some_and(|path| path.exists()) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::BAD_REQUEST
    }
}

async fn empty_items_result() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Items": [],
        "TotalRecordCount": 0,
        "StartIndex": 0
    }))
}

async fn display_preferences() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Id": "usersettings",
        "ViewType": "",
        "SortBy": "SortName",
        "IndexBy": "",
        "RememberIndexing": false,
        "PrimaryImageHeight": 0,
        "PrimaryImageWidth": 0,
        "CustomPrefs": {}
    }))
}

async fn system_endpoint() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "IsLocal": true,
        "IsInNetwork": true
    }))
}

#[derive(Debug, Deserialize)]
struct BitrateQuery {
    #[serde(alias = "Size")]
    size: Option<usize>,
}

async fn bitrate_test(Query(query): Query<BitrateQuery>) -> impl IntoResponse {
    let size = query.size.unwrap_or_default().min(1_000_000);
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        vec![0u8; size],
    )
}

async fn placeholder_png() -> impl IntoResponse {
    const TRANSPARENT_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1,
        13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    (
        [(header::CONTENT_TYPE, "image/png")],
        TRANSPARENT_PNG.to_vec(),
    )
}

async fn websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> axum::response::Response {
    if let Err(error) = require_user(&state.db, &headers, query.api_key.as_deref()).await {
        return error.into_response();
    }

    match ws {
        Ok(ws) => ws.on_upgrade(handle_websocket).into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

async fn handle_websocket(mut socket: WebSocket) {
    let _ = socket
        .send(Message::Text(
            serde_json::json!({
                "MessageType": "ForceKeepAlive",
                "Data": 300
            })
            .to_string()
            .into(),
        ))
        .await;
}

async fn localization_options() -> Json<Vec<LocalizationOptionDto>> {
    Json(vec![
        LocalizationOptionDto {
            name: "English".to_string(),
            value: "en-US".to_string(),
        },
        LocalizationOptionDto {
            name: "Espanol".to_string(),
            value: "es-ES".to_string(),
        },
    ])
}

async fn localization_cultures() -> Json<Vec<CultureDto>> {
    Json(load_cultures())
}

async fn localization_countries() -> Json<Vec<CountryDto>> {
    Json(load_countries())
}

fn load_cultures() -> Vec<CultureDto> {
    let mut cultures = ISO_639_2_DATA
        .lines()
        .filter_map(|line| {
            let parts: Vec<_> = line.split('|').collect();
            if parts.len() != 5 || parts[3].trim().is_empty() {
                return None;
            }

            let mut name = parts[3].to_string();
            let two_letter = parts[2].to_string();
            if two_letter.contains('-') {
                name = two_letter.clone();
            }

            let mut three_letter_names = vec![parts[0].to_string()];
            if !parts[1].is_empty() {
                three_letter_names.push(parts[1].to_string());
            }

            Some(CultureDto {
                name,
                display_name: parts[3].to_string(),
                two_letter_iso_language_name: two_letter,
                three_letter_iso_language_name: three_letter_names.first().cloned(),
                three_letter_iso_language_names: three_letter_names,
            })
        })
        .collect::<Vec<_>>();

    cultures.sort_by_key(|culture| culture.display_name.to_ascii_lowercase());
    cultures.dedup_by(|a, b| a.display_name.eq_ignore_ascii_case(&b.display_name));
    cultures
}

fn load_countries() -> Vec<CountryDto> {
    let mut countries: Vec<CountryDto> =
        serde_json::from_str(COUNTRIES_DATA).expect("bundled countries.json is valid");
    countries.sort_by_key(|country| country.display_name.to_ascii_lowercase());
    countries
}

#[derive(Debug, Deserialize)]
struct AuthQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
}

#[derive(Debug)]
struct ClientAuth {
    client: String,
    device: String,
    device_id: String,
    version: String,
}

fn startup_config_to_dto(config: StartupConfig) -> StartupConfigurationDto {
    StartupConfigurationDto {
        server_name: config.server_name,
        ui_culture: config.ui_culture,
        metadata_country_code: config.metadata_country_code,
        preferred_metadata_language: config.preferred_metadata_language,
    }
}

fn user_to_dto(user: &User, server_id: Uuid) -> UserDto {
    UserDto {
        id: user.id,
        name: user.name.clone(),
        server_id,
        has_password: true,
        has_configured_password: true,
        has_configured_easy_password: false,
        enable_auto_login: false,
        policy: UserPolicyDto {
            is_administrator: user.is_administrator,
            is_disabled: user.is_disabled,
            enable_all_devices: true,
            enable_remote_control_of_other_users: user.is_administrator,
            enable_shared_device_control: true,
            enable_remote_access: true,
        },
    }
}

fn authentication_result_to_dto(
    user: &User,
    token: &DeviceToken,
    server_id: Uuid,
) -> AuthenticationResultDto {
    AuthenticationResultDto {
        user: user_to_dto(user, server_id),
        session_info: SessionInfoDto {
            id: token.access_token.clone(),
            user_id: user.id,
            user_name: user.name.clone(),
            client: token.client.clone(),
            last_activity_date: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
            device_name: token.device_name.clone(),
            device_id: token.device_id.clone(),
            application_version: token.version.clone(),
            is_active: true,
        },
        access_token: token.access_token.clone(),
        server_id,
    }
}

async fn require_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(User, DeviceToken), ApiError> {
    let token = bearer_token(headers)
        .or_else(|| query_token.map(ToOwned::to_owned))
        .ok_or_else(|| ApiError::unauthorized("Missing token"))?;
    match db.user_by_token(&token).await {
        Ok(auth) => Ok(auth),
        Err(_) => db
            .user_by_api_key(&token)
            .await
            .map_err(|_| ApiError::unauthorized("Invalid token")),
    }
}

async fn require_startup_wizard_incomplete(db: &Database) -> Result<(), ApiError> {
    let server = db.server_state().await?;
    if server.startup_wizard_completed {
        return Err(ApiError::forbidden("Startup wizard is already complete"));
    }
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    for name in ["x-emby-token", "x-mediabrowser-token"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok())
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }

    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authorization_token)
}

fn client_auth_from_headers(headers: &HeaderMap) -> ClientAuth {
    let mut auth = ClientAuth {
        client: "Jellyfin Web".to_string(),
        device: "Browser".to_string(),
        device_id: Uuid::new_v4().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if let Some(header) = headers
        .get("authorization")
        .or_else(|| headers.get("x-emby-authorization"))
        .and_then(|value| value.to_str().ok())
    {
        for (key, value) in parse_media_browser_pairs(header) {
            match key.as_str() {
                "client" => auth.client = value,
                "device" => auth.device = value,
                "deviceid" => auth.device_id = value,
                "version" => auth.version = value,
                _ => {}
            }
        }
    }

    auth
}

fn parse_authorization_token(header: &str) -> Option<String> {
    parse_media_browser_pairs(header)
        .into_iter()
        .find_map(|(key, value)| (key == "token").then_some(value))
}

fn parse_media_browser_pairs(header: &str) -> Vec<(String, String)> {
    let payload = header
        .strip_prefix("MediaBrowser ")
        .or_else(|| header.strip_prefix("Emby "))
        .unwrap_or(header);

    payload
        .split(',')
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            Some((
                key.trim().to_ascii_lowercase(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect()
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    error: anyhow::Error,
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: error.into(),
        }
    }
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: anyhow::anyhow!(message.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(error = %self.error, status = %self.status, "request failed");
        (
            self.status,
            Json(serde_json::json!({
                "Error": self.status.canonical_reason().unwrap_or("Error"),
                "Message": self.error.to_string(),
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppState, load_countries, load_cultures, parse_authorization_token,
        parse_media_browser_pairs, router,
    };
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use jellyrin_db::Database;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    #[test]
    fn parses_media_browser_authorization_header() {
        let pairs = parse_media_browser_pairs(
            r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="abc", Version="1", Token="tok""#,
        );
        assert!(pairs.contains(&("client".to_string(), "Jellyfin Web".to_string())));
        assert!(pairs.contains(&("deviceid".to_string(), "abc".to_string())));
        assert_eq!(
            parse_authorization_token(
                r#"MediaBrowser Client="Jellyfin Web", DeviceId="abc", Token="tok""#
            ),
            Some("tok".to_string())
        );
    }

    #[test]
    fn bundled_localization_lists_match_jellyfin_scale() {
        let cultures = load_cultures();
        let countries = load_countries();

        assert!(cultures.len() > 400);
        assert!(countries.len() >= 140);
        assert!(cultures.iter().any(|culture| {
            culture.two_letter_iso_language_name == "es"
                && culture.display_name == "Spanish; Castilian"
        }));
        assert!(
            countries
                .iter()
                .any(|country| country.two_letter_iso_region_name == "ES")
        );
    }

    #[tokio::test]
    async fn startup_wizard_and_login_http_flow() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Startup/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/Complete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="test-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        let token = result["AccessToken"].as_str().unwrap();
        assert!(!token.is_empty());
        assert_eq!(result["User"]["Name"], "admin");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/System/Info")
                    .header(
                        header::AUTHORIZATION,
                        format!(
                            r#"MediaBrowser Client="Jellyfin Web", DeviceId="test-device", Token="{token}""#
                        ),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn startup_mutations_are_blocked_after_wizard_completion() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/Complete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "changed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authenticated_routes_accept_api_keys() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/System/Info?api_key={api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn websocket_requires_auth_and_accepts_query_or_header_token() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(websocket_request("/socket").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="socket-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        let token = result["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                websocket_request(&format!("/socket?api_key={token}&deviceId=socket-test"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);

        let response = app
            .oneshot(
                websocket_request("/socket")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }

    fn websocket_request(uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .uri(uri)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "x3JJHMbDL1EzLkh9GBhXDw==")
    }

    #[tokio::test]
    async fn m1_environment_library_and_image_compat_endpoints_exist() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Environment/Drives")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/environment/directorycontents?Path=/&IncludeFiles=false")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Library/VirtualFolders?name=Movies&collectionType=movies&paths=/media/movies")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/library/virtualfolders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(folders[0]["Name"], "Movies");
        assert_eq!(folders[0]["Locations"][0], "/media/movies");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/environment/validatepath")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/items/00000000-0000-0000-0000-000000000000/images/Primary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
    }
}
