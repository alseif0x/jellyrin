use std::collections::{BTreeMap, BTreeSet};

use jellyrin_core::stable_entity_id;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaItemFacetKind {
    Genre,
    MusicGenre,
    MusicArtist,
    MusicAlbumArtist,
    MusicAlbum,
    Person,
    Studio,
    Tag,
    Year,
}

impl MediaItemFacetKind {
    pub const ALL: [Self; 9] = [
        Self::Genre,
        Self::MusicGenre,
        Self::MusicArtist,
        Self::MusicAlbumArtist,
        Self::MusicAlbum,
        Self::Person,
        Self::Studio,
        Self::Tag,
        Self::Year,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Genre => "genre",
            Self::MusicGenre => "music_genre",
            Self::MusicArtist => "music_artist",
            Self::MusicAlbumArtist => "music_album_artist",
            Self::MusicAlbum => "music_album",
            Self::Person => "person",
            Self::Studio => "studio",
            Self::Tag => "tag",
            Self::Year => "year",
        }
    }

    pub const fn entity_type(self) -> &'static str {
        match self {
            Self::Genre => "Genre",
            Self::MusicGenre => "MusicGenre",
            Self::MusicArtist | Self::MusicAlbumArtist => "MusicArtist",
            Self::MusicAlbum => "MusicAlbum",
            Self::Person => "Person",
            Self::Studio => "Studio",
            Self::Tag => "Tag",
            Self::Year => "Year",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(value.trim()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedMediaItemFacet {
    pub kind: MediaItemFacetKind,
    pub normalized_value: String,
    pub display_value: String,
    pub stable_id: String,
    pub position: u32,
    pub payload: Value,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaItemFacetValue {
    pub normalized_value: String,
    pub display_value: String,
    pub stable_id: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaItemFacetCandidateQuery {
    pub kind: Option<MediaItemFacetKind>,
    pub normalized_values: Vec<String>,
    pub entity_ids: Vec<String>,
    pub virtual_folder_ids: Vec<Uuid>,
}

pub fn extract_media_item_facets(metadata: &Value) -> Vec<ExtractedMediaItemFacet> {
    let specs: [(MediaItemFacetKind, &[&str]); 9] = [
        (MediaItemFacetKind::Genre, &["Genres", "SeriesGenres"]),
        (MediaItemFacetKind::MusicGenre, &["MusicGenres"]),
        (MediaItemFacetKind::MusicArtist, &["Artists"]),
        (MediaItemFacetKind::MusicAlbumArtist, &["AlbumArtists"]),
        (
            MediaItemFacetKind::MusicAlbum,
            &["Album", "AlbumName", "Albums"],
        ),
        (
            MediaItemFacetKind::Person,
            &["People", "SeriesPeople", "Cast"],
        ),
        (MediaItemFacetKind::Studio, &["Studios", "SeriesStudios"]),
        (MediaItemFacetKind::Tag, &["Tags"]),
        (MediaItemFacetKind::Year, &["ProductionYear"]),
    ];
    let mut extracted = Vec::new();
    for (kind, keys) in specs {
        let mut facets = BTreeMap::<String, PendingFacet>::new();
        let mut position = 0u32;
        for key in keys {
            if let Some(value) = metadata.get(*key) {
                collect_facet_values(kind, value, &mut position, &mut facets);
            }
        }
        extracted.extend(facets.into_values().map(|facet| ExtractedMediaItemFacet {
            kind,
            stable_id: stable_entity_id(kind.entity_type(), &facet.display_value),
            normalized_value: facet.normalized_value,
            display_value: facet.display_value,
            position: facet.position,
            payload: facet.payload,
            aliases: facet.aliases.into_iter().collect(),
        }));
    }
    extracted.sort_by_key(|facet| (facet.kind, facet.position));
    extracted
}

#[derive(Debug)]
struct PendingFacet {
    normalized_value: String,
    display_value: String,
    position: u32,
    payload: Value,
    aliases: BTreeSet<String>,
}

fn collect_facet_values(
    kind: MediaItemFacetKind,
    value: &Value,
    position: &mut u32,
    facets: &mut BTreeMap<String, PendingFacet>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_facet_values(kind, value, position, facets);
            }
        }
        Value::String(display) => insert_facet(kind, display, value, None, position, facets),
        Value::Number(display) => {
            insert_facet(kind, &display.to_string(), value, None, position, facets)
        }
        Value::Object(object) => {
            let Some(display) = object.get("Name").and_then(Value::as_str) else {
                return;
            };
            let imported_id = (kind == MediaItemFacetKind::Person)
                .then(|| object.get("Id").and_then(Value::as_str))
                .flatten();
            insert_facet(kind, display, value, imported_id, position, facets);
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn insert_facet(
    kind: MediaItemFacetKind,
    display: &str,
    payload: &Value,
    imported_id: Option<&str>,
    position: &mut u32,
    facets: &mut BTreeMap<String, PendingFacet>,
) {
    let display = display.trim();
    if display.is_empty() {
        return;
    }
    let current_position = *position;
    *position = position.saturating_add(1);
    let normalized_value = display.to_ascii_lowercase();
    let stable_id = stable_entity_id(kind.entity_type(), display);
    let facet = facets
        .entry(normalized_value.clone())
        .or_insert_with(|| PendingFacet {
            normalized_value,
            display_value: display.to_string(),
            position: current_position,
            payload: payload.clone(),
            aliases: BTreeSet::from([stable_id.clone()]),
        });
    facet.aliases.insert(stable_id);
    if let Some(imported_id) = imported_id.map(str::trim).filter(|id| !id.is_empty()) {
        facet.aliases.insert(imported_id.to_ascii_lowercase());
        if let Ok(imported_uuid) = Uuid::parse_str(imported_id) {
            facet.aliases.insert(imported_uuid.simple().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extractor_preserves_first_display_and_explicit_person_aliases() {
        let facets = extract_media_item_facets(&json!({
            "Genres": [" Drama ", "drama", { "Name": "Comedy" }],
            "SeriesGenres": ["DRAMA"],
            "MusicGenres": "Rock",
            "Artists": ["Artist", { "Name": "artist" }],
            "AlbumArtists": ["Album Artist"],
            "Album": " First Album ",
            "AlbumName": "first album",
            "Albums": ["Second Album"],
            "People": [
                { "Name": " Jane Doe ", "Id": "Imported-Person", "Role": "Lead" },
                { "Name": "jane doe", "Id": "SECOND-ID" }
            ],
            "SeriesPeople": ["Other Person"],
            "Cast": [{ "Name": "Jane Doe", "Id": "imported-person" }],
            "Studios": ["Studio"],
            "SeriesStudios": ["studio"],
            "Tags": ["Featured", 2026],
            "ProductionYear": 2025
        }));

        let drama = facets
            .iter()
            .find(|facet| facet.kind == MediaItemFacetKind::Genre)
            .unwrap();
        assert_eq!(drama.normalized_value, "drama");
        assert_eq!(drama.display_value, "Drama");
        let jane = facets
            .iter()
            .find(|facet| {
                facet.kind == MediaItemFacetKind::Person && facet.normalized_value == "jane doe"
            })
            .unwrap();
        assert_eq!(jane.payload["Role"], "Lead");
        assert_eq!(jane.position, 0);
        assert_eq!(
            jane.aliases,
            vec![
                stable_entity_id("Person", "Jane Doe"),
                "imported-person".to_string(),
                "second-id".to_string(),
            ]
        );
        assert!(facets.iter().any(|facet| {
            facet.kind == MediaItemFacetKind::Tag && facet.display_value == "2026"
        }));
        assert!(facets.iter().any(|facet| {
            facet.kind == MediaItemFacetKind::MusicArtist && facet.display_value == "Artist"
        }));
        assert!(facets.iter().any(|facet| {
            facet.kind == MediaItemFacetKind::MusicAlbumArtist
                && facet.display_value == "Album Artist"
        }));
        assert!(facets.iter().any(|facet| {
            facet.kind == MediaItemFacetKind::Year && facet.display_value == "2025"
        }));
    }
}
