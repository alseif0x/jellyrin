CREATE TABLE IF NOT EXISTS startup_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    ui_culture TEXT NOT NULL DEFAULT 'en-US',
    metadata_country_code TEXT NOT NULL DEFAULT 'US',
    preferred_metadata_language TEXT NOT NULL DEFAULT 'en',
    enable_remote_access INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
    is_administrator INTEGER NOT NULL DEFAULT 0,
    is_disabled INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_passwords (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    algorithm TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS devices (
    access_token TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    client TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_activity_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_devices_user_device ON devices(user_id, device_id);
