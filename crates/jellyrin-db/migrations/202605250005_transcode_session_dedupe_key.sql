ALTER TABLE transcode_sessions
ADD COLUMN dedupe_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_transcode_sessions_active_dedupe_key
ON transcode_sessions(dedupe_key)
WHERE dedupe_key IS NOT NULL AND status IN ('starting', 'running');
