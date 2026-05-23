#![recursion_limit = "256"]

use std::{
    cmp::Ordering,
    collections::HashMap,
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
    routing::{delete, get, head, post},
};
use jellyrin_compat::{
    AuthenticateUserByNameDto, AuthenticationResultDto, CountryDto, CultureDto, HealthResponse,
    LocalizationOptionDto, PublicSystemInfo, SessionInfoDto, StartupConfigurationDto,
    StartupRemoteAccessDto, StartupUserDto, UserDto, UserPolicyDto,
};
use jellyrin_core::{DeviceToken, MediaItem, PlaybackState, StartupConfig, User, VirtualFolder};
use jellyrin_db::{ActivePlaybackSession, Database, DeviceSession, TaskRun};
use serde::Deserialize;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
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
        .route("/System/Configuration", post(update_system_configuration))
        .route("/system/configuration", post(update_system_configuration))
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
        .route("/Devices", delete(delete_device))
        .route("/devices", delete(delete_device))
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
            post(start_scheduled_task).delete(stop_scheduled_task),
        )
        .route(
            "/scheduledtasks/running/{task_id}",
            post(start_scheduled_task).delete(stop_scheduled_task),
        )
        .route(
            "/ScheduledTasks/{task_id}/Triggers",
            post(update_scheduled_task_triggers),
        )
        .route(
            "/scheduledtasks/{task_id}/triggers",
            post(update_scheduled_task_triggers),
        )
        .route("/Library/Refresh", post(refresh_library))
        .route("/library/refresh", post(refresh_library))
        .route("/Library/MediaFolders", get(user_views_result))
        .route("/library/mediafolders", get(user_views_result))
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
        .route("/Users/{user_id}/Items", get(user_items_result))
        .route("/users/{user_id}/items", get(user_items_result))
        .route("/Users/{user_id}/Items/Counts", get(user_item_counts))
        .route("/users/{user_id}/items/counts", get(user_item_counts))
        .route("/Users/{user_id}/Items/Latest", get(user_latest_items))
        .route("/users/{user_id}/items/latest", get(user_latest_items))
        .route("/Users/{user_id}/Items/Resume", get(user_resume_items))
        .route("/users/{user_id}/items/resume", get(user_resume_items))
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
        .route("/Items/Filters", get(item_filters))
        .route("/items/filters", get(item_filters))
        .route("/Items/{item_id}/Ancestors", get(item_ancestors))
        .route("/items/{item_id}/ancestors", get(item_ancestors))
        .route(
            "/Items/{item_id}/Similar",
            get(authenticated_item_empty_items),
        )
        .route(
            "/items/{item_id}/similar",
            get(authenticated_item_empty_items),
        )
        .route(
            "/Items/{item_id}/Images",
            get(authenticated_item_empty_json_array),
        )
        .route(
            "/items/{item_id}/images",
            get(authenticated_item_empty_json_array),
        )
        .route(
            "/Items/{item_id}/ThemeMedia",
            get(authenticated_item_theme_media),
        )
        .route(
            "/items/{item_id}/thememedia",
            get(authenticated_item_theme_media),
        )
        .route(
            "/Items/{item_id}/ThemeSongs",
            get(authenticated_item_theme_items),
        )
        .route(
            "/items/{item_id}/themesongs",
            get(authenticated_item_theme_items),
        )
        .route(
            "/Items/{item_id}/ThemeVideos",
            get(authenticated_item_theme_items),
        )
        .route(
            "/items/{item_id}/themevideos",
            get(authenticated_item_theme_items),
        )
        .route("/Items/{item_id}/PlaybackInfo", get(item_playback_info))
        .route("/items/{item_id}/playbackinfo", get(item_playback_info))
        .route(
            "/Items/{item_id}/PlaybackInfo",
            post(post_item_playback_info),
        )
        .route(
            "/items/{item_id}/playbackinfo",
            post(post_item_playback_info),
        )
        .route("/Videos/{item_id}/stream", get(direct_stream_item))
        .route("/videos/{item_id}/stream", get(direct_stream_item))
        .route("/Videos/{item_id}/stream", head(direct_stream_item_head))
        .route("/videos/{item_id}/stream", head(direct_stream_item_head))
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
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(item_placeholder_image),
        )
        .route(
            "/items/{item_id}/images/{image_type}",
            get(item_placeholder_image),
        )
        .route(
            "/Users/{user_id}/Images/{image_type}",
            get(user_placeholder_image),
        )
        .route(
            "/users/{user_id}/images/{image_type}",
            get(user_placeholder_image),
        )
        .route("/Shows/NextUp", get(authenticated_empty_items))
        .route("/shows/nextup", get(authenticated_empty_items))
        .route("/Shows/Upcoming", get(authenticated_empty_items))
        .route("/shows/upcoming", get(authenticated_empty_items))
        .route(
            "/Movies/Recommendations",
            get(authenticated_empty_json_array),
        )
        .route(
            "/movies/recommendations",
            get(authenticated_empty_json_array),
        )
        .route("/Genres", get(authenticated_empty_items))
        .route("/genres", get(authenticated_empty_items))
        .route("/Persons", get(authenticated_empty_items))
        .route("/persons", get(authenticated_empty_items))
        .route("/Studios", get(authenticated_empty_items))
        .route("/studios", get(authenticated_empty_items))
        .route("/Years", get(authenticated_empty_items))
        .route("/years", get(authenticated_empty_items))
        .route("/Artists", get(authenticated_empty_items))
        .route("/artists", get(authenticated_empty_items))
        .route("/AlbumArtists", get(authenticated_empty_items))
        .route("/albumartists", get(authenticated_empty_items))
        .route("/Albums", get(authenticated_empty_items))
        .route("/albums", get(authenticated_empty_items))
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

async fn session_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let sessions = if user.is_administrator {
        state.db.device_sessions().await?
    } else {
        state.db.device_sessions_for_user(user.id).await?
    };
    let active_playback = state
        .db
        .active_playback_sessions()
        .await?
        .into_iter()
        .map(|session| (session.session_id.clone(), session))
        .collect::<HashMap<_, _>>();
    let server_id = state.db.server_state().await?.server_id.to_string();
    Ok(Json(
        sessions
            .iter()
            .map(|session| {
                session_to_json(
                    session,
                    active_playback.get(&session.access_token),
                    &server_id,
                )
            })
            .collect::<Vec<serde_json::Value>>(),
    ))
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

