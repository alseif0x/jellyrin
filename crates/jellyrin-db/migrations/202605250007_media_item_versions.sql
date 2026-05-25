CREATE TABLE IF NOT EXISTS media_item_versions (
    primary_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    alternate_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (primary_item_id, alternate_item_id),
    CHECK (primary_item_id <> alternate_item_id)
);

CREATE INDEX IF NOT EXISTS idx_media_item_versions_primary
    ON media_item_versions(primary_item_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_media_item_versions_alternate
    ON media_item_versions(alternate_item_id);
