-- Schema 119 used custom GUCs as coordination hints, but PostgreSQL deliberately lets sessions
-- set arbitrary two-part options.  Schema 120 removes that ambient mechanism completely.  Keep
-- the trigger wrappers as SECURITY INVOKER so current_user distinguishes a trusted
-- SECURITY DEFINER publication function from jellyrin_runtime.  The privileged helper below can
-- only make a folder dirty; granting runtime EXECUTE on it cannot publish or preserve stale data.
CREATE FUNCTION jellyrin_mark_query_filter_summary_dirty(folder_ids uuid[])
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    )
    SELECT DISTINCT folder.id, 1, NULL::bigint, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    FROM unnest(folder_ids) AS requested(id)
    JOIN virtual_folders AS folder ON folder.id = requested.id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = media_item_query_filter_summary_revisions.source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;

    DELETE FROM media_item_query_filter_summary_coverage AS coverage
    WHERE coverage.virtual_folder_id = ANY(folder_ids);
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    summary_owner name;
BEGIN
    SELECT pg_get_userbyid(class.relowner)
    INTO summary_owner
    FROM pg_class AS class
    WHERE class.oid = 'media_item_query_filter_summary_values'::regclass;

    IF TG_TABLE_NAME IN (
           'media_item_query_filter_summary_values',
           'media_item_query_filter_sources',
           'media_item_query_filter_values'
       ) AND current_user = summary_owner THEN
        RETURN NULL;
    END IF;

    PERFORM jellyrin_mark_query_filter_summary_dirty(
        ARRAY(SELECT DISTINCT changed.virtual_folder_id FROM changed)
    );
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    summary_owner name;
BEGIN
    SELECT pg_get_userbyid(class.relowner)
    INTO summary_owner
    FROM pg_class AS class
    WHERE class.oid = 'media_item_query_filter_summary_values'::regclass;

    IF TG_TABLE_NAME IN (
           'media_item_query_filter_summary_values',
           'media_item_query_filter_sources',
           'media_item_query_filter_values'
       ) AND current_user = summary_owner THEN
        RETURN NULL;
    END IF;

    PERFORM jellyrin_mark_query_filter_summary_dirty(
        ARRAY(SELECT DISTINCT changed.virtual_folder_id FROM changed)
    );
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_mark_query_filter_summary_dirty_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    summary_owner name;
BEGIN
    SELECT pg_get_userbyid(class.relowner)
    INTO summary_owner
    FROM pg_class AS class
    WHERE class.oid = 'media_item_query_filter_summary_values'::regclass;

    IF TG_TABLE_NAME IN (
           'media_item_query_filter_summary_values',
           'media_item_query_filter_sources',
           'media_item_query_filter_values'
       ) AND current_user = summary_owner THEN
        RETURN NULL;
    END IF;

    PERFORM jellyrin_mark_query_filter_summary_dirty(ARRAY(
        SELECT virtual_folder_id FROM old_rows
        UNION
        SELECT virtual_folder_id FROM new_rows
    ));
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_initialize_query_filter_summary_revision()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
BEGIN
    PERFORM jellyrin_mark_query_filter_summary_dirty(ARRAY[NEW.id]);
    RETURN NULL;
END;
$$;