#[derive(Debug, Deserialize)]
struct DeleteDeviceQuery {
    #[serde(alias = "Id", alias = "id")]
    id: Option<String>,
}

async fn delete_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeleteDeviceQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    if let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        state.db.revoke_device(id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn system_configuration(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(system_configuration_json(
        state.db.startup_config().await?,
        state.db.server_state().await?.startup_wizard_completed,
    )))
}

#[derive(Debug, Deserialize)]
struct SystemConfigurationUpdate {
    #[serde(alias = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "UICulture", alias = "uiCulture")]
    ui_culture: Option<String>,
    #[serde(alias = "MetadataCountryCode")]
    metadata_country_code: Option<String>,
    #[serde(alias = "PreferredMetadataLanguage")]
    preferred_metadata_language: Option<String>,
    #[serde(alias = "EnableRemoteAccess")]
    enable_remote_access: Option<bool>,
}

async fn update_system_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<SystemConfigurationUpdate>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let current = state.db.startup_config().await?;
    state
        .db
        .update_startup_config(StartupConfig {
            server_name: non_empty_or_current(payload.server_name, current.server_name),
            ui_culture: non_empty_or_current(payload.ui_culture, current.ui_culture),
            metadata_country_code: non_empty_or_current(
                payload.metadata_country_code,
                current.metadata_country_code,
            ),
            preferred_metadata_language: non_empty_or_current(
                payload.preferred_metadata_language,
                current.preferred_metadata_language,
            ),
            enable_remote_access: payload
                .enable_remote_access
                .unwrap_or(current.enable_remote_access),
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn non_empty_or_current(update: Option<String>, current: String) -> String {
    update
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or(current)
}

fn system_configuration_json(
    config: StartupConfig,
    startup_wizard_completed: bool,
) -> serde_json::Value {
    serde_json::json!({
        "ServerName": config.server_name,
        "UICulture": config.ui_culture,
        "MetadataCountryCode": config.metadata_country_code,
        "PreferredMetadataLanguage": config.preferred_metadata_language,
        "EnableRemoteAccess": config.enable_remote_access,
        "EnableUPnP": false,
        "IsStartupWizardCompleted": startup_wizard_completed,
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
    })
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

const LIBRARY_SCAN_TASK_ID: &str = "scan-media-library";
const LIBRARY_SCAN_TASK_KEY: &str = "RefreshLibrary";
const STALE_TASK_HOURS: i64 = 24;

async fn scheduled_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    recover_stale_library_scan_runs(&state.db).await?;
    Ok(Json(vec![library_scan_task_json(&state.db).await?]))
}

async fn scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    recover_stale_library_scan_runs(&state.db).await?;
    if is_library_scan_task(&task_id) {
        return Ok(Json(library_scan_task_json(&state.db).await?));
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn start_scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        recover_stale_library_scan_runs(&state.db).await?;
        let run = match state.db.start_task_run(LIBRARY_SCAN_TASK_KEY).await {
            Ok(run) => run,
            Err(error) if format!("{error:#}").contains("task is already running") => {
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(error)
                if state
                    .db
                    .current_task_run(LIBRARY_SCAN_TASK_KEY)
                    .await?
                    .is_some() =>
            {
                let _ = error;
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(error) => return Err(error.into()),
        };
        let db = state.db.clone();
        tokio::spawn(async move {
            match scan_all_library_items(&db).await {
                Ok(scanned_count) => {
                    let _ = db
                        .complete_task_run(
                            run.id,
                            serde_json::json!({
                                "ItemsScanned": scanned_count,
                            }),
                        )
                        .await;
                }
                Err(error) => {
                    let message = format!("{error:?}");
                    let _ = db.fail_task_run(run.id, &message).await;
                }
            }
        });
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn stop_scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        state
            .db
            .fail_current_task_run(LIBRARY_SCAN_TASK_KEY, "Task run cancelled.")
            .await?;
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn update_scheduled_task_triggers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
    Json(_triggers): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn recover_stale_library_scan_runs(db: &Database) -> Result<(), ApiError> {
    db.fail_stale_task_runs(
        LIBRARY_SCAN_TASK_KEY,
        Duration::hours(STALE_TASK_HOURS),
        "Task run expired before completion.",
    )
    .await?;
    Ok(())
}

async fn refresh_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if !user.is_administrator {
        return Err(ApiError::forbidden("Administrator access required"));
    }
    scan_all_library_items(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn scan_all_library_items(db: &Database) -> Result<usize, ApiError> {
    let folders = db.virtual_folders().await?;
    let mut scanned = 0usize;
    for folder in folders {
        scanned += db.scan_virtual_folder_items(folder.id).await?;
    }
    Ok(scanned)
}

async fn library_scan_task_json(db: &Database) -> Result<serde_json::Value, ApiError> {
    let current_run = db.current_task_run(LIBRARY_SCAN_TASK_KEY).await?;
    let last_result = db.last_task_result(LIBRARY_SCAN_TASK_KEY).await?;
    let state = if current_run.is_some() {
        "Running"
    } else {
        "Idle"
    };

    Ok(serde_json::json!({
        "Name": "Scan Media Library",
        "State": state,
        "CurrentProgressPercentage": null,
        "Id": LIBRARY_SCAN_TASK_ID,
        "LastExecutionResult": last_result.as_ref().map(task_run_result_json),
        "Triggers": [
            {
                "Type": "IntervalTrigger",
                "TimeOfDayTicks": null,
                "IntervalTicks": 43_200_000_000_i64,
                "DayOfWeek": null,
                "MaxRuntimeTicks": null,
            }
        ],
        "Description": "Scans configured Jellyrin media libraries.",
        "Category": "Library",
        "IsHidden": false,
        "Key": LIBRARY_SCAN_TASK_KEY,
    }))
}

fn is_library_scan_task(task_id: &str) -> bool {
    task_id == LIBRARY_SCAN_TASK_ID || task_id == LIBRARY_SCAN_TASK_KEY
}

fn task_run_result_json(run: &TaskRun) -> serde_json::Value {
    let status = match run.status.as_str() {
        "completed" => "Completed",
        "failed" => "Failed",
        _ => "Running",
    };

    serde_json::json!({
        "Name": "Scan Media Library",
        "Key": run.task_key.clone(),
        "Id": run.id.to_string(),
        "Status": status,
        "StartTimeUtc": format_time_for_json(run.started_at),
        "EndTimeUtc": run.completed_at.map(format_time_for_json),
        "ErrorMessage": run.error_message.clone(),
        "Result": run.result_json.clone(),
    })
}

fn format_time_for_json(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug, Deserialize)]
struct PlaybackReportBody {
    #[serde(alias = "ItemId")]
    item_id: String,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
    #[serde(alias = "PlaySessionId")]
    _play_session_id: Option<String>,
    #[serde(alias = "PlayMethod")]
    _play_method: Option<String>,
    #[serde(alias = "CanSeek")]
    _can_seek: Option<bool>,
    #[serde(alias = "AudioStreamIndex")]
    _audio_stream_index: Option<i32>,
    #[serde(alias = "SubtitleStreamIndex")]
    _subtitle_stream_index: Option<i32>,
    #[serde(alias = "PlaylistItemId")]
    _playlist_item_id: Option<String>,
    #[serde(alias = "SessionId")]
    _session_id: Option<String>,
    #[serde(alias = "VolumeLevel")]
    _volume_level: Option<i32>,
    #[serde(alias = "IsMuted")]
    _is_muted: Option<bool>,
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
    report_playback(state, headers, query, payload, true).await
}

async fn report_playback_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, true).await
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
    playback_active: bool,
) -> Result<StatusCode, ApiError> {
    let (user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &payload.item_id).await?;
    let position_ticks = payload.position_ticks.unwrap_or_default();
    let is_paused = payload.is_paused.unwrap_or(false);
    state
        .db
        .upsert_playback_state(
            user.id,
            item.id,
            payload.media_source_id.as_deref(),
            position_ticks,
            is_paused,
            false,
        )
        .await?;
    if playback_active {
        state
            .db
            .upsert_active_playback_session(
                &token.access_token,
                user.id,
                item.id,
                payload.media_source_id.as_deref(),
                position_ticks,
                is_paused,
            )
            .await?;
    } else {
        state
            .db
            .clear_active_playback_session(&token.access_token)
            .await?;
    }
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
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
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
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<AddVirtualFolderQuery>,
    body: Option<Json<AddVirtualFolderBody>>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
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
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<AddMediaPathBody>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
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
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<VirtualFolderNameQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
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
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeleteMediaPathQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
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

async fn environment_drives(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![
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
    ]))
}

#[derive(Debug, Deserialize)]
struct DirectoryQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
    #[serde(alias = "IncludeFiles")]
    include_files: Option<bool>,
}

async fn environment_directory_contents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
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

    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_parent_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let parent = query
        .path
        .as_deref()
        .and_then(|path| path.parent())
        .map(|path| path.to_string_lossy().into_owned());
    Ok(Json(serde_json::json!({ "Path": parent })))
}

