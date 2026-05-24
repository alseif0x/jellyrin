CREATE TABLE IF NOT EXISTS backup_manifests (
    path TEXT PRIMARY KEY,
    server_version TEXT NOT NULL,
    backup_engine_version TEXT NOT NULL,
    options_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
