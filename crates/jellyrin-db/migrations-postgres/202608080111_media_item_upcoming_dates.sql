CREATE TABLE media_item_upcoming_dates (
    item_id uuid PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    unix_seconds bigint NOT NULL,
    nanosecond integer NOT NULL CHECK (nanosecond >= 0 AND nanosecond < 1000000000)
);

CREATE INDEX media_item_upcoming_dates_range_idx
    ON media_item_upcoming_dates (unix_seconds, nanosecond, item_id);
