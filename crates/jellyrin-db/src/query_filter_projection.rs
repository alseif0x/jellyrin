use serde_json::Value;

pub(crate) const MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION: i32 = 1;

/// Immutable fields used to derive one item's `/Items/Filters` contribution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MediaItemQueryFilterProjectionSource<'a> {
    pub path: &'a str,
    pub media_type: &'a str,
    pub media_streams: &'a [Value],
    pub metadata: &'a Value,
}

/// Scalar filter features that have exactly zero or one value per media item.
///
/// `container_present` distinguishes a path without an extension from a path ending in `.`;
/// Jellyfin exposes the latter as an empty container value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaItemQueryFilterProjectionFeatures {
    pub container_present: bool,
    pub container: Option<String>,
    pub media_type: String,
    pub is_video: bool,
    pub has_subtitles: bool,
    pub has_trailer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MediaItemQueryFilterValueKind {
    Albums,
    Artists,
    AudioLanguages,
    Genres,
    OfficialRatings,
    SeriesStatuses,
    StaffNames,
    Studios,
    SubtitleLanguages,
    Tags,
    Years,
}

impl MediaItemQueryFilterValueKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::AudioLanguages => "audio_languages",
            Self::Genres => "genres",
            Self::OfficialRatings => "official_ratings",
            Self::SeriesStatuses => "series_statuses",
            Self::StaffNames => "staff_names",
            Self::Studios => "studios",
            Self::SubtitleLanguages => "subtitle_languages",
            Self::Tags => "tags",
            Self::Years => "years",
        }
    }
}

/// One narrow source-aware candidate for a multivalue.
///
/// Normalization, de-duplication, and display-spelling selection deliberately remain in each SQL
/// adapter. This preserves the native driver's `lower`/collation semantics, including Unicode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaItemQueryFilterProjectedValue {
    pub kind: MediaItemQueryFilterValueKind,
    pub display_value: String,
    pub source_key: String,
    pub source_priority: i16,
    pub position: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaItemQueryFilterProjection {
    pub extractor_version: i32,
    pub features: MediaItemQueryFilterProjectionFeatures,
    pub values: Vec<MediaItemQueryFilterProjectedValue>,
}

/// Extracts the exact source-aware values used by the native PostgreSQL/SQLite filter queries.
pub(crate) fn extract_media_item_query_filter_projection(
    source: MediaItemQueryFilterProjectionSource<'_>,
) -> MediaItemQueryFilterProjection {
    let container = media_item_container(source.path);
    let mut values = Vec::new();

    for (kind, key, priority) in metadata_sources() {
        if let Some(value) = source.metadata.get(key) {
            collect_metadata_candidates(*kind, key, *priority, value, &mut Vec::new(), &mut values);
        }
    }

    let mut has_subtitles = false;
    for (position, stream) in source.media_streams.iter().enumerate() {
        let Some(stream_type) = stream.get("Type").and_then(Value::as_str) else {
            continue;
        };
        let stream_type = stream_type.to_lowercase();
        if stream_type == "subtitle" {
            has_subtitles = true;
        }
        let Some(kind) = (match stream_type.as_str() {
            "audio" => Some(MediaItemQueryFilterValueKind::AudioLanguages),
            "subtitle" => Some(MediaItemQueryFilterValueKind::SubtitleLanguages),
            _ => None,
        }) else {
            continue;
        };
        let Some(language) = stream.get("Language").and_then(Value::as_str) else {
            continue;
        };
        let language = trim_sql_spaces(language);
        if language.is_empty() || language.eq_ignore_ascii_case("und") {
            continue;
        }
        let display_value = match language.to_ascii_lowercase().as_str() {
            "fre" => "fra".to_owned(),
            "ger" => "deu".to_owned(),
            _ => language.to_owned(),
        };
        values.push(MediaItemQueryFilterProjectedValue {
            kind,
            display_value,
            source_key: "MediaStreams.Language".to_owned(),
            source_priority: 0,
            position: vec![position],
        });
    }

    MediaItemQueryFilterProjection {
        extractor_version: MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
        features: MediaItemQueryFilterProjectionFeatures {
            container_present: container.is_some(),
            container,
            media_type: source.media_type.to_owned(),
            is_video: source.media_type.eq_ignore_ascii_case("video"),
            has_subtitles,
            has_trailer: metadata_has_trailer(source.metadata),
        },
        values,
    }
}

/// Portable lexical encoding of a nested JSON-array position.
///
/// Fixed-width components preserve numeric ordering, while the separator preserves the rule that
/// a parent position sorts before any of its descendants.
pub(crate) fn encode_media_item_query_filter_position(position: &[usize]) -> String {
    position
        .iter()
        .map(|part| format!("{part:020}"))
        .collect::<Vec<_>>()
        .join(".")
}

