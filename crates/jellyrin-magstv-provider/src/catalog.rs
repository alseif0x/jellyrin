use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use crate::MagstvConfig;
use jellyrin_core::stable_entity_id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MagstvCategory {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MagstvChannel {
    pub id: String,
    pub name: String,
    pub category_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MagstvLiveTvImport {
    pub categories: Vec<MagstvCategory>,
    pub channels: Vec<MagstvChannel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JellyrinLiveTvCatalog {
    pub channels: Vec<Value>,
    pub categories: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagstvMediaKind {
    Movie,
    Series,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MagstvMediaItem {
    pub id: String,
    pub name: String,
    pub kind: MagstvMediaKind,
    pub overview: Option<String>,
    pub image_url: Option<String>,
    pub duration_seconds: Option<u64>,
    pub community_rating: Option<f64>,
    pub genres: Vec<String>,
    /// Column and request type are provider routing metadata. They are kept
    /// out of the Jellyfin-facing card but are needed by the later JIT VOD
    /// resolver.
    pub column_id: Option<i32>,
    pub request_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MagstvMediaEpisode {
    pub id: String,
    pub name: String,
    pub series_content_id: String,
    pub series_name: String,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub overview: Option<String>,
    pub image_url: Option<String>,
    pub duration_seconds: Option<u64>,
    pub column_id: Option<i32>,
    pub request_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MagstvMediaImport {
    pub movies: Vec<MagstvMediaItem>,
    pub series: Vec<MagstvMediaItem>,
    pub episodes: Vec<MagstvMediaEpisode>,
}

impl MagstvLiveTvImport {
    /// Applies user selection before mapping. Exclusions take precedence and
    /// the channel limit preserves the provider's deterministic order.
    pub fn filtered(mut self, config: &MagstvConfig) -> Self {
        self.channels
            .retain(|channel| config.allows_category(&channel.category_id));
        if let Some(limit) = config.channel_limit {
            self.channels.truncate(limit);
        }
        let visible_category_ids = self
            .channels
            .iter()
            .map(|channel| channel.category_id.as_str())
            .collect::<BTreeSet<_>>();
        self.categories.retain(|category| {
            config.allows_category(&category.id)
                && visible_category_ids.contains(category.id.as_str())
        });
        self
    }

    /// Maps a verified remote catalog to the Jellyfin-compatible Live TV DTO
    /// shape. Playback is intentionally absent: it must be resolved just in
    /// time from a valid authorised session.
    pub fn into_jellyrin_json(self) -> JellyrinLiveTvCatalog {
        let category_names = self
            .categories
            .iter()
            .map(|category| (category.id.clone(), category.name.clone()))
            .collect::<BTreeMap<_, _>>();
        let categories = self
            .categories
            .into_iter()
            .map(|category| json!({ "Id": category.id, "Name": category.name }))
            .collect();
        let channels = self
            .channels
            .into_iter()
            .map(|channel| {
                let category_name = category_names
                    .get(&channel.category_id)
                    .map(String::as_str)
                    .unwrap_or(channel.category_id.as_str());
                json!({
                    // Hash the opaque provider code instead of normalising it:
                    // distinct codes can differ only by punctuation/case.
                    "Id": stable_entity_id("livetv-magstv-channel", &channel.id),
                    "RemoteId": channel.id,
                    "Name": channel.name,
                    "SortName": channel.name,
                    "Number": channel.number.unwrap_or_default(),
                    "ChannelType": "TV",
                    "ImageUrl": channel.logo_url,
                    "Genres": [category_name],
                    "Tags": [category_name],
                    "GenreItems": [{
                        "Id": stable_entity_id("LiveTvGenre", category_name),
                        "Name": category_name,
                    }],
                    "ProviderIds": { "MAGSTV": channel.id },
                    "CategoryId": channel.category_id,
                })
            })
            .collect();
        JellyrinLiveTvCatalog {
            channels,
            categories,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_preserves_category_for_client_filters() {
        let catalog = MagstvLiveTvImport {
            categories: vec![MagstvCategory {
                id: "sports".to_string(),
                name: "Sports".to_string(),
            }],
            channels: vec![MagstvChannel {
                id: "42".to_string(),
                name: "Example Sports".to_string(),
                category_id: "sports".to_string(),
                number: Some("42".to_string()),
                logo_url: Some("https://images.example.invalid/42.png".to_string()),
            }],
        }
        .into_jellyrin_json();
        assert_eq!(catalog.categories[0]["Name"], "Sports");
        assert_eq!(catalog.channels[0]["Genres"], json!(["Sports"]));
        assert_eq!(catalog.channels[0]["CategoryId"], "sports");
        assert_eq!(catalog.channels[0]["GenreItems"][0]["Name"], "Sports");
        assert!(
            catalog.channels[0]["Id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert!(catalog.channels[0].get("Path").is_none());
    }

    #[test]
    fn exclusions_precede_inclusions_and_limit_is_deterministic() {
        let config = MagstvConfig {
            bootstrap_url: "https://portal.example.invalid".to_string(),
            secret_reference: "MAGSTV_ACCOUNT".to_string(),
            category_ids: BTreeSet::from(["sports".to_string(), "news".to_string()]),
            excluded_category_ids: BTreeSet::from(["sports".to_string()]),
            channel_limit: Some(1),
            cdn_edge_host: None,
        };
        let filtered = MagstvLiveTvImport {
            categories: vec![
                MagstvCategory {
                    id: "sports".to_string(),
                    name: "Sports".to_string(),
                },
                MagstvCategory {
                    id: "news".to_string(),
                    name: "News".to_string(),
                },
            ],
            channels: vec![
                MagstvChannel {
                    id: "sports-1".to_string(),
                    name: "Sports 1".to_string(),
                    category_id: "sports".to_string(),
                    number: None,
                    logo_url: None,
                },
                MagstvChannel {
                    id: "news-1".to_string(),
                    name: "News 1".to_string(),
                    category_id: "news".to_string(),
                    number: None,
                    logo_url: None,
                },
                MagstvChannel {
                    id: "news-2".to_string(),
                    name: "News 2".to_string(),
                    category_id: "news".to_string(),
                    number: None,
                    logo_url: None,
                },
            ],
        }
        .filtered(&config);

        assert_eq!(filtered.channels.len(), 1);
        assert_eq!(filtered.channels[0].id, "news-1");
        assert_eq!(filtered.categories.len(), 1);
        assert_eq!(filtered.categories[0].id, "news");
    }
}
