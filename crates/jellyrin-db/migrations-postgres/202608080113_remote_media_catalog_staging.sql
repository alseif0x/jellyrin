CREATE TABLE remote_media_catalog_stages (
    id uuid PRIMARY KEY,
    status text NOT NULL CHECK (status IN ('open', 'publishing', 'aborted')),
    extractor_version integer NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE remote_media_catalog_stage_libraries (
    stage_id uuid NOT NULL REFERENCES remote_media_catalog_stages(id) ON DELETE CASCADE,
    library_key text NOT NULL CHECK (library_key IN ('movies', 'series')),
    position smallint NOT NULL CHECK (position IN (0, 1)),
    library_name text NOT NULL CHECK (btrim(library_name) <> ''),
    collection_type text NOT NULL,
    source_location text NOT NULL,
    item_count bigint NOT NULL DEFAULT 0 CHECK (item_count >= 0 AND item_count <= 1000000),
    PRIMARY KEY (stage_id, library_key),
    UNIQUE (stage_id, position)
);

CREATE UNIQUE INDEX remote_media_catalog_stage_libraries_name_idx
    ON remote_media_catalog_stage_libraries (stage_id, lower(library_name));

CREATE TABLE remote_media_catalog_stage_items (
    stage_id uuid NOT NULL,
    library_key text NOT NULL,
    id uuid NOT NULL,
    name text NOT NULL,
    path text NOT NULL,
    media_type text NOT NULL,
    collection_type text NOT NULL,
    runtime_ticks bigint,
    bitrate bigint,
    width integer,
    height integer,
    media_streams jsonb NOT NULL,
    metadata jsonb NOT NULL,
    PRIMARY KEY (stage_id, id),
    UNIQUE (stage_id, path),
    FOREIGN KEY (stage_id, library_key)
        REFERENCES remote_media_catalog_stage_libraries(stage_id, library_key)
        ON DELETE CASCADE
);

CREATE INDEX remote_media_catalog_stage_items_library_idx
    ON remote_media_catalog_stage_items (stage_id, library_key, id);

CREATE TABLE remote_media_catalog_stage_facets (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    facet_kind text NOT NULL CHECK (facet_kind IN (
        'genre', 'music_genre', 'music_artist', 'music_album_artist', 'music_album',
        'person', 'studio', 'tag', 'year'
    )),
    normalized_value text NOT NULL,
    display_value text NOT NULL,
    stable_id text NOT NULL,
    position integer NOT NULL CHECK (position >= 0),
    payload jsonb NOT NULL,
    PRIMARY KEY (stage_id, item_id, facet_kind, normalized_value),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_facet_aliases (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    facet_kind text NOT NULL,
    normalized_value text NOT NULL,
    entity_id text NOT NULL,
    PRIMARY KEY (stage_id, item_id, facet_kind, normalized_value, entity_id),
    FOREIGN KEY (stage_id, item_id, facet_kind, normalized_value)
        REFERENCES remote_media_catalog_stage_facets(
            stage_id, item_id, facet_kind, normalized_value
        )
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_genre_selectors (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    selector text NOT NULL,
    PRIMARY KEY (stage_id, item_id, selector),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_filter_selectors (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    selector_kind text NOT NULL CHECK (selector_kind IN ('person', 'studio', 'tag')),
    selector text NOT NULL,
    PRIMARY KEY (stage_id, item_id, selector_kind, selector),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);

CREATE TABLE remote_media_catalog_stage_upcoming_dates (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    unix_seconds bigint NOT NULL,
    nanosecond integer NOT NULL CHECK (nanosecond >= 0 AND nanosecond < 1000000000),
    PRIMARY KEY (stage_id, item_id),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON DELETE CASCADE
);
