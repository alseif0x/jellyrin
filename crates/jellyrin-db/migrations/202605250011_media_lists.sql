CREATE TABLE IF NOT EXISTS media_lists (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    collection_type TEXT,
    owner_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_media_lists_kind
    ON media_lists(kind);

CREATE TABLE IF NOT EXISTS media_list_items (
    list_id TEXT NOT NULL REFERENCES media_lists(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    playlist_item_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    added_at TEXT NOT NULL,
    PRIMARY KEY (list_id, item_id),
    UNIQUE (playlist_item_id)
);

CREATE INDEX IF NOT EXISTS idx_media_list_items_list_position
    ON media_list_items(list_id, position);
