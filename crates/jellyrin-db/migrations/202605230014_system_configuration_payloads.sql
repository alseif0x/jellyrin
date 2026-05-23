CREATE TABLE IF NOT EXISTS system_configuration_payloads (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    content_types_json TEXT NOT NULL DEFAULT '[]',
    metadata_options_json TEXT NOT NULL DEFAULT '[]',
    path_substitutions_json TEXT NOT NULL DEFAULT '[]',
    plugin_repositories_json TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);
