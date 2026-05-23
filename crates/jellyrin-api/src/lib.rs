#![recursion_limit = "256"]

use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

use axum::{
    Json, Router,
    body::Body,
    extract::ws::{Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::{delete, get, post},
};
use jellyrin_compat::{
    AuthenticateUserByNameDto, AuthenticationResultDto, CountryDto, CultureDto, HealthResponse,
    LocalizationOptionDto, PublicSystemInfo, SessionInfoDto, StartupConfigurationDto,
    StartupRemoteAccessDto, StartupUserDto, UserDto, UserPolicyDto,
};
use jellyrin_core::{DeviceToken, MediaItem, PlaybackState, StartupConfig, User, VirtualFolder};
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
        .route("/System/Ping", get(ping))
        .route("/System/Ping", post(ping))
        .route("/system/ping", get(ping))
        .route("/system/ping", post(ping))
        .route("/System/Info/Storage", get(system_storage))
        .route("/system/info/storage", get(system_storage))
        .route("/System/ActivityLog/Entries", get(activity_log_entries))
        .route("/system/activitylog/entries", get(activity_log_entries))
        .route("/System/Configuration", get(system_configuration))
        .route("/system/configuration", get(system_configuration))
        .route("/System/Configuration", post(admin_no_content))
        .route("/system/configuration", post(admin_no_content))
        .route(
            "/System/Configuration/MetadataOptions/Default",
            get(default_metadata_options),
        )
        .route(
            "/system/configuration/metadataoptions/default",
            get(default_metadata_options),
        )
        .route("/System/Configuration/Branding", post(admin_no_content))
        .route("/system/configuration/branding", post(admin_no_content))
        .route("/System/Configuration/{key}", get(named_configuration))
        .route("/system/configuration/{key}", get(named_configuration))
        .route("/System/Configuration/{key}", post(admin_no_content))
        .route("/system/configuration/{key}", post(admin_no_content))
        .route(
            "/Dashboard/web/ConfigurationPages",
            get(dashboard_configuration_pages),
        )
        .route(
            "/dashboard/web/configurationpages",
            get(dashboard_configuration_pages),
        )
        .route("/Dashboard/web/ConfigurationPage", get(empty_text))
        .route("/dashboard/web/configurationpage", get(empty_text))
        .route("/Devices", get(devices))
        .route("/devices", get(devices))
        .route("/Devices/Info", get(device_info))
        .route("/devices/info", get(device_info))
        .route("/Devices/Options", get(device_options))
        .route("/devices/options", get(device_options))
        .route("/Devices/Options", post(admin_no_content))
        .route("/devices/options", post(admin_no_content))
        .route("/Devices", delete(admin_no_content))
        .route("/devices", delete(admin_no_content))
        .route("/Session/Sessions", get(session_sessions))
        .route("/session/sessions", get(session_sessions))
        .route("/Sessions", get(session_sessions))
        .route("/sessions", get(session_sessions))
        .route("/Plugins", get(installed_plugins))
        .route("/plugins", get(installed_plugins))
        .route("/Plugins/{plugin_id}/Configuration", get(empty_object))
        .route("/plugins/{plugin_id}/configuration", get(empty_object))
        .route("/Plugins/{plugin_id}/Manifest", get(plugin_manifest))
        .route("/plugins/{plugin_id}/manifest", get(plugin_manifest))
        .route("/Packages", get(available_packages))
        .route("/packages", get(available_packages))
        .route("/Repositories", get(package_repositories))
        .route("/repositories", get(package_repositories))
        .route("/ScheduledTasks", get(scheduled_tasks))
        .route("/scheduledtasks", get(scheduled_tasks))
        .route("/ScheduledTasks/{task_id}", get(scheduled_task))
        .route("/scheduledtasks/{task_id}", get(scheduled_task))
        .route(
            "/ScheduledTasks/Running/{task_id}",
            post(start_scheduled_task),
        )
        .route(
            "/scheduledtasks/running/{task_id}",
            post(start_scheduled_task),
        )
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
        .route("/Users/{user_id}/Views", get(user_views_result))
        .route("/users/{user_id}/views", get(user_views_result))
        .route("/Users/{user_id}", get(get_user_by_id))
        .route("/users/{user_id}", get(get_user_by_id))
        .route("/Sessions/Logout", post(logout))
        .route("/sessions/logout", post(logout))
        .route("/Sessions/Playing", post(report_playback_start))
        .route("/sessions/playing", post(report_playback_start))
        .route("/Sessions/Playing/Progress", post(report_playback_progress))
        .route("/sessions/playing/progress", post(report_playback_progress))
        .route("/Sessions/Playing/Stopped", post(report_playback_stopped))
        .route("/sessions/playing/stopped", post(report_playback_stopped))
        .route("/Sessions/Capabilities", post(no_content))
        .route("/Sessions/Capabilities/Full", post(no_content))
        .route("/sessions/capabilities", post(no_content))
        .route("/sessions/capabilities/full", post(no_content))
        .route("/QuickConnect/Enabled", get(quick_connect_enabled))
        .route("/quickconnect/enabled", get(quick_connect_enabled))
        .route("/SyncPlay/List", get(empty_json_array))
        .route("/syncplay/list", get(empty_json_array))
        .route("/LiveTv/Info", get(live_tv_info))
        .route("/livetv/info", get(live_tv_info))
        .route("/LiveTv/GuideInfo", get(live_tv_guide_info))
        .route("/livetv/guideinfo", get(live_tv_guide_info))
        .route("/LiveTv/Channels", get(empty_items_result))
        .route("/livetv/channels", get(empty_items_result))
        .route("/LiveTv/Programs", get(empty_items_result))
        .route("/livetv/programs", get(empty_items_result))
        .route("/LiveTv/RecommendedPrograms", get(empty_items_result))
        .route("/livetv/recommendedprograms", get(empty_items_result))
        .route("/LiveTv/Recordings", get(empty_items_result))
        .route("/livetv/recordings", get(empty_items_result))
        .route("/LiveTv/RecordingGroups", get(empty_items_result))
        .route("/livetv/recordinggroups", get(empty_items_result))
        .route("/LiveTv/Timers", get(empty_result_json))
        .route("/livetv/timers", get(empty_result_json))
        .route("/LiveTv/SeriesTimers", get(empty_result_json))
        .route("/livetv/seriestimers", get(empty_result_json))
        .route("/Branding/Configuration", get(branding_configuration))
        .route("/branding/configuration", get(branding_configuration))
        .route("/Branding/Css", get(empty_text))
        .route("/Branding/Css.css", get(empty_text))
        .route("/Branding/Splashscreen", get(empty_text))
        .route("/branding/splashscreen", get(empty_text))
        .route("/Library/VirtualFolders", get(get_virtual_folders))
        .route("/Library/VirtualFolders", post(add_virtual_folder))
        .route("/Library/VirtualFolders", delete(delete_virtual_folder))
        .route(
            "/Library/VirtualFolders/Paths",
            post(add_virtual_folder_path),
        )
        .route(
            "/Library/VirtualFolders/Paths",
            delete(delete_virtual_folder_path),
        )
        .route("/library/virtualfolders", get(get_virtual_folders))
        .route("/library/virtualfolders", post(add_virtual_folder))
        .route("/library/virtualfolders", delete(delete_virtual_folder))
        .route(
            "/library/virtualfolders/paths",
            post(add_virtual_folder_path),
        )
        .route(
            "/library/virtualfolders/paths",
            delete(delete_virtual_folder_path),
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
        .route("/UserViews", get(user_views_result))
        .route("/userviews", get(user_views_result))
        .route("/Items/Counts", get(item_counts))
        .route("/items/counts", get(item_counts))
        .route("/Items", get(items_result))
        .route("/items", get(items_result))
        .route("/Items/Latest", get(latest_items))
        .route("/items/latest", get(latest_items))
        .route("/Items/{item_id}/Ancestors", get(item_ancestors))
        .route("/items/{item_id}/ancestors", get(item_ancestors))
        .route("/Items/{item_id}/Similar", get(empty_items_result))
        .route("/items/{item_id}/similar", get(empty_items_result))
        .route("/Items/{item_id}/PlaybackInfo", get(item_playback_info))
        .route("/items/{item_id}/playbackinfo", get(item_playback_info))
        .route("/Videos/{item_id}/stream", get(direct_stream_item))
        .route("/videos/{item_id}/stream", get(direct_stream_item))
        .route("/Items/{item_id}", get(item_detail))
        .route("/items/{item_id}", get(item_detail))
        .route("/Users/{user_id}/Items/{item_id}", get(user_item_detail))
        .route("/users/{user_id}/items/{item_id}", get(user_item_detail))
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/users/{user_id}/playeditems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route(
            "/users/{user_id}/playeditems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route("/Items/{item_id}/Images/{image_type}", get(placeholder_png))
        .route("/items/{item_id}/images/{image_type}", get(placeholder_png))
        .route("/Users/{user_id}/Images/{image_type}", get(placeholder_png))
        .route("/users/{user_id}/images/{image_type}", get(placeholder_png))
        .route("/Shows/NextUp", get(empty_items_result))
        .route("/shows/nextup", get(empty_items_result))
        .route("/UserItems/Resume", get(resume_items))
        .route("/useritems/resume", get(resume_items))
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
    let users = state.db.users().await?;
    Ok(Json(
        users
            .iter()
            .map(|user| user_to_dto(user, server.server_id))
            .collect(),
    ))
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

async fn ping() -> &'static str {
    "Jellyfin Server"
}

async fn system_storage() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ProgramDataPath": null,
        "WebPath": null,
        "Items": []
    }))
}

