CREATE TABLE IF NOT EXISTS trickplay_infos (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    tile_width INTEGER NOT NULL,
    tile_height INTEGER NOT NULL,
    thumbnail_count INTEGER NOT NULL,
    interval_ms INTEGER NOT NULL,
    bandwidth INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (item_id, width)
);

CREATE INDEX IF NOT EXISTS idx_trickplay_infos_item ON trickplay_infos(item_id);
