use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use serde_json::{Map, Value, json};

pub(crate) const CONFIGURATION_KEY: &str = "metadataproviders";

const TMDB_ID: &str = "tmdb";
const OMDB_ID: &str = "omdb";
const MUSICBRAINZ_ID: &str = "musicbrainz";
const STUDIO_IMAGES_ID: &str = "studio-images";

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct ProviderSecretAvailability {
    pub(crate) tmdb: bool,
    pub(crate) omdb: bool,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct ProviderPolicy {
    disabled: HashSet<String>,
    order: HashMap<String, usize>,
}

impl ProviderPolicy {
    pub(crate) fn for_item_type(
        metadata_options: &Value,
        item_type: &str,
        image_provider: bool,
    ) -> Self {
        let Some(option) = metadata_options.as_array().and_then(|options| {
            options.iter().find(|option| {
                option
                    .get("ItemType")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(item_type))
            })
        }) else {
            return Self::default();
        };
        let (disabled_field, order_field) = if image_provider {
            ("DisabledImageFetchers", "ImageFetcherOrder")
        } else {
            ("DisabledMetadataFetchers", "MetadataFetcherOrder")
        };
        let disabled = normalized_string_set(option.get(disabled_field));
        let order = option
            .get(order_field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(provider_key)
            .filter(|key| !key.is_empty())
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect();
        Self { disabled, order }
    }

    pub(crate) fn allows(&self, provider_id: &str, provider_name: &str) -> bool {
        !self.disabled.contains(&provider_key(provider_id))
            && !self.disabled.contains(&provider_key(provider_name))
    }

    pub(crate) fn compare(
        &self,
        left_name: &str,
        left_priority: i64,
        right_name: &str,
        right_priority: i64,
    ) -> Ordering {
        provider_precedence(
            self.order.get(&provider_key(left_name)).copied(),
            left_priority,
        )
        .cmp(&provider_precedence(
            self.order.get(&provider_key(right_name)).copied(),
            right_priority,
        ))
        .then_with(|| provider_key(left_name).cmp(&provider_key(right_name)))
    }
}

fn provider_precedence(explicit_order: Option<usize>, priority: i64) -> (u8, usize, i64) {
    explicit_order.map_or((1, usize::MAX, priority), |order| (0, order, priority))
}

fn normalized_string_set(value: Option<&Value>) -> HashSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(provider_key)
        .filter(|key| !key.is_empty())
        .collect()
}

fn provider_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl ProviderSecretAvailability {
    pub(crate) fn from_environment() -> Self {
        Self {
            tmdb: environment_secret_is_configured("JELLYRIN_TMDB_API_KEY"),
            omdb: environment_secret_is_configured("JELLYRIN_OMDB_API_KEY"),
        }
    }
}

pub(crate) fn default_configuration() -> Value {
    json!({
        "Version": 1,
        "Providers": [
            {
                "Id": TMDB_ID,
                "Enabled": true,
                "Priority": 100,
                "PreferredLanguage": "",
                "CountryCode": "",
                "IncludeAdult": false,
                "ImportSeasonName": false,
                "MaxCastMembers": 15,
                "MaxCrewMembers": 15
            },
            {
                "Id": OMDB_ID,
                "Enabled": true,
                "Priority": 200,
                "CastAndCrew": false
            },
            {
                "Id": MUSICBRAINZ_ID,
                "Enabled": true,
                "Priority": 100,
                "RateLimitPerSecond": 1,
                "ReplaceArtistName": false
            },
            {
                "Id": STUDIO_IMAGES_ID,
                "Enabled": true,
                "Priority": 100
            }
        ]
    })
}

/// Normalize provider settings against a strict allowlist. API keys are deliberately absent:
/// accepting them here would persist credential material in named configuration JSON.
pub(crate) fn normalize_configuration(payload: Value) -> Value {
    let defaults = default_configuration();
    let submitted = payload
        .get("Providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let providers = defaults["Providers"]
        .as_array()
        .expect("built-in provider defaults must be an array")
        .iter()
        .map(|default| {
            let id = default["Id"]
                .as_str()
                .expect("built-in provider id must be a string");
            let Some(update) = submitted.iter().find(|provider| {
                provider
                    .get("Id")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(id))
            }) else {
                return default.clone();
            };
            normalize_provider(default, update)
        })
        .collect::<Vec<_>>();
    json!({ "Version": 1, "Providers": providers })
}

pub(crate) fn registry_entries(
    configuration: &Value,
    secrets: ProviderSecretAvailability,
) -> Vec<Value> {
    let configuration = normalize_configuration(configuration.clone());
    configuration["Providers"]
        .as_array()
        .expect("normalized providers must be an array")
        .iter()
        .filter_map(|provider| registry_entry(provider, secrets))
        .collect()
}

