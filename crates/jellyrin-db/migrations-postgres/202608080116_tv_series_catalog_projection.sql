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

-- Extract the JSON fields once into a narrow, transaction-local working set.  Apart
-- from avoiding repeated JSON evaluation, this keeps validation and grouping from
-- sorting the complete (and potentially heavily bloated) media_items rows.
CREATE TEMP TABLE jellyrin_tv_series_candidates ON COMMIT DROP AS
SELECT item.id AS item_id,
       item.virtual_folder_id,
       btrim(item.metadata->>'SeriesId') AS series_id,
       btrim(item.metadata->>'SeriesName') AS series_name
FROM media_items AS item
JOIN virtual_folders AS folder ON folder.id = item.virtual_folder_id
WHERE lower(coalesce(folder.collection_type, '')) = ANY(
          ARRAY['tvshows', 'tvshow', 'series']::text[]
      )
  AND item.missing_since IS NULL
  AND item.media_type = 'Video'
  AND lower(item.collection_type) = ANY(
        ARRAY['tvshows', 'tvshow', 'series']::text[]
      );

CREATE INDEX jellyrin_tv_series_candidates_folder_series
ON jellyrin_tv_series_candidates (virtual_folder_id, series_id);

ANALYZE jellyrin_tv_series_candidates;

CREATE TEMP TABLE jellyrin_tv_series_eligible (
    id uuid PRIMARY KEY
) ON COMMIT DROP;

INSERT INTO jellyrin_tv_series_eligible (id)
SELECT folder.id
FROM virtual_folders AS folder
WHERE lower(coalesce(folder.collection_type, '')) = ANY(
          ARRAY['tvshows', 'tvshow', 'series']::text[]
      );

DELETE FROM jellyrin_tv_series_eligible AS eligible
WHERE EXISTS (
    SELECT 1
    FROM jellyrin_tv_series_candidates AS candidate
    WHERE candidate.virtual_folder_id = eligible.id
      AND (
          NULLIF(candidate.series_id, '') IS NULL
          OR candidate.series_id !~ '^[0-9a-f]{32}$'
          OR NULLIF(candidate.series_name, '') IS NULL
      )
);

DELETE FROM jellyrin_tv_series_eligible AS eligible
WHERE EXISTS (
    SELECT 1
    FROM jellyrin_tv_series_candidates AS candidate
    WHERE candidate.virtual_folder_id = eligible.id
    GROUP BY candidate.series_id
    HAVING count(DISTINCT candidate.series_name) > 1
);

INSERT INTO media_item_tv_series (
    virtual_folder_id, series_id, series_name, episode_count
)
SELECT candidate.virtual_folder_id,
       candidate.series_id,
       min(candidate.series_name),
       count(*)
FROM jellyrin_tv_series_candidates AS candidate
JOIN jellyrin_tv_series_eligible AS eligible
  ON eligible.id = candidate.virtual_folder_id
GROUP BY candidate.virtual_folder_id, candidate.series_id;

INSERT INTO media_item_tv_series_members (item_id, virtual_folder_id, series_id)
SELECT candidate.item_id, candidate.virtual_folder_id, candidate.series_id
FROM jellyrin_tv_series_candidates AS candidate
JOIN media_item_tv_series AS series
  ON series.virtual_folder_id = candidate.virtual_folder_id
 AND series.series_id = candidate.series_id;

INSERT INTO media_item_tv_series_coverage (
    virtual_folder_id, projection_version, episode_count, series_count
)
SELECT eligible.id,
       1,
       (SELECT count(*) FROM media_item_tv_series_members AS member
        WHERE member.virtual_folder_id = eligible.id),
       (SELECT count(*) FROM media_item_tv_series AS series
        WHERE series.virtual_folder_id = eligible.id)
FROM jellyrin_tv_series_eligible AS eligible;

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
