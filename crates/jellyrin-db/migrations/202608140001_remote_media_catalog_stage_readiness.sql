ALTER TABLE remote_media_catalog_stages ADD COLUMN ready_at TEXT;
ALTER TABLE remote_media_catalog_stages ADD COLUMN source_revision TEXT NOT NULL DEFAULT '';
