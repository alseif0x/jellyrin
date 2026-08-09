CREATE INDEX IF NOT EXISTS idx_media_items_tv_series_invalid
ON media_items (id)
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) IN ('tvshows', 'tvshow', 'series')
  AND (
    NULLIF(trim(json_extract(metadata_json, '$.SeriesId')), '') IS NULL
    OR length(trim(json_extract(metadata_json, '$.SeriesId'))) <> 32
    OR trim(json_extract(metadata_json, '$.SeriesId')) <> lower(trim(json_extract(metadata_json, '$.SeriesId')))
    OR trim(json_extract(metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
    OR NULLIF(trim(json_extract(metadata_json, '$.SeriesName')), '') IS NULL
  );
