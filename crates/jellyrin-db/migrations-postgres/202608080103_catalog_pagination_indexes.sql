-- Support bounded /Items pages without sorting the complete visible catalog on every request.
-- Partial indexes keep missing/tombstoned rows out of the browse hot path.
CREATE INDEX media_items_visible_name_page_idx
    ON media_items (lower(name), id)
    WHERE missing_since IS NULL;

CREATE INDEX media_items_visible_folder_name_page_idx
    ON media_items (virtual_folder_id, lower(name), id)
    WHERE missing_since IS NULL;

CREATE INDEX media_items_visible_created_page_idx
    ON media_items (created_at, id)
    WHERE missing_since IS NULL;

CREATE INDEX media_items_visible_updated_page_idx
    ON media_items (updated_at, id)
    WHERE missing_since IS NULL;
