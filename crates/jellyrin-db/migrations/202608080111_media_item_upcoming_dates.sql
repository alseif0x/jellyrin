CREATE TABLE media_item_upcoming_dates (
    item_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    unix_seconds INTEGER NOT NULL,
    nanosecond INTEGER NOT NULL CHECK (nanosecond >= 0 AND nanosecond < 1000000000)
);

CREATE INDEX media_item_upcoming_dates_range_idx
    ON media_item_upcoming_dates (unix_seconds, nanosecond, item_id);
