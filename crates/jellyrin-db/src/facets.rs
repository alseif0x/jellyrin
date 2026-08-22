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
    /// Caps the candidate page for callers that only need a bounded sample, such as picking one
    /// representative item for a by-name image. `None` keeps the full selection used by filters.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaItemFilterSelectorKind {
    Person,
    Studio,
    Tag,
}

impl MediaItemFilterSelectorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Studio => "studio",
            Self::Tag => "tag",
        }
    }
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

/// Exact selector projection for Jellyfin's GenreIds filter.
///
/// Unlike display facets this also retains an imported `Id` from an object that has no `Name`,
/// because the legacy metadata matcher accepts that shape. Keeping selector tokens separate avoids
/// exposing an artificial empty genre through Filters/Genres.
pub fn extract_media_item_genre_selectors(metadata: &Value) -> Vec<String> {
    let mut selectors = BTreeSet::new();
    for key in ["Genres", "SeriesGenres"] {
        if let Some(value) = metadata_field_case_insensitive(metadata, key) {
            collect_genre_selectors(value, &mut selectors);
        }
    }
    selectors.into_iter().collect()
}

/// Exact selector projection for metadata-backed `/Items` filters.
///
/// Person selection preserves Jellyfin's inheritance rule: any non-empty `People` or `Cast`
/// value suppresses `SeriesPeople`. Entity selectors accept raw names, stable IDs and imported
/// object IDs, while Tags accept only their raw normalized values.
pub fn extract_media_item_filter_selectors(
    metadata: &Value,
) -> Vec<(MediaItemFilterSelectorKind, String)> {
    let mut selectors = BTreeSet::new();
    let people = metadata_field_case_insensitive(metadata, "People");
    let cast = metadata_field_case_insensitive(metadata, "Cast");
    let has_item_people =
        people.is_some_and(metadata_value_has_items) || cast.is_some_and(metadata_value_has_items);
    for value in [people, cast]
        .into_iter()
        .chain(
            (!has_item_people).then(|| metadata_field_case_insensitive(metadata, "SeriesPeople")),
        )
        .flatten()
    {
        collect_entity_filter_selectors(
            value,
            MediaItemFilterSelectorKind::Person,
            "Person",
            &mut selectors,
        );
    }
    for key in ["Studios", "SeriesStudios"] {
        if let Some(value) = metadata_field_case_insensitive(metadata, key) {
            collect_entity_filter_selectors(
                value,
                MediaItemFilterSelectorKind::Studio,
                "Studio",
                &mut selectors,
            );
        }
    }
    if let Some(value) = metadata_field_case_insensitive(metadata, "Tags") {
        collect_tag_filter_selectors(value, &mut selectors);
    }
    selectors.into_iter().collect()
}

