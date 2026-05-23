ALTER TABLE media_items ADD COLUMN last_seen_at TEXT;
ALTER TABLE media_items ADD COLUMN missing_since TEXT;
ALTER TABLE media_items ADD COLUMN file_size INTEGER;
ALTER TABLE media_items ADD COLUMN modified_at TEXT;

CREATE INDEX IF NOT EXISTS idx_media_items_visible
ON media_items(virtual_folder_id, missing_since);

CREATE INDEX IF NOT EXISTS idx_media_items_missing_identity
ON media_items(virtual_folder_id, media_type, file_size, modified_at, missing_since);