async fn activity_log_entries() -> Json<serde_json::Value> {
    Json(empty_result())
}

async fn session_sessions() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn devices() -> Json<serde_json::Value> {
    Json(empty_result())
}

async fn installed_plugins() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn available_packages() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn package_repositories() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn plugin_manifest(Path(plugin_id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Guid": plugin_id,
        "Name": plugin_id,
        "Overview": "Plugin manifests are not supported by Jellyrin yet.",
        "Description": "Plugin compatibility is intentionally disabled in this milestone.",
        "Owner": "Jellyrin",
        "Category": "General",
        "Versions": []
    }))
}

async fn device_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Name": null,
        "Id": null,
        "LastUserName": null,
        "AppName": null,
        "AppVersion": null,
        "LastUserId": null,
        "DateLastActivity": null,
        "Capabilities": null
    }))
}

async fn device_options() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "CustomName": null
    }))
}

async fn system_configuration() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ServerName": "Jellyrin",
        "UICulture": "en-US",
        "MetadataCountryCode": "US",
        "PreferredMetadataLanguage": "en",
        "EnableRemoteAccess": false,
        "EnableUPnP": false,
        "IsStartupWizardCompleted": true,
        "LibraryMonitorDelay": 60,
        "EnableRealtimeMonitor": false,
        "EnableCaseSensitiveItemIds": true,
        "ImageSavingConvention": "Compatible",
        "SkipDeserializationForBasicTypes": false,
        "SkipDeserializationForPrograms": false,
        "SaveMetadataHidden": false,
        "ContentTypes": [],
        "MetadataOptions": [],
        "PathSubstitutions": [],
        "PluginRepositories": [],
        "RemoteClientBitrateLimit": 0,
        "LogFileRetentionDays": 3,
        "RunAtStartup": false
    }))
}

