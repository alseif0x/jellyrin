-- Repository lookups treat the four one-row-per-plugin stores as case-insensitive. Block
-- concurrent mutations while the preflight and expression-index replacement run so a colliding
-- row cannot appear between the diagnostic check and CREATE UNIQUE INDEX.
LOCK TABLE installed_plugins,
           plugin_manifests,
           plugin_configurations,
           plugin_permissions,
           live_tv_categories,
           live_tv_channels
    IN SHARE ROW EXCLUSIVE MODE;

DO $$
DECLARE
    collision record;
BEGIN
    SELECT table_name, normalized_plugin_id, plugin_ids
    INTO collision
    FROM (
        SELECT 'installed_plugins'::text AS table_name,
               lower(plugin_id) AS normalized_plugin_id,
               array_agg(plugin_id ORDER BY plugin_id) AS plugin_ids
        FROM installed_plugins
        GROUP BY lower(plugin_id)
        HAVING count(*) > 1

        UNION ALL

        SELECT 'plugin_manifests'::text,
               lower(plugin_id),
               array_agg(plugin_id ORDER BY plugin_id)
        FROM plugin_manifests
        GROUP BY lower(plugin_id)
        HAVING count(*) > 1

        UNION ALL

        SELECT 'plugin_configurations'::text,
               lower(plugin_id),
               array_agg(plugin_id ORDER BY plugin_id)
        FROM plugin_configurations
        GROUP BY lower(plugin_id)
        HAVING count(*) > 1

        UNION ALL

        SELECT 'plugin_permissions'::text,
               lower(plugin_id),
               array_agg(plugin_id ORDER BY plugin_id)
        FROM plugin_permissions
        GROUP BY lower(plugin_id)
        HAVING count(*) > 1
    ) AS collisions
    ORDER BY table_name, normalized_plugin_id
    LIMIT 1;

    IF FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = '23505',
            MESSAGE = format(
                'case-insensitive plugin_id collision in %s for lower(plugin_id)=%L: %s; no rows were discarded automatically',
                collision.table_name,
                collision.normalized_plugin_id,
                array_to_string(collision.plugin_ids, ', ')
            ),
            HINT = 'Resolve every listed plugin_id collision explicitly, then rerun the migration.';
    END IF;
END
$$;

-- A channel category must belong to the same tuner as the channel. Refuse to reinterpret existing
-- cross-tuner data; the offending identities make repair explicit and reproducible.
DO $$
DECLARE
    mismatch record;
BEGIN
    SELECT channel.channel_id,
           channel.tuner_id AS channel_tuner_id,
           channel.category_id,
           category.tuner_id AS category_tuner_id
    INTO mismatch
    FROM live_tv_channels AS channel
    JOIN live_tv_categories AS category
      ON category.category_id = channel.category_id
    WHERE channel.tuner_id IS DISTINCT FROM category.tuner_id
    ORDER BY channel.channel_id
    LIMIT 1;

    IF FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = '23503',
            MESSAGE = format(
                'Live TV tuner/category mismatch: channel_id=%L has tuner_id=%L but category_id=%L belongs to tuner_id=%L',
                mismatch.channel_id,
                mismatch.channel_tuner_id,
                mismatch.category_id,
                mismatch.category_tuner_id
            ),
            HINT = 'Repair the channel/category tuner ownership explicitly, then rerun the migration.';
    END IF;
END
$$;

CREATE UNIQUE INDEX installed_plugins_plugin_id_ci_uniq
    ON installed_plugins (lower(plugin_id));
CREATE UNIQUE INDEX plugin_manifests_plugin_id_ci_uniq
    ON plugin_manifests (lower(plugin_id));
CREATE UNIQUE INDEX plugin_configurations_plugin_id_ci_uniq
    ON plugin_configurations (lower(plugin_id));
CREATE UNIQUE INDEX plugin_permissions_plugin_id_ci_uniq
    ON plugin_permissions (lower(plugin_id));

DROP INDEX installed_plugins_id_ci_idx;
DROP INDEX plugin_manifests_id_ci_idx;
DROP INDEX plugin_configurations_id_ci_idx;
DROP INDEX plugin_permissions_id_ci_idx;

ALTER TABLE live_tv_categories
    ADD CONSTRAINT live_tv_categories_category_tuner_key
        UNIQUE (category_id, tuner_id);

ALTER TABLE live_tv_channels
    DROP CONSTRAINT live_tv_channels_category_id_fkey,
    ADD CONSTRAINT live_tv_channels_category_tuner_fkey
        FOREIGN KEY (category_id, tuner_id)
        REFERENCES live_tv_categories (category_id, tuner_id)
        ON DELETE SET NULL (category_id);

-- PostgreSQL does not create indexes on the referencing side of foreign keys.
CREATE INDEX activity_log_entries_user_idx ON activity_log_entries (user_id);
CREATE INDEX display_preferences_user_idx ON display_preferences (user_id);
CREATE INDEX media_lists_owner_user_idx ON media_lists (owner_user_id);
