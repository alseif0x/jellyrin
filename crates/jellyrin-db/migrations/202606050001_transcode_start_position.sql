ALTER TABLE transcode_sessions
ADD COLUMN start_position_ticks INTEGER NOT NULL DEFAULT 0;
