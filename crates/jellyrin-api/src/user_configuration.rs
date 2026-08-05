use crate::{
    ApiError, AppState, AuthQuery, SessionService, UserService, bearer_token, ensure_user_access,
    record_activity, require_request_user, resolve_user_id, syncplay_remove_session,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct UserConfigurationQuery {
    #[serde(flatten)]
    pub(crate) auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId")]
    pub(crate) user_id: Option<String>,
}

pub(crate) async fn update_user_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserConfigurationQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    update_user_configuration_for_id(
        state,
        headers,
        query.auth,
        query.user_id.as_deref(),
        payload,
    )
    .await
}

pub(crate) async fn update_user_configuration_for_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth): Query<AuthQuery>,
    Path(user_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    update_user_configuration_for_id(state, headers, auth, Some(&user_id), payload).await
}

async fn update_user_configuration_for_id(
    state: AppState,
    headers: HeaderMap,
    auth: AuthQuery,
    user_id: Option<&str>,
    payload: serde_json::Value,
) -> Result<StatusCode, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, auth.api_key.as_deref()).await?;
    let service = UserService::new(&state.db);
    let user_id = match user_id {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    ensure_user_access(&auth_user, user_id)?;
    let current = service
        .configuration(user_id)
        .await?
        .unwrap_or_else(default_user_configuration);
    let merged = merge_user_configuration(current, payload)?;
    service.update_configuration(user_id, merged).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn merge_user_configuration(
    current: serde_json::Value,
    update: serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let serde_json::Value::Object(mut current) = current else {
        return Err(ApiError::bad_request(
            "Stored user configuration is invalid",
        ));
    };
    let serde_json::Value::Object(update) = update else {
        return Err(ApiError::bad_request(
            "User configuration body must be an object",
        ));
    };
    for (key, value) in update {
        current.insert(key, value);
    }
    Ok(serde_json::Value::Object(current))
}

pub(crate) fn default_user_configuration() -> serde_json::Value {
    serde_json::json!({
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": "",
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": false,
        "EnableLocalPassword": false,
        "OrderedViews": [],
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "HidePlayedInLatest": true,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true
    })
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let token = bearer_token(&headers)
        .or_else(|| query.api_key.clone())
        .ok_or_else(|| ApiError::unauthorized("Missing token"))?;
    let (user, _) = crate::require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let service = SessionService::new(&state.db);
    record_activity(
        &state.db,
        &format!("{} signed out", user.name),
        Some(&format!("{} signed out", user.name)),
        "Authentication",
        Some(user.id),
    )
    .await?;
    syncplay_remove_session(&token, "Leave").await;
    service.revoke_token(&token).await?;
    Ok(StatusCode::NO_CONTENT)
}
