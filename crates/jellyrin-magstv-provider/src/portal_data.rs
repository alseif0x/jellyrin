//! Sanitised MAGSTV portal contract recovered from the runtime DEX.
//!
//! The app sends these DTOs through a custom interceptor before they reach
//! the rotating portal hosts. The endpoint paths and JSON member names are
//! useful independently of that codec, so they live here as typed data. No
//! credentials, session tokens, host names, or encrypted bytes belong here.

use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

use crate::{
    MagstvCategory, MagstvChannel, MagstvLiveTvImport, MagstvMediaEpisode, MagstvMediaItem,
    MagstvMediaKind, MagstvProviderError, MagstvSecret, playback::MagstvLicenseGrant,
};

pub const MAGSTV_PORTAL_CONTENT_TYPE: &str = "application/json;charset=utf-8";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MagstvPortalEndpoint {
    Login,
    GetHome,
    GetLiveData,
    GetColumnContents,
    GetShelveData,
    GetItemData,
    GetAuthInfo,
    GetProgram,
    StartPlayLive,
    StartPlayVod,
    GetSlbInfo,
    TerminalAuth,
}

impl MagstvPortalEndpoint {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Login => "/api/portalCore/v8/login",
            Self::GetHome => "/api/portalCore/getHome",
            Self::GetLiveData => "/api/portalCore/v6/getLiveData",
            Self::GetColumnContents => "/api/portalCore/v3/getColumnContents",
            Self::GetShelveData => "/api/portalCore/v3/getShelveData",
            Self::GetItemData => "/api/portalCore/v4/getItemData",
            Self::GetAuthInfo => "/api/portalCore/v9/getAuthInfo",
            Self::GetProgram => "/api/portalCore/v3/getProgram",
            Self::StartPlayLive => "/api/portalCore/v4/startPlayLive",
            Self::StartPlayVod => "/api/portalCore/v10/startPlayVOD",
            // The current portal contract used by the installed 4.99.x APK
            // is v15.  v14 exists in older app DEX but returns an empty SLB
            // envelope for the current playback session.
            Self::GetSlbInfo => "/api/portalCore/v15/getSlbInfo",
            Self::TerminalAuth => "/api/portalCore/terminalAuth",
        }
    }

    /// Metadata only: these methods must pass through a verified codec before
    /// they can be sent to a real host.
    pub const fn requires_verified_codec(self) -> bool {
        true
    }

    pub const fn method(self) -> &'static str {
        "POST"
    }

    /// The runtime Retrofit annotations omit `needEncrypt:false` on these
    /// portal-core methods. That identifies the default encrypted path, but
    /// it is not a claim that the cipher has been reproduced here.
    pub const fn uses_app_request_codec(self) -> bool {
        true
    }

    /// The app marks these responses with `ProcessResult:false`; retain the
    /// distinction for the future codec instead of flattening all responses
    /// into one guessed format.
    pub const fn skips_app_response_processing(self) -> bool {
        matches!(
            self,
            Self::Login | Self::GetAuthInfo | Self::GetHome | Self::GetLiveData
        )
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvLoginRequest {
    pub account_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area_code: Option<String>,
    pub channel: String,
    pub mac_addr: String,
    /// The spelling is present in the app DTO and is part of the wire name.
    pub matadata: String,
    pub password: String,
    pub signdata: String,
    #[serde(rename = "type")]
    pub request_type: String,
    pub user_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_token: Option<String>,
}

impl MagstvLoginRequest {
    /// Reproduces the password-login DTO built by `fb/h.h0` in the decrypted
    /// runtime DEX. Gson omits the three null verification fields, but the
    /// seven non-null constructor arguments must be present even when empty.
    pub fn from_secret(
        secret: &MagstvSecret,
        mac_addr: impl Into<String>,
    ) -> Result<Self, MagstvProviderError> {
        secret.validate()?;
        // The app has two password-login presenters. `u9/a.q3` uses account
        // type 1 for phone numbers, while the email presenter `u9/c.P5`
        // sends account type 2. Keep this automatic so Jellyrin only asks
        // the operator for the same username and password they already use.
        let account_type = if secret.username.contains('@') {
            "2"
        } else {
            "1"
        };
        Ok(Self {
            account_type: account_type.to_string(),
            area_code: None,
            channel: "default".to_string(),
            mac_addr: mac_addr.into(),
            matadata: String::new(),
            password: combine_password(&secret.password),
            signdata: String::new(),
            request_type: "1".to_string(),
            user_name: secret.username.clone(),
            user_token: None,
            verification_code: None,
            verification_token: None,
        })
    }
}

/// Product adapter `p6/a.Y` transforms the password before either phone or
/// email login. The wire value is MD5(password + product salt) followed by
/// AES-128-ECB/PKCS5(password), encoded with standard Base64.
fn combine_password(password: &str) -> String {
    const AES_KEY: &[u8; 16] = b"ntFT65w6itH!lHCP";
    const MD5_SALT: &str = "ntFT65w6itH!lHCPw7D=@qnsFC5adD28";

    let digest = Md5::digest(format!("{password}{MD5_SALT}").as_bytes());
    let mut encrypted = password.as_bytes().to_vec();
    let padding = 16 - encrypted.len() % 16;
    encrypted.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher = Aes128::new(GenericArray::from_slice(AES_KEY));
    for block in encrypted.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    format!("{digest:x}{}", BASE64.encode(encrypted))
}

#[cfg(test)]
mod login_request_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn password_login_contains_the_runtime_required_fields() {
        let request = MagstvLoginRequest::from_secret(
            &MagstvSecret::new("subscriber", "secret"),
            "02:00:00:00:00:01",
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["accountType"], json!("1"));
        assert_eq!(value["type"], json!("1"));
        assert_eq!(value["channel"], json!("default"));
        assert_eq!(value["macAddr"], json!("02:00:00:00:00:01"));
        assert_eq!(value["matadata"], json!(""));
        assert_eq!(value["signdata"], json!(""));
        assert!(value.get("userToken").is_none());
        assert!(value.get("verificationCode").is_none());
    }

    #[test]
    fn email_login_uses_the_runtime_email_account_type() {
        let request = MagstvLoginRequest::from_secret(
            &MagstvSecret::new("subscriber@example.test", "secret"),
            "02:00:00:00:00:01",
        )
        .unwrap();

        assert_eq!(serde_json::to_value(request).unwrap()["accountType"], "2");
    }

    #[test]
    fn password_uses_the_product_adapter_wire_transform() {
        assert_eq!(
            combine_password("secret"),
            "c75a2fc371bb7474fe260e9c5c5e0c2bAOIdY3eVkBh5YlY2Un7c0w=="
        );
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetHomeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_page_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetLiveDataRequest {
    pub column_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_time_str: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_num: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetColumnContentsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_av1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_display: Option<i32>,
    pub page_num: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub special_flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetShelveRequest {
    pub user_token: String,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    pub column_id: i32,
    pub column_type: String,
    pub page_size: i32,
    pub page_num: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_display: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypt_version: Option<i32>,
}

/// Detail request used by the Android VOD screen.  Unlike shelf cards, this
/// response contains the episode list and the audio/subtitle capabilities of
/// the selected asset.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetItemDataRequest {
    pub content_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub mac_addr: String,
    pub portal_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_type: Option<String>,
    #[serde(rename = "type")]
    pub request_type: String,
    pub user_id: String,
    pub user_token: String,
}

/// Session entitlement state requested by the Android client immediately
/// after login.  It is kept separate from login because the VOD service uses
/// the resulting account state when authorising detail/play requests.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetAuthInfoRequest {
    pub user_token: String,
    pub user_id: String,
    #[serde(rename = "type")]
    pub request_type: String,
    pub portal_code: String,
    pub lang: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvStartPlayLiveRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvProgramRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_code: Option<String>,
    pub column_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvStartPlayVodRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    pub column_id: i32,
    pub content_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number_list: Option<Vec<i32>>,
    pub portal_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_content_id: Option<String>,
    pub start_time: i32,
    #[serde(rename = "type")]
    pub request_type: String,
    pub user_id: String,
    pub user_token: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSlbInfoRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_params: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_ver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enc_media_supported: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_pay: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_code_list: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pip_flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve1: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvPortalResponse<T> {
    pub return_code: Option<String>,
    pub error_message: Option<String>,
    pub data: Option<T>,
    pub total_size: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvLoginData {
    pub portal_code_list: Option<Vec<MagstvPortalCode>>,
    pub token: Option<String>,
    pub user_id: Option<String>,
    pub user_token: Option<String>,
}

impl MagstvLoginData {
    /// Extracts the runtime identity needed by later portal DTOs. Values are
    /// copied into a short-lived object; callers should not persist or log it.
    pub fn identity(&self) -> Result<MagstvPortalIdentity, MagstvProviderError> {
        let user_id = self
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let user_token = self
            .user_token
            .as_deref()
            .or(self.token.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (Some(user_id), Some(user_token)) = (user_id, user_token) else {
            return Err(MagstvProviderError::MissingPortalIdentity {
                user_id_present: user_id.is_some(),
                token_present: user_token.is_some(),
            });
        };
        let portal_code = self.portal_code_list.as_ref().and_then(|codes| {
            codes.iter().find_map(|code| {
                code.portal_code
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        });
        Ok(MagstvPortalIdentity {
            user_id: user_id.to_string(),
            user_token: user_token.to_string(),
            portal_code,
        })
    }
}

/// Runtime-only identity returned by the portal login. Its Debug
/// implementation intentionally redacts both identifiers.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvPortalIdentity {
    user_id: String,
    user_token: String,
    portal_code: Option<String>,
}

impl std::fmt::Debug for MagstvPortalIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MagstvPortalIdentity")
            .field("user_id", &"[REDACTED]")
            .field("user_token", &"[REDACTED]")
            .field("portal_code", &self.portal_code)
            .finish()
    }
}

impl MagstvPortalIdentity {
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn user_token(&self) -> &str {
        &self.user_token
    }

    pub fn portal_code(&self) -> Option<&str> {
        self.portal_code.as_deref()
    }

    pub fn column_contents_request(
        &self,
        column_id: Option<i32>,
        page_num: i32,
        page_size: Option<i32>,
    ) -> MagstvGetColumnContentsRequest {
        MagstvGetColumnContentsRequest {
            column_id,
            is_av1: None,
            num_display: None,
            page_num,
            page_size,
            portal_code: self.portal_code.clone(),
            special_flag: None,
            user_id: Some(self.user_id.clone()),
            user_token: Some(self.user_token.clone()),
        }
    }

    pub fn live_data_request(
        &self,
        column_id: i32,
        page_num: Option<i32>,
        page_size: Option<i32>,
    ) -> MagstvGetLiveDataRequest {
        MagstvGetLiveDataRequest {
            column_id,
            data_version: None,
            expire_time_str: None,
            page_num,
            page_size,
            portal_code: self.portal_code.clone(),
            user_id: Some(self.user_id.clone()),
            user_token: Some(self.user_token.clone()),
        }
    }

    pub fn shelve_request(
        &self,
        column_id: i32,
        column_type: impl Into<String>,
        page_num: i32,
        page_size: i32,
    ) -> MagstvGetShelveRequest {
        MagstvGetShelveRequest {
            user_token: self.user_token.clone(),
            user_id: self.user_id.clone(),
            portal_code: self.portal_code.clone(),
            column_id,
            column_type: column_type.into(),
            page_size,
            page_num,
            num_display: None,
            // The APK passes a boxed null for numDisplay and the primitive
            // value 0 for encryptVersion when loading ordinary VOD shelves.
            encrypt_version: Some(0),
        }
    }

    pub fn item_data_request(
        &self,
        content_id: impl Into<String>,
        request_type: impl Into<String>,
        sort_type: Option<String>,
        language: Option<String>,
        mac_addr: impl Into<String>,
    ) -> MagstvGetItemDataRequest {
        MagstvGetItemDataRequest {
            content_id: content_id.into(),
            language,
            mac_addr: mac_addr.into(),
            portal_code: self.portal_code.clone().unwrap_or_default(),
            sort_type,
            request_type: request_type.into(),
            user_id: self.user_id.clone(),
            user_token: self.user_token.clone(),
        }
    }

    pub fn auth_info_request(
        &self,
        request_type: impl Into<String>,
        lang: impl Into<String>,
    ) -> MagstvGetAuthInfoRequest {
        MagstvGetAuthInfoRequest {
            user_token: self.user_token.clone(),
            user_id: self.user_id.clone(),
            request_type: request_type.into(),
            portal_code: self.portal_code.clone().unwrap_or_default(),
            lang: lang.into(),
        }
    }

    pub fn slb_info_request(
        &self,
        app_version: impl Into<String>,
        has_pay: Option<String>,
        lang: impl Into<String>,
        user_identity: Option<String>,
        request_type: impl Into<String>,
        app_params: Option<String>,
    ) -> MagstvSlbInfoRequest {
        MagstvSlbInfoRequest {
            app_params,
            app_ver: Some(app_version.into()),
            enc_media_supported: Some(0),
            has_pay,
            lang: Some(lang.into()),
            live_code_list: None,
            pip_flag: Some("0".to_string()),
            portal_code: self.portal_code.clone(),
            reserve1: None,
            request_type: Some(request_type.into()),
            user_id: Some(self.user_id.clone()),
            user_identity,
            user_token: Some(self.user_token.clone()),
        }
    }

    pub fn start_play_vod_request(
        &self,
        column_id: i32,
        content_id: impl Into<String>,
        series_content_id: Option<String>,
        request_type: impl Into<String>,
        start_time: i32,
        auth_type: Option<String>,
        episode_number_list: Option<Vec<i32>>,
    ) -> MagstvStartPlayVodRequest {
        MagstvStartPlayVodRequest {
            auth_type,
            column_id,
            content_id: content_id.into(),
            episode_number_list,
            portal_code: self.portal_code.clone().unwrap_or_default(),
            series_content_id,
            start_time,
            request_type: request_type.into(),
            user_id: self.user_id.clone(),
            user_token: self.user_token.clone(),
        }
    }

    pub fn program_request(
        &self,
        channel_code: Option<String>,
        column_id: i32,
    ) -> MagstvProgramRequest {
        MagstvProgramRequest {
            channel_code,
            column_id,
            portal_code: self.portal_code.clone(),
            request_type: None,
            user_id: Some(self.user_id.clone()),
            user_token: Some(self.user_token.clone()),
        }
    }

    pub fn start_play_live_request(
        &self,
        channel_code: Option<String>,
        column_id: Option<i32>,
    ) -> MagstvStartPlayLiveRequest {
        MagstvStartPlayLiveRequest {
            channel_code,
            column_id,
            portal_code: self.portal_code.clone(),
            request_type: None,
            user_id: Some(self.user_id.clone()),
            user_token: Some(self.user_token.clone()),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvAuthInfo {
    pub content_id: Option<String>,
    pub effect_time: Option<String>,
    pub exp_invalid_time: Option<String>,
    pub invalid_time: Option<String>,
    pub name: Option<String>,
    pub price: Option<f32>,
    pub product_code: Option<String>,
    pub product_name: Option<String>,
    pub service_type: Option<String>,
    pub status: Option<String>,
    pub sub_time: Option<String>,
    pub user_name: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvPortalCode {
    pub portal_code: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvLiveData {
    pub apk_number_switch: Option<String>,
    #[serde(default)]
    pub channel_list: Vec<MagstvPortalChannel>,
    pub channel_list_total_size: Option<i32>,
    pub data_version: Option<String>,
    pub expire_time_str: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvPortalChannel {
    pub alias: Option<String>,
    pub channel_code: Option<String>,
    pub channel_number: Option<i32>,
    pub fixed_channel_number: Option<String>,
    pub is_fav: Option<bool>,
    pub is_lock: Option<bool>,
    pub key_words: Option<String>,
    #[serde(default)]
    pub live_address_list: Vec<MagstvLiveAddress>,
    #[serde(default)]
    pub mosaic_channe_array: Vec<Value>,
    pub mosaic_channel_list: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub poster_list: Vec<MagstvPoster>,
    pub poster_url: Option<String>,
    pub quality: Option<String>,
    pub restricted: Option<String>,
    pub show_channel_name: Option<String>,
    pub show_icon_url: Option<String>,
    pub show_poster_url: Option<String>,
    pub support_business: Option<String>,
    pub support_video_type: Option<String>,
    pub tags: Option<String>,
}

impl MagstvPortalChannel {
    /// Converts only the safe catalog identity. Playback addresses remain on
    /// the portal record and are not accidentally persisted as catalog data.
    pub fn into_catalog_channel(self, category_id: impl Into<String>) -> Option<MagstvChannel> {
        let id = self
            .channel_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let name = self
            .show_channel_name
            .as_deref()
            .or(self.name.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let logo_url = self
            .show_icon_url
            .or(self.show_poster_url)
            .or(self.poster_url);
        Some(MagstvChannel {
            id,
            name,
            category_id: category_id.into(),
            number: self
                .fixed_channel_number
                .or_else(|| self.channel_number.map(|number| number.to_string())),
            logo_url,
        })
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvLiveAddress {
    #[serde(rename = "AVFormat")]
    pub av_format: Option<String>,
    pub cdn_type: Option<String>,
    pub license: Option<String>,
    pub play_code: Option<String>,
    pub quality: Option<String>,
    pub tag: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvPoster {
    pub channel_code: Option<String>,
    pub file_type: Option<String>,
    pub file_url: Option<String>,
    pub name: Option<String>,
    pub size: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvStartPlayLiveData {
    #[serde(default)]
    pub live_address_list: Vec<MagstvLiveAddress>,
    pub name: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvColumnContentsData {
    #[serde(default)]
    pub child_column_list: Vec<MagstvChildColumn>,
    pub expire_time_str: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvShelveData {
    #[serde(default)]
    pub asset_list: Vec<MagstvAsset>,
    pub asset_list_total_size: Option<i32>,
    pub version: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetItemDataData {
    pub asset_data: Option<MagstvAssetData>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvGetAuthInfoData {
    #[serde(default)]
    pub auth_info_list: Vec<MagstvAuthInfo>,
    pub has_pay: Option<String>,
    pub restricted_status: Option<String>,
    pub remaining_days: Option<i32>,
    pub user_identity: Option<String>,
    pub user_type: Option<String>,
}

/// Runtime VOD detail.  Fields not needed by Jellyrin are deliberately left
/// out; serde ignores the rest of the app's large AssetData object.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvAssetData {
    pub audio_info: Option<String>,
    pub content_id: Option<String>,
    pub description: Option<String>,
    pub duration: Option<String>,
    pub language: Option<String>,
    pub more_audio: Option<i32>,
    pub more_subtitle: Option<i32>,
    pub name: Option<String>,
    #[serde(default)]
    pub poster_list: Vec<MagstvPoster>,
    pub program_type: Option<String>,
    pub score: Option<f32>,
    #[serde(default)]
    pub same_season_series_list: Vec<MagstvSameSeasonSeries>,
    #[serde(default)]
    pub simple_program_list: Vec<MagstvSimpleProgram>,
    pub subs_info: Option<String>,
    pub tags: Option<String>,
    pub update_count: Option<i32>,
    pub view_point: Option<String>,
    pub volumn_count: Option<i32>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSimpleProgram {
    pub content_id: Option<String>,
    pub duration: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub poster_list: Vec<MagstvPoster>,
    pub quality: Option<String>,
    pub series_number: Option<i32>,
    pub view_point: Option<String>,
}

impl MagstvAssetData {
    /// Converts the portal's `simpleProgramList` into Jellyfin episode
    /// descriptors. It deliberately carries no playback address, license,
    /// token, or subtitle URL; those are short-lived JIT data.
    pub fn into_episode_items(
        &self,
        series_content_id: impl Into<String>,
        series_name: impl Into<String>,
        season_number: Option<i32>,
        column_id: Option<i32>,
        request_type: Option<String>,
    ) -> Vec<MagstvMediaEpisode> {
        let series_content_id = series_content_id.into();
        let series_name = series_name.into();
        self.simple_program_list
            .iter()
            .enumerate()
            .filter_map(|(index, program)| {
                let id = program
                    .content_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?
                    .to_string();
                let episode_number = program
                    .series_number
                    .filter(|number| *number > 0)
                    .or_else(|| i32::try_from(index + 1).ok());
                let name = program
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&id)
                    .to_string();
                let image_url = program.poster_list.iter().find_map(|poster| {
                    poster.file_url.as_deref().filter(|url| {
                        let url = url.trim();
                        url.starts_with("http://") || url.starts_with("https://")
                    })
                });
                Some(MagstvMediaEpisode {
                    id,
                    name,
                    series_content_id: series_content_id.clone(),
                    series_name: series_name.clone(),
                    season_number,
                    episode_number,
                    overview: None,
                    image_url: image_url.map(ToOwned::to_owned),
                    duration_seconds: program.duration.as_deref().and_then(parse_duration_seconds),
                    column_id,
                    request_type: request_type.clone(),
                })
            })
            .collect()
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSameSeasonSeries {
    pub content_id: Option<String>,
    pub season_number: Option<i32>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvAsset {
    pub content_id: Option<String>,
    pub content_type: Option<String>,
    pub description: Option<String>,
    pub duration: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub poster_list: Vec<MagstvPoster>,
    pub program_type: Option<String>,
    pub score: Option<f32>,
    pub tags: Option<String>,
    /// Navigation/detail request type (`AssetList.type` in the APK).  This
    /// is distinct from `contentType`; the VOD detail screen forwards this
    /// value as the `type` member of getItemData.
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub update_count: Option<i32>,
    pub volumn_count: Option<i32>,
}

impl MagstvAsset {
    pub fn into_media_item(self, kind: MagstvMediaKind) -> Option<MagstvMediaItem> {
        let request_type = self.r#type.clone();
        let id = self.content_id.filter(|value| !value.trim().is_empty())?;
        let name = self
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let image_url = self.poster_list.into_iter().find_map(|poster| {
            poster.file_url.filter(|url| {
                let url = url.trim();
                url.starts_with("http://") || url.starts_with("https://")
            })
        });
        let duration_seconds = self.duration.as_deref().and_then(parse_duration_seconds);
        let genres = self
            .tags
            .as_deref()
            .map(|tags| {
                tags.split([',', '/', '|'])
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(MagstvMediaItem {
            id,
            name,
            kind,
            overview: self.description.filter(|value| !value.trim().is_empty()),
            image_url,
            duration_seconds,
            community_rating: self.score.map(f64::from),
            genres,
            column_id: None,
            request_type,
        })
    }
}

fn parse_duration_seconds(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let parts = value
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [minutes, seconds] => Some(minutes.saturating_mul(60).saturating_add(*seconds)),
        [hours, minutes, seconds] => Some(
            hours
                .saturating_mul(3600)
                .saturating_add(minutes.saturating_mul(60))
                .saturating_add(*seconds),
        ),
        _ => None,
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvChildColumn {
    #[serde(rename = "Sequence")]
    pub sequence: Option<i32>,
    pub alias: Option<String>,
    pub brief: Option<String>,
    pub code: Option<String>,
    pub free: Option<String>,
    pub id: Option<i32>,
    pub is_av1: Option<String>,
    pub name: Option<String>,
    pub order_flag: Option<String>,
    pub parent_id: Option<i32>,
    #[serde(default)]
    pub poster_list: Vec<MagstvPoster>,
    pub recmd_title: Option<String>,
    pub recommend_num: Option<i32>,
    pub remark: Option<String>,
    pub restricted: Option<String>,
    pub style: Option<String>,
    pub time_notice: Option<String>,
    pub try_see: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
}

impl MagstvChildColumn {
    pub fn into_catalog_category(self) -> Option<MagstvCategory> {
        let id = self
            .code
            .or_else(|| self.id.map(|id| id.to_string()))
            .filter(|value| !value.trim().is_empty())?;
        let name = self
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        Some(MagstvCategory { id, name })
    }
}

impl MagstvLiveData {
    /// Maps a verified live-data response to the provider's catalogue DTO.
    /// Address material is intentionally not copied into the persisted
    /// catalogue; playback must be resolved just in time.
    pub fn into_live_tv_import(
        self,
        category_id: impl Into<String>,
        category_name: impl Into<String>,
    ) -> MagstvLiveTvImport {
        let category_id = category_id.into();
        let category_name = category_name.into();
        let channels = self
            .channel_list
            .into_iter()
            .filter_map(|channel| channel.into_catalog_channel(category_id.clone()))
            .collect();
        MagstvLiveTvImport {
            categories: vec![MagstvCategory {
                id: category_id,
                name: category_name,
            }],
            channels,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvEpgData {
    pub alias: Option<String>,
    pub channel_code: Option<String>,
    pub channel_number: Option<String>,
    pub key_words: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub program_list: Vec<MagstvEpgProgram>,
    pub restricted: Option<String>,
    pub support_business: Option<String>,
    pub tags: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvEpgProgram {
    pub content_id: Option<String>,
    pub desc: Option<String>,
    pub end_time: Option<String>,
    pub program_name: Option<String>,
    pub remark: Option<String>,
    pub start_time: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvStartPlayVodData {
    #[serde(default)]
    pub episode_list: Vec<MagstvStartPlayVodItem>,
    pub name: Option<String>,
    pub series_flag: Option<String>,
    pub vod_free_count: Option<i32>,
    pub vod_free_flag: Option<String>,
}

impl MagstvStartPlayVodData {
    /// Selects the same playback variant as [`Self::jellyfin_media_streams`]
    /// so capability descriptors and the eventual CDN authorisation always
    /// refer to one variant.
    fn selected_variant(&self, program_content_id: Option<&str>) -> Option<&MagstvMovieListItem> {
        let episode = program_content_id
            .and_then(|content_id| {
                self.episode_list
                    .iter()
                    .find(|episode| episode.program_content_id.as_deref() == Some(content_id))
            })
            .or_else(|| self.episode_list.first())?;
        episode
            .total_movie_list
            .iter()
            .flat_map(|group| group.movie_list.iter())
            .find(|movie| {
                movie
                    .content_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
            })
    }

    /// Returns the first parseable license grant of the selected variant.
    /// Unparseable entries are skipped: they usually target other player
    /// revisions and cannot authorise this implementation anyway.
    pub fn selected_license_grant(
        &self,
        program_content_id: Option<&str>,
    ) -> Option<MagstvLicenseGrant> {
        self.selected_variant(program_content_id)
            .map(MagstvMovieListItem::license_grants)
            .and_then(|grants| grants.into_iter().next())
    }

    /// Resolves the subtitle file URL for one language of the selected
    /// episode. The URL is runtime-only: it is used immediately by the
    /// subtitle proxy and never persisted.
    pub fn subtitle_file_url(
        &self,
        program_content_id: Option<&str>,
        language: &str,
    ) -> Option<String> {
        let episode = program_content_id
            .and_then(|content_id| {
                self.episode_list
                    .iter()
                    .find(|episode| episode.program_content_id.as_deref() == Some(content_id))
            })
            .or_else(|| self.episode_list.first())?;
        episode
            .subtitle_list
            .iter()
            .filter(|subtitle| {
                subtitle
                    .language
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(language.trim()))
            })
            .flat_map(|subtitle| subtitle.file.iter())
            .find_map(|file| {
                file.url
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| !url.is_empty() && !url.chars().any(char::is_control))
                    .map(str::to_string)
            })
    }

    /// Maps the portal's authorised playback capabilities to Jellyfin stream
    /// descriptors. This contains only labels and opaque provider content
    /// identifiers: short-lived media URLs and licence material are never
    /// copied into the descriptor or persisted in the catalogue.
    pub fn jellyfin_media_streams(&self, program_content_id: Option<&str>) -> Vec<Value> {
        let episode = program_content_id
            .and_then(|content_id| {
                self.episode_list
                    .iter()
                    .find(|episode| episode.program_content_id.as_deref() == Some(content_id))
            })
            .or_else(|| self.episode_list.first());
        let Some(episode) = episode else {
            return Vec::new();
        };

        let mut streams = Vec::new();

        if let Some(variant) = self.selected_variant(program_content_id) {
            let codec = variant
                .video_format
                .as_deref()
                .or(variant.encode_format.as_deref())
                .or(variant.video_type.as_deref())
                .unwrap_or("unknown");
            streams.push(serde_json::json!({
                "Index": 0,
                "Type": "Video",
                "Codec": codec,
                "DisplayTitle": variant
                    .quality
                    .as_deref()
                    .or(variant.screen_format.as_deref())
                    .unwrap_or("Video"),
                "IsDefault": true,
                "IsForced": false,
                "IsExternal": false,
                "MagstvVariantContentId": variant.content_id,
            }));

            let audio_codec = variant.audio_type.as_deref().unwrap_or("unknown");
            // Keep the portal's audio order: it matches the audio PID order of
            // the transport stream, so the descriptor index doubles as the
            // ffmpeg input stream index at transcode time.
            let mut audio_labels: Vec<String> = Vec::new();
            for movie in episode
                .total_movie_list
                .iter()
                .flat_map(|group| group.movie_list.iter())
            {
                if let Some(audio_info) = movie.audio_info.as_deref() {
                    for label in audio_info
                        .split(',')
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                    {
                        if !audio_labels.iter().any(|existing| existing == label) {
                            audio_labels.push(label.to_owned());
                        }
                    }
                }
            }
            for (offset, language) in audio_labels.into_iter().enumerate() {
                streams.push(serde_json::json!({
                    "Index": i64::try_from(offset + 1).unwrap_or(i64::MAX),
                    "Type": "Audio",
                    "Codec": audio_codec,
                    "Language": language,
                    "DisplayTitle": language,
                    "IsDefault": offset == 0,
                    "IsForced": false,
                    "IsExternal": false,
                    "MagstvVariantContentId": variant.content_id,
                }));
            }
        }

        let mut subtitle_languages = BTreeSet::new();
        for subtitle in &episode.subtitle_list {
            let language = subtitle
                .language
                .as_deref()
                .map(str::trim)
                .filter(|language| !language.is_empty());
            let Some(language) = language else {
                continue;
            };
            let title = subtitle
                .trans_language
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or(language);
            if !subtitle_languages.insert(language.to_string()) {
                continue;
            }
            let file_type = subtitle
                .file
                .iter()
                .find_map(|file| {
                    file.file_type
                        .as_deref()
                        .map(str::trim)
                        .filter(|file_type| !file_type.is_empty())
                })
                .unwrap_or("text");
            let index = i64::try_from(streams.len()).unwrap_or(i64::MAX);
            streams.push(serde_json::json!({
                "Index": index,
                "Type": "Subtitle",
                "Codec": file_type,
                "Language": language,
                "Title": title,
                "DisplayTitle": title,
                "IsDefault": false,
                "IsForced": false,
                "IsExternal": true,
                "IsTextSubtitleStream": true,
                "SupportsExternalStream": true,
                "MagstvSubtitleLanguage": language,
            }));
        }

        streams
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvStartPlayVodItem {
    pub episode_number: Option<i32>,
    pub program_content_id: Option<String>,
    #[serde(default)]
    pub subtitle_list: Vec<MagstvSubtitleItem>,
    #[serde(default)]
    pub total_movie_list: Vec<MagstvTotalMovieListItem>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvTotalMovieListItem {
    pub quality: Option<String>,
    #[serde(default)]
    pub movie_list: Vec<MagstvMovieListItem>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvMovieListItem {
    pub content_id: Option<String>,
    pub terminal_type: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub audio_type: Option<String>,
    pub video_type: Option<String>,
    pub screen_format: Option<String>,
    pub encode_format: Option<String>,
    pub video_format: Option<String>,
    pub bit_rate_type: Option<String>,
    pub quality: Option<String>,
    pub audio_info: Option<String>,
    #[serde(default)]
    pub license_list: Vec<Value>,
    pub volume: Option<String>,
}

impl MagstvMovieListItem {
    /// Parses each `license_list` entry into a typed grant. Every entry is a
    /// JSON object whose `license` member is the query string described by
    /// the authorised runtime. Entries that do not fit the verified contract
    /// are skipped instead of being coerced.
    pub fn license_grants(&self) -> Vec<MagstvLicenseGrant> {
        self.license_list
            .iter()
            .filter_map(|entry| entry.get("license")?.as_str())
            .filter_map(|license| MagstvLicenseGrant::parse(license).ok())
            .collect()
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSubtitleItem {
    pub language: Option<String>,
    #[serde(default)]
    pub file: Vec<MagstvSubtitleFile>,
    pub trans_language: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSubtitleFile {
    pub url: Option<String>,
    pub file_type: Option<String>,
    pub md5: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagstvSlbInfoData {
    #[serde(default)]
    pub cdn_list: Vec<Value>,
    pub error_code: Option<i32>,
    pub expire_time_str: Option<String>,
    pub invalid_time: Option<String>,
    pub merge_rst_status: Option<i32>,
    pub now_time: Option<String>,
    pub play_params: Option<String>,
    pub reserve_a: Option<String>,
    pub reserve_b: Option<String>,
    pub rst_status: Option<i32>,
    pub switch_live_source_time: Option<i32>,
    pub switch_live_source_time_v2: Option<String>,
    pub switch_vod_source_time: Option<i32>,
    pub switch_vod_source_time_v2: Option<String>,
}

pub fn parse_portal_response<T: for<'de> Deserialize<'de>>(
    body: &[u8],
) -> Result<MagstvPortalResponse<T>, MagstvProviderError> {
    serde_json::from_slice(body).map_err(|_| MagstvProviderError::InvalidPortalPayload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_paths_match_runtime_retrofit_contract() {
        assert_eq!(
            MagstvPortalEndpoint::Login.path(),
            "/api/portalCore/v8/login"
        );
        assert_eq!(
            MagstvPortalEndpoint::GetLiveData.path(),
            "/api/portalCore/v6/getLiveData"
        );
        assert_eq!(
            MagstvPortalEndpoint::StartPlayLive.path(),
            "/api/portalCore/v4/startPlayLive"
        );
        assert_eq!(
            MagstvPortalEndpoint::GetSlbInfo.path(),
            "/api/portalCore/v15/getSlbInfo"
        );
        assert_eq!(MagstvPortalEndpoint::Login.method(), "POST");
        assert!(MagstvPortalEndpoint::Login.uses_app_request_codec());
        assert!(MagstvPortalEndpoint::Login.skips_app_response_processing());
        assert!(!MagstvPortalEndpoint::StartPlayLive.skips_app_response_processing());
    }

    #[test]
    fn request_serialization_keeps_runtime_json_member_names() {
        let request = MagstvGetLiveDataRequest {
            column_id: 26,
            data_version: None,
            expire_time_str: None,
            page_num: Some(1),
            page_size: Some(100),
            portal_code: Some("live".to_string()),
            user_id: Some("user-id".to_string()),
            user_token: Some("runtime-token".to_string()),
        };
        let value = serde_json::to_value(request).expect("serializable request");
        assert_eq!(value["columnId"], 26);
        assert_eq!(value["pageNum"], 1);
        assert_eq!(value["pageSize"], 100);
        assert_eq!(value["portalCode"], "live");
        assert!(value.get("dataVersion").is_none());
    }

    #[test]
    fn live_response_decodes_channels_and_playback_addresses() {
        let body = json!({
            "returnCode": "0",
            "errorMessage": null,
            "data": {
                "dataVersion": "v1",
                "channelList": [{
                    "channelCode": "mx-news",
                    "channelNumber": 7,
                    "name": "News",
                    "showIconUrl": "https://images.invalid/news.png",
                    "liveAddressList": [{
                        "AVFormat": "hls",
                        "cdnType": "cdn",
                        "playCode": "opaque-play-code"
                    }]
                }]
            }
        });
        let parsed: MagstvPortalResponse<MagstvLiveData> =
            parse_portal_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        let data = parsed.data.unwrap();
        let channel = &data.channel_list[0];
        assert_eq!(channel.channel_code.as_deref(), Some("mx-news"));
        assert_eq!(
            channel.live_address_list[0].av_format.as_deref(),
            Some("hls")
        );
        assert_eq!(
            channel
                .clone()
                .into_catalog_channel("live")
                .unwrap()
                .logo_url
                .as_deref(),
            Some("https://images.invalid/news.png")
        );

        let import = data.into_live_tv_import("live", "Live TV");
        assert_eq!(import.channels.len(), 1);
        assert_eq!(import.categories[0].name, "Live TV");
    }

    #[test]
    fn login_identity_requires_runtime_user_id_and_token() {
        let login = MagstvLoginData {
            portal_code_list: Some(vec![MagstvPortalCode {
                portal_code: Some("live".to_string()),
                r#type: None,
            }]),
            token: None,
            user_id: Some("user-id".to_string()),
            user_token: Some("runtime-token".to_string()),
        };
        let identity = login.identity().unwrap();
        assert_eq!(identity.portal_code(), Some("live"));
        assert!(!format!("{identity:?}").contains("runtime-token"));
    }

    #[test]
    fn malformed_portal_payload_fails_closed() {
        let result = parse_portal_response::<MagstvLiveData>(b"not-json");
        assert!(matches!(
            result,
            Err(MagstvProviderError::InvalidPortalPayload)
        ));
    }

    #[test]
    fn playback_capabilities_expose_audio_and_subtitles_without_urls() {
        let data: MagstvStartPlayVodData = serde_json::from_value(json!({
            "episodeList": [{
                "programContentId": "episode-1",
                "subtitleList": [{
                    "language": "es",
                    "transLanguage": "Español",
                    "file": [{"url": "https://temporary.invalid/sub.vtt", "fileType": "vtt"}]
                }, {
                    "language": "en",
                    "transLanguage": "English",
                    "file": [{"url": "https://temporary.invalid/sub-en.vtt", "fileType": "vtt"}]
                }],
                "totalMovieList": [{
                    "quality": "1080P",
                    "movieList": [{
                        "contentId": "variant-1",
                        "videoFormat": "h264",
                        "audioType": "aac",
                        "audioInfo": "es,en"
                    }]
                }]
            }]
        }))
        .unwrap();

        let streams = data.jellyfin_media_streams(Some("episode-1"));
        assert_eq!(
            streams
                .iter()
                .filter(|stream| stream["Type"] == "Video")
                .count(),
            1
        );
        assert_eq!(
            streams
                .iter()
                .filter(|stream| stream["Type"] == "Audio")
                .count(),
            2
        );
        assert_eq!(
            streams
                .iter()
                .filter(|stream| stream["Type"] == "Subtitle")
                .count(),
            2
        );
        assert!(streams.iter().all(|stream| stream.get("Url").is_none()));
        assert!(streams.iter().all(|stream| {
            serde_json::to_string(stream)
                .unwrap()
                .find("temporary.invalid")
                .is_none()
        }));
    }
}
