use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;

use crate::{
    ApiError, AppState, AuthQuery, COMPATIBLE_SERVER_VERSION, backup_manifest_json,
    backup_restore_snapshot_json, record_activity, require_admin, restore_backup_data,
};

#[derive(Debug, Deserialize)]
pub(crate) struct BackupManifestQuery {
    #[serde(flatten)]
    pub(crate) auth: AuthQuery,
    #[serde(alias = "Path")]
    pub(crate) path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BackupOptionsBody {
    #[serde(alias = "Metadata", alias = "metadata")]
    pub(crate) metadata: Option<bool>,
    #[serde(alias = "Trickplay", alias = "trickplay")]
    pub(crate) trickplay: Option<bool>,
    #[serde(alias = "Subtitles", alias = "subtitles")]
    pub(crate) subtitles: Option<bool>,
    #[serde(alias = "Database", alias = "database")]
    pub(crate) database: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BackupRestoreBody {
    #[serde(alias = "ArchiveFileName")]
    pub(crate) archive_file_name: String,
}

pub(crate) async fn backups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let manifests = state.db.backup_manifests().await?;
    Ok(Json(manifests.iter().map(backup_manifest_json).collect()))
}

pub(crate) async fn create_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<Option<BackupOptionsBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let options = backup_options_json(payload);
    let restore_snapshot = backup_restore_snapshot_json(&state.db).await?;
    let manifest = state
        .db
        .create_backup_manifest(
            COMPATIBLE_SERVER_VERSION,
            "1",
            options,
            Some(restore_snapshot),
        )
        .await?;
    record_activity(
        &state.db,
        "Backup manifest created",
        Some("A backup manifest was created."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(Json(backup_manifest_json(&manifest)))
}

pub(crate) async fn backup_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BackupManifestQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let manifest = state
        .db
        .backup_manifest(&query.path)
        .await?
        .ok_or_else(|| ApiError::not_found("Backup manifest not found"))?;
    Ok(Json(backup_manifest_json(&manifest)))
}

pub(crate) async fn restore_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<BackupRestoreBody>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let archive = payload.archive_file_name.trim();
    if archive.is_empty() {
        return Err(ApiError::bad_request("ArchiveFileName must not be empty"));
    }
    let manifest = state
        .db
        .backup_manifest(archive)
        .await?
        .ok_or_else(|| ApiError::not_found("Backup manifest not found"))?;
    let snapshot = manifest
        .restore_snapshot
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("Backup manifest does not contain restore data"))?;
    restore_backup_data(&state.db, snapshot).await?;
    record_activity(
        &state.db,
        "Backup restored",
        Some(&format!("Backup manifest {archive} was restored.")),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn backup_options_json(payload: Option<BackupOptionsBody>) -> serde_json::Value {
    let payload = payload.unwrap_or(BackupOptionsBody {
        metadata: None,
        trickplay: None,
        subtitles: None,
        database: None,
    });
    serde_json::json!({
        "Metadata": payload.metadata.unwrap_or(false),
        "Trickplay": payload.trickplay.unwrap_or(false),
        "Subtitles": payload.subtitles.unwrap_or(false),
        "Database": payload.database.unwrap_or(true)
    })
}
