CREATE TABLE IF NOT EXISTS activity_log_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    overview TEXT,
    short_overview TEXT,
    entry_type TEXT NOT NULL,
    severity TEXT NOT NULL DEFAULT 'Information',
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_activity_log_entries_created
ON activity_log_entries(created_at DESC, id DESC);
