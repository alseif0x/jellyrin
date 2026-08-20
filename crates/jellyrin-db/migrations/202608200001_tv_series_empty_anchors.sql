-- Provider catalogues can contain a valid series with no published episodes. Preserve those
-- zero-member series in the durable projection without weakening the member foreign key.
CREATE TABLE media_item_tv_series_new (
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    series_id TEXT NOT NULL,
    series_name TEXT NOT NULL,
    episode_count INTEGER NOT NULL CHECK (episode_count >= 0),
    PRIMARY KEY (virtual_folder_id, series_id)
);

CREATE TABLE media_item_tv_series_members_new (
    item_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    virtual_folder_id TEXT NOT NULL,
    series_id TEXT NOT NULL,
    FOREIGN KEY (virtual_folder_id, series_id)
        REFERENCES media_item_tv_series_new(virtual_folder_id, series_id)
        ON DELETE CASCADE
);

INSERT INTO media_item_tv_series_new (
    virtual_folder_id, series_id, series_name, episode_count
)
SELECT virtual_folder_id, series_id, series_name, episode_count
FROM media_item_tv_series;

INSERT INTO media_item_tv_series_members_new (item_id, virtual_folder_id, series_id)
SELECT item_id, virtual_folder_id, series_id
FROM media_item_tv_series_members;

DROP TABLE media_item_tv_series_members;
DROP TABLE media_item_tv_series;

ALTER TABLE media_item_tv_series_new RENAME TO media_item_tv_series;
ALTER TABLE media_item_tv_series_members_new RENAME TO media_item_tv_series_members;

CREATE INDEX idx_media_item_tv_series_folder_order
ON media_item_tv_series (
    virtual_folder_id,
    series_name COLLATE NOCASE,
    series_name,
    series_id
);

CREATE INDEX idx_media_item_tv_series_global_order
ON media_item_tv_series (series_name COLLATE NOCASE, series_name, series_id, virtual_folder_id);

CREATE INDEX idx_media_item_tv_series_members_folder_series
ON media_item_tv_series_members (virtual_folder_id, series_id, item_id);

CREATE INDEX idx_media_item_tv_series_members_series
ON media_item_tv_series_members (series_id, item_id);

CREATE INDEX idx_media_items_tv_series_anchor_id
ON media_items (trim(json_extract(metadata_json, '$.SeriesId')) COLLATE NOCASE)
WHERE missing_since IS NULL
  AND media_type = 'Series'
  AND lower(collection_type) IN ('tvshows', 'tvshow', 'series')
  AND lower(coalesce(json_extract(metadata_json, '$.PluginVodKind'), '')) = 'series';

-- Version 3 adds explicit zero-member series anchors. Older coverage is not complete under the new
-- source definition and must be rebuilt before it can be served as durable.
DELETE FROM media_item_tv_series_coverage;
