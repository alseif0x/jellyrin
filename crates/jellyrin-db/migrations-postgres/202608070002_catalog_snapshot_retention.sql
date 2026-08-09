-- Remote catalog entries are reconstructible, but state that references them is not. Keep stale
-- rows as tombstones so a transient or incomplete provider snapshot cannot cascade-delete
-- playback progress, playlists, lyrics, or audit history.
ALTER TABLE media_items DROP CONSTRAINT IF EXISTS media_items_path_key;

CREATE UNIQUE INDEX media_items_visible_path_unique
    ON media_items (path)
    WHERE missing_since IS NULL;

CREATE INDEX media_items_missing_retention_idx
    ON media_items (missing_since)
    WHERE missing_since IS NOT NULL;

CREATE TABLE catalog_sync_runs (
    id uuid PRIMARY KEY,
    virtual_folder_id uuid NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    generation_id uuid NOT NULL UNIQUE,
    status text NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    item_count bigint NOT NULL DEFAULT 0 CHECK (item_count >= 0),
    started_at timestamptz NOT NULL,
    completed_at timestamptz,
    error_message text,
    CHECK (
        (status = 'running' AND completed_at IS NULL)
        OR (status IN ('completed', 'failed') AND completed_at IS NOT NULL)
    )
);

CREATE INDEX catalog_sync_runs_folder_started_idx
    ON catalog_sync_runs (virtual_folder_id, started_at DESC);
