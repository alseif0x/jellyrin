CREATE TABLE IF NOT EXISTS active_viewing_sessions (
    session_id TEXT PRIMARY KEY REFERENCES devices(access_token) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_active_viewing_sessions_user
ON active_viewing_sessions(user_id);
