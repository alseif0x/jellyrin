use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::Deserialize;

use crate::{
    ApiError, AppState, AuthQuery, COMPATIBLE_SERVER_VERSION, analyze_jellyfin_migration,
    apply_jellyfin_migration, backup_restore_snapshot_json, record_activity, require_admin,
};

#[derive(Debug, Deserialize)]
pub(crate) struct JellyfinMigrationBody {
    #[serde(alias = "SourceName")]
    pub(crate) source_name: Option<String>,
    #[serde(alias = "Data")]
    pub(crate) data: serde_json::Value,
}

pub(crate) async fn jellyfin_migration_dry_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<JellyfinMigrationBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let report = analyze_jellyfin_migration(&state.db, &payload).await?;
    Ok(Json(report.json(None)))
}

pub(crate) async fn jellyfin_migration_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<JellyfinMigrationBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let backup_snapshot = backup_restore_snapshot_json(&state.db).await?;
    let backup = state
        .db
        .create_backup_manifest(
            COMPATIBLE_SERVER_VERSION,
            "1",
            serde_json::json!({
                "Database": true,
                "Metadata": true,
                "Reason": "Pre-migration safety backup"
            }),
            Some(backup_snapshot),
        )
        .await?;
    let mut report = apply_jellyfin_migration(&state.db, &payload).await?;
    record_activity(
        &state.db,
        "Jellyfin migration imported",
        Some(&format!(
            "Jellyfin migration import completed after safety backup {}.",
            backup.path
        )),
        "System",
        Some(user.id),
    )
    .await?;
    report.applied = true;
    Ok(Json(report.json(Some(&backup.path))))
}
