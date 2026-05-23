CREATE TABLE IF NOT EXISTS playback_states (
    user_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    media_source_id TEXT,
    position_ticks INTEGER NOT NULL DEFAULT 0,
    is_paused INTEGER NOT NULL DEFAULT 0,
    played INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, item_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (item_id) REFERENCES media_items(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_playback_states_user_resume
ON playback_states(user_id, position_ticks DESC, updated_at DESC);