async fn named_configuration(Path(key): Path<String>) -> Json<serde_json::Value> {
    let value = match key.as_str() {
        "branding" | "Branding" => serde_json::json!({
            "LoginDisclaimer": null,
            "CustomCss": null,
            "SplashscreenEnabled": true
        }),
        _ => serde_json::json!({}),
    };
    Json(value)
}

async fn default_metadata_options() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ItemType": null,
        "DisabledMetadataSavers": [],
        "LocalMetadataReaderOrder": [],
        "DisabledMetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "DisabledImageFetchers": [],
        "ImageFetcherOrder": [],
        "DisabledSubtitleFetchers": [],
        "SubtitleFetcherOrder": []
    }))
}

async fn dashboard_configuration_pages() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

fn empty_result() -> serde_json::Value {
    serde_json::json!({
        "Items": [],
        "TotalRecordCount": 0,
        "StartIndex": 0
    })
}

async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn admin_no_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if !user.is_administrator {
        return Err(ApiError::forbidden("Administrator access required"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn empty_object() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

async fn scheduled_tasks() -> Json<Vec<serde_json::Value>> {
    Json(vec![library_scan_task_json()])
}

async fn scheduled_task(Path(task_id): Path<String>) -> Result<Json<serde_json::Value>, ApiError> {
    if task_id == "scan-media-library" || task_id == "RefreshLibrary" {
        return Ok(Json(library_scan_task_json()));
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn start_scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if !user.is_administrator {
        return Err(ApiError::forbidden("Administrator access required"));
    }
    if task_id == "scan-media-library" || task_id == "RefreshLibrary" {
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

fn library_scan_task_json() -> serde_json::Value {
    serde_json::json!({
        "Name": "Scan Media Library",
        "State": "Idle",
        "CurrentProgressPercentage": null,
        "Id": "scan-media-library",
        "LastExecutionResult": null,
        "Triggers": [
            {
                "Type": "IntervalTrigger",
                "TimeOfDayTicks": null,
                "IntervalTicks": 43_200_000_000_i64,
                "DayOfWeek": null,
                "MaxRuntimeTicks": null,
            }
        ],
        "Description": "Scans configured Jellyrin media libraries. Execution is currently handled by explicit library scan calls; this task is exposed for Jellyfin Web compatibility.",
        "Category": "Library",
        "IsHidden": false,
        "Key": "RefreshLibrary",
    })
}

#[derive(Debug, Deserialize)]
struct PlaybackReportBody {
    #[serde(alias = "ItemId")]
    item_id: String,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
    #[serde(alias = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(alias = "IsPaused")]
    is_paused: Option<bool>,
}

async fn report_playback_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, false).await
}

async fn report_playback_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, false).await
}

async fn report_playback_stopped(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, false).await
}

async fn report_playback(
    state: AppState,
    headers: HeaderMap,
    query: AuthQuery,
    payload: PlaybackReportBody,
    played: bool,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &payload.item_id).await?;
    state
        .db
        .upsert_playback_state(
            user.id,
            item.id,
            payload.media_source_id.as_deref(),
            payload.position_ticks.unwrap_or_default(),
            payload.is_paused.unwrap_or(false),
            played,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn quick_connect_enabled() -> Json<bool> {
    Json(false)
}

async fn empty_json_array() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn empty_result_json() -> Json<serde_json::Value> {
    Json(empty_result())
}

async fn live_tv_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Services": [],
        "IsEnabled": false,
        "EnabledUsers": [],
        "TunerHosts": [],
        "ListingsProviders": []
    }))
}