#[derive(Debug, Deserialize)]
struct ValidatePathRequest {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_validate_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<ValidatePathRequest>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    if payload.path.as_deref().is_some_and(|path| path.exists()) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Ok(StatusCode::BAD_REQUEST)
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
    user_id: Option<String>,
    #[serde(alias = "Ids")]
    ids: Option<String>,
    #[serde(alias = "ParentId")]
    parent_id: Option<String>,
    #[serde(alias = "IncludeItemTypes")]
    include_item_types: Option<String>,
    #[serde(alias = "ExcludeItemTypes")]
    exclude_item_types: Option<String>,
    #[serde(alias = "MediaTypes")]
    media_types: Option<String>,
    #[serde(alias = "SearchTerm")]
    search_term: Option<String>,
    #[serde(alias = "IsPlayed")]
    is_played: Option<bool>,
    #[serde(alias = "IsFolder")]
    is_folder: Option<bool>,
    #[serde(alias = "Filters")]
    filters: Option<String>,
    #[serde(alias = "NameStartsWith")]
    name_starts_with: Option<String>,
    #[serde(alias = "NameStartsWithOrGreater")]
    name_starts_with_or_greater: Option<String>,
    #[serde(alias = "NameLessThan")]
    name_less_than: Option<String>,
    #[serde(alias = "Recursive")]
    _recursive: Option<String>,
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
    #[serde(alias = "ImageTypeLimit")]
    _image_type_limit: Option<String>,
    #[serde(alias = "EnableImages")]
    _enable_images: Option<String>,
    #[serde(alias = "EnableUserData")]
    _enable_user_data: Option<String>,
    #[serde(alias = "CollapseBoxSetItems")]
    _collapse_box_set_items: Option<String>,
    #[serde(alias = "ImageTypes")]
    _image_types: Option<String>,
    #[serde(alias = "EnableImageTypes")]
    _enable_image_types: Option<String>,
    #[serde(alias = "EnableTotalRecordCount")]
    _enable_total_record_count: Option<String>,
    #[serde(alias = "ExcludeLocationTypes")]
    _exclude_location_types: Option<String>,
}

async fn items_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let server_id = state.db.server_state().await?.server_id.to_string();
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    let total_record_count = filtered_items.len();
    let items = items_to_json(
        &state.db,
        paged_media_items(filtered_items, &query),
        &server_id,
        requested_user_id,
    )
    .await?;
    Ok(Json(query_result_with_total(
        items,
        total_record_count,
        query.start_index.unwrap_or(0),
    )))
}

async fn user_items_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    let total_record_count = filtered_items.len();
    let items = items_to_json(
        &state.db,
        paged_media_items(filtered_items, &query),
        &server_id,
        Some(requested_user_id),
    )
    .await?;
    Ok(Json(query_result_with_total(
        items,
        total_record_count,
        query.start_index.unwrap_or(0),
    )))
}

async fn item_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    Ok(Json(item_counts_json(&filtered_items)))
}

