CREATE TABLE IF NOT EXISTS named_configurations (
    key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
