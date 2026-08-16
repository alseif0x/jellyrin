-- Home and Latest endpoints order by the complete deterministic tuple below.
-- The older indexes omitted lower(name), forcing PostgreSQL to scan and sort
-- hundreds of thousands of visible catalogue rows before returning 20 items.
CREATE INDEX IF NOT EXISTS idx_media_items_visible_updated_name_page
    ON media_items (updated_at DESC, lower(name) DESC, id DESC)
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_updated_name_page
    ON media_items (virtual_folder_id, updated_at DESC, lower(name) DESC, id DESC)
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_collection_media_updated_name_page
    ON media_items (
        collection_type,
        media_type,
        updated_at DESC,
        lower(name) DESC,
        id DESC
    )
    WHERE missing_since IS NULL;