fn metadata_sources() -> &'static [(MediaItemQueryFilterValueKind, &'static str, i16)] {
    use MediaItemQueryFilterValueKind as Kind;

    &[
        (Kind::Albums, "Album", 0),
        (Kind::Albums, "AlbumName", 1),
        (Kind::Artists, "Artists", 0),
        (Kind::Artists, "AlbumArtists", 1),
        (Kind::Genres, "Genres", 0),
        (Kind::OfficialRatings, "OfficialRating", 0),
        (Kind::OfficialRatings, "OfficialRatings", 1),
        (Kind::SeriesStatuses, "SeriesStatus", 0),
        (Kind::StaffNames, "People", 0),
        (Kind::StaffNames, "SeriesPeople", 1),
        (Kind::Studios, "Studios", 0),
        (Kind::Tags, "Tags", 0),
        (Kind::Years, "ProductionYear", 0),
        (Kind::Years, "Years", 1),
    ]
}

fn collect_metadata_candidates(
    kind: MediaItemQueryFilterValueKind,
    source_key: &str,
    source_priority: i16,
    value: &Value,
    position: &mut Vec<usize>,
    candidates: &mut Vec<MediaItemQueryFilterProjectedValue>,
) {
    if let Some(array) = value.as_array() {
        for (index, child) in array.iter().enumerate() {
            position.push(index);
            collect_metadata_candidates(
                kind,
                source_key,
                source_priority,
                child,
                position,
                candidates,
            );
            position.pop();
        }
        return;
    }

    let display_value = match value {
        Value::String(value) => Some(trim_sql_spaces(value).to_owned()),
        Value::Number(value) => Some(value.to_string()),
        Value::Object(value) => value
            .get("Name")
            .and_then(Value::as_str)
            .map(trim_sql_spaces)
            .map(ToOwned::to_owned),
        _ => None,
    };
    let Some(display_value) = display_value.filter(|value| !value.is_empty()) else {
        return;
    };
    candidates.push(MediaItemQueryFilterProjectedValue {
        kind,
        display_value,
        source_key: source_key.to_owned(),
        source_priority,
        position: position.clone(),
    });
}

fn trim_sql_spaces(value: &str) -> &str {
    value.trim_matches(' ')
}

fn media_item_container(path: &str) -> Option<String> {
    let filename = path.rsplit('/').next()?;
    if filename.is_empty() || matches!(filename, "." | "..") {
        return None;
    }
    let dot = filename.rfind('.')?;
    if dot == 0 && !filename[1..].contains('.') {
        return None;
    }
    Some(filename[dot + 1..].to_owned())
}

fn metadata_has_trailer(metadata: &Value) -> bool {
    let Some(metadata) = metadata.as_object() else {
        return false;
    };
    metadata.iter().any(|(key, value)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "remotetrailers" | "trailers"
        ) && trailer_value_is_present(value)
    })
}