-- Rebuild one folder from the exact item-level projection and publish coverage last.  This is the
-- only full-publication entry point exposed to the runtime role.
CREATE FUNCTION jellyrin_rebuild_query_filter_summary(requested_folder_id uuid)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    captured_revision bigint;
    published boolean := FALSE;
    published_rows bigint := 0;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'jellyrin-query-filter-summary:' || requested_folder_id::text, 0
    ));

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    )
    SELECT folder.id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    FROM virtual_folders AS folder
    WHERE folder.id = requested_folder_id
    ON CONFLICT (virtual_folder_id) DO NOTHING;

    SELECT source_revision
    INTO STRICT captured_revision
    FROM media_item_query_filter_summary_revisions
    WHERE virtual_folder_id = requested_folder_id
    FOR UPDATE;

    UPDATE media_item_query_filter_summary_revisions
    SET reconciled_revision = NULL,
        dirty_at = COALESCE(dirty_at, CURRENT_TIMESTAMP),
        updated_at = CURRENT_TIMESTAMP
    WHERE virtual_folder_id = requested_folder_id;

    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = requested_folder_id;
    DELETE FROM media_item_query_filter_summary_values
    WHERE virtual_folder_id = requested_folder_id;

    INSERT INTO media_item_query_filter_summary_values (
        virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
        winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
        contributor_count
    )
    SELECT DISTINCT ON (
               item.virtual_folder_id, effective.effective_item_type, value.value_kind,
               lower(btrim(value.display_value))
           )
           item.virtual_folder_id, effective.effective_item_type, value.value_kind,
           lower(btrim(value.display_value)), value.display_value, lower(item.name), item.id,
           value.source_priority, value.source_position,
           count(*) OVER (
               PARTITION BY item.virtual_folder_id, effective.effective_item_type,
                            value.value_kind, lower(btrim(value.display_value))
           )
    FROM media_items AS item
    CROSS JOIN LATERAL (
        SELECT CASE
            WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
            WHEN item.media_type = 'Video'
                 AND item.collection_type IN ('musicvideos', 'musicvideo') THEN 'musicvideo'
            WHEN item.media_type = 'Video'
                 AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                 AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                THEN 'video'
            WHEN item.media_type = 'Video'
                 AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
            WHEN item.media_type = 'Video' THEN 'video'
            WHEN item.media_type = 'Audio' THEN 'audio'
            WHEN item.media_type = 'Photo' THEN 'photo'
            WHEN item.media_type = 'Book' THEN 'book'
            ELSE 'baseitem'
        END AS effective_item_type
    ) AS effective
    JOIN media_item_query_filter_sources AS source
      ON source.item_id = item.id
     AND source.virtual_folder_id = item.virtual_folder_id
     AND source.extractor_version = 1
    JOIN media_item_query_filter_values AS value
      ON value.item_id = source.item_id
     AND value.virtual_folder_id = source.virtual_folder_id
    WHERE item.virtual_folder_id = requested_folder_id
      AND item.missing_since IS NULL
      AND effective.effective_item_type = ANY(ARRAY['movie','episode']::text[])
    ORDER BY item.virtual_folder_id, effective.effective_item_type, value.value_kind,
             lower(btrim(value.display_value)), lower(item.name) COLLATE "C", item.id,
             value.source_priority, value.source_position;

    WITH sources AS MATERIALIZED (
        SELECT item.virtual_folder_id, item.id AS item_id,
               CASE
                   WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
                   WHEN item.media_type = 'Video'
                        AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                        AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                       THEN 'video'
                   WHEN item.media_type = 'Video'
                        AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
                   ELSE 'baseitem'
               END AS effective_item_type,
               source.container_present, source.container_value, source.media_type,
               source.is_video, source.has_subtitles, source.has_trailer
        FROM media_items AS item
        JOIN media_item_query_filter_sources AS source
          ON source.item_id = item.id
         AND source.virtual_folder_id = item.virtual_folder_id
         AND source.extractor_version = 1
        WHERE item.virtual_folder_id = requested_folder_id
          AND item.missing_since IS NULL
          AND CASE
                  WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                       AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                      THEN 'video'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
                  ELSE 'baseitem'
              END = ANY(ARRAY['movie','episode'])
    ), scalar_values AS (
        SELECT virtual_folder_id, effective_item_type, 'containers' AS value_kind,
               lower(container_value) AS normalized_value, lower(container_value) AS display_value,
               min(item_id::text)::uuid AS winner_item_id, count(*) AS contributor_count
        FROM sources WHERE container_present
        GROUP BY virtual_folder_id, effective_item_type, lower(container_value)
        UNION ALL
        SELECT virtual_folder_id, effective_item_type, 'media_types', media_type, media_type,
               min(item_id::text)::uuid, count(*)
        FROM sources
        GROUP BY virtual_folder_id, effective_item_type, media_type
        UNION ALL
        SELECT virtual_folder_id, effective_item_type, 'video_types', 'videofile', 'VideoFile',
               min(item_id::text)::uuid, count(*)
        FROM sources WHERE is_video
        GROUP BY virtual_folder_id, effective_item_type
        UNION ALL
        SELECT virtual_folder_id, effective_item_type, 'has_subtitles', 'true', 'true',
               min(item_id::text)::uuid, count(*)
        FROM sources WHERE has_subtitles
        GROUP BY virtual_folder_id, effective_item_type
        UNION ALL
        SELECT virtual_folder_id, effective_item_type, 'has_trailer', 'true', 'true',
               min(item_id::text)::uuid, count(*)
        FROM sources WHERE has_trailer
        GROUP BY virtual_folder_id, effective_item_type
    )
    INSERT INTO media_item_query_filter_summary_values (
        virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
        winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
        contributor_count
    )
    SELECT virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
           '', winner_item_id, 0, '', contributor_count
    FROM scalar_values;

    WITH folder_type AS (
        SELECT CASE
                   WHEN lower(folder.collection_type) = 'movies' THEN 'movie'
                   WHEN lower(folder.collection_type) = ANY(ARRAY['tvshows','tvshow','series'])
                       THEN 'episode'
               END AS effective_item_type
        FROM virtual_folders AS folder
        WHERE folder.id = requested_folder_id
          AND lower(folder.collection_type) = ANY(ARRAY['movies','tvshows','tvshow','series'])
    ), value_counts AS MATERIALIZED (
        SELECT value.item_id, count(*) AS actual_value_count
        FROM media_item_query_filter_values AS value
        WHERE value.virtual_folder_id = requested_folder_id
        GROUP BY value.item_id
    ), source_items AS MATERIALIZED (
        SELECT item.id, source.item_id AS source_item_id, source.extractor_version,
               source.projected_value_count,
               COALESCE(value_counts.actual_value_count, 0) AS actual_value_count
        FROM media_items AS item
        CROSS JOIN folder_type
        LEFT JOIN media_item_query_filter_sources AS source
          ON source.item_id = item.id AND source.virtual_folder_id = item.virtual_folder_id
        LEFT JOIN value_counts ON value_counts.item_id = item.id
        WHERE item.virtual_folder_id = requested_folder_id
          AND item.missing_since IS NULL
          AND CASE
                  WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                       AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                      THEN 'video'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
                  ELSE 'baseitem'
              END = folder_type.effective_item_type
    ), source_stats AS (
        SELECT count(*) AS item_count, count(source_item_id) AS source_count,
               COALESCE(sum(projected_value_count), 0) AS contribution_count,
               COALESCE(bool_and(extractor_version = 1
                   AND projected_value_count = actual_value_count), TRUE) AS complete
        FROM source_items
    )
    INSERT INTO media_item_query_filter_summary_coverage (
        virtual_folder_id, effective_item_type, projection_version, source_item_count,
        source_contribution_count, summary_value_count, completed_at, source_revision
    )
    SELECT requested_folder_id, folder_type.effective_item_type, 1, source_stats.source_count,
           source_stats.contribution_count,
           (SELECT count(*) FROM media_item_query_filter_summary_values
            WHERE virtual_folder_id = requested_folder_id
              AND effective_item_type = folder_type.effective_item_type),
           CURRENT_TIMESTAMP, captured_revision
    FROM folder_type, source_stats
    WHERE source_stats.complete
      AND source_stats.source_count = source_stats.item_count
      AND COALESCE((
          SELECT sum(summary.contributor_count)
          FROM media_item_query_filter_summary_values AS summary
          WHERE summary.virtual_folder_id = requested_folder_id
            AND summary.effective_item_type = folder_type.effective_item_type
            AND summary.value_kind = ANY(ARRAY[
                'albums','artists','audio_languages','genres','official_ratings',
                'series_statuses','staff_names','studios','subtitle_languages','tags','years'
            ])
      ), 0) = source_stats.contribution_count;

    GET DIAGNOSTICS published_rows = ROW_COUNT;
    published := published_rows = 1;
    IF published THEN
        UPDATE media_item_query_filter_summary_revisions
        SET reconciled_revision = captured_revision, dirty_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE virtual_folder_id = requested_folder_id
          AND source_revision = captured_revision;
        IF NOT FOUND THEN
            DELETE FROM media_item_query_filter_summary_coverage
            WHERE virtual_folder_id = requested_folder_id;
            published := FALSE;
        END IF;
    END IF;
    RETURN published;
