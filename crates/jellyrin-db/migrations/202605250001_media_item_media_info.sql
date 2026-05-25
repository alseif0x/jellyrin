ALTER TABLE media_items ADD COLUMN runtime_ticks INTEGER;
ALTER TABLE media_items ADD COLUMN bitrate INTEGER;
ALTER TABLE media_items ADD COLUMN width INTEGER;
ALTER TABLE media_items ADD COLUMN height INTEGER;
ALTER TABLE media_items ADD COLUMN media_streams_json TEXT NOT NULL DEFAULT '[]';
