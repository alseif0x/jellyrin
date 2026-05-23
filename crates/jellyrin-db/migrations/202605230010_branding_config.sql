CREATE TABLE IF NOT EXISTS branding_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    login_disclaimer TEXT,
    custom_css TEXT,
    splashscreen_enabled INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL
);
