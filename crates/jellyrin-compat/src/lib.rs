use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub id: Uuid,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub local_address: String,
    pub startup_wizard_completed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct StartupConfigurationDto {
    pub server_name: String,
    #[serde(rename = "UICulture")]
    pub ui_culture: String,
    pub metadata_country_code: String,
    pub preferred_metadata_language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct StartupRemoteAccessDto {
    pub enable_remote_access: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct StartupUserDto {
    pub name: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticateUserByNameDto {
    pub username: Option<String>,
    pub pw: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub id: Uuid,
    pub name: String,
    pub server_id: Uuid,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub has_configured_easy_password: bool,
    pub enable_auto_login: bool,
    pub configuration: serde_json::Value,
    pub policy: UserPolicyDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicyDto {
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub is_hidden: bool,
    pub enable_all_devices: bool,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub enable_remote_access: bool,
    pub enable_collection_management: bool,
    pub enable_subtitle_management: bool,
    pub enable_content_downloading: bool,
    pub enable_live_tv_management: bool,
    pub enable_live_tv_access: bool,
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub force_remote_source_transcoding: bool,
    pub enable_content_deletion: bool,
    pub enable_content_deletion_from_folders: Vec<String>,
    pub remote_client_bitrate_limit: i64,
    pub login_attempts_before_lockout: i32,
    pub max_active_sessions: i32,
    pub authentication_provider_id: String,
    pub password_reset_provider_id: String,
    pub sync_play_access: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResultDto {
    pub user: UserDto,
    pub session_info: SessionInfoDto,
    pub access_token: String,
    pub server_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfoDto {
    pub id: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub client: String,
    pub last_activity_date: String,
    pub device_name: String,
    pub device_id: String,
    pub application_version: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LocalizationOptionDto {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CultureDto {
    pub name: String,
    pub display_name: String,
    #[serde(rename = "TwoLetterISOLanguageName")]
    pub two_letter_iso_language_name: String,
    #[serde(rename = "ThreeLetterISOLanguageName")]
    pub three_letter_iso_language_name: Option<String>,
    #[serde(rename = "ThreeLetterISOLanguageNames")]
    pub three_letter_iso_language_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CountryDto {
    pub name: String,
    pub display_name: String,
    #[serde(rename = "TwoLetterISORegionName")]
    pub two_letter_iso_region_name: String,
    #[serde(rename = "ThreeLetterISORegionName")]
    pub three_letter_iso_region_name: String,
}

#[cfg(test)]
mod tests {
    use super::{
        CountryDto, CultureDto, HealthResponse, PublicSystemInfo, StartupConfigurationDto,
    };
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn public_system_info_uses_jellyfin_pascal_case_contract() {
        let payload = PublicSystemInfo {
            id: Uuid::nil(),
            server_name: "Jellyrin".to_string(),
            version: "12.0.0".to_string(),
            product_name: "Jellyfin Server".to_string(),
            operating_system: "Linux".to_string(),
            local_address: "http://127.0.0.1:8097".to_string(),
            startup_wizard_completed: false,
        };

        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            json!({
                "Id": "00000000-0000-0000-0000-000000000000",
                "ServerName": "Jellyrin",
                "Version": "12.0.0",
                "ProductName": "Jellyfin Server",
                "OperatingSystem": "Linux",
                "LocalAddress": "http://127.0.0.1:8097",
                "StartupWizardCompleted": false
            })
        );
    }

    #[test]
    fn health_response_uses_jellyfin_pascal_case_contract() {
        assert_eq!(
            serde_json::to_value(HealthResponse { status: "Healthy" }).unwrap(),
            json!({ "Status": "Healthy" })
        );
    }

    #[test]
    fn startup_config_uses_jellyfin_pascal_case_contract() {
        let payload = StartupConfigurationDto {
            server_name: "Casa".to_string(),
            ui_culture: "es-ES".to_string(),
            metadata_country_code: "ES".to_string(),
            preferred_metadata_language: "es".to_string(),
        };

        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            json!({
                "ServerName": "Casa",
                "UICulture": "es-ES",
                "MetadataCountryCode": "ES",
                "PreferredMetadataLanguage": "es"
            })
        );
    }

    #[test]
    fn localization_uses_jellyfin_iso_acronym_contract() {
        assert_eq!(
            serde_json::to_value(CultureDto {
                name: "es".to_string(),
                display_name: "Spanish".to_string(),
                two_letter_iso_language_name: "es".to_string(),
                three_letter_iso_language_name: Some("spa".to_string()),
                three_letter_iso_language_names: vec!["spa".to_string()],
            })
            .unwrap(),
            json!({
                "Name": "es",
                "DisplayName": "Spanish",
                "TwoLetterISOLanguageName": "es",
                "ThreeLetterISOLanguageName": "spa",
                "ThreeLetterISOLanguageNames": ["spa"]
            })
        );

        assert_eq!(
            serde_json::to_value(CountryDto {
                name: "Spain".to_string(),
                display_name: "Spain".to_string(),
                two_letter_iso_region_name: "ES".to_string(),
                three_letter_iso_region_name: "ESP".to_string(),
            })
            .unwrap(),
            json!({
                "Name": "Spain",
                "DisplayName": "Spain",
                "TwoLetterISORegionName": "ES",
                "ThreeLetterISORegionName": "ESP"
            })
        );
    }
}
