CREATE TABLE jellyrin_derived_projection_versions (
    projection_name text PRIMARY KEY,
    extractor_version integer NOT NULL CHECK (extractor_version > 0),
    completed_at timestamptz NOT NULL,
    source_item_count bigint NOT NULL CHECK (source_item_count >= 0),
    projected_facet_count bigint NOT NULL CHECK (projected_facet_count >= 0),
    projected_alias_count bigint NOT NULL CHECK (projected_alias_count >= 0)
);

DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_catalog.pg_roles
        WHERE rolname = 'jellyrin_runtime'
    ) THEN
        REVOKE ALL PRIVILEGES ON TABLE jellyrin_derived_projection_versions
            FROM jellyrin_runtime;
        GRANT SELECT ON TABLE jellyrin_derived_projection_versions
            TO jellyrin_runtime;
    END IF;
END;
$migration$;
