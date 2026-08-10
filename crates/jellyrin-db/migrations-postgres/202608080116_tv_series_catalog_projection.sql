CREATE TABLE media_item_tv_series (
    virtual_folder_id uuid NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    series_id text NOT NULL,
    series_name text NOT NULL,
    episode_count bigint NOT NULL CHECK (episode_count > 0),
    PRIMARY KEY (virtual_folder_id, series_id)
);

CREATE INDEX idx_media_item_tv_series_folder_order
ON media_item_tv_series (
    virtual_folder_id,
    lower(series_name),
    series_name,
    series_id
);

CREATE INDEX idx_media_item_tv_series_global_order
ON media_item_tv_series (lower(series_name), series_name, series_id, virtual_folder_id);

CREATE INDEX idx_media_items_tv_series_visible_folder
ON media_items (virtual_folder_id)
WHERE missing_since IS NULL
  AND media_type = 'Video'
  AND lower(collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[]);

CREATE TABLE media_item_tv_series_members (
    item_id uuid PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    virtual_folder_id uuid NOT NULL,
    series_id text NOT NULL,
    FOREIGN KEY (virtual_folder_id, series_id)
        REFERENCES media_item_tv_series(virtual_folder_id, series_id)
        ON DELETE CASCADE
);

CREATE INDEX idx_media_item_tv_series_members_folder_series
ON media_item_tv_series_members (virtual_folder_id, series_id, item_id);

CREATE INDEX idx_media_item_tv_series_members_series
ON media_item_tv_series_members (series_id, item_id);

CREATE TABLE media_item_tv_series_coverage (
    virtual_folder_id uuid PRIMARY KEY REFERENCES virtual_folders(id) ON DELETE CASCADE,
    projection_version integer NOT NULL CHECK (projection_version > 0),
    episode_count bigint NOT NULL CHECK (episode_count >= 0),
    series_count bigint NOT NULL CHECK (series_count >= 0),
    completed_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

WITH eligible AS (
    SELECT folder.id
    FROM virtual_folders AS folder
    WHERE lower(coalesce(folder.collection_type, '')) = ANY(
              ARRAY['tvshows', 'tvshow', 'series']::text[]
          )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS invalid
          WHERE invalid.virtual_folder_id = folder.id
            AND invalid.missing_since IS NULL
            AND invalid.media_type = 'Video'
            AND lower(invalid.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
                )
            AND (
                NULLIF(btrim(invalid.metadata->>'SeriesId'), '') IS NULL
                OR btrim(invalid.metadata->>'SeriesId') !~ '^[0-9a-f]{32}$'
                OR NULLIF(btrim(invalid.metadata->>'SeriesName'), '') IS NULL
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS conflicting
          WHERE conflicting.virtual_folder_id = folder.id
            AND conflicting.missing_since IS NULL
            AND conflicting.media_type = 'Video'
            AND lower(conflicting.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
                )
          GROUP BY btrim(conflicting.metadata->>'SeriesId')
          HAVING count(DISTINCT btrim(conflicting.metadata->>'SeriesName')) > 1
      )
)
INSERT INTO media_item_tv_series (
    virtual_folder_id, series_id, series_name, episode_count
)
SELECT item.virtual_folder_id,
       btrim(item.metadata->>'SeriesId'),
       min(btrim(item.metadata->>'SeriesName')),
       count(*)
FROM media_items AS item
JOIN eligible ON eligible.id = item.virtual_folder_id
WHERE item.missing_since IS NULL
  AND item.media_type = 'Video'
  AND lower(item.collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[])
GROUP BY item.virtual_folder_id, btrim(item.metadata->>'SeriesId');

INSERT INTO media_item_tv_series_members (item_id, virtual_folder_id, series_id)
SELECT item.id, item.virtual_folder_id, btrim(item.metadata->>'SeriesId')
FROM media_items AS item
JOIN media_item_tv_series AS series
  ON series.virtual_folder_id = item.virtual_folder_id
 AND series.series_id = btrim(item.metadata->>'SeriesId')
WHERE item.missing_since IS NULL
  AND item.media_type = 'Video'
  AND lower(item.collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[]);

WITH eligible AS (
    SELECT folder.id
    FROM virtual_folders AS folder
    WHERE lower(coalesce(folder.collection_type, '')) = ANY(
              ARRAY['tvshows', 'tvshow', 'series']::text[]
          )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS invalid
          WHERE invalid.virtual_folder_id = folder.id
            AND invalid.missing_since IS NULL
            AND invalid.media_type = 'Video'
            AND lower(invalid.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
                )
            AND (
                NULLIF(btrim(invalid.metadata->>'SeriesId'), '') IS NULL
                OR btrim(invalid.metadata->>'SeriesId') !~ '^[0-9a-f]{32}$'
                OR NULLIF(btrim(invalid.metadata->>'SeriesName'), '') IS NULL
            )
      )
      AND NOT EXISTS (
          SELECT 1
          FROM media_items AS conflicting
          WHERE conflicting.virtual_folder_id = folder.id
            AND conflicting.missing_since IS NULL
            AND conflicting.media_type = 'Video'
            AND lower(conflicting.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
                )
          GROUP BY btrim(conflicting.metadata->>'SeriesId')
          HAVING count(DISTINCT btrim(conflicting.metadata->>'SeriesName')) > 1
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

CREATE FUNCTION jellyrin_invalidate_tv_series_after_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT DISTINCT changed.virtual_folder_id FROM changed
    );
    RETURN NULL;
END;
$$;

CREATE FUNCTION jellyrin_invalidate_tv_series_after_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT DISTINCT changed.virtual_folder_id FROM changed
    );
    RETURN NULL;
END;
$$;

CREATE FUNCTION jellyrin_invalidate_tv_series_after_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT DISTINCT virtual_folder_id FROM old_rows
        UNION
        SELECT DISTINCT virtual_folder_id FROM new_rows
    );
    RETURN NULL;
END;
$$;

CREATE TRIGGER trg_media_items_tv_series_invalidate_insert
AFTER INSERT ON media_items
REFERENCING NEW TABLE AS changed
FOR EACH STATEMENT
EXECUTE FUNCTION jellyrin_invalidate_tv_series_after_insert();

CREATE TRIGGER trg_media_items_tv_series_invalidate_delete
AFTER DELETE ON media_items
REFERENCING OLD TABLE AS changed
FOR EACH STATEMENT
EXECUTE FUNCTION jellyrin_invalidate_tv_series_after_delete();

CREATE TRIGGER trg_media_items_tv_series_invalidate_update
AFTER UPDATE ON media_items
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION jellyrin_invalidate_tv_series_after_update();
