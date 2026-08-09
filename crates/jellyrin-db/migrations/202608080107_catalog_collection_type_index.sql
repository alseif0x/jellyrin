-- SQLite parity for the measured PostgreSQL collection browse hot path.
CREATE INDEX IF NOT EXISTS idx_media_items_visible_collection_name_page
    ON media_items(collection_type, lower(name), id)
    WHERE missing_since IS NULL;
