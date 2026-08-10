CREATE TABLE media_item_query_filter_summary_revisions (
    virtual_folder_id TEXT PRIMARY KEY REFERENCES virtual_folders(id) ON DELETE CASCADE,
    source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
    reconciled_revision INTEGER CHECK (
        reconciled_revision IS NULL
        OR (reconciled_revision >= 0 AND reconciled_revision <= source_revision)
    ),
    dirty_at TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
ADD COLUMN source_revision INTEGER NOT NULL DEFAULT 0 CHECK (source_revision >= 0);

DROP TRIGGER trg_media_items_query_filter_summary_invalidate_insert;
DROP TRIGGER trg_media_items_query_filter_summary_invalidate_delete;
DROP TRIGGER trg_media_items_query_filter_summary_invalidate_update;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_insert;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_delete;
DROP TRIGGER trg_query_filter_sources_summary_invalidate_update;
DROP TRIGGER trg_query_filter_values_summary_invalidate_insert;
DROP TRIGGER trg_query_filter_values_summary_invalidate_delete;
DROP TRIGGER trg_query_filter_values_summary_invalidate_update;
DROP TRIGGER trg_query_filter_summary_values_invalidate_insert;
DROP TRIGGER trg_query_filter_summary_values_invalidate_delete;
DROP TRIGGER trg_query_filter_summary_values_invalidate_update;

CREATE TRIGGER trg_media_items_query_filter_summary_dirty_insert
AFTER INSERT ON media_items
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_query_filter_summary_dirty_delete
AFTER DELETE ON media_items
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_media_items_query_filter_summary_dirty_update
AFTER UPDATE ON media_items
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) SELECT NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
      WHERE NEW.virtual_folder_id <> OLD.virtual_folder_id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_sources_summary_dirty_insert
AFTER INSERT ON media_item_query_filter_sources
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_sources_summary_dirty_delete
AFTER DELETE ON media_item_query_filter_sources
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_sources_summary_dirty_update
AFTER UPDATE ON media_item_query_filter_sources
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) SELECT NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
      WHERE NEW.virtual_folder_id <> OLD.virtual_folder_id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_values_summary_dirty_insert
AFTER INSERT ON media_item_query_filter_values
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_values_summary_dirty_delete
AFTER DELETE ON media_item_query_filter_values
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_values_summary_dirty_update
AFTER UPDATE ON media_item_query_filter_values
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (OLD.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) SELECT NEW.virtual_folder_id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
      WHERE NEW.virtual_folder_id <> OLD.virtual_folder_id
    ON CONFLICT (virtual_folder_id) DO UPDATE SET
        source_revision = source_revision + 1,
        reconciled_revision = NULL,
        dirty_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_query_filter_summary_values_dirty_insert
AFTER INSERT ON media_item_query_filter_summary_values
BEGIN
    UPDATE media_item_query_filter_summary_revisions
    SET reconciled_revision = NULL, dirty_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
    WHERE virtual_folder_id = NEW.virtual_folder_id;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = NEW.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_summary_values_dirty_delete
AFTER DELETE ON media_item_query_filter_summary_values
BEGIN
    UPDATE media_item_query_filter_summary_revisions
    SET reconciled_revision = NULL, dirty_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
    WHERE virtual_folder_id = OLD.virtual_folder_id;
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id = OLD.virtual_folder_id;
END;

CREATE TRIGGER trg_query_filter_summary_values_dirty_update
AFTER UPDATE ON media_item_query_filter_summary_values
BEGIN
    UPDATE media_item_query_filter_summary_revisions
    SET reconciled_revision = NULL, dirty_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
    DELETE FROM media_item_query_filter_summary_coverage
    WHERE virtual_folder_id IN (OLD.virtual_folder_id, NEW.virtual_folder_id);
END;

CREATE TRIGGER trg_virtual_folders_query_filter_summary_revision_insert
AFTER INSERT ON virtual_folders
BEGIN
    INSERT INTO media_item_query_filter_summary_revisions (
        virtual_folder_id, source_revision, reconciled_revision, dirty_at, updated_at
    ) VALUES (NEW.id, 1, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
    ON CONFLICT (virtual_folder_id) DO NOTHING;
END;
