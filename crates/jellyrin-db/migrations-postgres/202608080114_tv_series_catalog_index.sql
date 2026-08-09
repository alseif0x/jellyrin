CREATE INDEX IF NOT EXISTS idx_media_items_tv_series_id
ON media_items ((btrim(metadata->>'SeriesId')))
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[]);
