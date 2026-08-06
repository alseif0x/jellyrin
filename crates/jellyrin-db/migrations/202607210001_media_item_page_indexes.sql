CREATE INDEX IF NOT EXISTS idx_media_items_created_by_folder
ON media_items(virtual_folder_id, missing_since, created_at DESC, name COLLATE NOCASE);

