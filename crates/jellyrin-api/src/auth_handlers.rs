use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use jellyrin_compat::{AuthenticateUserByNameDto, AuthenticationResultDto};
use jellyrin_core::{DeviceToken, User};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    AUTH_FAILURE_MAX_ENTRIES, AUTH_FAILURE_PRUNE_INTERVAL, AUTH_FAILURE_RETENTION_SECONDS,
    AUTH_FAILURES, AUTH_LOCKOUT_FAILURE_LIMIT, AUTH_LOCKOUT_SECONDS, ApiError, AppState,
    AuthFailureRegistry, AuthFailureState, AuthQuery, UserService, authentication_result_to_dto,
    client_auth_from_headers, ensure_user_access, record_activity, require_user, resolve_user_id,
};

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateUserPasswordQuery {
    #[serde(flatten)]
    pub(crate) auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId")]
    pub(crate) user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateUserPasswordBody {
    #[serde(alias = "CurrentPw")]
    pub(crate) current_pw: Option<String>,
    #[serde(alias = "NewPw")]
    pub(crate) new_pw: Option<String>,
    #[serde(alias = "ResetPassword")]
    pub(crate) reset_password: Option<bool>,
}

pub(crate) async fn update_user_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UpdateUserPasswordQuery>,
    Json(payload): Json<UpdateUserPasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (auth_user, token) =
        require_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let requested_user_id = match query.user_id.as_deref() {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    update_user_password_inner(&state, &auth_user, &token, requested_user_id, payload).await
}

pub(crate) async fn update_user_password_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<UpdateUserPasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (auth_user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    update_user_password_inner(&state, &auth_user, &token, user_id, payload).await
}

async fn update_user_password_inner(
    state: &AppState,
    auth_user: &User,
    token: &DeviceToken,
    requested_user_id: Uuid,
    payload: UpdateUserPasswordBody,
) -> Result<StatusCode, ApiError> {
    let service = UserService::new(&state.db);
    let target = service
        .list()
        .await?
        .into_iter()
        .find(|user| user.id == requested_user_id)
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    ensure_user_access(auth_user, target.id)?;
    if payload.reset_password.unwrap_or(false) {
        service.reset_password(target.id).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if !auth_user.is_administrator || auth_user.id == target.id {
        service
            .verify_password(target.id, payload.current_pw.as_deref().unwrap_or_default())
            .await
            .map_err(|_| ApiError::forbidden("Invalid user or password entered"))?;
    }
    service
        .set_password(target.id, payload.new_pw.as_deref().unwrap_or_default())
        .await?;
    service
        .revoke_tokens_except(target.id, &token.access_token)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn authenticate_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AuthenticateUserByNameDto>,
) -> Result<Json<AuthenticationResultDto>, ApiError> {
    let auth = client_auth_from_headers(&headers);
    let service = UserService::new(&state.db);
    let username = payload
        .username
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("Username must not be empty"))?;
    let lockout_key = auth_lockout_key("name", username);
    ensure_auth_not_locked(&lockout_key).await?;
    let password = payload.pw.as_deref().unwrap_or("");
    tracing::info!(route = "Users/AuthenticateByName", username, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, password_present = !password.is_empty(), "authentication attempt");
    let auth_result = service
        .authenticate_by_name(
            username,
            password,
            &auth.device_id,
            &auth.device,
            &auth.client,
            &auth.version,
        )
        .await;
    let (user, token) = match auth_result {
        Ok(result) => {
            clear_auth_failure(&lockout_key).await;
            result
        }
        Err(_) => {
            record_auth_failure(&lockout_key).await;
            tracing::warn!(route = "Users/AuthenticateByName", username, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, password_present = !password.is_empty(), "authentication failed");
            return Err(ApiError::unauthorized("Invalid username or password"));
        }
    };
    tracing::info!(route = "Users/AuthenticateByName", username = %user.name, user_id = %user.id, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, "authentication succeeded");
    let server = state.db.server_state().await?;
    record_activity(
        &state.db,
        &format!("{} signed in", user.name),
        Some(&format!("{} signed in from {}", user.name, auth.client)),
        "Authentication",
        Some(user.id),
    )
    .await?;
    Ok(Json(
        authentication_result_to_dto(&state.db, &user, &token, server.server_id).await?,
    ))
}

pub(crate) async fn authenticate_user_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<AuthenticateUserByNameDto>,
) -> Result<Json<AuthenticationResultDto>, ApiError> {
    let auth = client_auth_from_headers(&headers);
    let service = UserService::new(&state.db);
    let lockout_key = auth_lockout_key("id", &user_id.to_string());
    ensure_auth_not_locked(&lockout_key).await?;
    let password = payload.pw.as_deref().unwrap_or("");
    tracing::info!(route = "Users/{user_id}/Authenticate", %user_id, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, password_present = !password.is_empty(), "authentication attempt");
    let auth_result = service
        .authenticate_by_id(
            user_id,
            password,
            &auth.device_id,
            &auth.device,
            &auth.client,
            &auth.version,
        )
        .await;
    let (user, token) = match auth_result {
        Ok(result) => {
            clear_auth_failure(&lockout_key).await;
            result
        }
        Err(_) => {
            record_auth_failure(&lockout_key).await;
            tracing::warn!(route = "Users/{user_id}/Authenticate", %user_id, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, password_present = !password.is_empty(), "authentication failed");
            return Err(ApiError::unauthorized("Invalid username or password"));
        }
    };
    tracing::info!(route = "Users/{user_id}/Authenticate", username = %user.name, user_id = %user.id, client = %auth.client, device = %auth.device, device_id = %auth.device_id, version = %auth.version, "authentication succeeded");
    let server = state.db.server_state().await?;
    record_activity(
        &state.db,
        &format!("{} signed in", user.name),
        Some(&format!("{} signed in from {}", user.name, auth.client)),
        "Authentication",
        Some(user.id),
    )
    .await?;
    Ok(Json(
        authentication_result_to_dto(&state.db, &user, &token, server.server_id).await?,
    ))
}

