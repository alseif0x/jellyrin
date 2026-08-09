-- SQLite is retained only as a test/legacy adapter, but its Xtream catalogue harness mirrors the
-- production PostgreSQL generation contract so provider tests catch partial publications.
CREATE TABLE IF NOT EXISTS catalog_sync_runs (
    id TEXT PRIMARY KEY,
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    generation_id TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    item_count INTEGER NOT NULL DEFAULT 0 CHECK (item_count >= 0),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    error_message TEXT,
    CHECK (
        (status = 'running' AND completed_at IS NULL)
        OR (status IN ('completed', 'failed') AND completed_at IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_catalog_sync_runs_folder_started
    ON catalog_sync_runs(virtual_folder_id, started_at DESC);