async fn live_tv_guide_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "StartDate": null,
        "EndDate": null
    }))
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

    let folder = state
        .db
        .upsert_virtual_folder(&query.name, query.collection_type.as_deref(), locations)
        .await?;
    state.db.scan_virtual_folder_items(folder.id).await?;
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
    let folder = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.name.eq_ignore_ascii_case(&payload.name))
        .ok_or_else(|| ApiError::bad_request("Virtual folder not found"))?;
    state.db.scan_virtual_folder_items(folder.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct VirtualFolderNameQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
}

async fn delete_virtual_folder(
    State(state): State<AppState>,
    Query(query): Query<VirtualFolderNameQuery>,
) -> Result<StatusCode, ApiError> {
    if state.db.delete_virtual_folder(&query.name).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Virtual folder not found"))
    }
}

#[derive(Debug, Deserialize)]
struct DeleteMediaPathQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
    #[serde(alias = "Path", alias = "path")]
    path: String,
}

async fn delete_virtual_folder_path(
    State(state): State<AppState>,
    Query(query): Query<DeleteMediaPathQuery>,
) -> Result<StatusCode, ApiError> {
    if state
        .db
        .remove_virtual_folder_path(&query.name, &query.path)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Virtual folder path not found"))
    }
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

async fn user_views_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let folders = state.db.virtual_folders().await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = folders
        .iter()
        .map(|folder| user_view_to_json(folder, &server_id))
        .collect::<Vec<_>>();
    Ok(Json(query_result(items)))
}

#[derive(Debug, Deserialize, Default)]
struct ItemsQuery {
    #[serde(alias = "UserId")]
    _user_id: Option<String>,
    #[serde(alias = "ParentId")]
    parent_id: Option<String>,
    #[serde(alias = "IncludeItemTypes")]
    include_item_types: Option<String>,
    #[serde(alias = "Recursive")]
    _recursive: Option<bool>,
    #[serde(alias = "StartIndex")]
    start_index: Option<usize>,
    #[serde(alias = "Limit")]
    limit: Option<usize>,
    #[serde(alias = "SortBy")]
    sort_by: Option<String>,
    #[serde(alias = "SortOrder")]
    sort_order: Option<String>,
    #[serde(alias = "Fields")]
    _fields: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LatestItemsQuery {
    #[serde(alias = "Limit")]
    limit: Option<usize>,
}

async fn items_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let filtered_items = filtered_media_items(state.db.media_items().await?, &query);
    let total_record_count = filtered_items.len();
    let items = paged_media_items(filtered_items, &query)
        .iter()
        .map(|item| media_item_to_json(item, &server_id))
        .collect::<Vec<_>>();
    Ok(Json(query_result_with_total(items, total_record_count)))
}

async fn item_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(item_counts_json(&state.db.media_items().await?)))
}

async fn latest_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<LatestItemsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let limit = query.limit.unwrap_or(20).min(100) as i64;
    Ok(Json(
        state
            .db
            .latest_media_items(limit)
            .await?
            .iter()
            .map(|item| media_item_to_json(item, &server_id))
            .collect(),
    ))
}

async fn resume_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = state
        .db
        .resume_items_for_user(user.id, 20)
        .await?
        .iter()
        .map(|(item, playback)| media_item_to_json_with_playback(item, &server_id, Some(playback)))
        .collect();
    Ok(Json(query_result(items)))
}

async fn item_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    Ok(Json(media_item_to_json(&item, &server_id)))
}

async fn user_item_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, user_id)?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let playback = state.db.playback_state_for_item(user_id, item.id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    Ok(Json(media_item_to_json_with_playback(
        &item,
        &server_id,
        playback.as_ref(),
    )))
}

async fn mark_item_played(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    set_item_played(state, headers, query, user_id, item_id, true).await
}

async fn mark_item_unplayed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    set_item_played(state, headers, query, user_id, item_id, false).await
}

async fn set_item_played(
    state: AppState,
    headers: HeaderMap,
    query: AuthQuery,
    user_id: String,
    item_id: String,
    played: bool,
) -> Result<StatusCode, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, user_id)?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    state
        .db
        .upsert_playback_state(user_id, item.id, None, 0, false, played)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn resolve_user_id(user_id: &str) -> Result<Uuid, ApiError> {
    parse_jellyfin_uuid(user_id).map_err(|_| ApiError::bad_request("Invalid user id"))
}

