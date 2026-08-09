-- Preserve path lookup performance after replacing the original global UNIQUE(path) constraint
-- with the tombstone-aware partial uniqueness rule in migration 002.
CREATE INDEX media_items_path_idx ON media_items (path);

-- PostgreSQL does not automatically index the referencing side of foreign keys. These indexes
-- keep item/user deletion and the corresponding joins bounded as the durable state grows.
CREATE INDEX api_keys_user_idx ON api_keys (user_id);
CREATE INDEX playback_states_item_idx ON playback_states (item_id);
CREATE INDEX active_playback_sessions_item_idx ON active_playback_sessions (item_id);
CREATE INDEX transcode_sessions_item_idx ON transcode_sessions (item_id);
CREATE INDEX quick_connect_sessions_user_idx
    ON quick_connect_sessions (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX media_list_items_item_idx ON media_list_items (item_id);
CREATE INDEX active_viewing_sessions_item_idx ON active_viewing_sessions (item_id);

-- Plugin identifiers are intentionally case-insensitive at the repository boundary.
CREATE INDEX installed_plugins_id_ci_idx ON installed_plugins (lower(plugin_id));
CREATE INDEX plugin_manifests_id_ci_idx ON plugin_manifests (lower(plugin_id));
CREATE INDEX plugin_configurations_id_ci_idx ON plugin_configurations (lower(plugin_id));
CREATE INDEX plugin_permissions_id_ci_idx ON plugin_permissions (lower(plugin_id));
CREATE INDEX plugin_runtime_instances_plugin_runtime_idx
    ON plugin_runtime_instances (lower(plugin_id), lower(runtime), updated_at DESC);
CREATE INDEX plugin_host_events_plugin_created_idx
    ON plugin_host_events (lower(plugin_id), created_at DESC, id DESC);
CREATE INDEX plugin_audit_log_plugin_created_idx
    ON plugin_audit_log (lower(plugin_id), created_at DESC, id DESC);
CREATE INDEX package_installations_guid_ci_idx
    ON package_installations (lower(package_guid)) WHERE package_guid IS NOT NULL;

-- Normalize any pre-constraint rows written by an earlier development build before enforcing
-- the public configuration contract at the database boundary.
UPDATE system_configuration_payloads
SET content_types = '[]'::jsonb
WHERE jsonb_typeof(content_types) <> 'array';
UPDATE system_configuration_payloads
SET metadata_options = '[]'::jsonb
WHERE jsonb_typeof(metadata_options) <> 'array';
UPDATE system_configuration_payloads
SET path_substitutions = '[]'::jsonb
WHERE jsonb_typeof(path_substitutions) <> 'array';
UPDATE system_configuration_payloads
SET plugin_repositories = '[]'::jsonb
WHERE jsonb_typeof(plugin_repositories) <> 'array';
UPDATE system_configuration_payloads
SET server_options = '{}'::jsonb
WHERE jsonb_typeof(server_options) <> 'object';

ALTER TABLE system_configuration_payloads
    ADD CONSTRAINT system_configuration_content_types_array
        CHECK (jsonb_typeof(content_types) = 'array'),
    ADD CONSTRAINT system_configuration_metadata_options_array
        CHECK (jsonb_typeof(metadata_options) = 'array'),
    ADD CONSTRAINT system_configuration_path_substitutions_array
        CHECK (jsonb_typeof(path_substitutions) = 'array'),
    ADD CONSTRAINT system_configuration_plugin_repositories_array
        CHECK (jsonb_typeof(plugin_repositories) = 'array'),
    ADD CONSTRAINT system_configuration_server_options_object
        CHECK (jsonb_typeof(server_options) = 'object');
