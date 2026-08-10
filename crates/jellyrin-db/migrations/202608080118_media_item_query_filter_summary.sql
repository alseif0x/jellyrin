CREATE TABLE media_item_query_filter_summary_values (
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    effective_item_type TEXT NOT NULL CHECK (effective_item_type IN (
        'movie', 'episode', 'musicvideo', 'video', 'audio', 'photo', 'book', 'baseitem'
    )),
    value_kind TEXT NOT NULL CHECK (value_kind IN (
        'albums', 'artists', 'audio_languages', 'genres', 'official_ratings',
        'series_statuses', 'staff_names', 'studios', 'subtitle_languages', 'tags', 'years',
        'containers', 'media_types', 'video_types', 'has_subtitles', 'has_trailer'
    )),
    normalized_value TEXT NOT NULL,
    display_value TEXT NOT NULL,
    winner_item_sort TEXT NOT NULL,
    winner_item_id TEXT NOT NULL,
    winner_source_priority INTEGER NOT NULL CHECK (winner_source_priority >= 0),
    winner_source_position TEXT NOT NULL,
    contributor_count INTEGER NOT NULL CHECK (contributor_count > 0),
    PRIMARY KEY (virtual_folder_id, effective_item_type, value_kind, normalized_value)
);

CREATE INDEX idx_media_item_query_filter_summary_values_global
ON media_item_query_filter_summary_values (
    effective_item_type, value_kind, normalized_value, virtual_folder_id
);

CREATE TABLE media_item_query_filter_summary_coverage (
    virtual_folder_id TEXT NOT NULL REFERENCES virtual_folders(id) ON DELETE CASCADE,
    effective_item_type TEXT NOT NULL CHECK (effective_item_type IN (
        'movie', 'episode', 'musicvideo', 'video', 'audio', 'photo', 'book', 'baseitem'
    )),
    projection_version INTEGER NOT NULL CHECK (projection_version > 0),
    source_item_count INTEGER NOT NULL CHECK (source_item_count >= 0),
    source_contribution_count INTEGER NOT NULL CHECK (source_contribution_count >= 0),
    summary_value_count INTEGER NOT NULL CHECK (summary_value_count >= 0),
    completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (virtual_folder_id, effective_item_type)
);

-- SQLite has no statement transition tables, but doing the expensive work once here still keeps
-- the initial projection atomic.  Coverage is published only for item sets that are complete in
-- projection 117 and whose stored contribution counts match its actual value rows.
CREATE TEMP TABLE jellyrin_query_filter_summary_items AS
SELECT item.id AS item_id,
       item.virtual_folder_id,
       CASE
           WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
           WHEN item.media_type = 'Video'
                AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                AND (('/' || lower(item.path) || '/') LIKE '%/extras/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/featurettes/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/special features/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/behind the scenes/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/deleted scenes/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/interviews/%'
                     OR ('/' || lower(item.path) || '/') LIKE '%/trailers/%') THEN 'video'
           WHEN item.media_type = 'Video'
                AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
           ELSE 'baseitem'
       END AS effective_item_type,
       lower(item.name) AS item_sort,
       source.item_id AS source_item_id,
       source.extractor_version,
       source.container_present,
       source.container_value,
       source.media_type,
       source.is_video,
       source.has_subtitles,
       source.has_trailer,
       source.projected_value_count,
       coalesce(value_count.actual_value_count, 0) AS actual_value_count
FROM media_items AS item
LEFT JOIN media_item_query_filter_sources AS source
  ON source.item_id = item.id
 AND source.virtual_folder_id = item.virtual_folder_id
LEFT JOIN (
    SELECT value.item_id, value.virtual_folder_id, count(*) AS actual_value_count
    FROM media_item_query_filter_values AS value
    GROUP BY value.item_id, value.virtual_folder_id
) AS value_count
  ON value_count.item_id = item.id
 AND value_count.virtual_folder_id = item.virtual_folder_id
