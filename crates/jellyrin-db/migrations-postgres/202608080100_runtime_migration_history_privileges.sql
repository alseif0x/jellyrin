-- SQLx migration history is part of runtime readiness, but it is owned and
-- mutated exclusively by the schema migrator. Managed PostgreSQL deployments
-- may use another runtime role, so keep this migration portable when the
-- Compose-specific role does not exist.
DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_catalog.pg_roles
        WHERE rolname = 'jellyrin_runtime'
    ) THEN
        REVOKE ALL PRIVILEGES ON TABLE _sqlx_migrations FROM jellyrin_runtime;
        GRANT SELECT ON TABLE _sqlx_migrations TO jellyrin_runtime;
    END IF;
END;
$migration$;
