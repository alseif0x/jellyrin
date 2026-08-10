CREATE UNIQUE INDEX idx_media_items_id_virtual_folder
ON media_items (id, virtual_folder_id);

CREATE TABLE media_item_query_filter_sources (
    item_id uuid PRIMARY KEY,
    virtual_folder_id uuid NOT NULL REFERENCES virtual_folders(id) ON UPDATE CASCADE ON DELETE CASCADE,
    extractor_version integer NOT NULL CHECK (extractor_version > 0),
    container_present boolean NOT NULL,
    container_value text,
    media_type text NOT NULL,
    is_video boolean NOT NULL,
    has_subtitles boolean NOT NULL,
    has_trailer boolean NOT NULL,
    projected_value_count integer NOT NULL CHECK (projected_value_count >= 0),
    completed_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (container_present = (container_value IS NOT NULL)),
    UNIQUE (item_id, virtual_folder_id),
    FOREIGN KEY (item_id, virtual_folder_id) REFERENCES media_items(id, virtual_folder_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE TABLE media_item_query_filter_values (
    item_id uuid NOT NULL,
    virtual_folder_id uuid NOT NULL,
    value_kind text NOT NULL CHECK (value_kind IN (
        'albums', 'artists', 'audio_languages', 'genres', 'official_ratings',
        'series_statuses', 'staff_names', 'studios', 'subtitle_languages', 'tags', 'years'
    )),
    display_value text NOT NULL,
    source_key text NOT NULL,
    source_priority integer NOT NULL CHECK (source_priority >= 0),
    source_position text NOT NULL,
    PRIMARY KEY (item_id, value_kind, source_key, source_position),
    FOREIGN KEY (item_id, virtual_folder_id)
        REFERENCES media_item_query_filter_sources(item_id, virtual_folder_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE INDEX idx_media_item_query_filter_values_lookup
ON media_item_query_filter_values (
    virtual_folder_id, value_kind, lower(btrim(display_value)), item_id
);

CREATE INDEX idx_media_item_query_filter_sources_folder
ON media_item_query_filter_sources (virtual_folder_id, item_id);

ALTER TABLE remote_media_catalog_stages
ADD COLUMN query_filter_extractor_version integer NOT NULL DEFAULT 1;

CREATE TABLE remote_media_catalog_stage_query_filter_sources (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    container_present boolean NOT NULL,
    container_value text,
    media_type text NOT NULL,
    is_video boolean NOT NULL,
    has_subtitles boolean NOT NULL,
    has_trailer boolean NOT NULL,
    projected_value_count integer NOT NULL CHECK (projected_value_count >= 0),
    PRIMARY KEY (stage_id, item_id),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_items(stage_id, id)
        ON UPDATE CASCADE ON DELETE CASCADE,
    CHECK (container_present = (container_value IS NOT NULL))
);

CREATE TABLE remote_media_catalog_stage_query_filter_values (
    stage_id uuid NOT NULL,
    item_id uuid NOT NULL,
    value_kind text NOT NULL,
    display_value text NOT NULL,
    source_key text NOT NULL,
    source_priority integer NOT NULL CHECK (source_priority >= 0),
    source_position text NOT NULL,
    PRIMARY KEY (stage_id, item_id, value_kind, source_key, source_position),
    FOREIGN KEY (stage_id, item_id)
        REFERENCES remote_media_catalog_stage_query_filter_sources(stage_id, item_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE FUNCTION jellyrin_invalidate_media_item_query_filter_projection()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_query_filter_sources WHERE item_id = NEW.id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER trg_media_items_query_filter_invalidate_update
AFTER UPDATE ON media_items
FOR EACH ROW
WHEN (
    OLD.path IS DISTINCT FROM NEW.path
    OR OLD.virtual_folder_id IS DISTINCT FROM NEW.virtual_folder_id
    OR OLD.media_type IS DISTINCT FROM NEW.media_type
    OR OLD.media_streams IS DISTINCT FROM NEW.media_streams
    OR OLD.metadata IS DISTINCT FROM NEW.metadata
)
EXECUTE FUNCTION jellyrin_invalidate_media_item_query_filter_projection();

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'jellyrin_runtime') THEN
        REVOKE ALL PRIVILEGES ON TABLE media_item_query_filter_sources,
            media_item_query_filter_values,
            remote_media_catalog_stage_query_filter_sources,
            remote_media_catalog_stage_query_filter_values FROM jellyrin_runtime;
        GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE media_item_query_filter_sources,
            media_item_query_filter_values,
            remote_media_catalog_stage_query_filter_sources,
            remote_media_catalog_stage_query_filter_values TO jellyrin_runtime;
    END IF;
END
$$;