async fn item_ancestors(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let folder = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.id == item.virtual_folder_id)
        .ok_or_else(|| ApiError::not_found("Parent folder not found"))?;
    Ok(Json(vec![user_view_to_json(&folder, &server_id)]))
}

async fn item_playback_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let item_json = media_item_to_json(&item, &server_id);
    Ok(Json(serde_json::json!({
        "MediaSources": item_json["MediaSources"].clone(),
        "PlaySessionId": null,
        "ErrorCode": null,
    })))
}

async fn direct_stream_item(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<axum::response::Response, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    if item.media_type != "Video" {
        return Err(ApiError::not_found("Video stream not found"));
    }

    let bytes = fs::read(media_item_path(&item))?;
    let total_len = bytes.len();
    let content_type = media_item_content_type(&item);

    if let Some((start, end)) = parse_range_header(&headers, total_len) {
        let body = bytes[start..=end].to_vec();
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_LENGTH, body.len().to_string()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                ),
            ],
            Body::from(body),
        )
            .into_response());
    }

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, total_len.to_string()),
        ],
        Body::from(bytes),
    )
        .into_response())
}

async fn media_item_by_id(db: &Database, item_id: &str) -> Result<MediaItem, ApiError> {
    let requested_id = parse_jellyfin_uuid(item_id)?;
    db.media_items()
        .await?
        .into_iter()
        .find(|item| item.id == requested_id)
        .ok_or_else(|| ApiError::not_found("Item not found"))
}

async fn empty_items_result() -> Json<serde_json::Value> {
    Json(query_result(Vec::new()))
}

fn query_result(items: Vec<serde_json::Value>) -> serde_json::Value {
    let total_record_count = items.len();
    query_result_with_total(items, total_record_count)
}

fn query_result_with_total(
    items: Vec<serde_json::Value>,
    total_record_count: usize,
) -> serde_json::Value {
    serde_json::json!({
        "TotalRecordCount": total_record_count,
        "StartIndex": 0,
        "Items": items,
    })
}

fn filtered_media_items(items: Vec<MediaItem>, query: &ItemsQuery) -> Vec<MediaItem> {
    let parent_id = query
        .parent_id
        .as_deref()
        .and_then(|parent_id| parse_jellyfin_uuid(parent_id).ok());
    let include_types = query.include_item_types.as_deref().map(|types| {
        types
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .collect::<Vec<_>>()
    });

    let mut items = items
        .into_iter()
        .filter(|item| parent_id.is_none_or(|parent_id| item.virtual_folder_id == parent_id))
        .filter(|item| {
            include_types.as_ref().is_none_or(|types| {
                let item_type = media_item_type(item).to_ascii_lowercase();
                types.iter().any(|allowed| allowed == &item_type)
            })
        })
        .collect::<Vec<_>>();

    let sort_by = query.sort_by.as_deref().unwrap_or("SortName");
    if sort_by.eq_ignore_ascii_case("DateCreated") {
        items.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    } else {
        items.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    }
    if query
        .sort_order
        .as_deref()
        .is_some_and(|order| order.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }

    items
}

fn paged_media_items(items: Vec<MediaItem>, query: &ItemsQuery) -> Vec<MediaItem> {
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(usize::MAX);
    items.into_iter().skip(start_index).take(limit).collect()
}

fn parse_jellyfin_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value)
        .or_else(|_| Uuid::parse_str(&hyphenate_uuid(value)))
        .map_err(|_| ApiError::bad_request("Invalid item id"))
}

fn hyphenate_uuid(value: &str) -> String {
    if value.len() == 32 {
        format!(
            "{}-{}-{}-{}-{}",
            &value[0..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..32]
        )
    } else {
        value.to_string()
    }
}

