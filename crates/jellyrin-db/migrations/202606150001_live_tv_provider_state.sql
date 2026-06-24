CREATE TABLE IF NOT EXISTS live_tv_tuners (
    tuner_id TEXT PRIMARY KEY,
    provider_type TEXT NOT NULL,
    name TEXT NOT NULL,
    source_url TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    configuration_json TEXT NOT NULL DEFAULT '{}',
    last_sync_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS live_tv_categories (
    category_id TEXT PRIMARY KEY,
    tuner_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    name TEXT NOT NULL,
    sort_name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(tuner_id) REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    UNIQUE(tuner_id, remote_id)
);

CREATE INDEX IF NOT EXISTS idx_live_tv_categories_tuner_sort
    ON live_tv_categories(tuner_id, sort_name COLLATE NOCASE);

CREATE TABLE IF NOT EXISTS live_tv_channels (
    channel_id TEXT PRIMARY KEY,
    tuner_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    category_id TEXT,
    name TEXT NOT NULL,
    sort_name TEXT NOT NULL,
    number TEXT,
    stream_url TEXT NOT NULL,
    logo_url TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    channel_type TEXT NOT NULL DEFAULT 'TV',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(tuner_id) REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    FOREIGN KEY(category_id) REFERENCES live_tv_categories(category_id) ON DELETE SET NULL,
    UNIQUE(tuner_id, remote_id)
);

CREATE INDEX IF NOT EXISTS idx_live_tv_channels_tuner_sort
    ON live_tv_channels(tuner_id, sort_name COLLATE NOCASE);

CREATE INDEX IF NOT EXISTS idx_live_tv_channels_category_sort
    ON live_tv_channels(category_id, sort_name COLLATE NOCASE);

CREATE INDEX IF NOT EXISTS idx_live_tv_channels_name
    ON live_tv_channels(name COLLATE NOCASE);

CREATE TABLE IF NOT EXISTS live_tv_programs (
    program_id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    remote_id TEXT,
    title TEXT NOT NULL,
    sort_title TEXT NOT NULL,
    overview TEXT,
    start_date TEXT NOT NULL,
    end_date TEXT NOT NULL,
    is_live INTEGER NOT NULL DEFAULT 0,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(channel_id) REFERENCES live_tv_channels(channel_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_live_tv_programs_channel_start
    ON live_tv_programs(channel_id, start_date, end_date);

CREATE INDEX IF NOT EXISTS idx_live_tv_programs_airing
    ON live_tv_programs(start_date, end_date);
