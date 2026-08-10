CREATE TABLE media_item_query_filter_summary_revisions (
    virtual_folder_id uuid PRIMARY KEY REFERENCES virtual_folders(id) ON DELETE CASCADE,
    source_revision bigint NOT NULL CHECK (source_revision >= 0),
    reconciled_revision bigint CHECK (
        reconciled_revision IS NULL
        OR (reconciled_revision >= 0 AND reconciled_revision <= source_revision)
    ),
    dirty_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO media_item_query_filter_summary_revisions (
    virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
)
SELECT folder.id,
       CASE WHEN coverage.virtual_folder_id IS NULL THEN 1 ELSE 0 END,
       CASE WHEN coverage.virtual_folder_id IS NULL THEN NULL ELSE 0 END,
       CASE WHEN coverage.virtual_folder_id IS NULL THEN CURRENT_TIMESTAMP ELSE NULL END,
       CURRENT_TIMESTAMP
FROM virtual_folders AS folder
LEFT JOIN (
    SELECT DISTINCT virtual_folder_id
    FROM media_item_query_filter_summary_coverage
) AS coverage ON coverage.virtual_folder_id = folder.id;

ALTER TABLE media_item_query_filter_summary_coverage
ADD COLUMN source_revision bigint NOT NULL DEFAULT 0 CHECK (source_revision >= 0);

CREATE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_TABLE_NAME = 'media_item_query_filter_summary_values'
       AND current_setting('jellyrin.query_filter_summary_rebuild', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;
    IF TG_TABLE_NAME IN ('media_items', 'media_item_query_filter_sources',
                         'media_item_query_filter_values')
       AND current_setting('jellyrin.query_filter_summary_source_patch', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    )
    SELECT DISTINCT changed.virtual_folder_id, 1, NULL::bigint,
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    FROM changed
    JOIN virtual_folders AS folder ON folder.id = changed.virtual_folder_id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = media_item_query_filter_summary_revisions.source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;

    DELETE FROM media_item_query_filter_summary_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT DISTINCT changed.virtual_folder_id FROM changed
    );
    RETURN NULL;
END;
$$;

CREATE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_TABLE_NAME = 'media_item_query_filter_summary_values'
       AND current_setting('jellyrin.query_filter_summary_rebuild', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;
    IF TG_TABLE_NAME IN ('media_items', 'media_item_query_filter_sources',
                         'media_item_query_filter_values')
       AND current_setting('jellyrin.query_filter_summary_source_patch', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    )
    SELECT DISTINCT changed.virtual_folder_id, 1, NULL::bigint,
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    FROM changed
    JOIN virtual_folders AS folder ON folder.id = changed.virtual_folder_id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = media_item_query_filter_summary_revisions.source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;

    DELETE FROM media_item_query_filter_summary_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT DISTINCT changed.virtual_folder_id FROM changed
    );
    RETURN NULL;
END;
$$;

CREATE FUNCTION jellyrin_mark_query_filter_summary_dirty_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_TABLE_NAME = 'media_item_query_filter_summary_values'
       AND current_setting('jellyrin.query_filter_summary_rebuild', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;
    IF TG_TABLE_NAME IN ('media_items', 'media_item_query_filter_sources',
                         'media_item_query_filter_values')
       AND current_setting('jellyrin.query_filter_summary_source_patch', TRUE) = 'on' THEN
        RETURN NULL;
    END IF;

    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    )
    SELECT DISTINCT folders.virtual_folder_id, 1, NULL::bigint,
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    FROM (
        SELECT virtual_folder_id FROM old_rows
        UNION
        SELECT virtual_folder_id FROM new_rows
    ) AS folders
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = media_item_query_filter_summary_revisions.source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;

    DELETE FROM media_item_query_filter_summary_coverage AS coverage
    WHERE coverage.virtual_folder_id IN (
        SELECT virtual_folder_id FROM old_rows
        UNION
        SELECT virtual_folder_id FROM new_rows
    );
    RETURN NULL;
END;
$$;

DROP TRIGGER trg_media_items_query_filter_summary_invalidate_insert ON media_items;
DROP TRIGGER trg_media_items_query_filter_summary_invalidate_delete ON media_items;
DROP TRIGGER trg_media_items_query_filter_summary_invalidate_update ON media_items;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_insert ON media_item_query_filter_sources;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_delete ON media_item_query_filter_sources;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_update ON media_item_query_filter_sources;
DROP TRIGGER trg_query_filter_values_summary_invalidate_insert ON media_item_query_filter_values;
DROP TRIGGER trg_query_filter_values_summary_invalidate_delete ON media_item_query_filter_values;
DROP TRIGGER trg_query_filter_values_summary_invalidate_update ON media_item_query_filter_values;
DROP TRIGGER trg_query_filter_summary_values_invalidate_insert ON media_item_query_filter_summary_values;
DROP TRIGGER trg_query_filter_summary_values_invalidate_delete ON media_item_query_filter_summary_values;
DROP TRIGGER trg_query_filter_summary_values_invalidate_update ON media_item_query_filter_summary_values;

CREATE TRIGGER trg_media_items_query_filter_summary_dirty_insert
AFTER INSERT ON media_items REFERENCING NEW TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert();
CREATE TRIGGER trg_media_items_query_filter_summary_dirty_delete
AFTER DELETE ON media_items REFERENCING OLD TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete();
CREATE TRIGGER trg_media_items_query_filter_summary_dirty_update
AFTER UPDATE ON media_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_update();

CREATE TRIGGER trg_query_filter_sources_summary_dirty_insert
AFTER INSERT ON media_item_query_filter_sources REFERENCING NEW TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert();
CREATE TRIGGER trg_query_filter_sources_summary_dirty_delete
AFTER DELETE ON media_item_query_filter_sources REFERENCING OLD TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete();
CREATE TRIGGER trg_query_filter_sources_summary_dirty_update
AFTER UPDATE ON media_item_query_filter_sources REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_update();

CREATE TRIGGER trg_query_filter_values_summary_dirty_insert
AFTER INSERT ON media_item_query_filter_values REFERENCING NEW TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert();
CREATE TRIGGER trg_query_filter_values_summary_dirty_delete
AFTER DELETE ON media_item_query_filter_values REFERENCING OLD TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete();
CREATE TRIGGER trg_query_filter_values_summary_dirty_update
AFTER UPDATE ON media_item_query_filter_values REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_update();

CREATE TRIGGER trg_query_filter_summary_values_dirty_insert
AFTER INSERT ON media_item_query_filter_summary_values REFERENCING NEW TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_insert();
CREATE TRIGGER trg_query_filter_summary_values_dirty_delete
AFTER DELETE ON media_item_query_filter_summary_values REFERENCING OLD TABLE AS changed
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_delete();
CREATE TRIGGER trg_query_filter_summary_values_dirty_update
AFTER UPDATE ON media_item_query_filter_summary_values REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT EXECUTE FUNCTION jellyrin_mark_query_filter_summary_dirty_update();

DROP FUNCTION jellyrin_invalidate_query_filter_summary_insert();
DROP FUNCTION jellyrin_invalidate_query_filter_summary_delete();
DROP FUNCTION jellyrin_invalidate_query_filter_summary_update();

CREATE FUNCTION jellyrin_initialize_query_filter_summary_revision()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (NEW.id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE TRIGGER trg_virtual_folders_query_filter_summary_revision_insert
AFTER INSERT ON virtual_folders
FOR EACH ROW EXECUTE FUNCTION jellyrin_initialize_query_filter_summary_revision();

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'jellyrin_runtime') THEN
        REVOKE ALL PRIVILEGES ON TABLE media_item_query_filter_summary_revisions
            FROM jellyrin_runtime;
        GRANT SELECT, INSERT, UPDATE ON TABLE media_item_query_filter_summary_revisions
            TO jellyrin_runtime;
    END IF;
END
$$;
