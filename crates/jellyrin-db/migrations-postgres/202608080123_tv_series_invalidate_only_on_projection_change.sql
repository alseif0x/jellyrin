-- The TV series projection is derived from folder membership, visibility, media and collection type,
-- and the persisted SeriesId/SeriesName. Its update trigger nevertheless dropped the coverage row for
-- every folder touched by any UPDATE on media_items, so writing probed media info -- which every
-- PlaybackInfo does -- unpublished the projection and pushed the Series listing onto the bounded live
-- page until the next folder sync.
--
-- Compare the fields the projection actually reads, the way schema 120 already narrowed the
-- query-filter summary trigger. Both the old and the new folder are invalidated so moving an item
-- still republishes on each side.

CREATE OR REPLACE FUNCTION jellyrin_invalidate_tv_series_after_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_tv_series_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT old_item.virtual_folder_id
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
    );
    RETURN NULL;
END;
$$;

DO $migration$
DECLARE
    installation_schema text := current_schema();
BEGIN
    EXECUTE format(
        'ALTER FUNCTION %I.jellyrin_invalidate_tv_series_after_update() '
        'SET search_path TO pg_catalog, %I, pg_temp',
        installation_schema, installation_schema
    );
END
$migration$;
