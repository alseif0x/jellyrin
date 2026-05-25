CREATE TABLE IF NOT EXISTS media_list_user_permissions (
    list_id TEXT NOT NULL REFERENCES media_lists(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    can_edit INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (list_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_media_list_user_permissions_user
    ON media_list_user_permissions(user_id);
