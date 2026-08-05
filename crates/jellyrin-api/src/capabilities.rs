use axum::{
    Json,
    extract::{Query, RawQuery, State},
    http::{HeaderMap, StatusCode},
};

use crate::{ApiError, AppState, AuthQuery, require_user};

pub(crate) async fn update_session_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    body: Option<Json<serde_json::Value>>,
) -> Result<StatusCode, ApiError> {
    let (_, token) = require_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let query = parse_session_capabilities_query(raw_query.as_deref());
    let capabilities = normalize_session_capabilities(body.map(|Json(value)| value), &query)?;
    let update_result = state
        .db
        .update_device_capabilities(&token.access_token, capabilities.clone())
        .await;
    if update_result.is_err() && token.client == "API Key" {
        state.db.ensure_device_session(&token).await?;
        state
            .db
            .update_device_capabilities(&token.access_token, capabilities)
            .await
            .map_err(|_| ApiError::not_found("Device session not found"))?;
    } else {
        update_result.map_err(|_| ApiError::not_found("Device session not found"))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default)]
struct SessionCapabilitiesQuery {
    playable_media_types: Vec<String>,
    supported_commands: Vec<String>,
    supports_remote_control: Option<bool>,
    supports_media_control: Option<bool>,
    supports_persistent_identifier: Option<bool>,
    supports_sync: Option<bool>,
}

fn parse_session_capabilities_query(raw_query: Option<&str>) -> SessionCapabilitiesQuery {
    let mut query = SessionCapabilitiesQuery::default();
    let Some(raw_query) = raw_query else {
        return query;
    };
    for pair in raw_query.split('&') {
        let Some((raw_key, raw_value)) = pair.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        match key.as_str() {
            "playablemediatypes" => query.playable_media_types.push(value.to_string()),
            "supportedcommands" => query.supported_commands.push(value.to_string()),
            "supportsremotecontrol" => {
                query.supports_remote_control = parse_bool_query_value(value)
            }
            "supportsmediacontrol" => query.supports_media_control = parse_bool_query_value(value),
            "supportspersistentidentifier" => {
                query.supports_persistent_identifier = parse_bool_query_value(value)
            }
            "supportssync" => query.supports_sync = parse_bool_query_value(value),
            _ => {}
        }
    }
    query
}

pub(crate) fn parse_bool_query_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn normalize_session_capabilities(
    payload: Option<serde_json::Value>,
    query: &SessionCapabilitiesQuery,
) -> Result<serde_json::Value, ApiError> {
    let mut object = match payload {
        Some(serde_json::Value::Object(object)) => object,
        Some(_) => {
            return Err(ApiError::bad_request(
                "Session capabilities body must be an object",
            ));
        }
        None => serde_json::Map::new(),
    };
    let serde_json::Value::Object(defaults) = default_session_capabilities() else {
        unreachable!("default session capabilities must be an object");
    };
    for (key, value) in defaults {
        object.entry(key).or_insert(value);
    }
    if !query.playable_media_types.is_empty() {
        object.insert(
            "PlayableMediaTypes".to_string(),
            serde_json::Value::Array(parse_capability_values(&query.playable_media_types)),
        );
    }
    if !query.supported_commands.is_empty() {
        object.insert(
            "SupportedCommands".to_string(),
            serde_json::Value::Array(parse_capability_values(&query.supported_commands)),
        );
    }
    if let Some(value) = query.supports_remote_control {
        object.insert(
            "SupportsRemoteControl".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(value) = query.supports_media_control {
        object.insert("SupportsMediaControl".to_string(), serde_json::json!(value));
    }
    if let Some(value) = query.supports_persistent_identifier {
        object.insert(
            "SupportsPersistentIdentifier".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(value) = query.supports_sync {
        object.insert("SupportsSync".to_string(), serde_json::json!(value));
    }
    Ok(serde_json::Value::Object(object))
}

fn parse_capability_values(values: &[String]) -> Vec<serde_json::Value> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| serde_json::Value::String(value.to_string()))
        .collect()
}

fn default_session_capabilities() -> serde_json::Value {
    serde_json::json!({
        "PlayableMediaTypes": [],
        "SupportedCommands": [],
        "SupportsRemoteControl": false,
        "SupportsMediaControl": false
    })
}
