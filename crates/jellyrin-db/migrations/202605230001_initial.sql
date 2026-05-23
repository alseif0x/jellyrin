CREATE TABLE IF NOT EXISTS server_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    server_id TEXT NOT NULL,
    server_name TEXT NOT NULL,
    startup_wizard_completed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

