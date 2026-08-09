CREATE TABLE remote_media_catalog_stages (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('open', 'publishing', 'aborted')),
    extractor_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE remote_media_catalog_stage_libraries (
    stage_id TEXT NOT NULL REFERENCES remote_media_catalog_stages(id) ON DELETE CASCADE,
    library_key TEXT NOT NULL CHECK (library_key IN ('movies', 'series')),
    position INTEGER NOT NULL CHECK (position IN (0, 1)),
    library_name TEXT NOT NULL CHECK (trim(library_name) <> ''),
    collection_type TEXT NOT NULL,
    source_location TEXT NOT NULL,
    item_count INTEGER NOT NULL DEFAULT 0 CHECK (item_count >= 0 AND item_count <= 1000000),
    PRIMARY KEY (stage_id, library_key),
    UNIQUE (stage_id, position)
);

CREATE UNIQUE INDEX remote_media_catalog_stage_libraries_name_idx
    ON remote_media_catalog_stage_libraries (stage_id, library_name COLLATE NOCASE);

CREATE TABLE remote_media_catalog_stage_items (
    stage_id TEXT NOT NULL,
    library_key TEXT NOT NULL,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    media_type TEXT NOT NULL,
    collection_type TEXT NOT NULL,
    runtime_ticks INTEGER,
    bitrate INTEGER,
    width INTEGER,
    height INTEGER,
    media_streams_json TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    PRIMARY KEY (stage_id, id),
    UNIQUE (stage_id, path),
    FOREIGN KEY (stage_id, library_key)
        REFERENCES remote_media_catalog_stage_libraries(stage_id, library_key)
        ON DELETE CASCADE
);

CREATE INDEX remote_media_catalog_stage_items_library_idx
    ON remote_media_catalog_stage_items (stage_id, library_key, id);

CREATE TABLE remote_media_catalog_stage_facets (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    facet_kind TEXT NOT NULL CHECK (facet_kind IN (
        'genre', 'music_genre', 'music_artist', 'music_album_artist', 'music_album',
        'person', 'studio', 'tag', 'year'
    )),
    normalized_value TEXT NOT NULL,
    display_value TEXT NOT NULL,
    stable_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    payload_json TEXT NOT NULL,
    PRIMARY KEY (stage_id, item_id, facet_kind, normalized_value),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_facet_aliases (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    facet_kind TEXT NOT NULL,
    normalized_value TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    PRIMARY KEY (stage_id, item_id, facet_kind, normalized_value, entity_id),
    FOREIGN KEY (stage_id, item_id, facet_kind, normalized_value)
        REFERENCES remote_media_catalog_stage_facets(
            stage_id, item_id, facet_kind, normalized_value
        )
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_genre_selectors (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    selector TEXT NOT NULL,
    PRIMARY KEY (stage_id, item_id, selector),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_filter_selectors (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    selector_kind TEXT NOT NULL CHECK (selector_kind IN ('person', 'studio', 'tag')),
    selector TEXT NOT NULL,
    PRIMARY KEY (stage_id, item_id, selector_kind, selector),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_upcoming_dates (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    unix_seconds INTEGER NOT NULL,
    nanosecond INTEGER NOT NULL CHECK (nanosecond >= 0 AND nanosecond < 1000000000),
    PRIMARY KEY (stage_id, item_id),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);