async fn user_item_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    Ok(Json(item_counts_json(&filtered_items)))
}

async fn latest_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let server_id = state.db.server_state().await?.server_id.to_string();
    let mut filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    filtered_items.sort_by(|left, right| {
        compare_media_items(
            right,
            left,
            &[SortField::DateLastMediaAdded, SortField::SortName],
        )
    });
    let limit = query.limit.unwrap_or(20).min(100);
    Ok(Json(
        items_to_json(
            &state.db,
            filtered_items.into_iter().take(limit).collect(),
            &server_id,
            requested_user_id,
        )
        .await?,
    ))
}

async fn user_latest_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let mut filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    filtered_items.sort_by(|left, right| {
        compare_media_items(
            right,
            left,
            &[SortField::DateLastMediaAdded, SortField::SortName],
        )
    });
    let limit = query.limit.unwrap_or(20).min(100);
    Ok(Json(
        items_to_json(
            &state.db,
            filtered_items.into_iter().take(limit).collect(),
            &server_id,
            Some(requested_user_id),
        )
        .await?,
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

async fn user_resume_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = state
        .db
        .resume_items_for_user(requested_user_id, 20)
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
    let requested_id = parse_jellyfin_uuid(&item_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    if let Some(folder) = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.id == requested_id)
    {
        return Ok(Json(user_view_to_json(&folder, &server_id)));
    }

    let item = state
        .db
        .media_items()
        .await?
        .into_iter()
        .find(|item| item.id == requested_id)
        .ok_or_else(|| ApiError::not_found("Item not found"))?;
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
    playback_info_response(&state.db, &item_id).await
}

async fn post_item_playback_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    drop(body);
    playback_info_response(&state.db, &item_id).await
}

async fn playback_info_response(
    db: &Database,
    item_id: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let item = media_item_by_id(db, item_id).await?;
    let server_id = db.server_state().await?.server_id.to_string();
    let item_json = media_item_to_json(&item, &server_id);
    Ok(Json(serde_json::json!({
        "MediaSources": item_json["MediaSources"].clone(),
        "PlaySessionId": Uuid::new_v4().simple().to_string(),
        "ErrorCode": null,
    })))
}

async fn direct_stream_item(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    if item.media_type != "Video" {
        return Err(ApiError::not_found("Video stream not found"));
    }

    stream_media_item(item, &headers, true).await
}

async fn direct_stream_item_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    if item.media_type != "Video" {
        return Err(ApiError::not_found("Video stream not found"));
    }

    stream_media_item(item, &headers, false).await
}

async fn stream_media_item(
    item: MediaItem,
    headers: &HeaderMap,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let mut file = tokio::fs::File::open(media_item_path(&item)).await?;
    let total_len = file.metadata().await?.len();
    let content_type = media_item_content_type(&item);

    let range = match parse_range_header(headers, total_len) {
        Ok(range) => range,
        Err(()) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_RANGE, format!("bytes */{total_len}")),
                ],
                Body::empty(),
            )
                .into_response());
        }
    };

    if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let content_length = end - start + 1;
        let stream = ReaderStream::new(file.take(content_length));
        let body = if include_body {
            Body::from_stream(stream)
        } else {
            Body::empty()
        };
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_LENGTH, content_length.to_string()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                ),
            ],
            body,
        )
            .into_response());
    }

    let stream = ReaderStream::new(file);
    let body = if include_body {
        Body::from_stream(stream)
    } else {
        Body::empty()
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, total_len.to_string()),
        ],
        body,
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

async fn authenticated_empty_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(empty_items_result().await)
}

async fn authenticated_item_empty_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_items_result().await)
}

async fn authenticated_empty_json_array(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(empty_json_array().await)
}

async fn authenticated_item_empty_json_array(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_json_array().await)
}

async fn authenticated_item_theme_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(Json(serde_json::json!({
        "ThemeVideosResult": query_result(Vec::new()),
        "ThemeSongsResult": query_result(Vec::new()),
        "SoundtrackSongsResult": query_result(Vec::new())
    })))
}

async fn authenticated_item_theme_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_items_result().await)
}

async fn item_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "Genres": [],
        "Tags": [],
        "OfficialRatings": [],
        "Years": [],
        "Containers": [],
        "MediaTypes": [],
        "VideoTypes": [],
        "SeriesStatuses": [],
        "Staff": [],
        "Artists": [],
        "Albums": [],
        "Studios": [],
        "Trailers": [],
        "Features": []
    })))
}

fn query_result(items: Vec<serde_json::Value>) -> serde_json::Value {
    let total_record_count = items.len();
    query_result_with_total(items, total_record_count, 0)
}

fn query_result_with_total(
    items: Vec<serde_json::Value>,
    total_record_count: usize,
    start_index: usize,
) -> serde_json::Value {
    serde_json::json!({
        "TotalRecordCount": total_record_count,
        "StartIndex": start_index,
        "Items": items,
    })
}

