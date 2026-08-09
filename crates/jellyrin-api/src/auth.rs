use axum::http::HeaderMap;
use jellyrin_core::{DeviceToken, User};
use uuid::Uuid;

use crate::{ApiError, Database};

#[derive(Debug)]
pub(crate) struct ClientAuth {
    pub(crate) client: String,
    pub(crate) device: String,
    pub(crate) device_id: String,
    pub(crate) version: String,
}

pub(crate) async fn require_user(
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

pub(crate) async fn require_request_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<User, ApiError> {
    require_user(db, headers, query_token)
        .await
        .map(|(user, _)| user)
}

pub(crate) async fn require_admin(
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

pub(crate) async fn require_admin_session(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(User, String), ApiError> {
    let (user, token) = require_user(db, headers, query_token).await?;
    if user.is_administrator {
        Ok((user, token.access_token))
    } else {
        Err(ApiError::forbidden("Administrator access required"))
    }
}

pub(crate) fn ensure_user_access(
    auth_user: &User,
    requested_user_id: Uuid,
) -> Result<(), ApiError> {
    if auth_user.id == requested_user_id || auth_user.is_administrator {
        Ok(())
    } else {
        Err(ApiError::forbidden("User access denied"))
    }
}

pub(crate) async fn require_admin_or_startup_incomplete(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    require_admin_or_startup_incomplete_user(db, headers, query_token)
        .await
        .map(|_| ())
}

pub(crate) async fn require_admin_or_startup_incomplete_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Option<User>, ApiError> {
    if !db.server_state().await?.startup_wizard_completed {
        return Ok(None);
    }

    require_admin(db, headers, query_token).await.map(Some)
}

pub(crate) async fn require_user_or_startup_incomplete(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    if !db.server_state().await?.startup_wizard_completed {
        return Ok(());
    }

    require_request_user(db, headers, query_token)
        .await
        .map(|_| ())
}

pub(crate) async fn require_startup_wizard_incomplete(db: &Database) -> Result<(), ApiError> {
    let server = db.server_state().await?;
    if server.startup_wizard_completed {
        return Err(ApiError::forbidden("Startup wizard is already complete"));
    }
    Ok(())
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    for name in ["x-emby-token", "x-mediabrowser-token"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok())
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }

    headers
        .get("authorization")
        .or_else(|| headers.get("x-emby-authorization"))
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authorization_token)
}

pub(crate) fn client_auth_from_headers(headers: &HeaderMap) -> ClientAuth {
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

pub(crate) fn parse_authorization_token(header: &str) -> Option<String> {
    parse_media_browser_pairs(header)
        .into_iter()
        .find_map(|(key, value)| (key == "token").then_some(value))
}

pub(crate) fn parse_media_browser_pairs(header: &str) -> Vec<(String, String)> {
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
