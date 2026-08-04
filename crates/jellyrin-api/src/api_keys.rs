use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use jellyrin_db::ApiKey;
use serde::Deserialize;

use crate::{ApiError, AppState, AuthQuery, format_time_for_json, require_admin};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateApiKeyQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "App")]
    app: Option<String>,
}

pub(crate) async fn api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let keys = state.db.api_keys().await?;
    let items = keys
        .iter()
        .enumerate()
        .map(|(index, key)| api_key_to_json(index, key))
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "Items": items,
        "TotalRecordCount": keys.len(),
        "StartIndex": 0
    })))
}

pub(crate) async fn create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CreateApiKeyQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let app = query
        .app
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("App must not be empty"))?;
    state.db.issue_api_key_for_user(user.id, app).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    state.db.revoke_api_key(&key).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn api_key_to_json(index: usize, key: &ApiKey) -> serde_json::Value {
    serde_json::json!({
        "Id": index + 1,
        "AccessToken": key.access_token,
        "DeviceId": null,
        "AppName": key.name,
        "AppVersion": null,
        "DeviceName": null,
        "UserId": key.user_id,
        "UserName": key.user_name,
        "IsActive": true,
        "DateCreated": format_time_for_json(key.created_at),
        "DateRevoked": null,
        "DateLastActivity": format_time_for_json(key.last_activity_at)
    })
}
