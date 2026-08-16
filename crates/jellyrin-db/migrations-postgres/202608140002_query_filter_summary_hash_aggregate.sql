-- A full remote-series publication can contribute hundreds of thousands of filter values while
-- producing only a few dozen summary buckets. The original DISTINCT ON + window aggregate sorted
-- every contribution globally and could exceed the bounded worker timeout. Replace only that
-- block with a parallel hash aggregate. The rest of the audited publication-boundary function,
-- including revision capture and fail-closed coverage checks, remains unchanged.

DO $migration$
DECLARE
    installation_schema text := current_schema();
    function_body text;
    block_start integer;
    block_end integer;
    replacement text := $replacement$
    WITH buckets AS MATERIALIZED (
        SELECT item.virtual_folder_id,
               effective.effective_item_type,
               value.value_kind,
               lower(btrim(value.display_value)) AS normalized_value,
               min(ARRAY[
                   lower(item.name) COLLATE "C",
                   item.id::text,
                   lpad(value.source_priority::text, 10, '0'),
                   value.source_position,
                   value.display_value
               ]) AS winner,
               count(*) AS contributor_count
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
        GROUP BY item.virtual_folder_id, effective.effective_item_type,
                 value.value_kind, lower(btrim(value.display_value))
    )
    INSERT INTO media_item_query_filter_summary_values (
        virtual_folder_id, effective_item_type, value_kind, normalized_value, display_value,
        winner_item_sort, winner_item_id, winner_source_priority, winner_source_position,
        contributor_count
    )
    SELECT virtual_folder_id, effective_item_type, value_kind, normalized_value,
           winner[5], winner[1], winner[2]::uuid, winner[3]::integer, winner[4],
           contributor_count
    FROM buckets;
$replacement$;
BEGIN
    SELECT procedure.prosrc
    INTO STRICT function_body
    FROM pg_catalog.pg_proc AS procedure
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
    WHERE namespace.nspname = installation_schema
      AND procedure.proname = 'jellyrin_rebuild_query_filter_summary'
      AND pg_catalog.pg_get_function_identity_arguments(procedure.oid) = 'requested_folder_id uuid';

    block_start := strpos(
        function_body,
        E'    INSERT INTO media_item_query_filter_summary_values (\n'
    );
    block_end := strpos(
        function_body,
        E'\n\n    WITH sources AS MATERIALIZED ('
    );
    IF block_start = 0 OR block_end = 0 OR block_end <= block_start THEN
        RAISE EXCEPTION 'query-filter summary winner block was not found';
    END IF;

    function_body := substring(function_body FROM 1 FOR block_start - 1)
        || replacement
        || substring(function_body FROM block_end);
    EXECUTE format(
        'CREATE OR REPLACE FUNCTION %I.jellyrin_rebuild_query_filter_summary(requested_folder_id uuid) '
        'RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER AS %L',
        installation_schema,
        function_body
    );
    EXECUTE format(
        'ALTER FUNCTION %I.jellyrin_rebuild_query_filter_summary(uuid) '
        'SET search_path TO pg_catalog, %I, pg_temp',
        installation_schema,
        installation_schema
    );
END
$migration$;

REVOKE ALL ON FUNCTION jellyrin_rebuild_query_filter_summary(uuid) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime') THEN
        GRANT EXECUTE ON FUNCTION jellyrin_rebuild_query_filter_summary(uuid)
            TO jellyrin_runtime;
    END IF;
END
$$;
