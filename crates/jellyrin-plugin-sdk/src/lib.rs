//! Stable types for Jellyrin Rust/WASI plugins.
//!
//! The SDK intentionally exposes JSON-compatible data structures first. That
//! keeps the plugin ABI narrow while the WASI runtime matures and lets fixtures
//! produce manifests and capability responses that the sidecar host can load.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const TARGET_ABI: &str = "jellyrin-wasi-0.1";
pub const CAPABILITY_SCHEDULED_TASK: &str = "ScheduledTask";
pub const CAPABILITY_METADATA_PROVIDER: &str = "MetadataProvider";
pub const CAPABILITY_IMAGE_PROVIDER: &str = "ImageProvider";
pub const CAPABILITY_CHANNEL_PROVIDER: &str = "ChannelProvider";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl PluginWebPage {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            display_name: None,
        }
    }

    pub fn display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PluginEmbeddedImage {
    pub image_type: String,
    pub path: String,
}

impl PluginEmbeddedImage {
    pub fn new(image_type: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            image_type: image_type.into(),
            path: path.into(),
        }
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
        .web_page(PluginWebPage::new("fixture-config", "config.html").display_name("Fixture"))
        .embedded_image(PluginEmbeddedImage::new("Primary", "logo.png"))
        .build()
        .into_json();

        assert_eq!(manifest["Runtime"], "RustWasi");
        assert_eq!(manifest["TargetAbi"], TARGET_ABI);
        assert_eq!(manifest["Capabilities"][0], CAPABILITY_SCHEDULED_TASK);
        assert_eq!(manifest["Permissions"][0]["Name"], "FileSystem");
        assert_eq!(manifest["Configuration"]["Enabled"], true);
        assert_eq!(manifest["WebPages"][0]["Name"], "fixture-config");
        assert_eq!(manifest["EmbeddedImages"][0]["ImageType"], "Primary");
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
}
