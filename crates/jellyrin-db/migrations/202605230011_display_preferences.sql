CREATE TABLE IF NOT EXISTS display_preferences (
    id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    client TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (id, user_id, client),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
