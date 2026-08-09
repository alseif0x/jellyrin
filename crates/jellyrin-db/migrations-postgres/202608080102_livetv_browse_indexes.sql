-- Keep MAGSTV/Live TV browse bounded by indexes that match the public stable order. This is a
-- separate migration so the checksum of integrity migration 101 remains immutable.
LOCK TABLE live_tv_channels IN SHARE ROW EXCLUSIVE MODE;

DROP INDEX IF EXISTS live_tv_channels_category_sort_idx;

CREATE INDEX live_tv_channels_enabled_sort_idx
    ON live_tv_channels (lower(sort_name), lower(name), channel_id)
    WHERE enabled;

CREATE INDEX live_tv_channels_enabled_category_sort_idx
    ON live_tv_channels (category_id, lower(sort_name), lower(name), channel_id)
    WHERE enabled;
