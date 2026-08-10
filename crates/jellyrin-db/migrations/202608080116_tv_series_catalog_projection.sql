CREATE TABLE media_item_tv_series (
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    series_id TEXT NOT NULL,
    series_name TEXT NOT NULL,
    episode_count INTEGER NOT NULL CHECK (episode_count > 0),
    PRIMARY KEY (virtual_folder_id, series_id)
);

CREATE INDEX idx_media_item_tv_series_folder_order
ON media_item_tv_series (
    virtual_folder_id,
    series_name COLLATE NOCASE,
    series_name,
    series_id
);

CREATE INDEX idx_media_item_tv_series_global_order
ON media_item_tv_series (series_name COLLATE NOCASE, series_name, series_id, virtual_folder_id);

CREATE INDEX idx_media_items_tv_series_visible_folder
ON media_items (virtual_folder_id)
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) IN ('tvshows', 'tvshow', 'series');

CREATE TABLE media_item_tv_series_members (
    item_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    virtual_folder_id TEXT NOT NULL,
    series_id TEXT NOT NULL,
    FOREIGN KEY (virtual_folder_id, series_id)
        REFERENCES media_item_tv_series(virtual_folder_id, series_id)
        ON DELETE CASCADE
);

CREATE INDEX idx_media_item_tv_series_members_folder_series
ON media_item_tv_series_members (virtual_folder_id, series_id, item_id);

CREATE INDEX idx_media_item_tv_series_members_series
ON media_item_tv_series_members (series_id, item_id);

CREATE TABLE media_item_tv_series_coverage (
    virtual_folder_id TEXT PRIMARY KEY REFERENCES virtual_folders(id) ON DELETE CASCADE,
    projection_version INTEGER NOT NULL CHECK (projection_version > 0),
    episode_count INTEGER NOT NULL CHECK (episode_count >= 0),
    series_count INTEGER NOT NULL CHECK (series_count >= 0),
    completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

WITH eligible AS (
    SELECT folder.id
    FROM virtual_folders AS folder
    WHERE lower(coalesce(folder.collection_type, '')) IN ('tvshows', 'tvshow', 'series')
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS invalid
          WHERE invalid.virtual_folder_id = folder.id
            AND invalid.missing_since IS NULL
            AND invalid.media_type = 'Video'
            AND lower(invalid.collection_type) IN ('tvshows', 'tvshow', 'series')
            AND (
                NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesId')), '') IS NULL
                OR length(trim(json_extract(invalid.metadata_json, '$.SeriesId'))) <> 32
                OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) <>
                   lower(trim(json_extract(invalid.metadata_json, '$.SeriesId')))
                OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
                OR NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesName')), '') IS NULL
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS conflicting
          WHERE conflicting.virtual_folder_id = folder.id
            AND conflicting.missing_since IS NULL
            AND conflicting.media_type = 'Video'
            AND lower(conflicting.collection_type) IN ('tvshows', 'tvshow', 'series')
          GROUP BY trim(json_extract(conflicting.metadata_json, '$.SeriesId'))
          HAVING count(DISTINCT trim(
              json_extract(conflicting.metadata_json, '$.SeriesName')
          )) > 1
      )
)
INSERT INTO media_item_tv_series (
    virtual_folder_id, series_id, series_name, episode_count
)
SELECT item.virtual_folder_id,
       trim(json_extract(item.metadata_json, '$.SeriesId')),
       min(trim(json_extract(item.metadata_json, '$.SeriesName'))),
       count(*)
FROM media_items AS item
JOIN eligible ON eligible.id = item.virtual_folder_id
WHERE item.missing_since IS NULL
  AND item.media_type = 'Video'
  AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series')
GROUP BY item.virtual_folder_id, trim(json_extract(item.metadata_json, '$.SeriesId'));

INSERT INTO media_item_tv_series_members (item_id, virtual_folder_id, series_id)
SELECT item.id, item.virtual_folder_id, trim(json_extract(item.metadata_json, '$.SeriesId'))
FROM media_items AS item
JOIN media_item_tv_series AS series
  ON series.virtual_folder_id = item.virtual_folder_id
 AND series.series_id = trim(json_extract(item.metadata_json, '$.SeriesId'))
WHERE item.missing_since IS NULL
  AND item.media_type = 'Video'
  AND lower(item.collection_type) IN ('tvshows', 'tvshow', 'series');

WITH eligible AS (
    SELECT folder.id
    FROM virtual_folders AS folder
    WHERE lower(coalesce(folder.collection_type, '')) IN ('tvshows', 'tvshow', 'series')
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS invalid
          WHERE invalid.virtual_folder_id = folder.id
            AND invalid.missing_since IS NULL
            AND invalid.media_type = 'Video'
            AND lower(invalid.collection_type) IN ('tvshows', 'tvshow', 'series')
            AND (
                NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesId')), '') IS NULL
                OR length(trim(json_extract(invalid.metadata_json, '$.SeriesId'))) <> 32
                OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) <>
                   lower(trim(json_extract(invalid.metadata_json, '$.SeriesId')))
                OR trim(json_extract(invalid.metadata_json, '$.SeriesId')) GLOB '*[^0-9a-f]*'
                OR NULLIF(trim(json_extract(invalid.metadata_json, '$.SeriesName')), '') IS NULL
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS conflicting
          WHERE conflicting.virtual_folder_id = folder.id
            AND conflicting.missing_since IS NULL
            AND conflicting.media_type = 'Video'
            AND lower(conflicting.collection_type) IN ('tvshows', 'tvshow', 'series')
          GROUP BY trim(json_extract(conflicting.metadata_json, '$.SeriesId'))
          HAVING count(DISTINCT trim(
              json_extract(conflicting.metadata_json, '$.SeriesName')
          )) > 1
      )
)
INSERT INTO media_item_tv_series_coverage (
    virtual_folder_id, projection_version, episode_count, series_count
)
SELECT eligible.id,
       1,
       (SELECT count(*) FROM media_item_tv_series_members AS member
        WHERE member.virtual_folder_id = eligible.id),
       (SELECT count(*) FROM media_item_tv_series AS series
        WHERE series.virtual_folder_id = eligible.id)
FROM eligible;

CREATE TRIGGER trg_media_items_tv_series_invalidate_insert
AFTER INSERT ON media_items
BEGIN
    DELETE FROM media_item_tv_series_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_tv_series_invalidate_delete
AFTER DELETE ON media_items
BEGIN
    DELETE FROM media_item_tv_series_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_tv_series_invalidate_update
AFTER UPDATE ON media_items
BEGIN
    DELETE FROM media_item_tv_series_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;
