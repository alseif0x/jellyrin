use std::path::{Path as FsPath, PathBuf};

use axum::{Json, extract::State};
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{ApiError, AppState, UserService, format_time_for_json, stable_entity_id};

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ForgotPasswordBody {
    #[serde(alias = "EnteredUsername", alias = "Username", alias = "Name")]
    pub(crate) entered_username: Option<String>,
}

pub(crate) async fn forgot_password(
    State(state): State<AppState>,
    body: Option<Json<ForgotPasswordBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entered_username = body
        .and_then(|Json(body)| body.entered_username)
        .unwrap_or_default();
    let expire_time = OffsetDateTime::now_utc() + time::Duration::minutes(30);
    let pin_file = password_reset_file_path(&entered_username);

    if let Some(user) = state
        .db
        .users()
        .await?
        .into_iter()
        .find(|user| user.name.eq_ignore_ascii_case(entered_username.trim()))
    {
        let pin = generate_password_reset_pin();
        let reset = serde_json::json!({
            "ExpirationDate": format_time_for_json(expire_time),
            "Pin": pin,
            "PinFile": pin_file.to_string_lossy(),
            "UserName": user.name
        });
        tokio::fs::write(&pin_file, serde_json::to_vec(&reset)?).await?;
    }

    Ok(Json(serde_json::json!({
        "Action": "PinCode",
        "PinFile": pin_file.to_string_lossy(),
        "PinExpirationDate": format_time_for_json(expire_time)
    })))
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ForgotPasswordPinBody {
    #[serde(alias = "Pin")]
    pub(crate) pin: Option<String>,
    #[serde(alias = "Password", alias = "NewPassword", alias = "NewPw")]
    pub(crate) password: Option<String>,
}

pub(crate) async fn forgot_password_pin(
    State(state): State<AppState>,
    body: Option<Json<ForgotPasswordPinBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pin = body
        .as_ref()
        .and_then(|Json(body)| body.pin.clone())
        .unwrap_or_default();
    let normalized_pin = normalize_password_reset_pin(&pin);
    let new_password = body
        .as_ref()
        .and_then(|Json(body)| body.password.as_deref())
        .map(str::trim)
        .filter(|password| !password.is_empty())
        .unwrap_or(pin.trim())
        .to_string();
    if normalized_pin.is_empty() {
        return Ok(Json(serde_json::json!({
            "Success": false,
            "UsersReset": []
        })));
    }

    let mut users_reset = Vec::new();
    let mut entries = tokio::fs::read_dir(password_reset_dir()).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !is_password_reset_file(&path) {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(reset) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let expiration = reset
            .get("ExpirationDate")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok());
        if expiration.is_some_and(|expiration| expiration < OffsetDateTime::now_utc()) {
            let _ = tokio::fs::remove_file(&path).await;
            continue;
        }
        let stored_pin = reset
            .get("Pin")
            .and_then(serde_json::Value::as_str)
            .map(normalize_password_reset_pin)
            .unwrap_or_default();
        if stored_pin != normalized_pin {
            continue;
        }
        let Some(username) = reset.get("UserName").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(user) = state
            .db
            .users()
            .await?
            .into_iter()
            .find(|user| user.name.eq_ignore_ascii_case(username))
        else {
            continue;
        };
        UserService::new(&state.db)
            .set_password(user.id, &new_password)
            .await?;
        users_reset.push(user.name);
        let _ = tokio::fs::remove_file(&path).await;
    }

    Ok(Json(serde_json::json!({
        "Success": !users_reset.is_empty(),
        "UsersReset": users_reset
    })))
}

fn password_reset_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn password_reset_file_path(username: &str) -> PathBuf {
    let username = username.trim();
    let suffix = if username.is_empty() {
        Uuid::new_v4().simple().to_string()
    } else {
        stable_entity_id("passwordreset", username)
    };
    password_reset_dir().join(format!("passwordreset{suffix}.json"))
}

fn is_password_reset_file(path: &FsPath) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("passwordreset") && name.ends_with(".json"))
}

fn generate_password_reset_pin() -> String {
    let mut bytes = [0_u8; 4];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("-")
}

fn normalize_password_reset_pin(pin: &str) -> String {
    pin.chars()
        .filter(|character| *character != '-')
        .flat_map(char::to_uppercase)
        .collect()
}
