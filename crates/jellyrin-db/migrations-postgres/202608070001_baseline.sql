CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE server_state (
    id smallint PRIMARY KEY CHECK (id = 1),
    server_id uuid NOT NULL,
    server_name text NOT NULL,
    startup_wizard_completed boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE startup_config (
    id smallint PRIMARY KEY CHECK (id = 1),
    ui_culture text NOT NULL DEFAULT 'en-US',
    metadata_country_code text NOT NULL DEFAULT 'US',
    preferred_metadata_language text NOT NULL DEFAULT 'en',
    enable_remote_access boolean NOT NULL DEFAULT false,
    dummy_chapter_duration bigint NOT NULL DEFAULT 0,
    chapter_image_resolution text NOT NULL DEFAULT 'MatchSource',
    updated_at timestamptz NOT NULL
);

CREATE TABLE users (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    is_administrator boolean NOT NULL DEFAULT false,
    is_disabled boolean NOT NULL DEFAULT false,
    sync_play_access text NOT NULL DEFAULT 'CreateAndJoinGroups',
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX users_name_ci_unique ON users (lower(name));

CREATE TABLE user_passwords (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    algorithm text NOT NULL,
    password_hash text NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE devices (
    access_token text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id text NOT NULL,
    device_name text NOT NULL,
    client text NOT NULL,
    version text NOT NULL,
    capabilities jsonb,
    created_at timestamptz NOT NULL,
    last_activity_at timestamptz NOT NULL,
    UNIQUE (user_id, device_id)
);

CREATE TABLE virtual_folders (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    collection_type text,
    locations jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(locations) = 'array'),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX virtual_folders_name_ci_unique ON virtual_folders (lower(name));

CREATE TABLE api_keys (
    access_token text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name text NOT NULL,
    created_at timestamptz NOT NULL,
    last_activity_at timestamptz NOT NULL
);

CREATE TABLE media_items (
    id uuid PRIMARY KEY,
    virtual_folder_id uuid NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    name text NOT NULL,
    path text NOT NULL UNIQUE,
    media_type text NOT NULL,
    collection_type text,
    last_seen_at timestamptz,
    missing_since timestamptz,
    file_size bigint,
    modified_at timestamptz,
    runtime_ticks bigint,
    bitrate bigint,
    width integer,
    height integer,
    media_streams jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(media_streams) = 'array'),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX media_items_virtual_folder_idx ON media_items (virtual_folder_id);
CREATE INDEX media_items_type_idx ON media_items (media_type);
CREATE INDEX media_items_visible_idx ON media_items (virtual_folder_id, missing_since);
CREATE INDEX media_items_missing_identity_idx
    ON media_items (virtual_folder_id, media_type, file_size, modified_at, missing_since);
CREATE INDEX media_items_latest_by_folder_idx
    ON media_items (virtual_folder_id, missing_since, updated_at DESC, lower(name));
CREATE INDEX media_items_name_trgm_idx ON media_items USING gin (lower(name) gin_trgm_ops);
CREATE INDEX media_items_metadata_gin_idx ON media_items USING gin (metadata jsonb_path_ops);

CREATE TABLE playback_states (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id text,
    audio_stream_index bigint,
    subtitle_stream_index bigint,
    position_ticks bigint NOT NULL DEFAULT 0,
    is_paused boolean NOT NULL DEFAULT false,
    played boolean NOT NULL DEFAULT false,
    is_favorite boolean NOT NULL DEFAULT false,
    rating double precision,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (user_id, item_id)
);
CREATE INDEX playback_states_user_resume_idx
    ON playback_states (user_id, position_ticks DESC, updated_at DESC);

CREATE TABLE task_runs (
    id uuid PRIMARY KEY,
    task_key text NOT NULL,
    status text NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    started_at timestamptz NOT NULL,
    completed_at timestamptz,
    result jsonb,
    error_message text,
    updated_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX task_runs_one_running_idx ON task_runs (task_key) WHERE status = 'running';
CREATE INDEX task_runs_task_latest_idx ON task_runs (task_key, completed_at DESC);

CREATE TABLE active_playback_sessions (
    session_id text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id text,
    audio_stream_index bigint,
    subtitle_stream_index bigint,
    position_ticks bigint NOT NULL DEFAULT 0,
    is_paused boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL
);
CREATE INDEX active_playback_sessions_user_idx ON active_playback_sessions (user_id);

CREATE TABLE branding_config (
    id smallint PRIMARY KEY CHECK (id = 1),
    login_disclaimer text,
    custom_css text,
    splashscreen_enabled boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL
);

CREATE TABLE display_preferences (
    id text NOT NULL,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (id, user_id, client)
);

CREATE TABLE user_configurations (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE system_configuration_payloads (
    id smallint PRIMARY KEY CHECK (id = 1),
    content_types jsonb NOT NULL DEFAULT '[]'::jsonb,
    metadata_options jsonb NOT NULL DEFAULT '[]'::jsonb,
    path_substitutions jsonb NOT NULL DEFAULT '[]'::jsonb,
    plugin_repositories jsonb NOT NULL DEFAULT '[]'::jsonb,
    server_options jsonb NOT NULL DEFAULT '{}'::jsonb,
    updated_at timestamptz NOT NULL
);

CREATE TABLE activity_log_entries (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    overview text,
    short_overview text,
    entry_type text NOT NULL,
    severity text NOT NULL DEFAULT 'Information',
    user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    item_id uuid REFERENCES media_items(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL
);
CREATE INDEX activity_log_entries_created_idx ON activity_log_entries (created_at DESC, id DESC);
CREATE INDEX activity_log_entries_item_idx ON activity_log_entries (item_id);

CREATE TABLE backup_manifests (
    path text PRIMARY KEY,
    server_version text NOT NULL,
    backup_engine_version text NOT NULL,
    options jsonb NOT NULL,
    restore_snapshot jsonb,
    created_at timestamptz NOT NULL
);

CREATE TABLE named_configurations (
    key text PRIMARY KEY,
    payload jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE transcode_sessions (
    play_session_id text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id text,
    audio_stream_index bigint,
    subtitle_stream_index bigint,
    video_stream_index bigint,
    output_path text NOT NULL,
    process_id bigint,
    status text NOT NULL,
    progress_percent double precision,
    position_ticks bigint NOT NULL DEFAULT 0,
    start_position_ticks bigint NOT NULL DEFAULT 0,
    dedupe_key text,
    device_id text,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX transcode_sessions_status_updated_idx ON transcode_sessions (status, updated_at DESC);
CREATE INDEX transcode_sessions_user_item_idx ON transcode_sessions (user_id, item_id);
CREATE UNIQUE INDEX transcode_sessions_active_dedupe_key_idx
    ON transcode_sessions (dedupe_key)
    WHERE dedupe_key IS NOT NULL AND status IN ('starting', 'running');

CREATE TABLE media_item_versions (
    primary_item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    alternate_item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (primary_item_id, alternate_item_id),
    CHECK (primary_item_id <> alternate_item_id),
    UNIQUE (alternate_item_id)
);

CREATE TABLE trickplay_infos (
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    width integer NOT NULL,
    height integer NOT NULL,
    tile_width integer NOT NULL,
    tile_height integer NOT NULL,
    thumbnail_count integer NOT NULL,
    interval_ms bigint NOT NULL,
    bandwidth bigint NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (item_id, width)
);

CREATE TABLE quick_connect_sessions (
    secret text PRIMARY KEY,
    code text NOT NULL UNIQUE,
    device_id text NOT NULL,
    device_name text NOT NULL,
    client text NOT NULL,
    version text NOT NULL,
    user_id uuid REFERENCES users(id) ON DELETE CASCADE,
    authorized boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE INDEX quick_connect_sessions_expires_idx ON quick_connect_sessions (expires_at);

CREATE TABLE media_lists (
    id uuid PRIMARY KEY,
    kind text NOT NULL,
    name text NOT NULL,
    collection_type text,
    owner_user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX media_lists_kind_idx ON media_lists (kind);

CREATE TABLE media_list_items (
    list_id uuid NOT NULL REFERENCES media_lists(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    playlist_item_id uuid NOT NULL UNIQUE,
    position bigint NOT NULL,
    added_at timestamptz NOT NULL,
    PRIMARY KEY (list_id, item_id)
);
CREATE INDEX media_list_items_list_position_idx ON media_list_items (list_id, position);

CREATE TABLE media_item_lyrics (
    item_id uuid PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    lyrics jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE media_list_user_permissions (
    list_id uuid NOT NULL REFERENCES media_lists(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    can_edit boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (list_id, user_id)
);
CREATE INDEX media_list_user_permissions_user_idx ON media_list_user_permissions (user_id);

CREATE TABLE media_item_deletions (
    path text PRIMARY KEY,
    item_id uuid NOT NULL,
    deleted_by_user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    deleted_at timestamptz NOT NULL
);
CREATE INDEX media_item_deletions_item_idx ON media_item_deletions (item_id);

CREATE TABLE active_viewing_sessions (
    session_id text PRIMARY KEY REFERENCES devices(access_token) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    updated_at timestamptz NOT NULL
);
CREATE INDEX active_viewing_sessions_user_idx ON active_viewing_sessions (user_id);

CREATE TABLE active_session_users (
    session_id text NOT NULL REFERENCES devices(access_token) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    added_at timestamptz NOT NULL,
    PRIMARY KEY (session_id, user_id)
);
CREATE INDEX active_session_users_user_idx ON active_session_users (user_id);

CREATE TABLE plugin_repositories (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    url text NOT NULL UNIQUE,
    enabled boolean NOT NULL DEFAULT true,
    payload jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE package_catalog_cache (
    id uuid PRIMARY KEY,
    repository_url text NOT NULL,
    package_guid text,
    package_name text NOT NULL,
    package_version text NOT NULL,
    runtime text NOT NULL DEFAULT 'Unknown',
    target_abi text NOT NULL DEFAULT '',
    payload jsonb NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (repository_url, package_name, package_version)
);

CREATE TABLE package_installations (
    id uuid PRIMARY KEY,
    package_name text NOT NULL,
    package_guid text,
    version text NOT NULL,
    runtime text NOT NULL,
    status text NOT NULL,
    source_url text,
    payload jsonb NOT NULL,
    installed_at timestamptz,
    updated_at timestamptz NOT NULL
);

CREATE TABLE installed_plugins (
    plugin_id text PRIMARY KEY,
    name text NOT NULL,
    version text NOT NULL,
    runtime text NOT NULL,
    runtime_version text NOT NULL DEFAULT '',
    target_abi text NOT NULL DEFAULT '',
    server_compatibility jsonb NOT NULL DEFAULT '{}'::jsonb,
    status text NOT NULL,
    capabilities jsonb NOT NULL DEFAULT '[]'::jsonb,
    permissions jsonb NOT NULL DEFAULT '[]'::jsonb,
    configuration_state text NOT NULL DEFAULT 'Default',
    last_error text,
    health jsonb NOT NULL DEFAULT '{}'::jsonb,
    manifest jsonb NOT NULL DEFAULT '{}'::jsonb,
    installed_at timestamptz,
    updated_at timestamptz NOT NULL
);

CREATE TABLE plugin_manifests (
    plugin_id text PRIMARY KEY,
    manifest jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE TABLE plugin_configurations (
    plugin_id text PRIMARY KEY,
    configuration jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE TABLE plugin_permissions (
    plugin_id text PRIMARY KEY,
    permissions jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE TABLE plugin_runtime_instances (
    instance_id uuid PRIMARY KEY,
    plugin_id text,
    runtime text NOT NULL,
    runtime_version text NOT NULL DEFAULT '',
    status text NOT NULL,
    process_id bigint,
    endpoint text,
    health jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_error text,
    started_at timestamptz,
    updated_at timestamptz NOT NULL
);
CREATE TABLE plugin_host_events (
    id uuid PRIMARY KEY,
    plugin_id text,
    runtime text,
    event_type text NOT NULL,
    severity text NOT NULL,
    message text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL
);
CREATE TABLE plugin_audit_log (
    id uuid PRIMARY KEY,
    plugin_id text,
    action text NOT NULL,
    actor_user_id uuid,
    status text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL
);

CREATE TABLE live_tv_tuners (
    tuner_id text PRIMARY KEY,
    provider_type text NOT NULL,
    name text NOT NULL,
    source_url text,
    enabled boolean NOT NULL DEFAULT true,
    configuration jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_sync_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE TABLE live_tv_categories (
    category_id text PRIMARY KEY,
    tuner_id text NOT NULL REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    remote_id text NOT NULL,
    name text NOT NULL,
    sort_name text NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (tuner_id, remote_id)
);
CREATE INDEX live_tv_categories_tuner_sort_idx ON live_tv_categories (tuner_id, lower(sort_name));
CREATE TABLE live_tv_channels (
    channel_id text PRIMARY KEY,
    tuner_id text NOT NULL REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    remote_id text NOT NULL,
    category_id text REFERENCES live_tv_categories(category_id) ON DELETE SET NULL,
    name text NOT NULL,
    sort_name text NOT NULL,
    number text,
    stream_url text NOT NULL,
    logo_url text,
    enabled boolean NOT NULL DEFAULT true,
    channel_type text NOT NULL DEFAULT 'TV',
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (tuner_id, remote_id)
);
CREATE INDEX live_tv_channels_tuner_sort_idx ON live_tv_channels (tuner_id, lower(sort_name));
CREATE INDEX live_tv_channels_category_sort_idx ON live_tv_channels (category_id, lower(sort_name));
CREATE INDEX live_tv_channels_name_idx ON live_tv_channels (lower(name));
CREATE INDEX live_tv_channels_name_trgm_idx ON live_tv_channels USING gin (lower(name) gin_trgm_ops);
CREATE TABLE live_tv_programs (
    program_id text PRIMARY KEY,
    channel_id text NOT NULL REFERENCES live_tv_channels(channel_id) ON DELETE CASCADE,
    remote_id text,
    title text NOT NULL,
    sort_title text NOT NULL,
    overview text,
    start_date timestamptz NOT NULL,
    end_date timestamptz NOT NULL,
    is_live boolean NOT NULL DEFAULT false,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX live_tv_programs_channel_start_idx
    ON live_tv_programs (channel_id, start_date, end_date);
CREATE INDEX live_tv_programs_airing_idx ON live_tv_programs (start_date, end_date);
