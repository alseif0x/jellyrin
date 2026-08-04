use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use jellyrin_compat::{
    HealthResponse, PublicSystemInfo, StartupConfigurationDto, StartupRemoteAccessDto,
    StartupUserDto, UserDto,
};
use jellyrin_core::StartupConfig;
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    ApiError, AppState, AuthQuery, COMPATIBLE_PRODUCT_NAME, COMPATIBLE_SERVER_VERSION,
    DEFAULT_AUTHENTICATION_PROVIDER_ID, DEFAULT_PASSWORD_RESET_PROVIDER_ID, format_time_for_json,
    require_admin, require_startup_wizard_incomplete, require_user, startup_config_to_dto,
    user_to_dto,
};

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "Healthy" })
}

pub(crate) async fn ready(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    sqlx::query("SELECT 1").execute(state.db.pool()).await?;
    Ok(Json(HealthResponse { status: "Ready" }))
}

pub(crate) async fn system_info_public(
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

pub(crate) async fn system_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Value>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    let data_root = state
        .log_dir
        .parent()
        .unwrap_or(state.log_dir.as_path())
        .to_string_lossy()
        .to_string();
    let web_path = state.web_dir.to_string_lossy().to_string();
    let log_path = state.log_dir.to_string_lossy().to_string();
    Ok(Json(serde_json::json!({
        "Id": server.server_id,
        "ServerName": server.server_name,
        "Version": COMPATIBLE_SERVER_VERSION,
        "ProductName": COMPATIBLE_PRODUCT_NAME,
        "OperatingSystem": "Linux",
        "OperatingSystemDisplayName": "Linux",
        "LocalAddress": state.local_address,
        "StartupWizardCompleted": server.startup_wizard_completed,
        "CachePath": data_root,
        "CanLaunchWebBrowser": false,
        "CanSelfRestart": false,
        "CastReceiverApplications": [],
        "CompletedInstallations": [],
        "EncoderLocation": "System",
        "HasPendingRestart": false,
        "HasUpdateAvailable": false,
        "InternalMetadataPath": data_root,
        "IsShuttingDown": false,
        "ItemsByNamePath": data_root,
        "LogPath": log_path,
        "ProgramDataPath": data_root,
        "SupportsLibraryMonitor": true,
        "SystemArchitecture": std::env::consts::ARCH,
        "TranscodingTempPath": data_root,
        "WebPath": web_path,
        "WebSocketPortNumber": 0
    })))
}

pub(crate) async fn time_sync_utc_time() -> Json<Value> {
    let now = format_time_for_json(OffsetDateTime::now_utc());
    Json(serde_json::json!({
        "RequestReceptionTime": now,
        "ResponseTransmissionTime": now
    }))
}

pub(crate) async fn tmdb_client_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Value>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "BaseUrl": "http://image.tmdb.org/t/p/",
        "SecureBaseUrl": "https://image.tmdb.org/t/p/",
        "BackdropSizes": ["w300", "w780", "w1280", "original"],
        "LogoSizes": ["w45", "w92", "w154", "w185", "w300", "w500", "original"],
        "PosterSizes": ["w92", "w154", "w185", "w342", "w500", "w780", "original"],
        "ProfileSizes": ["w45", "w185", "h632", "original"],
        "StillSizes": ["w92", "w185", "w300", "original"]
    })))
}

pub(crate) async fn get_startup_configuration(
    State(state): State<AppState>,
) -> Result<Json<StartupConfigurationDto>, ApiError> {
    Ok(Json(startup_config_to_dto(
        state.db.startup_config().await?,
    )))
}

pub(crate) async fn post_startup_configuration(
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
            dummy_chapter_duration: current.dummy_chapter_duration,
            chapter_image_resolution: current.chapter_image_resolution,
            enable_remote_access: current.enable_remote_access,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn post_startup_remote_access(
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

pub(crate) async fn get_startup_user(
    State(state): State<AppState>,
) -> Result<Json<StartupUserDto>, ApiError> {
    let user = state.db.first_user().await?;
    Ok(Json(StartupUserDto {
        name: Some(user.name),
        password: None,
    }))
}

pub(crate) async fn post_startup_user(
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

pub(crate) async fn post_startup_complete(
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    state.db.complete_startup_wizard().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_public_users(
    State(state): State<AppState>,
) -> Result<Json<Vec<UserDto>>, ApiError> {
    let server = state.db.server_state().await?;
    if !server.startup_wizard_completed {
        return Ok(Json(Vec::new()));
    }
    let users = state.db.users().await?;
    let mut dtos = Vec::with_capacity(users.len());
    for user in &users {
        if user.is_disabled {
            continue;
        }
        let dto = user_to_dto(&state.db, user, server.server_id).await?;
        // Jellyfin's public-user surface is limited to passwordless accounts;
        // configured accounts must authenticate through /Users/AuthenticateByName.
        if !dto.has_password {
            dtos.push(dto);
        }
    }
    Ok(Json(dtos))
}

pub(crate) async fn get_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<UserDto>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    let users = state.db.users().await?;
    let mut dtos = Vec::with_capacity(users.len());
    for user in &users {
        dtos.push(user_to_dto(&state.db, user, server.server_id).await?);
    }
    Ok(Json(dtos))
}

pub(crate) async fn authentication_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![serde_json::json!({
        "Name": "Default",
        "Id": DEFAULT_AUTHENTICATION_PROVIDER_ID
    })]))
}

pub(crate) async fn password_reset_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![serde_json::json!({
        "Name": "Default",
        "Id": DEFAULT_PASSWORD_RESET_PROVIDER_ID
    })]))
}