async fn filtered_media_items(
    items: Vec<MediaItem>,
    query: &ItemsQuery,
    user_id: Option<Uuid>,
    db: &Database,
) -> Result<Vec<MediaItem>, ApiError> {
    let parent_id = query
        .parent_id
        .as_deref()
        .map(parse_jellyfin_uuid)
        .transpose()?;
    let ids = query.ids.as_deref().map(parse_uuid_list).transpose()?;
    let include_types = csv_lowercase(query.include_item_types.as_deref());
    let exclude_types = csv_lowercase(query.exclude_item_types.as_deref());
    let media_types = csv_lowercase(query.media_types.as_deref());
    let filters = csv_lowercase(query.filters.as_deref());
    let search_term = query
        .search_term
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let name_starts_with = normalized_prefix(query.name_starts_with.as_deref());
    let name_starts_with_or_greater =
        normalized_prefix(query.name_starts_with_or_greater.as_deref());
    let name_less_than = normalized_prefix(query.name_less_than.as_deref());
    let is_folder_filter = query
        .is_folder
        .or_else(|| filters.as_deref().and_then(folder_filter_value));
    let is_played_filter = query
        .is_played
        .or_else(|| filters.as_deref().and_then(played_filter_value));

    let mut items = items
        .into_iter()
        .filter(|item| ids.as_ref().is_none_or(|ids| ids.contains(&item.id)))
        .filter(|item| parent_id.is_none_or(|parent_id| item.virtual_folder_id == parent_id))
        .filter(|item| {
            include_types.as_ref().is_none_or(|types| {
                let item_type = media_item_type(item).to_ascii_lowercase();
                types.iter().any(|allowed| allowed == &item_type)
            })
        })
        .filter(|item| {
            exclude_types.as_ref().is_none_or(|types| {
                let item_type = media_item_type(item).to_ascii_lowercase();
                !types.iter().any(|excluded| excluded == &item_type)
            })
        })
        .filter(|item| {
            media_types.as_ref().is_none_or(|types| {
                let media_type = item.media_type.to_ascii_lowercase();
                types.iter().any(|allowed| allowed == &media_type)
            })
        })
        .filter(|_| is_folder_filter.is_none_or(|is_folder| !is_folder))
        .filter(|item| {
            search_term
                .as_ref()
                .is_none_or(|term| item.name.to_ascii_lowercase().contains(term))
        })
        .filter(|item| {
            name_starts_with
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().starts_with(prefix))
        })
        .filter(|item| {
            name_starts_with_or_greater
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().as_str() >= prefix.as_str())
        })
        .filter(|item| {
            name_less_than
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().as_str() < prefix.as_str())
        })
        .collect::<Vec<_>>();

    if let Some(is_played) = is_played_filter {
        let Some(user_id) = user_id else {
            items.clear();
            return Ok(items);
        };
        let mut filtered = Vec::new();
        for item in items {
            let played = db
                .playback_state_for_item(user_id, item.id)
                .await?
                .is_some_and(|state| state.played);
            if played == is_played {
                filtered.push(item);
            }
        }
        items = filtered;
    }

    let sort_fields = sort_fields(query.sort_by.as_deref());
    if query
        .sort_order
        .as_deref()
        .is_some_and(|order| order.eq_ignore_ascii_case("Descending"))
    {
        items.sort_by(|left, right| compare_media_items(right, left, &sort_fields));
    } else {
        items.sort_by(|left, right| compare_media_items(left, right, &sort_fields));
    }

    Ok(items)
}

fn paged_media_items(items: Vec<MediaItem>, query: &ItemsQuery) -> Vec<MediaItem> {
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(usize::MAX);
    items.into_iter().skip(start_index).take(limit).collect()
}

async fn items_to_json(
    db: &Database,
    items: Vec<MediaItem>,
    server_id: &str,
    user_id: Option<Uuid>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let playback = if let Some(user_id) = user_id {
            db.playback_state_for_item(user_id, item.id).await?
        } else {
            None
        };
        values.push(media_item_to_json_with_playback(
            &item,
            server_id,
            playback.as_ref(),
        ));
    }
    Ok(values)
}