WHERE item.missing_since IS NULL
  AND (
      (item.media_type = 'Video' AND item.collection_type = 'movies')
      OR (item.media_type = 'Video'
          AND item.collection_type IN ('tvshows', 'tvshow', 'series')
          AND NOT (('/' || lower(item.path) || '/') LIKE '%/extras/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/featurettes/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/special features/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/behind the scenes/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/deleted scenes/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/interviews/%'
                   OR ('/' || lower(item.path) || '/') LIKE '%/trailers/%'))
  );

CREATE INDEX jellyrin_query_filter_summary_items_group
ON jellyrin_query_filter_summary_items (virtual_folder_id, effective_item_type, item_id);

CREATE TEMP TABLE jellyrin_query_filter_summary_eligible AS
SELECT item.virtual_folder_id,
       item.effective_item_type,
       count(*) AS source_item_count,
       coalesce(sum(item.projected_value_count), 0) AS source_contribution_count
FROM jellyrin_query_filter_summary_items AS item
GROUP BY item.virtual_folder_id, item.effective_item_type
HAVING count(*) = count(item.source_item_id)
   AND min(CASE WHEN item.extractor_version = 1 THEN 1 ELSE 0 END) = 1
   AND min(CASE WHEN item.projected_value_count = item.actual_value_count THEN 1 ELSE 0 END) = 1
UNION ALL
SELECT folder.id,
       CASE
           WHEN lower(coalesce(folder.collection_type, '')) = 'movies' THEN 'movie'
           ELSE 'episode'
       END,
       0,
       0
FROM virtual_folders AS folder
WHERE lower(coalesce(folder.collection_type, '')) IN (
          'movies', 'tvshows', 'tvshow', 'series'
      )
  AND NOT EXISTS (
      SELECT 1
      FROM jellyrin_query_filter_summary_items AS item
      WHERE item.virtual_folder_id = folder.id
        AND item.effective_item_type = CASE
            WHEN lower(coalesce(folder.collection_type, '')) = 'movies' THEN 'movie'
            ELSE 'episode'
        END
  );

CREATE UNIQUE INDEX jellyrin_query_filter_summary_eligible_key
ON jellyrin_query_filter_summary_eligible (virtual_folder_id, effective_item_type);

INSERT INTO media_item_query_filter_summary_values (
    virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
    winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
    contributor_count
)
WITH candidates AS (
    SELECT item.virtual_folder_id, item.effective_item_type, value.value_kind,
           lower(trim(value.display_value)) AS normalized_value, value.display_value,
           item.item_sort, item.item_id, value.source_priority, value.source_position,
           count(*) OVER (
               PARTITION BY item.virtual_folder_id, item.effective_item_type, value.value_kind,
                            lower(trim(value.display_value))
           ) AS contributor_count,
           row_number() OVER (
               PARTITION BY item.virtual_folder_id, item.effective_item_type, value.value_kind,
                            lower(trim(value.display_value))
               ORDER BY item.item_sort COLLATE BINARY, item.item_id,
                        value.source_priority, value.source_position
           ) AS spelling_rank
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    JOIN media_item_query_filter_values AS value
      ON value.item_id = item.item_id
     AND value.virtual_folder_id = item.virtual_folder_id
)
SELECT virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
       item_sort, item_id, source_priority, source_position, contributor_count
FROM candidates
WHERE spelling_rank = 1;

