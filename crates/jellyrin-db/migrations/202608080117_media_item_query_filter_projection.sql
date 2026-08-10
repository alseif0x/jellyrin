CREATE UNIQUE INDEX idx_media_items_id_virtual_folder
ON media_items (id, virtual_folder_id);

CREATE TABLE media_item_query_filter_sources (
    item_id TEXT PRIMARY KEY,
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON UPDATE CASCADE ON DELETE CASCADE,
    extractor_version INTEGER NOT NULL CHECK (extractor_version > 0),
    container_present INTEGER NOT NULL CHECK (container_present IN (0, 1)),
    container_value TEXT,
    media_type TEXT NOT NULL,
    is_video INTEGER NOT NULL CHECK (is_video IN (0, 1)),
    has_subtitles INTEGER NOT NULL CHECK (has_subtitles IN (0, 1)),
    has_trailer INTEGER NOT NULL CHECK (has_trailer IN (0, 1)),
    projected_value_count INTEGER NOT NULL CHECK (projected_value_count >= 0),
    completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (container_present = (container_value IS NOT NULL)),
    UNIQUE (item_id, virtual_folder_id),
    FOREIGN KEY (item_id, virtual_folder_id) REFERENCES media_items(id, virtual_folder_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE TABLE media_item_query_filter_values (
    item_id TEXT NOT NULL,
    virtual_folder_id TEXT NOT NULL,
    value_kind TEXT NOT NULL CHECK (value_kind IN (
        'albums', 'artists', 'audio_languages', 'genres', 'official_ratings',
        'series_statuses', 'staff_names', 'studios', 'subtitle_languages', 'tags', 'years'
    )),
    display_value TEXT NOT NULL,
    source_key TEXT NOT NULL,
    source_priority INTEGER NOT NULL CHECK (source_priority >= 0),
    source_position TEXT NOT NULL,
    PRIMARY KEY (item_id, value_kind, source_key, source_position),
    FOREIGN KEY (item_id, virtual_folder_id)
        REFERENCES media_item_query_filter_sources(item_id, virtual_folder_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE INDEX idx_media_item_query_filter_values_lookup
ON media_item_query_filter_values (
    virtual_folder_id, value_kind, display_value COLLATE NOCASE, item_id
);

CREATE INDEX idx_media_item_query_filter_sources_folder
ON media_item_query_filter_sources (virtual_folder_id, item_id);

ALTER TABLE remote_media_catalog_stages
ADD COLUMN query_filter_extractor_version INTEGER NOT NULL DEFAULT 1;

CREATE TABLE remote_media_catalog_stage_query_filter_sources (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    container_present INTEGER NOT NULL CHECK (container_present IN (0, 1)),
    container_value TEXT,
    media_type TEXT NOT NULL,
    is_video INTEGER NOT NULL CHECK (is_video IN (0, 1)),
    has_subtitles INTEGER NOT NULL CHECK (has_subtitles IN (0, 1)),
    has_trailer INTEGER NOT NULL CHECK (has_trailer IN (0, 1)),
    projected_value_count INTEGER NOT NULL CHECK (projected_value_count >= 0),
    PRIMARY KEY (stage_id, item_id),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    CHECK (container_present = (container_value IS NOT NULL))
);

CREATE TABLE remote_media_catalog_stage_query_filter_values (
    stage_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    value_kind TEXT NOT NULL,
    display_value TEXT NOT NULL,
    source_key TEXT NOT NULL,
    source_priority INTEGER NOT NULL CHECK (source_priority >= 0),
    source_position TEXT NOT NULL,
    PRIMARY KEY (stage_id, item_id, value_kind, source_key, source_position),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_query_filter_sources(stage_id, item_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE TRIGGER trg_media_items_query_filter_invalidate_update
AFTER UPDATE OF virtual_folder_id, path, media_type, media_streams_json, metadata_json ON media_items
WHEN OLD.virtual_folder_id IS NOT NEW.virtual_folder_id
  OR OLD.path IS NOT NEW.path
  OR OLD.media_type IS NOT NEW.media_type
  OR OLD.media_streams_json IS NOT NEW.media_streams_json
  OR OLD.metadata_json IS NOT NEW.metadata_json
BEGIN
    DELETE FROM media_item_query_filter_sources WHERE item_id = NEW.id;
END;
