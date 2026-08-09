-- SQLite parity for catalog paging tests and transitional legacy deployments.
CREATE INDEX IF NOT EXISTS idx_media_items_visible_name_page
    ON media_items(lower(name), id)
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_name_page
    ON media_items(virtual_folder_id, lower(name), id)
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_created_page
    ON media_items(created_at, id)
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_updated_page
    ON media_items(updated_at, id)
    WHERE missing_since IS NULL;