END;
$$;

-- Metadata and streams are reconciled by the controlled point writer.  Structural changes alter
-- eligibility, type or winner ordering and must keep the old fail-closed invalidation behavior.
DROP TRIGGER trg_media_items_query_filter_invalidate_update ON media_items;
CREATE TRIGGER trg_media_items_query_filter_invalidate_update
AFTER UPDATE ON media_items
FOR EACH ROW
WHEN (
    OLD.path IS DISTINCT FROM NEW.path
    OR OLD.virtual_folder_id IS DISTINCT FROM NEW.virtual_folder_id
    OR OLD.media_type IS DISTINCT FROM NEW.media_type
)
EXECUTE FUNCTION jellyrin_invalidate_media_item_query_filter_projection();

CREATE FUNCTION jellyrin_mark_media_items_query_filter_summary_dirty_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
BEGIN
    PERFORM jellyrin_mark_query_filter_summary_dirty(ARRAY(
        SELECT old_item.virtual_folder_id
        FROM old_rows AS old_item
        JOIN new_rows AS new_item ON new_item.id = old_item.id
        WHERE ROW(old_item.virtual_folder_id, old_item.name, old_item.path,
                  old_item.media_type, old_item.collection_type, old_item.missing_since)
              IS DISTINCT FROM
              ROW(new_item.virtual_folder_id, new_item.name, new_item.path,
                  new_item.media_type, new_item.collection_type, new_item.missing_since)
        UNION
        SELECT new_item.virtual_folder_id
        FROM old_rows AS old_item
        JOIN new_rows AS new_item ON new_item.id = old_item.id
        WHERE ROW(old_item.virtual_folder_id, old_item.name, old_item.path,
                  old_item.media_type, old_item.collection_type, old_item.missing_since)
              IS DISTINCT FROM
              ROW(new_item.virtual_folder_id, new_item.name, new_item.path,
                  new_item.media_type, new_item.collection_type, new_item.missing_since)
    ));
    RETURN NULL;
