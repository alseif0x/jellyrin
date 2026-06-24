CREATE INDEX IF NOT EXISTS idx_media_items_latest_by_folder
ON media_items(virtual_folder_id, missing_since, updated_at DESC, name COLLATE NOCASE);