pub(crate) fn provider_is_ready(
    configuration: &Value,
    provider_id: &str,
    secrets: ProviderSecretAvailability,
) -> bool {
    registry_entries(configuration, secrets)
        .iter()
        .any(|entry| {
            entry["Id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(provider_id))
                && entry["Ready"].as_bool() == Some(true)
        })
}

pub(crate) fn provider_configuration(configuration: &Value, provider_id: &str) -> Option<Value> {
    normalize_configuration(configuration.clone())["Providers"]
        .as_array()?
        .iter()
        .find(|provider| {
            provider["Id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(provider_id))
        })
        .cloned()
}

fn normalize_provider(default: &Value, update: &Value) -> Value {
    let mut normalized = default
        .as_object()
        .cloned()
        .expect("built-in provider default must be an object");
    let id = default["Id"].as_str().unwrap_or_default();
    copy_bool(update, &mut normalized, "Enabled");
    copy_bounded_u64(update, &mut normalized, "Priority", 0, 10_000);
    match id {
        TMDB_ID => {
            copy_short_string(update, &mut normalized, "PreferredLanguage", 32);
            copy_short_string(update, &mut normalized, "CountryCode", 8);
            copy_bool(update, &mut normalized, "IncludeAdult");
            copy_bool(update, &mut normalized, "ImportSeasonName");
            copy_bounded_u64(update, &mut normalized, "MaxCastMembers", 0, 250);
            copy_bounded_u64(update, &mut normalized, "MaxCrewMembers", 0, 250);
        }
        OMDB_ID => copy_bool(update, &mut normalized, "CastAndCrew"),
        MUSICBRAINZ_ID => {
            copy_bounded_u64(update, &mut normalized, "RateLimitPerSecond", 1, 10);
            copy_bool(update, &mut normalized, "ReplaceArtistName");
        }
        STUDIO_IMAGES_ID => {}
        _ => unreachable!("unknown built-in metadata provider"),
    }
    Value::Object(normalized)
}

fn registry_entry(provider: &Value, secrets: ProviderSecretAvailability) -> Option<Value> {
    let id = provider.get("Id")?.as_str()?;
    let enabled = provider
        .get("Enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let (name, capabilities, item_types, api_key_configured) = match id {
        TMDB_ID => (
            "TheMovieDb",
            vec!["MetadataProvider", "ImageProvider"],
            vec!["Movie", "Series", "Season", "Episode", "Person", "BoxSet"],
            Some(secrets.tmdb),
        ),
        OMDB_ID => (
            "The Open Movie Database",
            vec!["MetadataProvider", "ImageProvider"],
            vec!["Movie", "Series", "Episode"],
            Some(secrets.omdb),
        ),
        MUSICBRAINZ_ID => (
            "MusicBrainz",
            vec!["MetadataProvider"],
            vec!["MusicArtist", "MusicAlbum", "Audio"],
            None,
        ),
        STUDIO_IMAGES_ID => ("Studio Images", vec!["ImageProvider"], vec!["Studio"], None),
        _ => return None,
    };
    let ready = enabled && api_key_configured.unwrap_or(true);
    let status = if !enabled {
        "Disabled"
    } else if ready {
        "Active"
    } else {
        "Unconfigured"
    };
    let mut entry = json!({
        "Id": id,
        "Name": name,
        "Runtime": "Builtin",
        "Status": status,
        "Enabled": enabled,
        "Ready": ready,
        "Priority": provider.get("Priority").cloned().unwrap_or(json!(100)),
        "Capabilities": capabilities,
        "ItemTypes": item_types,
        "CanUninstall": false,
        "Configuration": provider
    });
    if let Some(configured) = api_key_configured {
        entry["ApiKeyConfigured"] = json!(configured);
    }
    Some(entry)
}

fn environment_secret_is_configured(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn copy_bool(source: &Value, target: &mut Map<String, Value>, field: &str) {
    if let Some(value) = source.get(field).and_then(Value::as_bool) {
        target.insert(field.to_string(), json!(value));
    }
}

fn copy_bounded_u64(
    source: &Value,
    target: &mut Map<String, Value>,
    field: &str,
    minimum: u64,
    maximum: u64,
) {
    if let Some(value) = source
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| (minimum..=maximum).contains(value))
    {
        target.insert(field.to_string(), json!(value));
    }
}

fn copy_short_string(source: &Value, target: &mut Map<String, Value>, field: &str, limit: usize) {
    if let Some(value) = source
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.len() <= limit && !value.chars().any(char::is_control))
    {
        target.insert(field.to_string(), json!(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_register_four_non_uninstallable_providers() {
        let entries = registry_entries(
            &default_configuration(),
            ProviderSecretAvailability {
                tmdb: true,
                omdb: true,
            },
        );
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().all(|entry| entry["Runtime"] == "Builtin"));
        assert!(entries.iter().all(|entry| entry["CanUninstall"] == false));
        assert!(entries.iter().all(|entry| entry["Status"] == "Active"));
    }

    #[test]
    fn missing_api_keys_are_reported_without_exposing_secret_fields() {
        let entries = registry_entries(
            &default_configuration(),
            ProviderSecretAvailability::default(),
        );
        for id in [TMDB_ID, OMDB_ID] {
            let entry = entries.iter().find(|entry| entry["Id"] == id).unwrap();
            assert_eq!(entry["Status"], "Unconfigured");
            assert_eq!(entry["ApiKeyConfigured"], false);
            assert!(entry.get("ApiKey").is_none());
        }
        for id in [MUSICBRAINZ_ID, STUDIO_IMAGES_ID] {
            let entry = entries.iter().find(|entry| entry["Id"] == id).unwrap();
            assert_eq!(entry["Status"], "Active");
            assert!(entry.get("ApiKeyConfigured").is_none());
        }
    }

    #[test]
    fn normalization_rejects_unknown_fields_secrets_and_out_of_range_values() {
        let normalized = normalize_configuration(json!({
            "Version": 99,
            "Providers": [{
                "Id": "tmdb",
                "Enabled": false,
                "Priority": 50_000,
                "MaxCastMembers": 20,
                "ApiKey": "must-not-persist",
                "Endpoint": "http://127.0.0.1/private"
            }]
        }));
        let tmdb = &normalized["Providers"][0];
        assert_eq!(normalized["Version"], 1);
        assert_eq!(tmdb["Enabled"], false);
        assert_eq!(tmdb["Priority"], 100);
        assert_eq!(tmdb["MaxCastMembers"], 20);
        assert!(tmdb.get("ApiKey").is_none());
        assert!(tmdb.get("Endpoint").is_none());
    }

    #[test]
    fn normalization_keeps_all_defaults_when_update_is_partial() {
        let normalized = normalize_configuration(json!({
            "Providers": [{ "Id": "musicbrainz", "RateLimitPerSecond": 2 }]
        }));
        let providers = normalized["Providers"].as_array().unwrap();
        assert_eq!(providers.len(), 4);
        let musicbrainz = providers
            .iter()
            .find(|provider| provider["Id"] == MUSICBRAINZ_ID)
            .unwrap();
        assert_eq!(musicbrainz["RateLimitPerSecond"], 2);
        assert_eq!(musicbrainz["Enabled"], true);
    }

    #[test]
    fn ready_state_honors_enabled_and_required_secrets() {
        let configuration = normalize_configuration(json!({
            "Providers": [{ "Id": "musicbrainz", "Enabled": false }]
        }));
        assert!(!provider_is_ready(
            &configuration,
            MUSICBRAINZ_ID,
            ProviderSecretAvailability::default()
        ));
        assert!(!provider_is_ready(
            &configuration,
            TMDB_ID,
            ProviderSecretAvailability::default()
        ));
        assert!(provider_is_ready(
            &configuration,
            TMDB_ID,
            ProviderSecretAvailability {
                tmdb: true,
                omdb: false,
            }
        ));
    }

    #[test]
    fn provider_configuration_returns_only_normalized_safe_fields() {
        let configuration = json!({
            "Providers": [{
                "Id": "tmdb",
                "PreferredLanguage": "es-ES",
                "ApiKey": "must-not-survive"
            }]
        });
        let provider = provider_configuration(&configuration, "TMDB").unwrap();
        assert_eq!(provider["PreferredLanguage"], "es-ES");
        assert!(provider.get("ApiKey").is_none());
    }

    #[test]
    fn provider_policy_honors_item_type_disable_lists_and_alias_formatting() {
        let policy = ProviderPolicy::for_item_type(
            &json!([{
                "ItemType": "Movie",
                "DisabledMetadataFetchers": ["The Open Movie Database"],
                "MetadataFetcherOrder": ["Custom.Provider", "TheMovieDb"]
            }]),
            "movie",
            false,
        );

        assert!(!policy.allows("omdb", "The Open Movie Database"));
        assert!(policy.allows("tmdb", "TheMovieDb"));
        assert!(policy.allows("custom-provider", "Custom Provider"));
        assert_eq!(
            policy.compare("Custom Provider", 1_000, "TheMovieDb", 100),
            Ordering::Less
        );
    }

    #[test]
    fn metadata_and_image_provider_policies_are_independent() {
        let options = json!([{
            "ItemType": "Series",
            "DisabledMetadataFetchers": ["Metadata only"],
            "DisabledImageFetchers": ["Images only"]
        }]);
        let metadata = ProviderPolicy::for_item_type(&options, "Series", false);
        let images = ProviderPolicy::for_item_type(&options, "Series", true);

        assert!(!metadata.allows("metadata", "Metadata only"));
        assert!(metadata.allows("images", "Images only"));
        assert!(images.allows("metadata", "Metadata only"));
        assert!(!images.allows("images", "Images only"));
    }
}