END;
$$;

DROP TRIGGER trg_media_items_query_filter_summary_dirty_update ON media_items;
CREATE TRIGGER trg_media_items_query_filter_summary_dirty_update
AFTER UPDATE ON media_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_media_items_query_filter_summary_dirty_update();

-- Capture the installation schema in every function's proconfig.  PostgreSQL otherwise searches
-- pg_temp before an implicit path entry, which would let a runtime session shadow a relation with
-- a temporary table.  The test harness installs migrations into a unique schema, so `public`
-- cannot be hard-coded here.
DO $migration$
DECLARE
    installation_schema text := current_schema();
    function_signature text;
BEGIN
    FOREACH function_signature IN ARRAY ARRAY[
        'jellyrin_mark_query_filter_summary_dirty(uuid[])',
        'jellyrin_mark_query_filter_summary_dirty_insert()',
        'jellyrin_mark_query_filter_summary_dirty_delete()',
        'jellyrin_mark_query_filter_summary_dirty_update()',
        'jellyrin_initialize_query_filter_summary_revision()',
        'jellyrin_invalidate_media_item_query_filter_projection()',
        'jellyrin_mark_media_items_query_filter_summary_dirty_update()',
        'jellyrin_rebuild_query_filter_summary(uuid)'
    ]
    LOOP
        EXECUTE format(
            'ALTER FUNCTION %I.%s SET search_path TO pg_catalog, %I, pg_temp',
            installation_schema, function_signature, installation_schema
        );
    END LOOP;

END
$migration$;

REVOKE ALL ON FUNCTION jellyrin_mark_query_filter_summary_dirty(uuid[]) FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_mark_query_filter_summary_dirty_insert() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_mark_query_filter_summary_dirty_delete() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_mark_query_filter_summary_dirty_update() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_initialize_query_filter_summary_revision() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_invalidate_media_item_query_filter_projection() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_mark_media_items_query_filter_summary_dirty_update() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_rebuild_query_filter_summary(uuid) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime') THEN
        REVOKE ALL PRIVILEGES ON TABLE media_item_query_filter_summary_values,
            media_item_query_filter_summary_coverage,
            media_item_query_filter_summary_revisions FROM jellyrin_runtime;
        GRANT SELECT ON TABLE media_item_query_filter_summary_values,
            media_item_query_filter_summary_coverage,
            media_item_query_filter_summary_revisions TO jellyrin_runtime;
        GRANT EXECUTE ON FUNCTION jellyrin_mark_query_filter_summary_dirty(uuid[])
            TO jellyrin_runtime;
        GRANT EXECUTE ON FUNCTION jellyrin_rebuild_query_filter_summary(uuid)
            TO jellyrin_runtime;
    END IF;
END
$$;

