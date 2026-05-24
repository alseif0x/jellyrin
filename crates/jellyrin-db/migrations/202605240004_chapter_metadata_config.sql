ALTER TABLE startup_config ADD COLUMN dummy_chapter_duration INTEGER NOT NULL DEFAULT 0;
ALTER TABLE startup_config ADD COLUMN chapter_image_resolution TEXT NOT NULL DEFAULT 'MatchSource';
