use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerState {
    pub server_id: Uuid,
    pub server_name: String,
    pub startup_wizard_completed: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupConfig {
    pub server_name: String,
    pub ui_culture: String,
    pub metadata_country_code: String,
    pub preferred_metadata_language: String,
    pub enable_remote_access: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceToken {
    pub access_token: String,
    pub user_id: Uuid,
    pub device_id: String,
    pub device_name: String,
    pub client: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualFolder {
    pub id: Uuid,
    pub name: String,
    pub collection_type: Option<String>,
    pub locations: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