WITH scalar_values AS (
    SELECT item.virtual_folder_id, item.effective_item_type, 'containers' AS value_kind,
           lower(item.container_value) AS normalized_value,
           lower(item.container_value) AS display_value,
           min(item.item_id) AS winner_item_id, count(*) AS contributor_count
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    WHERE item.container_present = 1
    GROUP BY item.virtual_folder_id, item.effective_item_type, lower(item.container_value)
    UNION ALL
    SELECT item.virtual_folder_id, item.effective_item_type, 'media_types', item.media_type,
           item.media_type, min(item.item_id), count(*)
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    GROUP BY item.virtual_folder_id, item.effective_item_type, item.media_type
    UNION ALL
    SELECT item.virtual_folder_id, item.effective_item_type, 'video_types', 'videofile',
           'VideoFile', min(item.item_id), count(*)
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    WHERE item.is_video = 1 GROUP BY item.virtual_folder_id, item.effective_item_type
    UNION ALL
    SELECT item.virtual_folder_id, item.effective_item_type, 'has_subtitles', 'true', 'true',
           min(item.item_id), count(*)
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    WHERE item.has_subtitles = 1 GROUP BY item.virtual_folder_id, item.effective_item_type
    UNION ALL
    SELECT item.virtual_folder_id, item.effective_item_type, 'has_trailer', 'true', 'true',
           min(item.item_id), count(*)
    FROM jellyrin_query_filter_summary_items AS item
    JOIN jellyrin_query_filter_summary_eligible AS eligible
      ON eligible.virtual_folder_id = item.virtual_folder_id
     AND eligible.effective_item_type = item.effective_item_type
    WHERE item.has_trailer = 1 GROUP BY item.virtual_folder_id, item.effective_item_type
)
INSERT INTO media_item_query_filter_summary_values (
    virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
    winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
    contributor_count
)
SELECT virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
       '', winner_item_id, 0, '', contributor_count
FROM scalar_values;

INSERT INTO media_item_query_filter_summary_coverage (
    virtual_folder_id, effective_item_type, projection_version, source_item_count,
    source_contribution_count, summary_value_count, completed_at
)
SELECT eligible.virtual_folder_id,
       eligible.effective_item_type,
       1,
       eligible.source_item_count,
       eligible.source_contribution_count,
       count(summary.normalized_value),
       CURRENT_TIMESTAMP
FROM jellyrin_query_filter_summary_eligible AS eligible
LEFT JOIN media_item_query_filter_summary_values AS summary
  ON summary.virtual_folder_id = eligible.virtual_folder_id
 AND summary.effective_item_type = eligible.effective_item_type
GROUP BY eligible.virtual_folder_id, eligible.effective_item_type,
         eligible.source_item_count, eligible.source_contribution_count
HAVING coalesce(sum(CASE WHEN summary.value_kind IN (
           'albums', 'artists', 'audio_languages', 'genres', 'official_ratings',
           'series_statuses', 'staff_names', 'studios', 'subtitle_languages', 'tags', 'years'
       ) THEN summary.contributor_count ELSE 0 END), 0) = eligible.source_contribution_count;

DROP TABLE jellyrin_query_filter_summary_eligible;
DROP TABLE jellyrin_query_filter_summary_items;

CREATE TRIGGER trg_media_items_query_filter_summary_invalidate_insert
AFTER INSERT ON media_items
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_query_filter_summary_invalidate_delete
AFTER DELETE ON media_items
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_query_filter_summary_invalidate_update
AFTER UPDATE OF virtual_folder_id, name, path, media_type, collection_type, missing_since ON media_items
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_sources_summary_invalidate_insert
AFTER INSERT ON media_item_query_filter_sources
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_sources_summary_invalidate_delete
AFTER DELETE ON media_item_query_filter_sources
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_sources_summary_invalidate_update
AFTER UPDATE ON media_item_query_filter_sources
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_values_summary_invalidate_insert
AFTER INSERT ON media_item_query_filter_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_values_summary_invalidate_delete
AFTER DELETE ON media_item_query_filter_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_values_summary_invalidate_update
AFTER UPDATE ON media_item_query_filter_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_summary_values_invalidate_insert
AFTER INSERT ON media_item_query_filter_summary_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_summary_values_invalidate_delete
AFTER DELETE ON media_item_query_filter_summary_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_summary_values_invalidate_update
AFTER UPDATE ON media_item_query_filter_summary_values
BEGIN
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;