fn csv_lowercase(value: Option<&str>) -> Option<Vec<String>> {
    let values = value?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn normalized_prefix(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn folder_filter_value(filters: &[String]) -> Option<bool> {
    if filters.iter().any(|filter| filter == "isfolder") {
        Some(true)
    } else if filters.iter().any(|filter| filter == "isnotfolder") {
        Some(false)
    } else {
        None
    }
}

fn played_filter_value(filters: &[String]) -> Option<bool> {
    if filters.iter().any(|filter| filter == "isplayed") {
        Some(true)
    } else if filters.iter().any(|filter| filter == "isunplayed") {
        Some(false)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum SortField {
    SortName,
    DateCreated,
    DateLastMediaAdded,
}

fn sort_fields(sort_by: Option<&str>) -> Vec<SortField> {
    let fields = sort_by
        .unwrap_or("SortName")
        .split(',')
        .filter_map(|field| match field.trim().to_ascii_lowercase().as_str() {
            "sortname" | "name" => Some(SortField::SortName),
            "datecreated" => Some(SortField::DateCreated),
            "datelastmediaadded" => Some(SortField::DateLastMediaAdded),
            _ => None,
        })
        .collect::<Vec<_>>();

    if fields.is_empty() {
        vec![SortField::SortName]
    } else {
        fields
    }
}

fn compare_media_items(left: &MediaItem, right: &MediaItem, fields: &[SortField]) -> Ordering {
    fields
        .iter()
        .map(|field| match field {
            SortField::SortName => left
                .name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase()),
            SortField::DateCreated => left.created_at.cmp(&right.created_at),
            SortField::DateLastMediaAdded => left.updated_at.cmp(&right.updated_at),
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or_else(|| left.id.cmp(&right.id))
}

fn parse_uuid_list(value: &str) -> Result<Vec<Uuid>, ApiError> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_jellyfin_uuid)
        .collect()
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
    let media_streams = media_item_streams(item);

    let media_source = serde_json::json!({
        "Protocol": "File",
        "Id": playback.and_then(|state| state.media_source_id.clone()).unwrap_or_else(|| item_id.clone()),
        "Path": item.path,
        "Type": "Default",
        "Container": container,
        "Size": file_size,
        "Name": item.name,
        "IsRemote": false,
        "DirectStreamUrl": format!("/Videos/{item_id}/stream"),
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
        "DefaultAudioStreamIndex": null,
        "DefaultSubtitleStreamIndex": null,
        "MediaStreams": media_streams.clone(),
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
        "MediaStreams": media_streams,
        "PlayAccess": "Full",
        "RemoteTrailers": [],
        "ProviderIds": {},
        "IsFolder": false,
        "ParentId": item.virtual_folder_id.simple().to_string(),
        "Type": item_type,
        "MediaType": item.media_type,
        "RunTimeTicks": null,
        "UserData": { "PlaybackPositionTicks": playback_position_ticks, "PlayCount": play_count, "IsFavorite": false, "Played": played, "Key": item_id, "ItemId": item_id, "PlayedPercentage": null, "LastPlayedDate": null },
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

fn media_item_streams(item: &MediaItem) -> Vec<serde_json::Value> {
    if item.media_type != "Video" {
        return Vec::new();
    }

    vec![serde_json::json!({
        "Codec": media_item_container(item).unwrap_or_else(|| "unknown".to_string()),
        "Language": null,
        "ColorTransfer": null,
        "ColorPrimaries": null,
        "ColorSpace": null,
        "TimeBase": null,
        "VideoRange": null,
        "DisplayTitle": "Video",
        "NalLengthSize": null,
        "IsInterlaced": false,
        "IsAVC": null,
        "BitRate": null,
        "BitDepth": null,
        "RefFrames": null,
        "IsDefault": true,
        "IsForced": false,
        "Height": null,
        "Width": null,
        "AverageFrameRate": null,
        "RealFrameRate": null,
        "Profile": null,
        "Type": "Video",
        "AspectRatio": null,
        "Index": 0,
        "IsExternal": false,
        "IsTextSubtitleStream": false,
        "SupportsExternalStream": false,
        "Path": null,
        "PixelFormat": null,
        "Level": null,
        "IsAnamorphic": null
    })]
}

fn parse_range_header(headers: &HeaderMap, total_len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(range) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    if total_len == 0 {
        return Err(());
    }

    let range = range.to_str().map_err(|_| ())?;
    let range = range.strip_prefix("bytes=").ok_or(())?;
    let (start, end) = range.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix_len = end.parse::<u64>().map_err(|_| ())?.min(total_len);
        (total_len - suffix_len, total_len - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        let end = if end.is_empty() {
            total_len - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_len - 1)
        };
        (start, end)
    };

    if start <= end && start < total_len {
        Ok(Some((start, end)))
    } else {
        Err(())
    }
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

async fn item_placeholder_image(
    State(state): State<AppState>,
    Path((item_id, _image_type)): Path<(String, String)>,
) -> Result<axum::response::Response, ApiError> {
    media_item_or_folder_by_id(&state.db, &item_id).await?;
    Ok(placeholder_png_response())
}

async fn user_placeholder_image(
    State(state): State<AppState>,
    Path((user_id, _image_type)): Path<(Uuid, String)>,
) -> Result<axum::response::Response, ApiError> {
    if !state
        .db
        .users()
        .await?
        .into_iter()
        .any(|user| user.id == user_id)
    {
        return Err(ApiError::not_found("User not found"));
    }
    Ok(placeholder_png_response())
}

fn placeholder_png_response() -> axum::response::Response {
    const TRANSPARENT_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1,
        13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    (
        [(header::CONTENT_TYPE, "image/png")],
        TRANSPARENT_PNG.to_vec(),
    )
        .into_response()
}

async fn media_item_or_folder_by_id(db: &Database, item_id: &str) -> Result<(), ApiError> {
    let requested_id = parse_jellyfin_uuid(item_id)?;
    if db
        .media_items()
        .await?
        .into_iter()
        .any(|item| item.id == requested_id)
    {
        return Ok(());
    }
    if db
        .virtual_folders()
        .await?
        .into_iter()
        .any(|folder| folder.id == requested_id)
    {
        return Ok(());
    }
    Err(ApiError::not_found("Item not found"))
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

#[derive(Debug, Deserialize)]
struct StreamQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "Static", alias = "static", alias = "static_")]
    _static_file: Option<bool>,
    #[serde(alias = "MediaSourceId")]
    _media_source_id: Option<String>,
    #[serde(alias = "DeviceId")]
    _device_id: Option<String>,
    #[serde(alias = "PlaySessionId")]
    _play_session_id: Option<String>,
    #[serde(alias = "Tag")]
    _tag: Option<String>,
    #[serde(alias = "StartTimeTicks")]
    _start_time_ticks: Option<i64>,
    #[serde(alias = "AudioStreamIndex")]
    _audio_stream_index: Option<i32>,
    #[serde(alias = "SubtitleStreamIndex")]
    _subtitle_stream_index: Option<i32>,
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

fn session_to_json(
    session: &DeviceSession,
    active_playback: Option<&ActivePlaybackSession>,
    server_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "Id": session.access_token,
        "UserId": session.user_id,
        "UserName": session.user_name,
        "Client": session.client,
        "LastActivityDate": format_time_for_json(session.last_activity_at),
        "DeviceName": session.device_name,
        "DeviceId": session.device_id,
        "ApplicationVersion": session.version,
        "IsActive": true,
        "SupportsRemoteControl": false,
        "PlayableMediaTypes": [],
        "SupportedCommands": [],
        "NowPlayingItem": active_playback.map(|playback| media_item_to_json(&playback.item, server_id)),
        "PlayState": active_playback.map(active_playback_state_json),
        "NowViewingItem": null,
    })
}

fn active_playback_state_json(playback: &ActivePlaybackSession) -> serde_json::Value {
    serde_json::json!({
        "PositionTicks": playback.position_ticks,
        "CanSeek": true,
        "IsPaused": playback.is_paused,
        "IsMuted": false,
        "VolumeLevel": 100,
        "AudioStreamIndex": null,
        "SubtitleStreamIndex": null,
        "MediaSourceId": playback.media_source_id.clone(),
        "PlayMethod": "DirectPlay",
        "RepeatMode": "RepeatNone",
    })
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

async fn require_admin(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<User, ApiError> {
    let user = require_request_user(db, headers, query_token).await?;
    if user.is_administrator {
        Ok(user)
    } else {
        Err(ApiError::forbidden("Administrator access required"))
    }
}

fn ensure_user_access(auth_user: &User, requested_user_id: Uuid) -> Result<(), ApiError> {
    if auth_user.id == requested_user_id || auth_user.is_administrator {
        Ok(())
    } else {
        Err(ApiError::forbidden("User access denied"))
    }
}

async fn require_admin_or_startup_incomplete(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    if !db.server_state().await?.startup_wizard_completed {
        return Ok(());
    }

    let user = require_request_user(db, headers, query_token).await?;
    if user.is_administrator {
        Ok(())
    } else {
        Err(ApiError::forbidden("Administrator access required"))
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
            .clone()
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert_eq!(sessions[0]["UserName"], "admin");
        assert_eq!(sessions[0]["DeviceId"], "test-device");
        assert_eq!(sessions[0]["Client"], "Jellyfin Web");
        assert_eq!(sessions[0]["IsActive"], true);
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/ScheduledTasks?IsEnabled=true&api_key={api_key}"))
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
                    .header("X-Emby-Token", &api_key)
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

        let mut task = json!({});
        for _ in 0..20 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/ScheduledTasks/scan-media-library")
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            task = serde_json::from_slice(&body).unwrap();
            if task["LastExecutionResult"]["Status"] == "Completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(task["State"], "Idle");
        assert_eq!(task["LastExecutionResult"]["Status"], "Completed");
        assert_eq!(task["LastExecutionResult"]["Key"], "RefreshLibrary");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/ScheduledTasks/Running/scan-media-library")
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
                    .method(Method::POST)
                    .uri("/ScheduledTasks/scan-media-library/Triggers")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!([]).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ScheduledTasks/unknown/Triggers")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!([]).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
                        .header("X-Emby-Token", &api_key)
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
    async fn system_configuration_post_persists_startup_config() {
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
            db: db.clone(),
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ServerName": "Jellyrin Admin QA",
                            "UICulture": "es-ES",
                            "MetadataCountryCode": "ES",
                            "PreferredMetadataLanguage": "es",
                            "EnableRemoteAccess": true,
                            "UnknownJellyfinWebSetting": "ignored",
                            "MetadataOptions": [{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }],
                            "ContentTypes": [{ "Name": "Movies", "Value": "movies" }],
                            "PathSubstitutions": [{ "From": "/mnt/a", "To": "/mnt/b" }],
                            "PluginRepositories": [{ "Name": "Example", "Url": "https://example.invalid" }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ServerName": "Jellyrin Admin QA",
                            "UICulture": "es-ES",
                            "MetadataCountryCode": "ES",
                            "PreferredMetadataLanguage": "es",
                            "EnableRemoteAccess": true,
                            "UnknownJellyfinWebSetting": "ignored",
                            "MetadataOptions": [{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }],
                            "ContentTypes": [{ "Name": "Movies", "Value": "movies" }],
                            "PathSubstitutions": [{ "From": "/mnt/a", "To": "/mnt/b" }],
                            "PluginRepositories": [{ "Name": "Example", "Url": "https://example.invalid" }]
                        })
                        .to_string(),
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
                    .uri("/system/configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let updated_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated_config["ServerName"], "Jellyrin Admin QA");
        assert_eq!(updated_config["UICulture"], "es-ES");
        assert_eq!(updated_config["MetadataCountryCode"], "ES");
        assert_eq!(updated_config["PreferredMetadataLanguage"], "es");
        assert_eq!(updated_config["EnableRemoteAccess"], true);
        assert_eq!(updated_config["IsStartupWizardCompleted"], false);
        assert_eq!(
            updated_config["MetadataOptions"].as_array().unwrap().len(),
            0
        );
        assert_eq!(updated_config["ContentTypes"].as_array().unwrap().len(), 0);
        assert_eq!(
            updated_config["PathSubstitutions"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            updated_config["PluginRepositories"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let persisted_config = db.startup_config().await.unwrap();
        assert_eq!(persisted_config.server_name, "Jellyrin Admin QA");
        assert_eq!(persisted_config.ui_culture, "es-ES");
        assert_eq!(persisted_config.metadata_country_code, "ES");
        assert_eq!(persisted_config.preferred_metadata_language, "es");
        assert!(persisted_config.enable_remote_access);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ServerName": " " }).to_string()))
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
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "EnableRemoteAccess": false }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let preserved_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preserved_config["ServerName"], "Jellyrin Admin QA");
        assert_eq!(preserved_config["UICulture"], "es-ES");
        assert_eq!(preserved_config["EnableRemoteAccess"], false);
    }

    #[tokio::test]
    async fn device_delete_revokes_existing_session() {
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
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Delete Test", DeviceId="delete-device", Version="dev""#,
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
        let login: Value = serde_json::from_slice(&body).unwrap();
        let token = login["AccessToken"].as_str().unwrap();
        assert_eq!(login["SessionInfo"]["DeviceId"], "delete-device");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert_eq!(sessions[0]["DeviceId"], "delete-device");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/devices?Id=delete-device")
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
                    .uri("/devices?Id=delete-device")
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
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions_after_delete: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions_after_delete.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
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
    async fn library_and_environment_routes_require_admin_after_startup() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        db.server_state().await.unwrap();
        db.complete_startup_wizard().await.unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        for endpoint in ["/Library/VirtualFolders", "/Environment/Drives"] {
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
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
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
                    .method(Method::POST)
                    .uri("/Environment/ValidatePath")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Environment/ValidatePath")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
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
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
                        "/Items/Counts?ParentId={parent_id}&IncludeItemTypes=Movie"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_counts["MovieCount"], 1);
        assert_eq!(filtered_counts["ItemCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Counts?SearchTerm=missing-title")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let empty_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(empty_counts["MovieCount"], 0);
        assert_eq!(empty_counts["ItemCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/Latest?ParentId={parent_id}&IncludeItemTypes=Movie&Limit=1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let latest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(latest.as_array().unwrap().len(), 1);
        assert_eq!(latest[0]["Name"], "Example Movie");

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
        assert_eq!(filtered["StartIndex"], 0);
        assert_eq!(filtered["Items"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["Items"][0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?ParentId=not-a-valid-id")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items?IncludeItemTypes=Movie"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_items["TotalRecordCount"], 1);
        assert_eq!(user_items["Items"][0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Filters")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filters: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filters["Genres"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Genres")
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
                    .uri("/Genres")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let genres: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(genres["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Persons")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let persons: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(persons["StartIndex"], 0);
        assert_eq!(persons["TotalRecordCount"], 0);
        assert_eq!(persons["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Movies/Recommendations")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let recommendations: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(recommendations.as_array().unwrap().len(), 0);

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
        assert_eq!(
            detail["MediaSources"][0]["DirectStreamUrl"],
            format!("/Videos/{item_id}/stream")
        );
        assert_eq!(
            detail["MediaSources"][0]["MediaStreams"][0]["Type"],
            "Video"
        );
        assert_eq!(detail["MediaStreams"][0]["Index"], 0);
        assert_eq!(detail["People"].as_array().unwrap().len(), 0);
        assert_eq!(detail["Studios"].as_array().unwrap().len(), 0);
        assert_eq!(detail["GenreItems"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let item_images: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(item_images.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images/Primary"))
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{parent_id}/Images/Primary"))
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images/Primary"))
                    .header("X-Emby-Token", &api_key)
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/not-a-valid-id/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/00000000-0000-0000-0000-000000000000/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Images/Primary"))
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Images/Primary"))
                    .header("X-Emby-Token", &api_key)
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

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users/00000000-0000-0000-0000-000000000000/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/ThemeMedia"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let theme_media: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(theme_media["ThemeVideosResult"]["TotalRecordCount"], 0);
        assert_eq!(
            theme_media["ThemeSongsResult"]["Items"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(theme_media["SoundtrackSongsResult"]["StartIndex"], 0);

        for endpoint in [
            format!("/Items/{item_id}/ThemeSongs"),
            format!("/Items/{item_id}/ThemeVideos"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let theme_items: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(theme_items["TotalRecordCount"], 0);
            assert_eq!(theme_items["StartIndex"], 0);
            assert_eq!(theme_items["Items"].as_array().unwrap().len(), 0);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/00000000-0000-0000-0000-000000000000/ThemeSongs")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

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
        assert!(
            playback_info["PlaySessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(playback_info["MediaSources"][0]["SupportsDirectPlay"], true);
        assert_eq!(
            playback_info["MediaSources"][0]["SupportsTranscoding"],
            false
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][0]["Type"],
            "Video"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "DeviceProfile": {} }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let posted_playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            posted_playback_info["PlaySessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="playback-device", Version="dev""#,
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
        let login: Value = serde_json::from_slice(&body).unwrap();
        let playback_token = login["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Playing/Progress")
                    .header("X-Emby-Token", playback_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ItemId": item_id,
                            "MediaSourceId": item_id,
                            "PositionTicks": 25_000_000,
                            "CanSeek": true,
                            "IsPaused": false,
                        })
                        .to_string(),
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
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceId"], "playback-device");
        assert_eq!(sessions[0]["NowPlayingItem"]["Id"], item_id);
        assert_eq!(sessions[0]["PlayState"]["PositionTicks"], 25_000_000);
        assert_eq!(sessions[0]["PlayState"]["IsPaused"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Playing/Stopped")
                    .header("X-Emby-Token", playback_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ItemId": item_id,
                            "MediaSourceId": item_id,
                            "PositionTicks": 25_000_000,
                            "IsPaused": false,
                        })
                        .to_string(),
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
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let stopped_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(stopped_sessions[0]["NowPlayingItem"], Value::Null);

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
                    .uri(format!("/Users/{user_id}/Items?IsPlayed=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let played_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(played_items["TotalRecordCount"], 1);
        assert_eq!(played_items["Items"][0]["Id"], item_id);
        assert_eq!(played_items["Items"][0]["UserData"]["Played"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items?UserId={user_id}&IsPlayed=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_context_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_context_items["TotalRecordCount"], 1);
        assert_eq!(user_context_items["Items"][0]["UserData"]["Played"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items/Counts?IncludeItemTypes=Movie&Filters=IsPlayed"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_played_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_played_counts["MovieCount"], 1);
        assert_eq!(user_played_counts["ItemCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items/Latest?IncludeItemTypes=Movie&Limit=1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_latest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_latest.as_array().unwrap().len(), 1);
        assert_eq!(user_latest[0]["Id"], item_id);
        assert_eq!(user_latest[0]["UserData"]["Played"], true);

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
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 5-9/10"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"video");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "10"
        );
        assert_eq!(
            response.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=99-100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes */10"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/stream?Static=true&MediaSourceId={item_id}&DeviceId=test-device&PlaySessionId=test-play-session&Tag=test-tag"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );

        let extra_movie = tmp.path().join("Second Movie.mp4");
        tokio::fs::write(&extra_movie, b"second video")
            .await
            .unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ScheduledTasks/Running/scan-media-library")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let mut rescanned = json!({});
        for _ in 0..20 {
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
            rescanned = serde_json::from_slice(&body).unwrap();
            if rescanned["TotalRecordCount"] == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(rescanned["TotalRecordCount"], 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?IncludeItemTypes=Movie&StartIndex=1&Limit=1&SortBy=SortName")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let paged: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paged["TotalRecordCount"], 2);
        assert_eq!(paged["StartIndex"], 1);
        assert_eq!(paged["Items"].as_array().unwrap().len(), 1);
        assert_eq!(paged["Items"][0]["Name"], "Second Movie");

        let second_item_id = paged["Items"][0]["Id"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items?Ids={second_item_id}&SearchTerm=second&MediaTypes=Video&ExcludeItemTypes=Audio&IsFolder=false"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_items["TotalRecordCount"], 1);
        assert_eq!(filtered_items["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?ExcludeItemTypes=Movie")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let excluded_movies: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(excluded_movies["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?IsFolder=true")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders_only: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(folders_only["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?Filters=IsNotFolder&NameStartsWith=Sec&ImageTypeLimit=1&EnableImages=true&EnableUserData=true&CollapseBoxSetItems=false")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let prefixed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(prefixed["TotalRecordCount"], 1);
        assert_eq!(prefixed["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?Filters=IsFolder")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_folders: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_folders["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items?Filters=IsUnplayed&NameStartsWith=Sec"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let unplayed_filter_alias: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(unplayed_filter_alias["TotalRecordCount"], 1);
        assert_eq!(unplayed_filter_alias["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?NameStartsWithOrGreater=Second&NameLessThan=Third&SortBy=DateCreated,SortName&SortOrder=Descending&Fields=PrimaryImageAspectRatio,MediaSources&ImageTypes=Primary&EnableTotalRecordCount=true")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let name_window: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(name_window["TotalRecordCount"], 1);
        assert_eq!(name_window["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?SearchTerm=missing-title")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let no_matches: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(no_matches["TotalRecordCount"], 0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Similar"))
                    .header("X-Emby-Token", &api_key)
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
