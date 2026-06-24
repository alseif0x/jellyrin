//! Stable types for Jellyrin Rust/WASI plugins.
//!
//! The SDK intentionally exposes JSON-compatible data structures first. That
//! keeps the plugin ABI narrow while the WASI runtime matures and lets fixtures
//! produce manifests and capability responses that the sidecar host can load.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const TARGET_ABI: &str = "jellyrin-wasi-0.1";
pub const CAPABILITY_SCHEDULED_TASK: &str = "ScheduledTask";
pub const CAPABILITY_METADATA_PROVIDER: &str = "MetadataProvider";
pub const CAPABILITY_IMAGE_PROVIDER: &str = "ImageProvider";
pub const CAPABILITY_CHANNEL_PROVIDER: &str = "ChannelProvider";
pub const CAPABILITY_LIVE_TV_PROVIDER: &str = "LiveTvProvider";

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
}

impl MetadataResult {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            overview: None,
            genres: Vec::new(),
            provider_ids: Map::new(),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LiveTvProviderRequest {
    pub action: String,
    #[serde(default)]
    pub tuner_config: Value,
    #[serde(default)]
    pub arguments: Value,
}

impl LiveTvProviderRequest {
    pub fn import_channels(tuner_config: Value) -> Self {
        Self {
            action: "ImportChannels".to_string(),
            tuner_config,
            arguments: json!({}),
        }
    }

    pub fn import_programs(tuner_config: Value) -> Self {
        Self {
            action: "ImportPrograms".to_string(),
            tuner_config,
            arguments: json!({}),
        }
    }

    pub fn sync_media(tuner_config: Value) -> Self {
        Self {
            action: "SyncMedia".to_string(),
            tuner_config,
            arguments: json!({}),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CapabilityResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
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