fn auth_lockout_key(kind: &str, value: &str) -> String {
    format!("{}:{}", kind, value.trim().to_ascii_lowercase())
}

async fn ensure_auth_not_locked(key: &str) -> Result<(), ApiError> {
    let now = epoch_seconds();
    let mut failures = AUTH_FAILURES
        .get_or_init(|| tokio::sync::Mutex::new(AuthFailureRegistry::default()))
        .lock()
        .await;
    if auth_attempt_is_limited(&mut failures, key, now, AUTH_FAILURE_MAX_ENTRIES) {
        return Err(ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            error: anyhow::anyhow!("Too many failed login attempts"),
        });
    }
    Ok(())
}

async fn record_auth_failure(key: &str) {
    let now = epoch_seconds();
    let mut failures = AUTH_FAILURES
        .get_or_init(|| tokio::sync::Mutex::new(AuthFailureRegistry::default()))
        .lock()
        .await;
    record_auth_failure_in_registry(&mut failures, key, now, AUTH_FAILURE_MAX_ENTRIES);
}

async fn clear_auth_failure(key: &str) {
    if let Some(failures) = AUTH_FAILURES.get() {
        failures.lock().await.entries.remove(key);
    }
}

fn auth_failure_expired(state: &AuthFailureState, now: u64) -> bool {
    match state.locked_until_epoch {
        Some(locked_until) => locked_until <= now,
        None => {
            state
                .last_failure_epoch
                .saturating_add(AUTH_FAILURE_RETENTION_SECONDS)
                <= now
        }
    }
}

fn prune_auth_failures(registry: &mut AuthFailureRegistry, now: u64) {
    registry
        .entries
        .retain(|_, state| !auth_failure_expired(state, now));
    registry.operations_since_prune = 0;
}

fn maintain_auth_failure_registry(registry: &mut AuthFailureRegistry, now: u64, force: bool) {
    registry.operations_since_prune = registry.operations_since_prune.saturating_add(1);
    if force || registry.operations_since_prune >= AUTH_FAILURE_PRUNE_INTERVAL {
        prune_auth_failures(registry, now);
    }
}

