CREATE INDEX IF NOT EXISTS idx_media_items_tv_season_id
ON media_items (trim(json_extract(metadata_json, '$.SeasonId')) COLLATE NOCASE)
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) IN ('tvshows', 'tvshow', 'series');
