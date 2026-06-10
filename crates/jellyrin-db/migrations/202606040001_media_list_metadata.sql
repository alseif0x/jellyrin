ALTER TABLE media_lists
ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';