fn trailer_value_is_present(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(trailer_value_is_present),
        Value::String(value) => !trim_sql_spaces(value).is_empty(),
        Value::Object(object) => {
            for key in ["Url", "url", "Path", "path"] {
                if let Some(value) = object.get(key) {
                    return value
                        .as_str()
                        .is_some_and(|value| !trim_sql_spaces(value).is_empty());
                }
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn extract(
        path: &str,
        media_type: &str,
        media_streams: &[Value],
        metadata: &Value,
    ) -> MediaItemQueryFilterProjection {
        extract_media_item_query_filter_projection(MediaItemQueryFilterProjectionSource {
            path,
            media_type,
            media_streams,
            metadata,
        })
    }

    fn displays(
        projection: &MediaItemQueryFilterProjection,
        kind: MediaItemQueryFilterValueKind,
    ) -> Vec<&str> {
        projection
            .values
            .iter()
            .filter(|value| value.kind == kind)
            .map(|value| value.display_value.as_str())
            .collect()
    }

    #[test]
    fn extracts_the_complete_legacy_filter_surface_without_broadening_keys() {
        let metadata = json!({
            "Album": "Album One",
            "AlbumName": ["Album Two"],
            "Albums": ["Must Not Leak"],
            "Artists": [{"Name": "Artist One"}],
            "AlbumArtists": ["Artist Two"],
            "artists": ["Wrong Case"],
            "Genres": [" Drama ", "drama"],
            "MusicGenres": ["Must Not Leak"],
            "OfficialRating": "PG-13",
            "OfficialRatings": ["R"],
            "SeriesStatus": "Continuing",
            "People": [{"Name": "Actor One", "Role": "Lead"}],
            "SeriesPeople": ["Actor Two"],
            "Studios": [{"Name": "Studio One"}],
            "SeriesStudios": ["Must Not Leak"],
            "Tags": ["Featured"],
            "ProductionYear": 2025,
            "Years": [2024],
            "rEmOtEtRaIlErS": [{"Url": "https://example.invalid/trailer"}]
        });
        let streams = vec![
            json!({"Type": "Video"}),
            json!({"Type": "Audio", "Language": "fre"}),
            json!({"Type": "Subtitle", "Language": "spa"}),
        ];
        let projection = extract(
            "provider://filters/target.MKV",
            "Video",
            &streams,
            &metadata,
        );

        assert_eq!(projection.extractor_version, 1);
        assert_eq!(projection.features.container.as_deref(), Some("MKV"));
        assert_eq!(projection.features.media_type, "Video");
        assert!(projection.features.is_video);
        assert!(projection.features.has_subtitles);
        assert!(projection.features.has_trailer);
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Albums),
            ["Album One", "Album Two"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Artists),
            ["Artist One", "Artist Two"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::AudioLanguages),
            ["fra"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Genres),
            ["Drama", "drama"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::OfficialRatings),
            ["PG-13", "R"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::SeriesStatuses),
            ["Continuing"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::StaffNames),
            ["Actor One", "Actor Two"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Studios),
            ["Studio One"]
        );
        assert_eq!(
            displays(
                &projection,
                MediaItemQueryFilterValueKind::SubtitleLanguages
            ),
            ["spa"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Tags),
            ["Featured"]
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Years),
            ["2025", "2024"]
        );
        assert!(!format!("{projection:?}").contains("Must Not Leak"));
        assert!(!format!("{projection:?}").contains("Wrong Case"));
    }

    #[test]
    fn recursively_expands_arrays_and_accepts_only_supported_scalar_shapes() {
        let projection = extract(
            "item.mp4",
            "Video",
            &[],
            &json!({
                "Genres": [
                    " First ",
                    ["Nested", [42, 2.5]],
                    {"Name": "Object"},
                    {"Name": 123},
                    {"name": "wrong case"},
                    true,
                    null,
                    ""
                ]
            }),
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Genres),
            ["First", "Nested", "42", "2.5", "Object"]
        );
        let nested = projection
            .values
            .iter()
            .find(|value| value.display_value == "42")
            .unwrap();
        assert_eq!(nested.position, [1, 1, 0]);
        assert_eq!(nested.source_key, "Genres");
    }

    #[test]
    fn duplicate_spellings_are_retained_with_source_priority_and_nested_position() {
        let projection = extract(
            "item.mp3",
            "Audio",
            &[],
            &json!({
                "Artists": [[" FIRST "], "First"],
                "AlbumArtists": ["first", "Second"]
            }),
        );
        let artists = projection
            .values
            .iter()
            .filter(|value| value.kind == MediaItemQueryFilterValueKind::Artists)
            .collect::<Vec<_>>();
        let first = artists[0];
        assert_eq!(first.display_value, "FIRST");
        assert_eq!(first.source_key, "Artists");
        assert_eq!(first.source_priority, 0);
        assert_eq!(first.position, [0, 0]);
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Artists),
            ["FIRST", "First", "first", "Second"]
        );
        assert_eq!(artists[1].position, [1]);
        assert_eq!(artists[2].source_key, "AlbumArtists");
        assert_eq!(artists[2].source_priority, 1);
        assert_eq!(artists[2].position, [0]);
    }

    #[test]
    fn candidates_preserve_unicode_and_exact_duplicate_spellings_for_driver_normalization() {
        let projection = extract(
            "item.mkv",
            "Video",
            &[],
            &json!({"Genres": ["Straße", "STRASSE", "İ", "i̇", "Straße"]}),
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Genres),
            ["Straße", "STRASSE", "İ", "i̇", "Straße"]
        );
        assert_eq!(
            projection
                .values
                .iter()
                .map(|value| value.position.as_slice())
                .collect::<Vec<_>>(),
            [&[0][..], &[1][..], &[2][..], &[3][..], &[4][..]]
        );
    }

    #[test]
    fn stream_languages_and_subtitle_feature_match_legacy_rules() {
        let streams = vec![
            json!({"Type": "Audio", "Language": " fre "}),
            json!({"Type": "Audio", "Language": "ger"}),
            json!({"Type": "Audio", "Language": "ENG"}),
            json!({"Type": "Audio", "Language": "und"}),
            json!({"Type": " Audio ", "Language": "ita"}),
            json!({"Type": "Audio", "Language": 123}),
            json!({"Type": "Subtitle"}),
            json!({"Type": "SUBTITLE", "Language": " spa "}),
            json!({"Type": 123, "Language": "por"}),
        ];
        let projection = extract("item.mkv", "Video", &streams, &json!({}));
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::AudioLanguages),
            ["fra", "deu", "ENG"]
        );
        assert_eq!(
            displays(
                &projection,
                MediaItemQueryFilterValueKind::SubtitleLanguages
            ),
            ["spa"]
        );
        assert!(projection.features.has_subtitles);
    }

    #[test]
    fn trailer_object_key_precedence_does_not_fall_through_invalid_values() {
        for invalid in [json!(""), Value::Null, json!(123)] {
            let projection = extract(
                "item.mp4",
                "Video",
                &[],
                &json!({"Trailers": [{"Url": invalid, "url": "https://must-not-win"}]}),
            );
            assert!(!projection.features.has_trailer);
        }
        for metadata in [
            json!({"Trailers": [[" direct "]]}),
            json!({"remoteTRAILERS": [{"path": " local "}]}),
            json!({"RemoteTrailers": [{"Path": "path"}]}),
        ] {
            assert!(
                extract("item.mp4", "Video", &[], &metadata)
                    .features
                    .has_trailer
            );
        }
        assert!(
            !extract("item.mp4", "Video", &[], &json!({"Trailer": ["wrong key"]}))
                .features
                .has_trailer
        );
    }

    #[test]
    fn container_extraction_preserves_empty_extension_and_path_rules() {
        for (path, expected) in [
            ("provider://x/movie.MKV", Some("MKV")),
            ("provider://x/.hidden", None),
            ("provider://x/.foo.bar", Some("bar")),
            ("provider://x/foo.", Some("")),
            ("provider://x/foo.tar.gz", Some("gz")),
            ("provider://x/slash/", None),
            ("provider://x/back\\slash.mkv", Some("mkv")),
            ("provider://x/.", None),
            ("provider://x/..", None),
            ("provider://x/no-extension", None),
        ] {
            let projection = extract(path, "Video", &[], &json!({}));
            assert_eq!(projection.features.container.as_deref(), expected, "{path}");
            assert_eq!(
                projection.features.container_present,
                expected.is_some(),
                "{path}"
            );
        }
    }

    #[test]
    fn media_type_keeps_exact_spelling_but_video_detection_is_case_insensitive() {
        let upper = extract("item", "Video", &[], &json!({}));
        let lower = extract("item", "video", &[], &json!({}));
        let padded = extract("item", " Video ", &[], &json!({}));
        assert_eq!(upper.features.media_type, "Video");
        assert_eq!(lower.features.media_type, "video");
        assert!(upper.features.is_video);
        assert!(lower.features.is_video);
        assert!(!padded.features.is_video);
    }

    #[test]
    fn sql_trim_removes_spaces_only() {
        let projection = extract(
            "item",
            "Video",
            &[],
            &json!({"Genres": ["  Drama  ", "\tTabbed\t", "   "]}),
        );
        assert_eq!(
            displays(&projection, MediaItemQueryFilterValueKind::Genres),
            ["Drama", "\tTabbed\t"]
        );
    }

    #[test]
    fn all_kinds_have_stable_storage_names() {
        use MediaItemQueryFilterValueKind as Kind;
        assert_eq!(
            [
                Kind::Albums,
                Kind::Artists,
                Kind::AudioLanguages,
                Kind::Genres,
                Kind::OfficialRatings,
                Kind::SeriesStatuses,
                Kind::StaffNames,
                Kind::Studios,
                Kind::SubtitleLanguages,
                Kind::Tags,
                Kind::Years,
            ]
            .map(Kind::as_str),
            [
                "albums",
                "artists",
                "audio_languages",
                "genres",
                "official_ratings",
                "series_statuses",
                "staff_names",
                "studios",
                "subtitle_languages",
                "tags",
                "years",
            ]
        );
    }

    #[test]
    fn encoded_nested_positions_preserve_lexicographic_array_order() {
        let parent = encode_media_item_query_filter_position(&[1]);
        let nested_ten = encode_media_item_query_filter_position(&[1, 10]);
        let sibling_two = encode_media_item_query_filter_position(&[2]);
        assert!(parent < nested_ten);
        assert!(nested_ten < sibling_two);
        assert_eq!(
            encode_media_item_query_filter_position(&[1, 10]),
            encode_media_item_query_filter_position(&[1, 10])
        );
    }
}
