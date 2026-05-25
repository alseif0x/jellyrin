CREATE TABLE IF NOT EXISTS transcode_sessions (
    play_session_id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    media_source_id TEXT,
    audio_stream_index INTEGER,
    subtitle_stream_index INTEGER,
    video_stream_index INTEGER,
    output_path TEXT NOT NULL,
    process_id INTEGER,
    status TEXT NOT NULL,
    progress_percent REAL,
    position_ticks INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_transcode_sessions_status_updated
ON transcode_sessions(status, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_transcode_sessions_user_item
ON transcode_sessions(user_id, item_id);