fn metadata_value_has_items(value: &Value) -> bool {
    match value {
        Value::Array(values) => !values.is_empty(),
        Value::String(value) => !value.trim().is_empty(),
        Value::Object(object) => !object.is_empty(),
        Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn collect_entity_filter_selectors(
    value: &Value,
    kind: MediaItemFilterSelectorKind,
    entity_type: &str,
    selectors: &mut BTreeSet<(MediaItemFilterSelectorKind, String)>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_entity_filter_selectors(value, kind, entity_type, selectors);
            }
        }
        Value::String(name) => insert_entity_filter_name(name, kind, entity_type, selectors),
        Value::Number(name) => {
            insert_entity_filter_name(&name.to_string(), kind, entity_type, selectors);
        }
        Value::Object(object) => {
            if let Some(imported_id) = object
                .get("Id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                selectors.insert((kind, imported_id.to_ascii_lowercase()));
            }
            if let Some(name) = object.get("Name").and_then(Value::as_str) {
                insert_entity_filter_name(name, kind, entity_type, selectors);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn insert_entity_filter_name(
    name: &str,
    kind: MediaItemFilterSelectorKind,
    entity_type: &str,
    selectors: &mut BTreeSet<(MediaItemFilterSelectorKind, String)>,
) {
    let name = name.trim();
    if !name.is_empty() {
        selectors.insert((kind, name.to_ascii_lowercase()));
        selectors.insert((
            kind,
            stable_entity_id(entity_type, name).to_ascii_lowercase(),
        ));
    }
}

fn collect_tag_filter_selectors(
    value: &Value,
    selectors: &mut BTreeSet<(MediaItemFilterSelectorKind, String)>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_tag_filter_selectors(value, selectors);
            }
        }
        Value::String(tag) => insert_tag_filter_selector(tag, selectors),
        Value::Number(tag) => insert_tag_filter_selector(&tag.to_string(), selectors),
        Value::Object(object) => {
            if let Some(name) = object.get("Name").and_then(Value::as_str) {
                insert_tag_filter_selector(name, selectors);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn insert_tag_filter_selector(
    tag: &str,
    selectors: &mut BTreeSet<(MediaItemFilterSelectorKind, String)>,
) {
    let tag = tag.trim();
    if !tag.is_empty() {
        selectors.insert((MediaItemFilterSelectorKind::Tag, tag.to_ascii_lowercase()));
    }
}

fn collect_genre_selectors(value: &Value, selectors: &mut BTreeSet<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_genre_selectors(value, selectors);
            }
        }
        Value::String(name) => insert_genre_name_selectors(name, selectors),
        Value::Number(name) => insert_genre_name_selectors(&name.to_string(), selectors),
        Value::Object(object) => {
            if let Some(imported_id) = object
                .get("Id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                selectors.insert(imported_id.to_ascii_lowercase());
            }
            if let Some(name) = object.get("Name").and_then(Value::as_str) {
                insert_genre_name_selectors(name, selectors);
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

fn insert_genre_name_selectors(name: &str, selectors: &mut BTreeSet<String>) {
    let name = name.trim();
    if !name.is_empty() {
        selectors.insert(name.to_ascii_lowercase());
        selectors.insert(stable_entity_id("Genre", name).to_ascii_lowercase());
    }
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
            aliases: BTreeSet::new(),
        });
    if let Some(imported_id) = imported_id.map(str::trim).filter(|id| !id.is_empty()) {
        let imported_id = imported_id.to_ascii_lowercase();
        if imported_id != stable_id {
            facet.aliases.insert(imported_id.clone());
        }
        if kind == MediaItemFacetKind::Person
            && let Ok(imported_uuid) = Uuid::parse_str(&imported_id)
        {
            facet.aliases.insert(imported_uuid.simple().to_string());
        }
    }
}

fn metadata_field_case_insensitive<'a>(metadata: &'a Value, key: &str) -> Option<&'a Value> {
    metadata
        .as_object()?
        .iter()
        .find_map(|(candidate, value)| candidate.eq_ignore_ascii_case(key).then_some(value))
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
        let genre_selectors = extract_media_item_genre_selectors(&json!({
            "gEnReS": [
                {"Id": "Id-Only"},
                {"Name": "Comedy", "Id": "Imported-Genre"},
                2026,
                [["Nested Genre"]],
                {"Id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}
            ]
        }));
        assert_eq!(
            genre_selectors,
            BTreeSet::from([
                stable_entity_id("Genre", "2026"),
                stable_entity_id("Genre", "Comedy"),
                stable_entity_id("Genre", "Nested Genre"),
                "2026".to_string(),
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
                "comedy".to_string(),
                "id-only".to_string(),
                "imported-genre".to_string(),
                "nested genre".to_string(),
            ])
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert!(!genre_selectors.contains(&"aaaaaaaabbbbccccddddeeeeeeeeeeee".to_string()));
        let filter_selectors = extract_media_item_filter_selectors(&json!({
            "pEoPlE": [
                {"Name": " Jane Doe ", "Id": "IMPORTED-PERSON"},
                2026
            ],
            "SeriesPeople": ["Inherited Person"],
            "sTuDiOs": [{"Id": "STUDIO-ID"}, {"Name": "HBO"}],
            "tAgS": [" Featured ", 7, {"Name": "Object Tag", "Id": "ignored"}]
        }));
        let filter_selectors = filter_selectors.into_iter().collect::<BTreeSet<_>>();
        for selector in [
            "jane doe".to_string(),
            stable_entity_id("Person", "Jane Doe"),
            "imported-person".to_string(),
            "2026".to_string(),
            stable_entity_id("Person", "2026"),
        ] {
            assert!(filter_selectors.contains(&(MediaItemFilterSelectorKind::Person, selector)));
        }
        assert!(!filter_selectors.contains(&(
            MediaItemFilterSelectorKind::Person,
            "inherited person".to_string()
        )));
        assert!(
            filter_selectors
                .contains(&(MediaItemFilterSelectorKind::Studio, "studio-id".to_string()))
        );
        assert!(filter_selectors.contains(&(
            MediaItemFilterSelectorKind::Studio,
            stable_entity_id("Studio", "HBO")
        )));
        for selector in ["featured", "7", "object tag"] {
            assert!(
                filter_selectors
                    .contains(&(MediaItemFilterSelectorKind::Tag, selector.to_string()))
            );
        }
        assert!(!filter_selectors.contains(&(
            MediaItemFilterSelectorKind::Tag,
            stable_entity_id("Tag", "Featured")
        )));
        assert!(
            extract_media_item_filter_selectors(&json!({
                "People": [],
                "Cast": " ",
                "SeriesPeople": ["Inherited Person"]
            }))
            .contains(&(
                MediaItemFilterSelectorKind::Person,
                "inherited person".to_string()
            ))
        );
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
            vec!["imported-person".to_string(), "second-id".to_string()]
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