-- Replace one persisted item projection and patch only its old/new aggregate buckets.  Old values
-- are explicit because schema 117 deliberately deletes the live projection as soon as metadata or
-- streams change.  All parallel arrays are validated before any destructive statement.
CREATE FUNCTION jellyrin_reconcile_query_filter_summary_item(
    requested_item_id uuid,
    old_extractor_version integer,
    old_container_present boolean,
    old_container_value text,
    old_media_type text,
    old_is_video boolean,
    old_has_subtitles boolean,
    old_has_trailer boolean,
    old_value_kinds text[],
    old_display_values text[],
    old_source_keys text[],
    old_source_priorities integer[],
    old_source_positions text[],
    new_extractor_version integer,
    new_container_present boolean,
    new_container_value text,
    new_media_type text,
    new_is_video boolean,
    new_has_subtitles boolean,
    new_has_trailer boolean,
    new_value_kinds text[],
    new_display_values text[],
    new_source_keys text[],
    new_source_priorities integer[],
    new_source_positions text[]
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
AS $$
DECLARE
    folder_id uuid;
    target_item_type text;
    item_sort text;
    prior_item_count bigint;
    prior_contribution_count bigint;
    prior_revision bigint;
    next_revision bigint;
    next_contribution_count bigint;
    actual_contribution_count bigint;
    next_summary_value_count bigint;
    affected_kinds text[];
    affected_displays text[];
    old_projection_matches boolean := FALSE;
    affected_scalar_kinds text[];
    affected_scalar_values text[];
BEGIN
    IF old_extractor_version <= 0 OR new_extractor_version <= 0 THEN
        RAISE EXCEPTION 'query-filter extractor version must be positive';
    END IF;
    IF old_container_present <> (old_container_value IS NOT NULL)
       OR new_container_present <> (new_container_value IS NOT NULL) THEN
        RAISE EXCEPTION 'query-filter container presence/value mismatch';
    END IF;
    IF cardinality(old_value_kinds) <> cardinality(old_display_values)
       OR cardinality(old_value_kinds) <> cardinality(old_source_keys)
       OR cardinality(old_value_kinds) <> cardinality(old_source_priorities)
       OR cardinality(old_value_kinds) <> cardinality(old_source_positions)
       OR cardinality(new_value_kinds) <> cardinality(new_display_values)
       OR cardinality(new_value_kinds) <> cardinality(new_source_keys)
       OR cardinality(new_value_kinds) <> cardinality(new_source_priorities)
       OR cardinality(new_value_kinds) <> cardinality(new_source_positions) THEN
        RAISE EXCEPTION 'query-filter projection arrays have different lengths';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM unnest(old_value_kinds, old_display_values, old_source_keys,
                    old_source_priorities, old_source_positions)
             AS value(kind, display_value, source_key, source_priority, source_position)
        WHERE kind IS NULL OR display_value IS NULL OR source_key IS NULL
           OR source_priority IS NULL OR source_priority < 0 OR source_position IS NULL
           OR kind <> ALL(ARRAY[
               'albums','artists','audio_languages','genres','official_ratings',
               'series_statuses','staff_names','studios','subtitle_languages','tags','years'
           ])
    ) OR EXISTS (
        SELECT 1
        FROM unnest(new_value_kinds, new_display_values, new_source_keys,
                    new_source_priorities, new_source_positions)
             AS value(kind, display_value, source_key, source_priority, source_position)
        WHERE kind IS NULL OR display_value IS NULL OR source_key IS NULL
           OR source_priority IS NULL OR source_priority < 0 OR source_position IS NULL
           OR kind <> ALL(ARRAY[
               'albums','artists','audio_languages','genres','official_ratings',
               'series_statuses','staff_names','studios','subtitle_languages','tags','years'
           ])
    ) THEN
        RAISE EXCEPTION 'query-filter projection contains an invalid value';
    END IF;
    IF (SELECT count(*) FROM unnest(old_value_kinds, old_source_keys, old_source_positions)
            AS value(kind, source_key, source_position)) <>
       (SELECT count(DISTINCT (kind, source_key, source_position))
        FROM unnest(old_value_kinds, old_source_keys, old_source_positions)
             AS value(kind, source_key, source_position))
       OR (SELECT count(*) FROM unnest(new_value_kinds, new_source_keys, new_source_positions)
               AS value(kind, source_key, source_position)) <>
          (SELECT count(DISTINCT (kind, source_key, source_position))
           FROM unnest(new_value_kinds, new_source_keys, new_source_positions)
                AS value(kind, source_key, source_position)) THEN
        RAISE EXCEPTION 'query-filter projection contains duplicate source positions';
    END IF;

    SELECT item.virtual_folder_id,
           CASE
               WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
               WHEN item.media_type = 'Video'
                    AND item.collection_type IN ('musicvideos', 'musicvideo') THEN 'musicvideo'
               WHEN item.media_type = 'Video'
                    AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                    AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                   THEN 'video'
               WHEN item.media_type = 'Video'
                    AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
               WHEN item.media_type = 'Video' THEN 'video'
               WHEN item.media_type = 'Audio' THEN 'audio'
               WHEN item.media_type = 'Photo' THEN 'photo'
               WHEN item.media_type = 'Book' THEN 'book'
               ELSE 'baseitem'
           END,
           lower(item.name) COLLATE "C"
    INTO STRICT folder_id, target_item_type, item_sort
    FROM media_items AS item
    WHERE item.id = requested_item_id
    FOR UPDATE;

    PERFORM pg_advisory_xact_lock(hashtextextended(
        'jellyrin-query-filter-summary:' || folder_id::text, 0
    ));

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO NOTHING;
    PERFORM source_revision
    FROM media_item_query_filter_summary_revisions
    WHERE virtual_folder_id = folder_id
    FOR UPDATE;

    SELECT EXISTS (
               SELECT 1
               FROM media_item_query_filter_sources AS source
               WHERE source.item_id = requested_item_id
                 AND source.virtual_folder_id = folder_id
                 AND source.extractor_version = old_extractor_version
                 AND source.container_present = old_container_present
                 AND source.container_value IS NOT DISTINCT FROM old_container_value
                 AND source.media_type = old_media_type
                 AND source.is_video = old_is_video
                 AND source.has_subtitles = old_has_subtitles
                 AND source.has_trailer = old_has_trailer
                 AND source.projected_value_count = cardinality(old_value_kinds)
           )
           AND NOT EXISTS (
               (SELECT value.value_kind, value.display_value, value.source_key,
                       value.source_priority, value.source_position
                FROM media_item_query_filter_values AS value
                WHERE value.item_id = requested_item_id
                  AND value.virtual_folder_id = folder_id
                EXCEPT
                SELECT kind, display_value, source_key, source_priority, source_position
                FROM unnest(old_value_kinds, old_display_values, old_source_keys,
                            old_source_priorities, old_source_positions)
                     AS supplied(kind, display_value, source_key, source_priority, source_position))
               UNION ALL
               (SELECT kind, display_value, source_key, source_priority, source_position
                FROM unnest(old_value_kinds, old_display_values, old_source_keys,
                            old_source_priorities, old_source_positions)
                     AS supplied(kind, display_value, source_key, source_priority, source_position)
                EXCEPT
                SELECT value.value_kind, value.display_value, value.source_key,
                       value.source_priority, value.source_position
                FROM media_item_query_filter_values AS value
                WHERE value.item_id = requested_item_id
                  AND value.virtual_folder_id = folder_id)
           )
    INTO old_projection_matches;
    IF NOT old_projection_matches THEN
        PERFORM jellyrin_mark_query_filter_summary_dirty(ARRAY[folder_id]);
        RETURN FALSE;
    END IF;

    SELECT coverage.source_item_count, coverage.source_contribution_count,
           coverage.source_revision
    INTO prior_item_count, prior_contribution_count, prior_revision
    FROM media_item_query_filter_summary_coverage AS coverage
    JOIN media_item_query_filter_summary_revisions AS revision
      ON revision.virtual_folder_id = coverage.virtual_folder_id
     AND revision.source_revision = coverage.source_revision
     AND revision.reconciled_revision = revision.source_revision
    WHERE coverage.virtual_folder_id = folder_id
      AND coverage.effective_item_type = target_item_type
      AND coverage.projection_version = 1;

    SELECT array_agg(kind ORDER BY kind, display_value),
           array_agg(display_value ORDER BY kind, display_value)
    INTO affected_kinds, affected_displays
    FROM (
        SELECT DISTINCT kind, display_value
        FROM (
            SELECT kind, display_value
            FROM unnest(old_value_kinds, old_display_values) AS old_value(kind, display_value)
            UNION ALL
            SELECT kind, display_value
            FROM unnest(new_value_kinds, new_display_values) AS new_value(kind, display_value)
        ) AS combined
    ) AS affected;

    DELETE FROM media_item_query_filter_sources WHERE item_id = requested_item_id;
    INSERT INTO media_item_query_filter_sources (
        item_id, virtual_folder_id, extractor_version, container_present, container_value,
        media_type, is_video, has_subtitles, has_trailer, projected_value_count, completed_at
    ) VALUES (
        requested_item_id, folder_id, new_extractor_version, new_container_present,
        new_container_value, new_media_type, new_is_video, new_has_subtitles,
        new_has_trailer, cardinality(new_value_kinds), CURRENT_TIMESTAMP
    );
    INSERT INTO media_item_query_filter_values (
        item_id, virtual_folder_id, value_kind, display_value, source_key,
        source_priority, source_position
    )
    SELECT requested_item_id, folder_id, kind, display_value, source_key,
           source_priority, source_position
    FROM unnest(new_value_kinds, new_display_values, new_source_keys,
                new_source_priorities, new_source_positions)
         AS value(kind, display_value, source_key, source_priority, source_position);

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = media_item_query_filter_summary_revisions.source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP
    RETURNING source_revision INTO next_revision;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = folder_id;

    IF prior_revision IS NULL OR target_item_type <> ALL(ARRAY['movie','episode']) THEN
        RETURN FALSE;
    END IF;

    IF affected_kinds IS NOT NULL THEN
        DELETE FROM media_item_query_filter_summary_values AS summary
        USING unnest(affected_kinds, affected_displays) AS affected(kind, display_value)
        WHERE summary.virtual_folder_id = folder_id
          AND summary.effective_item_type = target_item_type
          AND summary.value_kind = affected.kind
          AND summary.normalized_value = lower(btrim(affected.display_value));

        INSERT INTO media_item_query_filter_summary_values (
            virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
            winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
            contributor_count
        )
        SELECT DISTINCT ON (value.value_kind, lower(btrim(value.display_value)))
               item.virtual_folder_id, target_item_type, value.value_kind,
               lower(btrim(value.display_value)), value.display_value, lower(item.name), item.id,
               value.source_priority, value.source_position,
               count(*) OVER (
                   PARTITION BY value.value_kind, lower(btrim(value.display_value))
               )
        FROM media_items AS item
        JOIN media_item_query_filter_sources AS source
          ON source.item_id = item.id
         AND source.virtual_folder_id = item.virtual_folder_id
         AND source.extractor_version = 1
        JOIN media_item_query_filter_values AS value
          ON value.item_id = source.item_id
         AND value.virtual_folder_id = source.virtual_folder_id
        JOIN (
            SELECT DISTINCT kind, lower(btrim(display_value)) AS normalized_value
            FROM unnest(affected_kinds, affected_displays) AS raw(kind, display_value)
        ) AS affected
          ON affected.kind = value.value_kind
         AND affected.normalized_value = lower(btrim(value.display_value))
        WHERE item.virtual_folder_id = folder_id
          AND item.missing_since IS NULL
          AND CASE
                  WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                       AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                      THEN 'video'
                  WHEN item.media_type = 'Video'
                       AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
                  ELSE 'baseitem'
              END = target_item_type
        ORDER BY value.value_kind, lower(btrim(value.display_value)),
                 lower(item.name) COLLATE "C", item.id, value.source_priority,
                 value.source_position;
    END IF;

    WITH old_scalars(kind, normalized_value) AS (
        SELECT 'containers'::text, lower(old_container_value) WHERE old_container_present
        UNION ALL SELECT 'media_types', old_media_type
        UNION ALL SELECT 'video_types', 'videofile' WHERE old_is_video
        UNION ALL SELECT 'has_subtitles', 'true' WHERE old_has_subtitles
        UNION ALL SELECT 'has_trailer', 'true' WHERE old_has_trailer
    ), new_scalars(kind, normalized_value) AS (
        SELECT 'containers'::text, lower(new_container_value) WHERE new_container_present
        UNION ALL SELECT 'media_types', new_media_type
        UNION ALL SELECT 'video_types', 'videofile' WHERE new_is_video
        UNION ALL SELECT 'has_subtitles', 'true' WHERE new_has_subtitles
        UNION ALL SELECT 'has_trailer', 'true' WHERE new_has_trailer
    ), affected AS (
        (SELECT * FROM old_scalars EXCEPT SELECT * FROM new_scalars)
        UNION
        (SELECT * FROM new_scalars EXCEPT SELECT * FROM old_scalars)
    )
    SELECT array_agg(kind ORDER BY kind, normalized_value),
           array_agg(normalized_value ORDER BY kind, normalized_value)
    INTO affected_scalar_kinds, affected_scalar_values
    FROM affected;

    IF affected_scalar_kinds IS NOT NULL THEN
        DELETE FROM media_item_query_filter_summary_values AS summary
        USING unnest(affected_scalar_kinds, affected_scalar_values)
              AS affected(kind, normalized_value)
        WHERE summary.virtual_folder_id = folder_id
          AND summary.effective_item_type = target_item_type
          AND summary.value_kind = affected.kind
          AND summary.normalized_value = affected.normalized_value;

        WITH sources AS MATERIALIZED (
            SELECT item.id AS item_id, source.container_present, source.container_value,
                   source.media_type, source.is_video, source.has_subtitles, source.has_trailer
            FROM media_items AS item
            JOIN media_item_query_filter_sources AS source
              ON source.item_id = item.id
             AND source.virtual_folder_id = item.virtual_folder_id
             AND source.extractor_version = 1
            WHERE item.virtual_folder_id = folder_id
              AND item.missing_since IS NULL
              AND CASE
                      WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
                      WHEN item.media_type = 'Video'
                           AND item.collection_type IN ('tvshows', 'tvshow', 'series')
                           AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
                          THEN 'video'
                      WHEN item.media_type = 'Video'
                           AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
                      ELSE 'baseitem'
                  END = target_item_type
        ), scalar_values AS (
            SELECT 'containers'::text AS kind, lower(container_value) AS normalized_value,
                   lower(container_value) AS display_value, min(item_id::text)::uuid AS winner_id,
                   count(*) AS contributor_count
            FROM sources WHERE container_present GROUP BY lower(container_value)
            UNION ALL
            SELECT 'media_types', media_type, media_type, min(item_id::text)::uuid, count(*)
            FROM sources GROUP BY media_type
            UNION ALL
            SELECT 'video_types', 'videofile', 'VideoFile', min(item_id::text)::uuid, count(*)
            FROM sources WHERE is_video HAVING count(*) > 0
            UNION ALL
            SELECT 'has_subtitles', 'true', 'true', min(item_id::text)::uuid, count(*)
            FROM sources WHERE has_subtitles HAVING count(*) > 0
            UNION ALL
            SELECT 'has_trailer', 'true', 'true', min(item_id::text)::uuid, count(*)
            FROM sources WHERE has_trailer HAVING count(*) > 0
        )
        INSERT INTO media_item_query_filter_summary_values (
            virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
            winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
            contributor_count
        )
        SELECT folder_id, target_item_type, scalar.kind, scalar.normalized_value,
               scalar.display_value, '', scalar.winner_id, 0, '', scalar.contributor_count
        FROM scalar_values AS scalar
        JOIN unnest(affected_scalar_kinds, affected_scalar_values)
             AS affected(kind, normalized_value)
          ON affected.kind = scalar.kind
         AND affected.normalized_value = scalar.normalized_value;
    END IF;

    next_contribution_count := prior_contribution_count
        - cardinality(old_value_kinds) + cardinality(new_value_kinds);
    SELECT COALESCE(sum(summary.contributor_count), 0)::bigint
    INTO actual_contribution_count
    FROM media_item_query_filter_summary_values AS summary
    WHERE summary.virtual_folder_id = folder_id
      AND summary.effective_item_type = target_item_type
      AND summary.value_kind = ANY(ARRAY[
          'albums','artists','audio_languages','genres','official_ratings',
          'series_statuses','staff_names','studios','subtitle_languages','tags','years'
      ]);
    IF actual_contribution_count <> next_contribution_count THEN
        RETURN jellyrin_rebuild_query_filter_summary(folder_id);
    END IF;

    SELECT count(*) INTO next_summary_value_count
    FROM media_item_query_filter_summary_values
    WHERE virtual_folder_id = folder_id AND effective_item_type = target_item_type;

    INSERT INTO media_item_query_filter_summary_coverage (
        virtual_folder_id, effective_item_type, projection_version, source_item_count,
        source_contribution_count, summary_value_count, completed_at, source_revision
    ) VALUES (
        folder_id, target_item_type, 1, prior_item_count, next_contribution_count,
        next_summary_value_count, CURRENT_TIMESTAMP, next_revision
    );
    UPDATE media_item_query_filter_summary_revisions
    SET reconciled_revision = next_revision, dirty_at = NULL, updated_at = CURRENT_TIMESTAMP
    WHERE virtual_folder_id = folder_id AND source_revision = next_revision;
    IF NOT FOUND THEN
        DELETE FROM media_item_query_filter_summary_coverage
        WHERE virtual_folder_id = folder_id;
        RETURN FALSE;
    END IF;
    RETURN TRUE;
