ALTER TABLE playback_states
ADD COLUMN is_favorite INTEGER NOT NULL DEFAULT 0;

ALTER TABLE playback_states
ADD COLUMN rating REAL;
