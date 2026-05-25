CREATE TABLE IF NOT EXISTS quick_connect_sessions (
    secret TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    client TEXT NOT NULL,
    version TEXT NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    authorized INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_quick_connect_sessions_code
ON quick_connect_sessions(code);

CREATE INDEX IF NOT EXISTS idx_quick_connect_sessions_expires
ON quick_connect_sessions(expires_at);
