-- Android TV loads each home library independently with a newest-first page.
-- Lead with the folder so a cold query cannot scan another provider's catalogue.
CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_collection_media_updated_name_page
ON media_items (
    virtual_folder_id,
    collection_type,
    media_type,
    updated_at DESC,
    lower(name) DESC,
    id DESC
)
WHERE missing_since IS NULL;
