CREATE INDEX IF NOT EXISTS idx_media_items_tv_series_invalid
ON media_items (id)
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[])
  AND (
    NULLIF(btrim(metadata->>'SeriesId'), '') IS NULL
    OR btrim(metadata->>'SeriesId') !~ '^[0-9a-f]{32}$'
    OR NULLIF(btrim(metadata->>'SeriesName'), '') IS NULL
  );
