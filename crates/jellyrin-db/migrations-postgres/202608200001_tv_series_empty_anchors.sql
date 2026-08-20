-- Provider catalogues can contain a valid series with no published episodes.
ALTER TABLE media_item_tv_series
    DROP CONSTRAINT media_item_tv_series_episode_count_check;

ALTER TABLE media_item_tv_series
    ADD CONSTRAINT media_item_tv_series_episode_count_check CHECK (episode_count >= 0);

CREATE INDEX idx_media_items_tv_series_anchor_id
ON media_items ((btrim(metadata->>'SeriesId')))
WHERE missing_since IS NULL
  AND media_type = 'Series'
  AND lower(collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[])
  AND lower(coalesce(metadata->>'PluginVodKind', '')) = 'series';

-- Version 3 adds explicit zero-member series anchors. Older coverage is not complete under the new
-- source definition and must be rebuilt before it can be served as durable.
DELETE FROM media_item_tv_series_coverage;
