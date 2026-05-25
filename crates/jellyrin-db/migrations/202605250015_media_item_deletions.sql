CREATE TABLE IF NOT EXISTS media_item_deletions (
    path TEXT PRIMARY KEY,
    item_id TEXT NOT NULL,
    deleted_by_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    deleted_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_media_item_deletions_item
    ON media_item_deletions(item_id);
