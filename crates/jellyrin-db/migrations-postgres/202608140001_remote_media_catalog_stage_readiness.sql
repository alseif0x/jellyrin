ALTER TABLE remote_media_catalog_stages ADD COLUMN ready_at timestamptz;
ALTER TABLE remote_media_catalog_stages ADD COLUMN source_revision text NOT NULL DEFAULT '';