END;
$$;

DO $migration$
DECLARE
    installation_schema text := current_schema();
BEGIN
    EXECUTE format(
        'ALTER FUNCTION %I.jellyrin_reconcile_query_filter_summary_item('
        'uuid,integer,boolean,text,text,boolean,boolean,boolean,text[],text[],text[],integer[],text[],'
        'integer,boolean,text,text,boolean,boolean,boolean,text[],text[],text[],integer[],text[]) '
        'SET search_path TO pg_catalog, %I, pg_temp',
        installation_schema, installation_schema
    );
END
$migration$;

REVOKE ALL ON FUNCTION jellyrin_reconcile_query_filter_summary_item(
    uuid, integer, boolean, text, text, boolean, boolean, boolean,
    text[], text[], text[], integer[], text[],
    integer, boolean, text, text, boolean, boolean, boolean,
    text[], text[], text[], integer[], text[]
) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime') THEN
        GRANT EXECUTE ON FUNCTION jellyrin_reconcile_query_filter_summary_item(
            uuid, integer, boolean, text, text, boolean, boolean, boolean,
            text[], text[], text[], integer[], text[],
            integer, boolean, text, text, boolean, boolean, boolean,
            text[], text[], text[], integer[], text[]
        ) TO jellyrin_runtime;
    END IF;
END
$$;
