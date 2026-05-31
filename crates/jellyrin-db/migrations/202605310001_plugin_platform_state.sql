CREATE TABLE IF NOT EXISTS plugin_repositories (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1,
    payload_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS package_catalog_cache (
    id TEXT PRIMARY KEY,
    repository_url TEXT NOT NULL,
    package_guid TEXT,
    package_name TEXT NOT NULL,
    package_version TEXT NOT NULL,
    runtime TEXT NOT NULL DEFAULT 'Unknown',
    target_abi TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(repository_url, package_name, package_version)
);

CREATE TABLE IF NOT EXISTS package_installations (
    id TEXT PRIMARY KEY,
    package_name TEXT NOT NULL,
    package_guid TEXT,
    version TEXT NOT NULL,
    runtime TEXT NOT NULL,
    status TEXT NOT NULL,
    source_url TEXT,
    payload_json TEXT NOT NULL,
    installed_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS installed_plugins (
    plugin_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    runtime TEXT NOT NULL,
    runtime_version TEXT NOT NULL DEFAULT '',
    target_abi TEXT NOT NULL DEFAULT '',
    server_compatibility_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    permissions_json TEXT NOT NULL DEFAULT '[]',
    configuration_state TEXT NOT NULL DEFAULT 'Default',
    last_error TEXT,
    health_json TEXT NOT NULL DEFAULT '{}',
    manifest_json TEXT NOT NULL DEFAULT '{}',
    installed_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_manifests (
    plugin_id TEXT PRIMARY KEY,
    manifest_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_configurations (
    plugin_id TEXT PRIMARY KEY,
    configuration_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_permissions (
    plugin_id TEXT PRIMARY KEY,
    permissions_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_runtime_instances (
    instance_id TEXT PRIMARY KEY,
    plugin_id TEXT,
    runtime TEXT NOT NULL,
    runtime_version TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    process_id INTEGER,
    endpoint TEXT,
    health_json TEXT NOT NULL DEFAULT '{}',
    last_error TEXT,
    started_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_host_events (
    id TEXT PRIMARY KEY,
    plugin_id TEXT,
    runtime TEXT,
    event_type TEXT NOT NULL,
    severity TEXT NOT NULL,
    message TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_audit_log (
    id TEXT PRIMARY KEY,
    plugin_id TEXT,
    action TEXT NOT NULL,
    actor_user_id TEXT,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