fn user_view_to_json(folder: &VirtualFolder, server_id: &str) -> serde_json::Value {
    serde_json::json!({
        "Name": folder.name,
        "ServerId": server_id,
        "Id": folder.id.simple().to_string(),
        "Etag": null,
        "DateCreated": folder.created_at.to_string(),
        "CanDelete": false,
        "CanDownload": false,
        "SortName": folder.name,
        "ExternalUrls": [],
        "Path": null,
        "EnableMediaSourceDisplay": true,
        "ChannelId": null,
        "Taglines": [],
        "Genres": [],
        "PlayAccess": "Full",
        "RemoteTrailers": [],
        "ProviderIds": {},
        "IsFolder": true,
        "ParentId": null,
        "Type": "CollectionFolder",
        "CollectionType": folder.collection_type,
        "UserData": { "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false },
        "ImageTags": { "Primary": "placeholder" },
        "PrimaryImageAspectRatio": 0.6666667,
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaType": null,
    })
}

fn media_item_to_json(item: &MediaItem, server_id: &str) -> serde_json::Value {
    media_item_to_json_with_playback(item, server_id, None)
}

fn media_item_to_json_with_playback(
    item: &MediaItem,
    server_id: &str,
    playback: Option<&PlaybackState>,
) -> serde_json::Value {
    let item_type = media_item_type(item);
    let item_id = item.id.simple().to_string();
    let container = media_item_container(item);
    let file_name = media_item_file_name(item);
    let file_size = media_item_file_size(item);
    let playback_position_ticks = playback.map_or(0, |state| state.position_ticks);
    let played = playback.is_some_and(|state| state.played);
    let play_count = i32::from(played);

    let media_source = serde_json::json!({
        "Protocol": "File",
        "Id": playback.and_then(|state| state.media_source_id.clone()).unwrap_or_else(|| item_id.clone()),
        "Path": item.path,
        "Type": "Default",
        "Container": container,
        "Size": file_size,
        "Name": item.name,
        "IsRemote": false,
        "ETag": null,
        "RunTimeTicks": null,
        "ReadAtNativeFramerate": false,
        "IgnoreDts": false,
        "IgnoreIndex": false,
        "GenPtsInput": false,
        "SupportsTranscoding": false,
        "SupportsDirectStream": true,
        "SupportsDirectPlay": true,
        "IsInfiniteStream": false,
        "RequiresOpening": false,
        "RequiresClosing": false,
        "RequiresLooping": false,
        "SupportsProbing": true,
        "VideoType": "VideoFile",
        "MediaStreams": [],
        "Formats": [],
        "Bitrate": null,
    });

    serde_json::json!({
        "Name": item.name,
        "OriginalTitle": null,
        "ServerId": server_id,
        "Id": item_id,
        "Etag": null,
        "DateCreated": item.created_at.to_string(),
        "DateLastMediaAdded": item.updated_at.to_string(),
        "PremiereDate": null,
        "ProductionYear": null,
        "CommunityRating": null,
        "CriticRating": null,
        "OfficialRating": null,
        "Overview": "",
        "CanDelete": false,
        "CanDownload": true,
        "SortName": item.name,
        "ForcedSortName": null,
        "ExternalUrls": [],
        "Path": item.path,
        "FileName": file_name,
        "Container": container,
        "Size": file_size,
        "EnableMediaSourceDisplay": true,
        "ChannelId": null,
        "Taglines": [],
        "Genres": [],
        "GenreItems": [],
        "Studios": [],
        "People": [],
        "Tags": [],
        "TagItems": [],
        "Chapters": [],
        "MediaStreams": [],
        "PlayAccess": "Full",
        "RemoteTrailers": [],
        "ProviderIds": {},
        "IsFolder": false,
        "ParentId": item.virtual_folder_id.simple().to_string(),
        "Type": item_type,
        "MediaType": item.media_type,
        "RunTimeTicks": null,
        "UserData": { "PlaybackPositionTicks": playback_position_ticks, "PlayCount": play_count, "IsFavorite": false, "Played": played, "Key": item_id },
        "ImageTags": { "Primary": "placeholder" },
        "PrimaryImageAspectRatio": 0.6666667,
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaSources": [media_source],
    })
}

fn item_counts_json(items: &[MediaItem]) -> serde_json::Value {
    let mut movie_count = 0;
    let mut series_count = 0;
    let mut episode_count = 0;
    let mut song_count = 0;
    let mut album_count = 0;
    let mut book_count = 0;
    let mut music_video_count = 0;

    for item in items {
        match media_item_type(item) {
            "Movie" => movie_count += 1,
            "Series" => series_count += 1,
            "Episode" => episode_count += 1,
            "Audio" => song_count += 1,
            "MusicAlbum" => album_count += 1,
            "Book" => book_count += 1,
            "MusicVideo" => music_video_count += 1,
            _ => {}
        }
    }

    serde_json::json!({
        "MovieCount": movie_count,
        "SeriesCount": series_count,
        "EpisodeCount": episode_count,
        "ArtistCount": 0,
        "ProgramCount": 0,
        "TrailerCount": 0,
        "SongCount": song_count,
        "AlbumCount": album_count,
        "MusicVideoCount": music_video_count,
        "BoxSetCount": 0,
        "BookCount": book_count,
        "ItemCount": items.len(),
    })
}

fn media_item_path(item: &MediaItem) -> &FsPath {
    FsPath::new(&item.path)
}

fn media_item_container(item: &MediaItem) -> Option<String> {
    media_item_path(item)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn media_item_file_name(item: &MediaItem) -> Option<String> {
    media_item_path(item)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .map(ToOwned::to_owned)
}

fn media_item_file_size(item: &MediaItem) -> Option<u64> {
    fs::metadata(media_item_path(item))
        .ok()
        .map(|metadata| metadata.len())
}

fn media_item_content_type(item: &MediaItem) -> String {
    match media_item_container(item).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("avi") => "video/x-msvideo",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn parse_range_header(headers: &HeaderMap, total_len: usize) -> Option<(usize, usize)> {
    if total_len == 0 {
        return None;
    }

    let range = headers.get(header::RANGE)?.to_str().ok()?;
    let range = range.strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = if end.is_empty() {
        total_len - 1
    } else {
        end.parse::<usize>().ok()?.min(total_len - 1)
    };

    (start <= end && start < total_len).then_some((start, end))
}

fn media_item_type(item: &MediaItem) -> &'static str {
    match (item.media_type.as_str(), item.collection_type.as_deref()) {
        ("Video", Some("movies")) => "Movie",
        ("Video", _) => "Video",
        ("Audio", _) => "Audio",
        ("Photo", _) => "Photo",
        ("Book", _) => "Book",
        _ => "BaseItem",
    }
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

async fn require_request_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<User, ApiError> {
    require_user(db, headers, query_token)
        .await
        .map(|(user, _)| user)
}

fn ensure_user_access(auth_user: &User, requested_user_id: Uuid) -> Result<(), ApiError> {
    if auth_user.id == requested_user_id || auth_user.is_administrator {
        Ok(())
    } else {
        Err(ApiError::forbidden("User access denied"))
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

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
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
    async fn scheduled_tasks_endpoints_expose_library_scan_stub() {
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
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ScheduledTasks?IsEnabled=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let tasks: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        assert_eq!(tasks[0]["Id"], "scan-media-library");
        assert_eq!(tasks[0]["Key"], "RefreshLibrary");
        assert_eq!(tasks[0]["Name"], "Scan Media Library");
        assert_eq!(tasks[0]["State"], "Idle");
        assert_eq!(tasks[0]["IsHidden"], false);
        assert_eq!(tasks[0]["Triggers"].as_array().unwrap().len(), 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ScheduledTasks/scan-media-library")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let task: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(task["Id"], "scan-media-library");

        for endpoint in [
            "/ScheduledTasks/Running/scan-media-library",
            "/scheduledtasks/running/scan-media-library",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }
    }

    #[tokio::test]
    async fn admin_dashboard_shell_stubs_avoid_404s() {
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

        for endpoint in [
            "/System/Ping",
            "/System/Info/Storage",
            "/System/ActivityLog/Entries",
            "/System/Configuration",
            "/System/Configuration/MetadataOptions/Default",
            "/System/Configuration/branding",
            "/Dashboard/web/ConfigurationPages",
            "/Dashboard/web/ConfigurationPage?name=home.html",
            "/Devices",
            "/Devices/Info?Id=test-device",
            "/Devices/Options?Id=test-device",
            "/Session/Sessions",
            "/Sessions",
            "/Plugins",
            "/plugins",
            "/Plugins/test-plugin/Configuration",
            "/Plugins/test-plugin/Manifest",
            "/Packages",
            "/packages",
            "/Repositories",
            "/repositories",
            "/LiveTv/Info",
            "/livetv/info",
            "/LiveTv/GuideInfo?UserId=test-user",
            "/LiveTv/Channels?UserId=test-user",
            "/LiveTv/Programs?UserId=test-user",
            "/LiveTv/RecommendedPrograms?UserId=test-user",
            "/LiveTv/Recordings?UserId=test-user",
            "/LiveTv/RecordingGroups?UserId=test-user",
            "/LiveTv/Timers",
            "/LiveTv/SeriesTimers",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["ServerName"], "Jellyrin");
        assert_eq!(config["EnableRemoteAccess"], false);
        assert_eq!(config["ContentTypes"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries?StartIndex=0&Limit=20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let activity: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(activity["TotalRecordCount"], 0);
        assert_eq!(activity["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Plugins")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let plugins: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(plugins.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Plugins/test-plugin/Manifest")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let manifest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(manifest["Guid"], "test-plugin");
        assert_eq!(manifest["Versions"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Repositories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let repositories: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(repositories.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/Info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let live_tv_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live_tv_info["IsEnabled"], false);
        assert_eq!(live_tv_info["Services"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/Channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let live_tv_channels: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live_tv_channels["TotalRecordCount"], 0);
        assert_eq!(live_tv_channels["Items"].as_array().unwrap().len(), 0);

        for endpoint in [
            "/System/Configuration",
            "/System/Configuration/Branding",
            "/System/Configuration/metadata",
            "/Devices/Options",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
                    .uri("/Library/VirtualFolders/Paths")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "Movies", "Path": "/media/more-movies" }).to_string(),
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
                    .method(Method::DELETE)
                    .uri("/Library/VirtualFolders/Paths?Name=Movies&Path=/media/more-movies")
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
                    .method(Method::DELETE)
                    .uri("/Library/VirtualFolders?Name=Movies")
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
                    .uri("/library/virtualfolders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders: Value = serde_json::from_slice(&body).unwrap();
        assert!(folders.as_array().unwrap().is_empty());

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
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/SyncPlay/List")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let syncplay: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(syncplay.as_array().unwrap().len(), 0);

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

    #[tokio::test]
    async fn virtual_folder_scan_populates_items_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Example Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let user_id = user.id.simple().to_string();
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
                    .uri(format!(
                        "/Library/VirtualFolders?name=Movies&collectionType=movies&paths={}",
                        tmp.path().to_string_lossy()
                    ))
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
                    .uri("/Items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["Items"][0]["Name"], "Example Movie");
        assert_eq!(result["Items"][0]["Type"], "Movie");
        assert_eq!(result["Items"][0]["Path"], movie.to_string_lossy().as_ref());
        let item_id = result["Items"][0]["Id"].as_str().unwrap();
        let parent_id = result["Items"][0]["ParentId"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Counts")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["ItemCount"], 1);
        assert_eq!(counts["SongCount"], 0);
        assert_eq!(counts["BookCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/items?ParentId={parent_id}&IncludeItemTypes=Movie&StartIndex=0&Limit=1&SortBy=SortName"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered["TotalRecordCount"], 1);
        assert_eq!(filtered["Items"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["Items"][0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let detail: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(detail["Id"], item_id);
        assert_eq!(detail["Name"], "Example Movie");
        assert_eq!(detail["Container"], "mp4");
        assert_eq!(detail["Size"], 10);
        assert_eq!(detail["FileName"], "Example Movie.mp4");
        assert_eq!(detail["ImageTags"]["Primary"], "placeholder");
        assert_eq!(detail["PrimaryImageAspectRatio"], 0.6666667);
        assert_eq!(detail["MediaSources"][0]["Id"], item_id);
        assert_eq!(
            detail["MediaSources"][0]["Path"],
            movie.to_string_lossy().as_ref()
        );
        assert_eq!(detail["MediaSources"][0]["Container"], "mp4");
        assert_eq!(detail["MediaSources"][0]["Size"], 10);
        assert_eq!(detail["People"].as_array().unwrap().len(), 0);
        assert_eq!(detail["Studios"].as_array().unwrap().len(), 0);
        assert_eq!(detail["GenreItems"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_detail: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_detail["Id"], item_id);
        assert_eq!(user_detail["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Ancestors"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ancestors: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(ancestors.as_array().unwrap().len(), 1);
        assert_eq!(ancestors[0]["Id"], parent_id);
        assert_eq!(ancestors[0]["Type"], "CollectionFolder");
        assert_eq!(ancestors[0]["ImageTags"]["Primary"], "placeholder");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(playback_info["ErrorCode"], Value::Null);
        assert_eq!(playback_info["MediaSources"][0]["SupportsDirectPlay"], true);
        assert_eq!(
            playback_info["MediaSources"][0]["SupportsTranscoding"],
            false
        );

        for (endpoint, position_ticks) in [
            ("/Sessions/Playing", 0_i64),
            ("/Sessions/Playing/Progress", 50_000_000_i64),
            ("/Sessions/Playing/Stopped", 50_000_000_i64),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({
                                "ItemId": item_id,
                                "MediaSourceId": item_id,
                                "PositionTicks": position_ticks,
                                "CanSeek": true,
                                "IsPaused": false,
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/UserItems/Resume")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resume: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resume["TotalRecordCount"], 1);
        assert_eq!(resume["Items"][0]["Id"], item_id);
        assert_eq!(
            resume["Items"][0]["UserData"]["PlaybackPositionTicks"],
            50_000_000
        );
        assert_eq!(resume["Items"][0]["UserData"]["Played"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/{user_id}/PlayedItems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
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
                    .uri(format!("/Users/{user_id}/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_item_after_played: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_item_after_played["UserData"]["Played"], true);
        assert_eq!(user_item_after_played["UserData"]["PlayCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/UserItems/Resume")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resume_after_played: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resume_after_played["TotalRecordCount"], 0);
        assert_eq!(resume_after_played["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/users/{user_id}/playeditems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
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
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 0-3/10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"fake");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Similar"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let similar: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(similar["TotalRecordCount"], 0);
        assert_eq!(similar["Items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn public_users_lists_all_configured_users() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.update_first_user("admin".to_string(), "admin-secret")
            .await
            .unwrap();
        db.upsert_admin_user("jellyrin-e2e-admin", "e2e-secret")
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
                    .uri("/Users/Public")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let users: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(users.as_array().unwrap().len(), 2);
        assert!(
            users
                .as_array()
                .unwrap()
                .iter()
                .any(|user| user["Name"] == "jellyrin-e2e-admin")
        );
    }
}