fn remove_expired_auth_failure(registry: &mut AuthFailureRegistry, key: &str, now: u64) {
    if registry
        .entries
        .get(key)
        .is_some_and(|state| auth_failure_expired(state, now))
    {
        registry.entries.remove(key);
    }
}

fn auth_attempt_is_limited(
    registry: &mut AuthFailureRegistry,
    key: &str,
    now: u64,
    max_entries: usize,
) -> bool {
    remove_expired_auth_failure(registry, key, now);
    let missing_at_capacity =
        !registry.entries.contains_key(key) && registry.entries.len() >= max_entries;
    maintain_auth_failure_registry(registry, now, missing_at_capacity);
    registry
        .entries
        .get(key)
        .and_then(|state| state.locked_until_epoch)
        .is_some_and(|locked_until| locked_until > now)
        || (!registry.entries.contains_key(key) && registry.entries.len() >= max_entries)
}

fn record_auth_failure_in_registry(
    registry: &mut AuthFailureRegistry,
    key: &str,
    now: u64,
    max_entries: usize,
) -> bool {
    remove_expired_auth_failure(registry, key, now);
    let missing_at_capacity =
        !registry.entries.contains_key(key) && registry.entries.len() >= max_entries;
    maintain_auth_failure_registry(registry, now, missing_at_capacity);
    if !registry.entries.contains_key(key) && registry.entries.len() >= max_entries {
        return false;
    }
    let entry = registry
        .entries
        .entry(key.to_string())
        .or_insert(AuthFailureState {
            failures: 0,
            locked_until_epoch: None,
            last_failure_epoch: now,
        });
    entry.failures = entry.failures.saturating_add(1);
    entry.last_failure_epoch = now;
    if entry.failures >= AUTH_LOCKOUT_FAILURE_LIMIT {
        entry.locked_until_epoch = Some(now.saturating_add(AUTH_LOCKOUT_SECONDS));
    }
    true
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_registry_prunes_expired_entries() {
        let now = 10_000;
        let mut registry = AuthFailureRegistry::default();
        registry.entries.insert(
            "recent".to_string(),
            AuthFailureState {
                failures: 1,
                locked_until_epoch: None,
                last_failure_epoch: now - AUTH_FAILURE_RETENTION_SECONDS + 1,
            },
        );
        registry.entries.insert(
            "stale".to_string(),
            AuthFailureState {
                failures: 1,
                locked_until_epoch: None,
                last_failure_epoch: now - AUTH_FAILURE_RETENTION_SECONDS,
            },
        );
        registry.entries.insert(
            "locked".to_string(),
            AuthFailureState {
                failures: AUTH_LOCKOUT_FAILURE_LIMIT,
                locked_until_epoch: Some(now + 1),
                last_failure_epoch: now,
            },
        );
        registry.entries.insert(
            "expired-lock".to_string(),
            AuthFailureState {
                failures: AUTH_LOCKOUT_FAILURE_LIMIT,
                locked_until_epoch: Some(now),
                last_failure_epoch: now - AUTH_LOCKOUT_SECONDS,
            },
        );

        prune_auth_failures(&mut registry, now);

        assert_eq!(registry.entries.len(), 2);
        assert!(registry.entries.contains_key("recent"));
        assert!(registry.entries.contains_key("locked"));
    }

    #[test]
    fn auth_failure_registry_reuses_entries_and_enforces_capacity() {
        let now = 20_000;
        let mut registry = AuthFailureRegistry::default();

        assert!(record_auth_failure_in_registry(&mut registry, "a", now, 2));
        assert!(record_auth_failure_in_registry(&mut registry, "b", now, 2));
        assert!(!record_auth_failure_in_registry(&mut registry, "c", now, 2));
        assert_eq!(registry.entries.len(), 2);

        assert!(record_auth_failure_in_registry(
            &mut registry,
            "a",
            now + 1,
            2
        ));
        assert_eq!(registry.entries["a"].failures, 2);
        assert!(auth_attempt_is_limited(&mut registry, "c", now + 1, 2));

        assert!(!auth_attempt_is_limited(
            &mut registry,
            "c",
            now + AUTH_FAILURE_RETENTION_SECONDS + 1,
            2
        ));
        assert!(registry.entries.is_empty());
    }
}
