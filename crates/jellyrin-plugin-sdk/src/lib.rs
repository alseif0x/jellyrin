//! Stable types for Jellyrin Rust/WASI plugins.
//!
//! The SDK intentionally exposes JSON-compatible data structures first. That
//! keeps the plugin ABI narrow while the WASI runtime matures and lets fixtures
//! produce manifests and capability responses that the sidecar host can load.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use zeroize::Zeroize;

pub const TARGET_ABI: &str = "jellyrin-wasi-0.1";
pub const CAPABILITY_SCHEDULED_TASK: &str = "ScheduledTask";
pub const CAPABILITY_METADATA_PROVIDER: &str = "MetadataProvider";
pub const CAPABILITY_IMAGE_PROVIDER: &str = "ImageProvider";
pub const CAPABILITY_CHANNEL_PROVIDER: &str = "ChannelProvider";
pub const CAPABILITY_LIVE_TV_PROVIDER: &str = "LiveTvProvider";
/// Permission a plugin must request in its manifest, and an administrator must grant, before
/// Jellyrin may issue ephemeral provider-secret grants to it.
pub const PERMISSION_PROVIDER_SECRETS: &str = "ProviderSecrets";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScheduledTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default)]
    pub arguments: Value,
}

impl ScheduledTaskRequest {
    pub fn manual() -> Self {
        Self {
            trigger: Some("Manual".to_string()),
            arguments: json!({}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ScheduledTaskResult {
    pub task_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_processed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ScheduledTaskResult {
    pub fn completed(task_name: impl Into<String>) -> Self {
        Self {
            task_name: task_name.into(),
            items_processed: None,
            message: None,
        }
    }

    pub fn items_processed(mut self, items_processed: u64) -> Self {
        self.items_processed = Some(items_processed);
        self
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MetadataLookupRequest {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub provider_ids: Map<String, Value>,
}

impl MetadataLookupRequest {
    pub fn new(item_id: impl Into<String>) -> Self {
        Self {
            item_id: item_id.into(),
            name: None,
            provider_id: None,
            provider_ids: Map::new(),
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MetadataResult {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default)]
    pub provider_ids: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_rating: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_rating: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub studios: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<MetadataPerson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MetadataPerson {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_type: Option<String>,
}

impl MetadataResult {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            overview: None,
            genres: Vec::new(),
            provider_ids: Map::new(),
            production_year: None,
            community_rating: None,
            official_rating: None,
            studios: Vec::new(),
            people: Vec::new(),
            image_url: None,
            premiere_date: None,
            tagline: None,
        }
    }

    pub fn overview(mut self, overview: impl Into<String>) -> Self {
        self.overview = Some(overview.into());
        self
    }

    pub fn genre(mut self, genre: impl Into<String>) -> Self {
        self.genres.push(genre.into());
        self
    }

    pub fn production_year(mut self, year: i32) -> Self {
        self.production_year = Some(year);
        self
    }

    pub fn community_rating(mut self, rating: f64) -> Self {
        self.community_rating = Some(rating);
        self
    }

    pub fn official_rating(mut self, rating: impl Into<String>) -> Self {
        self.official_rating = Some(rating.into());
        self
    }

    pub fn studio(mut self, studio: impl Into<String>) -> Self {
        self.studios.push(studio.into());
        self
    }

    pub fn person(
        mut self,
        name: impl Into<String>,
        role: Option<String>,
        person_type: Option<String>,
    ) -> Self {
        self.people.push(MetadataPerson {
            name: name.into(),
            role,
            person_type,
        });
        self
    }

    pub fn image_url(mut self, url: impl Into<String>) -> Self {
        self.image_url = Some(url.into());
        self
    }

    pub fn premiere_date(mut self, date: impl Into<String>) -> Self {
        self.premiere_date = Some(date.into());
        self
    }

    pub fn tagline(mut self, tagline: impl Into<String>) -> Self {
        self.tagline = Some(tagline.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageLookupRequest {
    pub item_id: String,
    pub image_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
}

impl ImageLookupRequest {
    pub fn new(item_id: impl Into<String>, image_type: impl Into<String>) -> Self {
        Self {
            item_id: item_id.into(),
            image_type: image_type.into(),
            max_width: None,
            max_height: None,
        }
    }

    pub fn max_width(mut self, width: u32) -> Self {
        self.max_width = Some(width);
        self
    }

    pub fn max_height(mut self, height: u32) -> Self {
        self.max_height = Some(height);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl ImageResult {
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            image_url: Some(url.into()),
            image_data: None,
            content_type: None,
            width: None,
            height: None,
        }
    }

    pub fn data(data: impl Into<String>) -> Self {
        Self {
            image_url: None,
            image_data: Some(data.into()),
            content_type: None,
            width: None,
            height: None,
        }
    }

    pub fn content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ChannelItem {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

impl ChannelItem {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            media_type: None,
            path: None,
            image_url: None,
        }
    }

    pub fn media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ChannelResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ChannelItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_record_count: Option<u64>,
}

impl ChannelResult {
    pub fn new(items: Vec<ChannelItem>) -> Self {
        let total_record_count = Some(items.len() as u64);
        Self {
            items,
            total_record_count,
        }
    }
}

/// A string containing secret material.
///
/// Serialization is intentionally transparent for the short-lived plugin RPC message. Debug
/// output is always redacted and the owned allocation is zeroized when it is dropped.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SensitiveString(String);

impl SensitiveString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the secret only at the point where it is needed by the provider implementation.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Transfers ownership to another secret-bearing type without cloning the allocation.
    ///
    /// The caller becomes responsible for zeroizing the returned `String`; this is intended for
    /// moving a grant into a provider's own zeroizing credential container.
    pub fn into_secret(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl std::fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveString([REDACTED])")
    }
}

impl Zeroize for SensitiveString {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SensitiveString {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Credentials released to one plugin invocation only.
///
/// The host must validate the complete scope and the `ProviderSecrets` permission before
/// creating this value. Plugins must reject a grant whose plugin, tuner, or action binding does
/// not match the invocation they are handling.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvProviderSecretGrant {
    pub plugin_id: String,
    pub tuner_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    pub username: SensitiveString,
    pub password: SensitiveString,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, SensitiveString>,
}

impl LiveTvProviderSecretGrant {
    pub fn new(
        plugin_id: impl Into<String>,
        tuner_id: impl Into<String>,
        action: impl Into<String>,
        username: SensitiveString,
        password: SensitiveString,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            tuner_id: tuner_id.into(),
            action: action.into(),
            secret_id: None,
            revision: None,
            username,
            password,
            fields: BTreeMap::new(),
        }
    }

    pub fn secret_reference(mut self, secret_id: impl Into<String>, revision: i64) -> Self {
        self.secret_id = Some(secret_id.into());
        self.revision = Some(revision);
        self
    }

    pub fn field(mut self, name: impl Into<String>, value: SensitiveString) -> Self {
        self.fields.insert(name.into(), value);
        self
    }
}

impl std::fmt::Debug for LiveTvProviderSecretGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveTvProviderSecretGrant")
            .field("plugin_id", &self.plugin_id)
            .field("tuner_id", &self.tuner_id)
            .field("action", &self.action)
            .field("secret_id_present", &self.secret_id.is_some())
            .field("revision_present", &self.revision.is_some())
            .field("username_present", &true)
            .field("password_present", &true)
            .field("additional_fields_present", &(!self.fields.is_empty()))
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvProviderRequest {
    pub action: String,
    #[serde(default)]
    pub tuner_config: Value,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_grant: Option<LiveTvProviderSecretGrant>,
}

impl std::fmt::Debug for LiveTvProviderRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveTvProviderRequest")
            .field("action", &self.action)
            .field("tuner_config", &"[REDACTED]")
            .field("arguments", &"[REDACTED]")
            .field("secret_grant", &self.secret_grant)
            .finish()
    }
}

impl LiveTvProviderRequest {
    pub fn import_channels(tuner_config: Value) -> Self {
        Self {
            action: "ImportChannels".to_string(),
            tuner_config,
            arguments: json!({}),
            secret_grant: None,
        }
    }

    pub fn import_programs(tuner_config: Value) -> Self {
        Self {
            action: "ImportPrograms".to_string(),
            tuner_config,
            arguments: json!({}),
            secret_grant: None,
        }
    }

    pub fn sync_media(tuner_config: Value) -> Self {
        Self {
            action: "SyncMedia".to_string(),
            tuner_config,
            arguments: json!({}),
            secret_grant: None,
        }
    }

    /// Requests an ephemeral playback URL for an opaque catalog reference. Credentials and
    /// signed URLs must never be embedded in `provider_reference` or persisted by the server.
    pub fn resolve_playback(
        tuner_config: Value,
        provider_reference: impl Into<String>,
        context: LiveTvPlaybackContext,
    ) -> Self {
        let mut arguments = serde_json::to_value(context)
            .expect("LiveTvPlaybackContext must serialize as an object");
        if let Value::Object(object) = &mut arguments {
            object.insert(
                "ProviderReference".to_string(),
                Value::String(provider_reference.into()),
            );
        }
        Self {
            action: "ResolvePlayback".to_string(),
            tuner_config,
            arguments,
            secret_grant: None,
        }
    }

    pub fn with_secret_grant(mut self, secret_grant: LiveTvProviderSecretGrant) -> Self {
        self.secret_grant = Some(secret_grant);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvPlaybackContext {
    /// Trusted public egress address used by the provider sidecar, never the viewer's address.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "ClientIp")]
    pub egress_ip: Option<String>,
    pub delivery_capabilities: LiveTvDeliveryCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvDeliveryCapabilities {
    pub direct_proxy: bool,
    pub hls_remux: bool,
}

/// A secret URL that can cross the plugin RPC boundary without becoming printable through
/// `Debug`. Callers should expose it only at the point that opens the upstream connection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SensitiveUrl(String);

impl SensitiveUrl {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SensitiveUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveUrl([REDACTED])")
    }
}

impl Zeroize for SensitiveUrl {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SensitiveUrl {
    fn drop(&mut self) {
        self.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvPlaybackDelivery {
    pub container: String,
    pub preferred: String,
    pub requires_provider_egress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvPlaybackResult {
    pub source_url: SensitiveUrl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub delivery: LiveTvPlaybackDelivery,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_streams: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvProviderResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_items: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movie_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_episode_count: Option<u64>,
}

impl LiveTvProviderResult {
    pub fn channels(channels: Vec<Value>, categories: Vec<Value>) -> Self {
        Self {
            channels,
            categories,
            programs: Vec::new(),
            media_items: Vec::new(),
            movie_count: None,
            series_episode_count: None,
        }
    }

    pub fn programs(programs: Vec<Value>) -> Self {
        Self {
            channels: Vec::new(),
            categories: Vec::new(),
            programs,
            media_items: Vec::new(),
            movie_count: None,
            series_episode_count: None,
        }
    }

    pub fn media_sync(movie_count: u64, series_episode_count: u64) -> Self {
        Self {
            channels: Vec::new(),
            categories: Vec::new(),
            programs: Vec::new(),
            media_items: Vec::new(),
            movie_count: Some(movie_count),
            series_episode_count: Some(series_episode_count),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginManifest {
    pub guid: String,
    pub name: String,
    pub version: String,
    pub runtime: String,
    pub target_abi: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PluginPermission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_pages: Vec<PluginWebPage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_images: Vec<PluginEmbeddedImage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capability_handlers: BTreeMap<String, CapabilityHandler>,
}

impl PluginManifest {
    pub fn builder(
        guid: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> PluginManifestBuilder {
        PluginManifestBuilder {
            manifest: Self {
                guid: guid.into(),
                name: name.into(),
                version: version.into(),
                runtime: "RustWasi".to_string(),
                target_abi: TARGET_ABI.to_string(),
                capabilities: Vec::new(),
                permissions: Vec::new(),
                configuration: None,
                web_pages: Vec::new(),
                embedded_images: Vec::new(),
                capability_handlers: BTreeMap::new(),
            },
        }
    }

    pub fn into_json(self) -> Value {
        serde_json::to_value(self).expect("PluginManifest must serialize")
    }
}

#[derive(Debug, Clone)]
pub struct PluginManifestBuilder {
    manifest: PluginManifest,
}

impl PluginManifestBuilder {
    pub fn capability(mut self, capability: impl Into<String>) -> Self {
        self.manifest.capabilities.push(capability.into());
        self
    }

    pub fn permission(mut self, permission: PluginPermission) -> Self {
        self.manifest.permissions.push(permission);
        self
    }

    pub fn configuration(mut self, configuration: Value) -> Self {
        self.manifest.configuration = Some(configuration);
        self
    }

    pub fn web_page(mut self, page: PluginWebPage) -> Self {
        self.manifest.web_pages.push(page);
        self
    }

    pub fn embedded_image(mut self, image: PluginEmbeddedImage) -> Self {
        self.manifest.embedded_images.push(image);
        self
    }

    pub fn capability_handler(
        mut self,
        capability: impl Into<String>,
        handler: CapabilityHandler,
    ) -> Self {
        let capability = capability.into();
        if !self
            .manifest
            .capabilities
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&capability))
        {
            self.manifest.capabilities.push(capability.clone());
        }
        self.manifest
            .capability_handlers
            .insert(capability, handler);
        self
    }

    pub fn build(self) -> PluginManifest {
        self.manifest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginPermission {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PluginPermission {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reason: None,
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginWebPage {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub enable_in_main_menu: bool,
}

impl PluginWebPage {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            display_name: None,
            enable_in_main_menu: false,
        }
    }

    pub fn display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    pub fn enable_in_main_menu(mut self) -> Self {
        self.enable_in_main_menu = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginEmbeddedImage {
    pub image_type: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl PluginEmbeddedImage {
    pub fn new(image_type: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            image_type: image_type.into(),
            path: path.into(),
            mime_type: None,
        }
    }

    pub fn mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityHandler {
    pub result: Value,
    #[serde(default, skip_serializing_if = "is_false")]
    pub echo_arguments: bool,
}

impl CapabilityHandler {
    pub fn new(result: Value) -> Self {
        Self {
            result,
            echo_arguments: false,
        }
    }

    pub fn echo_arguments(mut self) -> Self {
        self.echo_arguments = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityRequest {
    pub capability: String,
    #[serde(default)]
    pub arguments: Value,
}

impl CapabilityRequest {
    pub fn new(capability: impl Into<String>, arguments: Value) -> Self {
        Self {
            capability: capability.into(),
            arguments,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

impl std::fmt::Debug for CapabilityResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityResponse")
            .field("status", &self.status)
            .field("capability", &self.capability)
            .field("result", &self.result.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl CapabilityResponse {
    pub fn executed(capability: impl Into<String>, result: Value) -> Self {
        Self {
            status: "Executed".to_string(),
            capability: Some(capability.into()),
            result: Some(result),
        }
    }

    pub fn not_supported(capability: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: "NotSupported".to_string(),
            capability: Some(capability.into()),
            result: Some(json!({ "Reason": reason.into() })),
        }
    }

    pub fn scheduled_task(result: ScheduledTaskResult) -> Self {
        Self::executed(
            CAPABILITY_SCHEDULED_TASK,
            serde_json::to_value(result).expect("ScheduledTaskResult must serialize"),
        )
    }

    pub fn metadata(result: MetadataResult) -> Self {
        Self::executed(
            CAPABILITY_METADATA_PROVIDER,
            serde_json::to_value(result).expect("MetadataResult must serialize"),
        )
    }

    pub fn channel(result: ChannelResult) -> Self {
        Self::executed(
            CAPABILITY_CHANNEL_PROVIDER,
            serde_json::to_value(result).expect("ChannelResult must serialize"),
        )
    }

    pub fn live_tv_provider(result: LiveTvProviderResult) -> Self {
        Self::executed(
            CAPABILITY_LIVE_TV_PROVIDER,
            serde_json::to_value(result).expect("LiveTvProviderResult must serialize"),
        )
    }

    pub fn live_tv_playback(result: LiveTvPlaybackResult) -> Self {
        Self::executed(
            CAPABILITY_LIVE_TV_PROVIDER,
            serde_json::to_value(result).expect("LiveTvPlaybackResult must serialize"),
        )
    }

    pub fn into_host_value(self) -> Value {
        let mut value = serde_json::to_value(self).expect("CapabilityResponse must serialize");
        if let Value::Object(object) = &mut value {
            flatten_result_object(object);
        }
        value
    }
}

fn flatten_result_object(object: &mut Map<String, Value>) {
    let Some(Value::Object(result)) = object.remove("Result") else {
        return;
    };
    for (key, value) in result {
        object.insert(key, value);
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_builder_serializes_host_compatible_pascal_case() {
        let manifest = PluginManifest::builder(
            "11111111-1111-1111-1111-111111111111",
            "Fixture Task",
            "0.1.0",
        )
        .capability(CAPABILITY_SCHEDULED_TASK)
        .permission(PluginPermission::new("FileSystem").reason("Read fixture media"))
        .configuration(json!({ "Enabled": true }))
        .web_page(
            PluginWebPage::new("fixture-config", "config.html")
                .display_name("Fixture")
                .enable_in_main_menu(),
        )
        .embedded_image(PluginEmbeddedImage::new("Primary", "logo.png").mime_type("image/png"))
        .capability_handler(
            CAPABILITY_SCHEDULED_TASK,
            CapabilityHandler::new(
                CapabilityResponse::scheduled_task(ScheduledTaskResult::completed("Fixture Task"))
                    .into_host_value(),
            )
            .echo_arguments(),
        )
        .build()
        .into_json();

        assert_eq!(manifest["Runtime"], "RustWasi");
        assert_eq!(manifest["TargetAbi"], TARGET_ABI);
        assert_eq!(manifest["Capabilities"][0], CAPABILITY_SCHEDULED_TASK);
        assert_eq!(manifest["Permissions"][0]["Name"], "FileSystem");
        assert_eq!(manifest["Configuration"]["Enabled"], true);
        assert_eq!(manifest["WebPages"][0]["Name"], "fixture-config");
        assert_eq!(manifest["WebPages"][0]["EnableInMainMenu"], true);
        assert_eq!(manifest["EmbeddedImages"][0]["ImageType"], "Primary");
        assert_eq!(manifest["EmbeddedImages"][0]["MimeType"], "image/png");
        assert_eq!(
            manifest["CapabilityHandlers"]["ScheduledTask"]["Result"]["TaskName"],
            "Fixture Task"
        );
        assert_eq!(
            manifest["CapabilityHandlers"]["ScheduledTask"]["EchoArguments"],
            true
        );
    }

    #[test]
    fn capability_response_flattens_result_for_runtime_host_contract() {
        let value = CapabilityResponse::executed(
            CAPABILITY_SCHEDULED_TASK,
            json!({
                "TaskName": "Fixture Task",
                "ItemsProcessed": 3
            }),
        )
        .into_host_value();

        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], CAPABILITY_SCHEDULED_TASK);
        assert_eq!(value["TaskName"], "Fixture Task");
        assert_eq!(value["ItemsProcessed"], 3);
        assert!(value.get("Result").is_none());
    }

    #[test]
    fn live_tv_playback_contract_keeps_source_url_out_of_debug_output() {
        let request = LiveTvProviderRequest::resolve_playback(
            json!({"SecretReference": "provider/account-a"}),
            "provider:v1:opaque.signature",
            LiveTvPlaybackContext {
                egress_ip: Some("203.0.113.7".to_string()),
                delivery_capabilities: LiveTvDeliveryCapabilities {
                    direct_proxy: true,
                    hls_remux: true,
                },
            },
        );
        let request = serde_json::to_value(request).unwrap();
        assert_eq!(request["Action"], "ResolvePlayback");
        assert_eq!(
            request["Arguments"]["ProviderReference"],
            "provider:v1:opaque.signature"
        );
        assert_eq!(request["Arguments"]["EgressIp"], "203.0.113.7");

        let playback = LiveTvPlaybackResult {
            source_url: SensitiveUrl::new("https://provider.invalid/signed?token=secret"),
            expires_at: Some("2030-01-01T00:00:00Z".to_string()),
            delivery: LiveTvPlaybackDelivery {
                container: "MpegTs".to_string(),
                preferred: "DirectProxy".to_string(),
                requires_provider_egress: true,
                fallback: Some("HlsRemux".to_string()),
                video_codec: Some("h264".to_string()),
                audio_codec: Some("aac".to_string()),
            },
            media_streams: Vec::new(),
        };
        let debug = format!("{playback:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("token=secret"));
        let response = CapabilityResponse::live_tv_playback(playback);
        let response_debug = format!("{response:?}");
        assert!(response_debug.contains("[REDACTED]"));
        assert!(!response_debug.contains("token=secret"));
        let wire = response.into_host_value();
        assert_eq!(
            wire["SourceUrl"],
            "https://provider.invalid/signed?token=secret"
        );
    }

    #[test]
    fn live_tv_provider_request_without_grant_remains_wire_compatible() {
        let legacy_wire = json!({
            "Action": "ImportChannels",
            "TunerConfig": { "Type": "plugin:magstv" },
            "Arguments": {}
        });

        let request: LiveTvProviderRequest = serde_json::from_value(legacy_wire.clone()).unwrap();
        assert!(request.secret_grant.is_none());
        assert_eq!(serde_json::to_value(request).unwrap(), legacy_wire);

        let constructed =
            serde_json::to_value(LiveTvProviderRequest::import_channels(json!({}))).unwrap();
        assert!(constructed.get("SecretGrant").is_none());
    }

    #[test]
    fn live_tv_provider_secret_grant_uses_pascal_case_wire_contract() {
        let request = LiveTvProviderRequest::import_channels(json!({
            "Type": "plugin:magstv"
        }))
        .with_secret_grant(
            LiveTvProviderSecretGrant::new(
                "magstv-plugin",
                "tuner-a",
                "ImportChannels",
                SensitiveString::new("provider-user"),
                SensitiveString::new("provider-password"),
            )
            .secret_reference("secret-a", 7)
            .field("DeviceId", SensitiveString::new("device-secret")),
        );

        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["SecretGrant"]["PluginId"], "magstv-plugin");
        assert_eq!(wire["SecretGrant"]["TunerId"], "tuner-a");
        assert_eq!(wire["SecretGrant"]["Action"], "ImportChannels");
        assert_eq!(wire["SecretGrant"]["SecretId"], "secret-a");
        assert_eq!(wire["SecretGrant"]["Revision"], 7);
        assert_eq!(wire["SecretGrant"]["Username"], "provider-user");
        assert_eq!(wire["SecretGrant"]["Password"], "provider-password");
        assert_eq!(wire["SecretGrant"]["Fields"]["DeviceId"], "device-secret");

        let decoded: LiveTvProviderRequest = serde_json::from_value(wire).unwrap();
        let grant = decoded.secret_grant.unwrap();
        assert_eq!(grant.username.expose_secret(), "provider-user");
        assert_eq!(grant.password.expose_secret(), "provider-password");
        assert_eq!(grant.fields["DeviceId"].expose_secret(), "device-secret");
    }

    #[test]
    fn live_tv_provider_secret_debug_reveals_scope_but_never_values() {
        let request = LiveTvProviderRequest::import_channels(json!({
            "LegacyPassword": "legacy-config-secret"
        }))
        .with_secret_grant(
            LiveTvProviderSecretGrant::new(
                "magstv-plugin",
                "tuner-a",
                "ImportChannels",
                SensitiveString::new("provider-user"),
                SensitiveString::new("provider-password"),
            )
            .secret_reference("secret-identifier", 7)
            .field("DeviceSecret", SensitiveString::new("device-secret")),
        );

        let debug = format!("{request:?}");
        assert!(debug.contains("magstv-plugin"));
        assert!(debug.contains("tuner-a"));
        assert!(debug.contains("ImportChannels"));
        assert!(debug.contains("secret_id_present: true"));
        assert!(debug.contains("additional_fields_present: true"));
        for secret in [
            "legacy-config-secret",
            "provider-user",
            "provider-password",
            "secret-identifier",
            "device-secret",
        ] {
            assert!(!debug.contains(secret), "Debug leaked {secret}");
        }
    }

    #[test]
    fn sensitive_string_is_transparent_redacted_and_zeroizable() {
        let mut secret = SensitiveString::new("provider-password");
        assert_eq!(serde_json::to_value(&secret).unwrap(), "provider-password");
        assert_eq!(format!("{secret:?}"), "SensitiveString([REDACTED])");

        secret.zeroize();
        assert!(secret.expose_secret().is_empty());

        let mut transferred = SensitiveString::new("one-use-secret").into_secret();
        assert_eq!(transferred, "one-use-secret");
        transferred.zeroize();
        assert!(transferred.is_empty());
    }

    #[test]
    fn sensitive_url_is_transparent_redacted_and_zeroizable() {
        let mut url = SensitiveUrl::new("https://provider.invalid/live?token=secret");
        assert_eq!(
            serde_json::to_value(&url).unwrap(),
            "https://provider.invalid/live?token=secret"
        );
        assert_eq!(format!("{url:?}"), "SensitiveUrl([REDACTED])");

        url.zeroize();
        assert!(url.expose_secret().is_empty());
    }

    #[test]
    fn not_supported_response_carries_reason() {
        let value = CapabilityResponse::not_supported(
            CAPABILITY_CHANNEL_PROVIDER,
            "Channel provider ABI is not loaded.",
        )
        .into_host_value();

        assert_eq!(value["Status"], "NotSupported");
        assert_eq!(value["Capability"], CAPABILITY_CHANNEL_PROVIDER);
        assert_eq!(value["Reason"], "Channel provider ABI is not loaded.");
    }

    #[test]
    fn scheduled_task_response_matches_host_capability_shape() {
        let value = CapabilityResponse::scheduled_task(
            ScheduledTaskResult::completed("Fixture Task")
                .items_processed(7)
                .message("done"),
        )
        .into_host_value();

        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], CAPABILITY_SCHEDULED_TASK);
        assert_eq!(value["TaskName"], "Fixture Task");
        assert_eq!(value["ItemsProcessed"], 7);
        assert_eq!(value["Message"], "done");
    }

    #[test]
    fn metadata_response_matches_provider_capability_shape() {
        let value = CapabilityResponse::metadata(
            MetadataResult::new("Fixture Movie")
                .overview("Metadata from Rust/WASI fixture")
                .genre("Drama"),
        )
        .into_host_value();

        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], CAPABILITY_METADATA_PROVIDER);
        assert_eq!(value["Name"], "Fixture Movie");
        assert_eq!(value["Overview"], "Metadata from Rust/WASI fixture");
        assert_eq!(value["Genres"][0], "Drama");
    }

    #[test]
    fn channel_response_matches_provider_capability_shape() {
        let value = CapabilityResponse::channel(ChannelResult::new(vec![
            ChannelItem::new("channel-fixture-1", "Fixture Channel")
                .media_type("Video")
                .path("https://example.invalid/channel.m3u8"),
        ]))
        .into_host_value();

        assert_eq!(value["Status"], "Executed");
        assert_eq!(value["Capability"], CAPABILITY_CHANNEL_PROVIDER);
        assert_eq!(value["Items"][0]["Id"], "channel-fixture-1");
        assert_eq!(value["Items"][0]["Name"], "Fixture Channel");
        assert_eq!(value["TotalRecordCount"], 1);
    }
}
