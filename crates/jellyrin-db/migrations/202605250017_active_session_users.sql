CREATE TABLE IF NOT EXISTS active_session_users (
    session_id TEXT NOT NULL REFERENCES devices(access_token) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    added_at TEXT NOT NULL,
    PRIMARY KEY (session_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_active_session_users_user
ON active_session_users(user_id);
