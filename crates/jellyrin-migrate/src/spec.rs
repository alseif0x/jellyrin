#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonShape {
    Any,
    Array,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Bytes,
    Uuid,
    Timestamp,
    Bool,
    I16,
    I32,
    I64,
    F64,
    Json(JsonShape),
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnSpec {
    pub source: &'static str,
    pub target: &'static str,
    pub kind: ColumnKind,
    pub nullable: bool,
}

impl ColumnSpec {
    const fn required(source: &'static str, target: &'static str, kind: ColumnKind) -> Self {
        Self {
            source,
            target,
            kind,
            nullable: false,
        }
    }

    const fn optional(source: &'static str, target: &'static str, kind: ColumnKind) -> Self {
        Self {
            source,
            target,
            kind,
            nullable: true,
        }
    }
}

#[derive(Debug)]
pub struct TableSpec {
    pub source: &'static str,
    pub target: &'static str,
    pub columns: &'static [ColumnSpec],
    pub order_by: &'static str,
}

const SERVER_STATE: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::I16),
    ColumnSpec::required("server_id", "server_id", ColumnKind::Uuid),
    ColumnSpec::required("server_name", "server_name", ColumnKind::Text),
    ColumnSpec::required(
        "startup_wizard_completed",
        "startup_wizard_completed",
        ColumnKind::Bool,
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const STARTUP_CONFIG: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::I16),
    ColumnSpec::required("ui_culture", "ui_culture", ColumnKind::Text),
    ColumnSpec::required(
        "metadata_country_code",
        "metadata_country_code",
        ColumnKind::Text,
    ),
    ColumnSpec::required(
        "preferred_metadata_language",
        "preferred_metadata_language",
        ColumnKind::Text,
    ),
    ColumnSpec::required(
        "enable_remote_access",
        "enable_remote_access",
        ColumnKind::Bool,
    ),
    ColumnSpec::required(
        "dummy_chapter_duration",
        "dummy_chapter_duration",
        ColumnKind::I64,
    ),
    ColumnSpec::required(
        "chapter_image_resolution",
        "chapter_image_resolution",
        ColumnKind::Text,
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const USERS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::required("is_administrator", "is_administrator", ColumnKind::Bool),
    ColumnSpec::required("is_disabled", "is_disabled", ColumnKind::Bool),
    ColumnSpec::required("sync_play_access", "sync_play_access", ColumnKind::Text),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const USER_PASSWORDS: &[ColumnSpec] = &[
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("algorithm", "algorithm", ColumnKind::Text),
    ColumnSpec::required("password_hash", "password_hash", ColumnKind::Text),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const DEVICES: &[ColumnSpec] = &[
    ColumnSpec::required("access_token", "access_token", ColumnKind::Text),
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("device_id", "device_id", ColumnKind::Text),
    ColumnSpec::required("device_name", "device_name", ColumnKind::Text),
    ColumnSpec::required("client", "client", ColumnKind::Text),
    ColumnSpec::required("version", "version", ColumnKind::Text),
    ColumnSpec::optional(
        "capabilities_json",
        "capabilities",
        ColumnKind::Json(JsonShape::Any),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required(
        "last_activity_at",
        "last_activity_at",
        ColumnKind::Timestamp,
    ),
];

const API_KEYS: &[ColumnSpec] = &[
    ColumnSpec::required("access_token", "access_token", ColumnKind::Text),
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required(
        "last_activity_at",
        "last_activity_at",
        ColumnKind::Timestamp,
    ),
];

const VIRTUAL_FOLDERS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::optional("collection_type", "collection_type", ColumnKind::Text),
    ColumnSpec::required(
        "locations_json",
        "locations",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const MEDIA_ITEMS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("virtual_folder_id", "virtual_folder_id", ColumnKind::Uuid),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::required("path", "path", ColumnKind::Text),
    ColumnSpec::required("media_type", "media_type", ColumnKind::Text),
    ColumnSpec::optional("collection_type", "collection_type", ColumnKind::Text),
    ColumnSpec::optional("last_seen_at", "last_seen_at", ColumnKind::Timestamp),
    ColumnSpec::optional("missing_since", "missing_since", ColumnKind::Timestamp),
    ColumnSpec::optional("file_size", "file_size", ColumnKind::I64),
    ColumnSpec::optional("modified_at", "modified_at", ColumnKind::Timestamp),
    ColumnSpec::optional("runtime_ticks", "runtime_ticks", ColumnKind::I64),
    ColumnSpec::optional("bitrate", "bitrate", ColumnKind::I64),
    ColumnSpec::optional("width", "width", ColumnKind::I32),
    ColumnSpec::optional("height", "height", ColumnKind::I32),
    ColumnSpec::required(
        "media_streams_json",
        "media_streams",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "metadata_json",
        "metadata",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const BRANDING_CONFIG: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::I16),
    ColumnSpec::optional("login_disclaimer", "login_disclaimer", ColumnKind::Text),
    ColumnSpec::optional("custom_css", "custom_css", ColumnKind::Text),
    ColumnSpec::required(
        "splashscreen_enabled",
        "splashscreen_enabled",
        ColumnKind::Bool,
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const DISPLAY_PREFERENCES: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Text),
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("client", "client", ColumnKind::Text),
    ColumnSpec::required("payload_json", "payload", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const USER_CONFIGURATIONS: &[ColumnSpec] = &[
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("payload_json", "payload", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const SYSTEM_CONFIGURATION_PAYLOADS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::I16),
    ColumnSpec::required(
        "content_types_json",
        "content_types",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "metadata_options_json",
        "metadata_options",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "path_substitutions_json",
        "path_substitutions",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "plugin_repositories_json",
        "plugin_repositories",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "server_options_json",
        "server_options",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const NAMED_CONFIGURATIONS: &[ColumnSpec] = &[
    ColumnSpec::required("key", "key", ColumnKind::Text),
    ColumnSpec::required("payload_json", "payload", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PLAYBACK_STATES: &[ColumnSpec] = &[
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("item_id", "item_id", ColumnKind::Uuid),
    ColumnSpec::optional("media_source_id", "media_source_id", ColumnKind::Text),
    ColumnSpec::optional("audio_stream_index", "audio_stream_index", ColumnKind::I64),
    ColumnSpec::optional(
        "subtitle_stream_index",
        "subtitle_stream_index",
        ColumnKind::I64,
    ),
    ColumnSpec::required("position_ticks", "position_ticks", ColumnKind::I64),
    ColumnSpec::required("is_paused", "is_paused", ColumnKind::Bool),
    ColumnSpec::required("played", "played", ColumnKind::Bool),
    ColumnSpec::required("is_favorite", "is_favorite", ColumnKind::Bool),
    ColumnSpec::optional("rating", "rating", ColumnKind::F64),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const MEDIA_LISTS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("kind", "kind", ColumnKind::Text),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::optional("collection_type", "collection_type", ColumnKind::Text),
    ColumnSpec::optional("owner_user_id", "owner_user_id", ColumnKind::Uuid),
    ColumnSpec::required(
        "metadata_json",
        "metadata",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const MEDIA_LIST_ITEMS: &[ColumnSpec] = &[
    ColumnSpec::required("list_id", "list_id", ColumnKind::Uuid),
    ColumnSpec::required("item_id", "item_id", ColumnKind::Uuid),
    ColumnSpec::required("playlist_item_id", "playlist_item_id", ColumnKind::Uuid),
    ColumnSpec::required("position", "position", ColumnKind::I64),
    ColumnSpec::required("added_at", "added_at", ColumnKind::Timestamp),
];

const MEDIA_LIST_USER_PERMISSIONS: &[ColumnSpec] = &[
    ColumnSpec::required("list_id", "list_id", ColumnKind::Uuid),
    ColumnSpec::required("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::required("can_edit", "can_edit", ColumnKind::Bool),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const MEDIA_ITEM_LYRICS: &[ColumnSpec] = &[
    ColumnSpec::required("item_id", "item_id", ColumnKind::Uuid),
    ColumnSpec::required("lyrics_json", "lyrics", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const ACTIVITY_LOG_ENTRIES: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::I64),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::optional("overview", "overview", ColumnKind::Text),
    ColumnSpec::optional("short_overview", "short_overview", ColumnKind::Text),
    ColumnSpec::required("entry_type", "entry_type", ColumnKind::Text),
    ColumnSpec::required("severity", "severity", ColumnKind::Text),
    ColumnSpec::optional("user_id", "user_id", ColumnKind::Uuid),
    ColumnSpec::optional("item_id", "item_id", ColumnKind::Uuid),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
];

const BACKUP_MANIFESTS: &[ColumnSpec] = &[
    ColumnSpec::required("path", "path", ColumnKind::Text),
    ColumnSpec::required("server_version", "server_version", ColumnKind::Text),
    ColumnSpec::required(
        "backup_engine_version",
        "backup_engine_version",
        ColumnKind::Text,
    ),
    ColumnSpec::required("options_json", "options", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::optional(
        "restore_snapshot_json",
        "restore_snapshot",
        ColumnKind::Json(JsonShape::Any),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
];

const MEDIA_ITEM_DELETIONS: &[ColumnSpec] = &[
    ColumnSpec::required("path", "path", ColumnKind::Text),
    ColumnSpec::required("item_id", "item_id", ColumnKind::Uuid),
    ColumnSpec::optional("deleted_by_user_id", "deleted_by_user_id", ColumnKind::Uuid),
    ColumnSpec::required("deleted_at", "deleted_at", ColumnKind::Timestamp),
];

const PLUGIN_REPOSITORIES: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::required("url", "url", ColumnKind::Text),
    ColumnSpec::required("enabled", "enabled", ColumnKind::Bool),
    ColumnSpec::required("payload_json", "payload", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PACKAGE_INSTALLATIONS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::required("package_name", "package_name", ColumnKind::Text),
    ColumnSpec::optional("package_guid", "package_guid", ColumnKind::Text),
    ColumnSpec::required("version", "version", ColumnKind::Text),
    ColumnSpec::required("runtime", "runtime", ColumnKind::Text),
    ColumnSpec::required("status", "status", ColumnKind::Text),
    ColumnSpec::optional("source_url", "source_url", ColumnKind::Text),
    ColumnSpec::required("payload_json", "payload", ColumnKind::Json(JsonShape::Any)),
    ColumnSpec::optional("installed_at", "installed_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const INSTALLED_PLUGINS: &[ColumnSpec] = &[
    ColumnSpec::required("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::required("version", "version", ColumnKind::Text),
    ColumnSpec::required("runtime", "runtime", ColumnKind::Text),
    ColumnSpec::required("runtime_version", "runtime_version", ColumnKind::Text),
    ColumnSpec::required("target_abi", "target_abi", ColumnKind::Text),
    ColumnSpec::required(
        "server_compatibility_json",
        "server_compatibility",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("status", "status", ColumnKind::Text),
    ColumnSpec::required(
        "capabilities_json",
        "capabilities",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "permissions_json",
        "permissions",
        ColumnKind::Json(JsonShape::Array),
    ),
    ColumnSpec::required(
        "configuration_state",
        "configuration_state",
        ColumnKind::Text,
    ),
    ColumnSpec::optional("last_error", "last_error", ColumnKind::Text),
    ColumnSpec::required("health_json", "health", ColumnKind::Json(JsonShape::Object)),
    ColumnSpec::required(
        "manifest_json",
        "manifest",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::optional("installed_at", "installed_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PLUGIN_MANIFESTS: &[ColumnSpec] = &[
    ColumnSpec::required("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::required(
        "manifest_json",
        "manifest",
        ColumnKind::Json(JsonShape::Any),
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PLUGIN_CONFIGURATIONS: &[ColumnSpec] = &[
    ColumnSpec::required("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::required(
        "configuration_json",
        "configuration",
        ColumnKind::Json(JsonShape::Any),
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PLUGIN_PERMISSIONS: &[ColumnSpec] = &[
    ColumnSpec::required("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::required(
        "permissions_json",
        "permissions",
        ColumnKind::Json(JsonShape::Any),
    ),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PLUGIN_HOST_EVENTS: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::optional("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::optional("runtime", "runtime", ColumnKind::Text),
    ColumnSpec::required("event_type", "event_type", ColumnKind::Text),
    ColumnSpec::required("severity", "severity", ColumnKind::Text),
    ColumnSpec::required("message", "message", ColumnKind::Text),
    ColumnSpec::required(
        "payload_json",
        "payload",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
];

const PLUGIN_AUDIT_LOG: &[ColumnSpec] = &[
    ColumnSpec::required("id", "id", ColumnKind::Uuid),
    ColumnSpec::optional("plugin_id", "plugin_id", ColumnKind::Text),
    ColumnSpec::required("action", "action", ColumnKind::Text),
    ColumnSpec::optional("actor_user_id", "actor_user_id", ColumnKind::Uuid),
    ColumnSpec::required("status", "status", ColumnKind::Text),
    ColumnSpec::required(
        "payload_json",
        "payload",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
];

const LIVE_TV_TUNERS: &[ColumnSpec] = &[
    ColumnSpec::required("tuner_id", "tuner_id", ColumnKind::Text),
    ColumnSpec::required("provider_type", "provider_type", ColumnKind::Text),
    ColumnSpec::required("name", "name", ColumnKind::Text),
    ColumnSpec::optional("source_url", "source_url", ColumnKind::Text),
    ColumnSpec::required("enabled", "enabled", ColumnKind::Bool),
    ColumnSpec::required(
        "configuration_json",
        "configuration",
        ColumnKind::Json(JsonShape::Object),
    ),
    ColumnSpec::optional("last_sync_at", "last_sync_at", ColumnKind::Timestamp),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

const PROVIDER_SECRETS: &[ColumnSpec] = &[
    ColumnSpec::required("secret_id", "secret_id", ColumnKind::Text),
    ColumnSpec::required("provider_type", "provider_type", ColumnKind::Text),
    ColumnSpec::required("envelope_version", "envelope_version", ColumnKind::I16),
    ColumnSpec::required("key_id", "key_id", ColumnKind::Text),
    ColumnSpec::required("nonce", "nonce", ColumnKind::Bytes),
    ColumnSpec::required("ciphertext", "ciphertext", ColumnKind::Bytes),
    ColumnSpec::required("revision", "revision", ColumnKind::I64),
    ColumnSpec::required("created_at", "created_at", ColumnKind::Timestamp),
    ColumnSpec::required("updated_at", "updated_at", ColumnKind::Timestamp),
];

pub static MIGRATED_TABLES: &[TableSpec] = &[
    TableSpec {
        source: "server_state",
        target: "server_state",
        columns: SERVER_STATE,
        order_by: "id",
    },
    TableSpec {
        source: "startup_config",
        target: "startup_config",
        columns: STARTUP_CONFIG,
        order_by: "id",
    },
    TableSpec {
        source: "users",
        target: "users",
        columns: USERS,
        order_by: "id",
    },
    TableSpec {
        source: "user_passwords",
        target: "user_passwords",
        columns: USER_PASSWORDS,
        order_by: "user_id",
    },
    TableSpec {
        source: "devices",
        target: "devices",
        columns: DEVICES,
        order_by: "access_token",
    },
    TableSpec {
        source: "api_keys",
        target: "api_keys",
        columns: API_KEYS,
        order_by: "access_token",
    },
    TableSpec {
        source: "virtual_folders",
        target: "virtual_folders",
        columns: VIRTUAL_FOLDERS,
        order_by: "id",
    },
    TableSpec {
        source: "media_items",
        target: "media_items",
        columns: MEDIA_ITEMS,
        order_by: "id",
    },
    TableSpec {
        source: "branding_config",
        target: "branding_config",
        columns: BRANDING_CONFIG,
        order_by: "id",
    },
    TableSpec {
        source: "display_preferences",
        target: "display_preferences",
        columns: DISPLAY_PREFERENCES,
        order_by: "id, user_id, client",
    },
    TableSpec {
        source: "user_configurations",
        target: "user_configurations",
        columns: USER_CONFIGURATIONS,
        order_by: "user_id",
    },
    TableSpec {
        source: "system_configuration_payloads",
        target: "system_configuration_payloads",
        columns: SYSTEM_CONFIGURATION_PAYLOADS,
        order_by: "id",
    },
    TableSpec {
        source: "named_configurations",
        target: "named_configurations",
        columns: NAMED_CONFIGURATIONS,
        order_by: "key",
    },
    TableSpec {
        source: "playback_states",
        target: "playback_states",
        columns: PLAYBACK_STATES,
        order_by: "user_id, item_id",
    },
    TableSpec {
        source: "media_lists",
        target: "media_lists",
        columns: MEDIA_LISTS,
        order_by: "id",
    },
    TableSpec {
        source: "media_list_items",
        target: "media_list_items",
        columns: MEDIA_LIST_ITEMS,
        order_by: "list_id, position, item_id",
    },
    TableSpec {
        source: "media_list_user_permissions",
        target: "media_list_user_permissions",
        columns: MEDIA_LIST_USER_PERMISSIONS,
        order_by: "list_id, user_id",
    },
    TableSpec {
        source: "media_item_lyrics",
        target: "media_item_lyrics",
        columns: MEDIA_ITEM_LYRICS,
        order_by: "item_id",
    },
    TableSpec {
        source: "activity_log_entries",
        target: "activity_log_entries",
        columns: ACTIVITY_LOG_ENTRIES,
        order_by: "id",
    },
    TableSpec {
        source: "backup_manifests",
        target: "backup_manifests",
        columns: BACKUP_MANIFESTS,
        order_by: "path",
    },
    TableSpec {
        source: "media_item_deletions",
        target: "media_item_deletions",
        columns: MEDIA_ITEM_DELETIONS,
        order_by: "path",
    },
    TableSpec {
        source: "plugin_repositories",
        target: "plugin_repositories",
        columns: PLUGIN_REPOSITORIES,
        order_by: "id",
    },
    TableSpec {
        source: "package_installations",
        target: "package_installations",
        columns: PACKAGE_INSTALLATIONS,
        order_by: "id",
    },
    TableSpec {
        source: "installed_plugins",
        target: "installed_plugins",
        columns: INSTALLED_PLUGINS,
        order_by: "plugin_id",
    },
    TableSpec {
        source: "plugin_manifests",
        target: "plugin_manifests",
        columns: PLUGIN_MANIFESTS,
        order_by: "plugin_id",
    },
    TableSpec {
        source: "plugin_configurations",
        target: "plugin_configurations",
        columns: PLUGIN_CONFIGURATIONS,
        order_by: "plugin_id",
    },
    TableSpec {
        source: "plugin_permissions",
        target: "plugin_permissions",
        columns: PLUGIN_PERMISSIONS,
        order_by: "plugin_id",
    },
    TableSpec {
        source: "plugin_host_events",
        target: "plugin_host_events",
        columns: PLUGIN_HOST_EVENTS,
        order_by: "id",
    },
    TableSpec {
        source: "plugin_audit_log",
        target: "plugin_audit_log",
        columns: PLUGIN_AUDIT_LOG,
        order_by: "id",
    },
    TableSpec {
        source: "provider_secrets",
        target: "provider_secrets",
        columns: PROVIDER_SECRETS,
        order_by: "secret_id",
    },
    TableSpec {
        source: "live_tv_tuners",
        target: "live_tv_tuners",
        columns: LIVE_TV_TUNERS,
        order_by: "tuner_id",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct OmittedTableSpec {
    pub table: &'static str,
    pub strategy: &'static str,
    pub reason: &'static str,
}

pub static OMITTED_TABLES: &[OmittedTableSpec] = &[
    OmittedTableSpec {
        table: "media_item_facets",
        strategy: "rebuild",
        reason: "derived searchable catalog facet projection",
    },
    OmittedTableSpec {
        table: "media_item_facet_aliases",
        strategy: "rebuild",
        reason: "derived stable and imported facet identifiers",
    },
    OmittedTableSpec {
        table: "media_item_versions",
        strategy: "rebuild",
        reason: "derived catalog relationship",
    },
    OmittedTableSpec {
        table: "trickplay_infos",
        strategy: "rebuild",
        reason: "derived media artifact",
    },
    OmittedTableSpec {
        table: "live_tv_categories",
        strategy: "rebuild",
        reason: "provider catalog",
    },
    OmittedTableSpec {
        table: "live_tv_channels",
        strategy: "rebuild",
        reason: "provider catalog",
    },
    OmittedTableSpec {
        table: "live_tv_programs",
        strategy: "rebuild",
        reason: "EPG data",
    },
    OmittedTableSpec {
        table: "active_playback_sessions",
        strategy: "omit_ephemeral",
        reason: "active session",
    },
    OmittedTableSpec {
        table: "active_viewing_sessions",
        strategy: "omit_ephemeral",
        reason: "active session",
    },
    OmittedTableSpec {
        table: "active_session_users",
        strategy: "omit_ephemeral",
        reason: "active session membership",
    },
    OmittedTableSpec {
        table: "transcode_sessions",
        strategy: "omit_ephemeral",
        reason: "active process and HLS output",
    },
    OmittedTableSpec {
        table: "quick_connect_sessions",
        strategy: "omit_ephemeral",
        reason: "short-lived authorization session",
    },
    OmittedTableSpec {
        table: "plugin_runtime_instances",
        strategy: "omit_ephemeral",
        reason: "active plugin process",
    },
    OmittedTableSpec {
        table: "task_runs",
        strategy: "omit_operational_history",
        reason: "scheduler execution state is not needed for cutover",
    },
    OmittedTableSpec {
        table: "catalog_sync_runs",
        strategy: "omit_operational_history",
        reason: "catalog synchronization execution state is not needed for cutover",
    },
    OmittedTableSpec {
        table: "package_catalog_cache",
        strategy: "rebuild",
        reason: "remote package cache",
    },
];

pub static TARGET_ONLY_OMITTED_TABLES: &[OmittedTableSpec] = &[];

/// Target-owned migration infrastructure. These tables are schema prerequisites, not imported
/// application data, so they are intentionally excluded from target emptiness checks.
pub static TARGET_INFRASTRUCTURE_TABLES: &[&str] = &["jellyrin_derived_projection_versions"];
