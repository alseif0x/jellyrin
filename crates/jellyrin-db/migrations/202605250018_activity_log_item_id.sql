ALTER TABLE activity_log_entries
ADD COLUMN item_id TEXT REFERENCES media_items(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_activity_log_entries_item
ON activity_log_entries(item_id);
