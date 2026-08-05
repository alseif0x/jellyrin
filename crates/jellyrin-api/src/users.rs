use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use jellyrin_compat::UserDto;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthQuery, UserService, require_admin, require_user, resolve_user_id,
    user_to_dto,
};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateUserByNameBody {
    #[serde(alias = "Name")]
    pub(crate) name: Option<String>,
    #[serde(alias = "Password")]
    pub(crate) password: Option<String>,
}

pub(crate) async fn create_user_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<CreateUserByNameBody>,
) -> Result<Json<UserDto>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    let name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::bad_request("Name must not be empty"))?;
    let user = service
        .create(name, payload.password.as_deref())
        .await
        .map_err(|_| ApiError::conflict("User could not be created"))?;
    let server = state.db.server_state().await?;
    Ok(Json(user_to_dto(&state.db, &user, server.server_id).await?))
}

pub(crate) async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    service
        .by_id(user_id)
        .await
        .map_err(|_| ApiError::not_found("User not found"))?;
    service
        .delete(user_id)
        .await
        .map_err(|_| ApiError::conflict("User could not be deleted"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateUserQuery {
    #[serde(flatten)]
    pub(crate) auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId", alias = "user_id")]
    pub(crate) user_id: Option<String>,
}

pub(crate) async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UpdateUserQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    let user_id = query
        .user_id
        .as_deref()
        .or_else(|| payload.get("Id").and_then(serde_json::Value::as_str))
        .ok_or_else(|| ApiError::bad_request("User id is required"))?;
    update_user_profile_from_payload(&service, resolve_user_id(user_id)?, payload).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn update_user_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    update_user_profile_from_payload(&service, user_id, payload).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn update_user_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    update_user_profile_from_payload(&service, user_id, serde_json::json!({ "Policy": payload }))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_user_profile_from_payload(
    service: &UserService<'_>,
    user_id: Uuid,
    payload: serde_json::Value,
) -> Result<jellyrin_core::User, ApiError> {
    let current = service
        .list()
        .await?
        .into_iter()
        .find(|user| user.id == user_id)
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let policy = payload.get("Policy");
    let name = payload
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&current.name);
    let is_administrator =
        bool_value(policy, "IsAdministrator").unwrap_or(current.is_administrator);
    let is_disabled = bool_value(policy, "IsDisabled").unwrap_or(current.is_disabled);
    let sync_play_access = policy
        .and_then(|policy| policy.get("SyncPlayAccess"))
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_sync_play_access)
        .unwrap_or(current.sync_play_access);
    service
        .update_profile(
            user_id,
            name,
            is_administrator,
            is_disabled,
            &sync_play_access,
        )
        .await
        .map_err(ApiError::from)
}

fn bool_value(payload: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    payload
        .and_then(|payload| payload.get(key))
        .and_then(serde_json::Value::as_bool)
}

pub(crate) fn normalize_sync_play_access(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "createandjoingroups" => Some("CreateAndJoinGroups".to_string()),
        "joingroups" => Some("JoinGroups".to_string()),
        "none" => Some("None".to_string()),
        _ => None,
    }
}

pub(crate) async fn get_current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<UserDto>, ApiError> {
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    Ok(Json(user_to_dto(&state.db, &user, server.server_id).await?))
}

pub(crate) async fn get_user_by_id(
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
    let requested_user = if user.id == user_id {
        user
    } else {
        UserService::new(&state.db)
            .list()
            .await?
            .into_iter()
            .find(|candidate| candidate.id == user_id)
            .ok_or_else(|| ApiError::not_found("User not found"))?
    };
    Ok(Json(
        user_to_dto(&state.db, &requested_user, server.server_id).await?,
    ))
}
