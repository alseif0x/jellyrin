CREATE TABLE IF NOT EXISTS media_items (
    id TEXT PRIMARY KEY,
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    media_type TEXT NOT NULL,
    collection_type TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_media_items_virtual_folder ON media_items(virtual_folder_id);
CREATE INDEX IF NOT EXISTS idx_media_items_type ON media_items(media_type);
