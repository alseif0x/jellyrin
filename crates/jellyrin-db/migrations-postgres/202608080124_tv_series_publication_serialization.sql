-- An on-demand TV-series rebuild is a multi-statement transaction.  Schema 123 made invalidation
-- selective, but its triggers did not take the rebuild's per-folder advisory lock.  A relevant
-- media_items write that committed while coverage was absent could therefore delete zero rows and
-- let the older rebuild publish coverage afterwards.  Serialize every source invalidation with the
-- same lock, acquired in UUID order when a statement touches more than one folder.

CREATE OR REPLACE FUNCTION jellyrin_invalidate_tv_series_after_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    affected_folder_ids uuid[];
    locked_folder_id uuid;
BEGIN
    SELECT array_agg(folder_id ORDER BY folder_id)
    INTO affected_folder_ids
    FROM (SELECT DISTINCT virtual_folder_id AS folder_id FROM changed) AS affected;

    IF affected_folder_ids IS NULL THEN
        RETURN NULL;
    END IF;
    FOREACH locked_folder_id IN ARRAY affected_folder_ids LOOP
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'jellyrin-tv-series-projection:' || locked_folder_id::text, 0
        ));
    END LOOP;

    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id = ANY(affected_folder_ids);
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_invalidate_tv_series_after_delete()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    affected_folder_ids uuid[];
    locked_folder_id uuid;
BEGIN
    SELECT array_agg(folder_id ORDER BY folder_id)
    INTO affected_folder_ids
    FROM (SELECT DISTINCT virtual_folder_id AS folder_id FROM changed) AS affected;

    IF affected_folder_ids IS NULL THEN
        RETURN NULL;
    END IF;
    FOREACH locked_folder_id IN ARRAY affected_folder_ids LOOP
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'jellyrin-tv-series-projection:' || locked_folder_id::text, 0
        ));
    END LOOP;

    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id = ANY(affected_folder_ids);
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION jellyrin_invalidate_tv_series_after_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY INVOKER
AS $$
DECLARE
    affected_folder_ids uuid[];
    locked_folder_id uuid;
BEGIN
    SELECT array_agg(folder_id ORDER BY folder_id)
    INTO affected_folder_ids
    FROM (
        SELECT old_item.virtual_folder_id AS folder_id
        FROM old_rows AS old_item
        JOIN new_rows AS new_item ON new_item.id = old_item.id
        WHERE ROW(old_item.virtual_folder_id, old_item.missing_since, old_item.media_type,
                  old_item.collection_type,
                  btrim(old_item.metadata->>'SeriesId'),
                  btrim(old_item.metadata->>'SeriesName'))
              IS DISTINCT FROM
              ROW(new_item.virtual_folder_id, new_item.missing_since, new_item.media_type,
                  new_item.collection_type,
                  btrim(new_item.metadata->>'SeriesId'),
                  btrim(new_item.metadata->>'SeriesName'))
        UNION
        SELECT new_item.virtual_folder_id
        FROM old_rows AS old_item
        JOIN new_rows AS new_item ON new_item.id = old_item.id
        WHERE ROW(old_item.virtual_folder_id, old_item.missing_since, old_item.media_type,
                  old_item.collection_type,
                  btrim(old_item.metadata->>'SeriesId'),
                  btrim(old_item.metadata->>'SeriesName'))
              IS DISTINCT FROM
              ROW(new_item.virtual_folder_id, new_item.missing_since, new_item.media_type,
                  new_item.collection_type,
                  btrim(new_item.metadata->>'SeriesId'),
                  btrim(new_item.metadata->>'SeriesName'))
    ) AS affected;

    IF affected_folder_ids IS NULL THEN
        RETURN NULL;
    END IF;
    FOREACH locked_folder_id IN ARRAY affected_folder_ids LOOP
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'jellyrin-tv-series-projection:' || locked_folder_id::text, 0
        ));
    END LOOP;

    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id = ANY(affected_folder_ids);
    RETURN NULL;
END;
$$;

DO $migration$
DECLARE
    installation_schema text := current_schema();
    function_name text;
BEGIN
    FOREACH function_name IN ARRAY ARRAY[
        'jellyrin_invalidate_tv_series_after_insert',
        'jellyrin_invalidate_tv_series_after_delete',
        'jellyrin_invalidate_tv_series_after_update'
    ] LOOP
        EXECUTE format(
            'ALTER FUNCTION %I.%I() SET search_path TO pg_catalog, %I, pg_temp',
            installation_schema, function_name, installation_schema
        );
    END LOOP;
END
$migration$;

REVOKE ALL ON FUNCTION jellyrin_invalidate_tv_series_after_insert() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_invalidate_tv_series_after_delete() FROM PUBLIC;
REVOKE ALL ON FUNCTION jellyrin_invalidate_tv_series_after_update() FROM PUBLIC;
