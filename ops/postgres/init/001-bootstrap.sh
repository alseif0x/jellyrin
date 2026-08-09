#!/usr/bin/env bash
set -Eeuo pipefail

: "${POSTGRES_DB:?POSTGRES_DB is required}"
: "${POSTGRES_USER:?POSTGRES_USER is required}"
: "${POSTGRES_MIGRATOR_PASSWORD:?POSTGRES_MIGRATOR_PASSWORD is required}"
: "${POSTGRES_RUNTIME_PASSWORD:?POSTGRES_RUNTIME_PASSWORD is required}"

if [[ "${POSTGRES_USER}" != "postgres" || "${POSTGRES_DB}" != "jellyrin" ]]; then
    echo "001-bootstrap.sh requires POSTGRES_USER=postgres and POSTGRES_DB=jellyrin" >&2
    exit 1
fi

# psql's literal interpolation (%L) keeps generated passwords out of SQL syntax. This script is
# only run by the official image while initializing a new data directory; existing volumes are
# deliberately never mutated behind an operator's back.
psql \
    --username "${POSTGRES_USER}" \
    --dbname "${POSTGRES_DB}" \
    --set=ON_ERROR_STOP=1 \
    --set=migrator_password="${POSTGRES_MIGRATOR_PASSWORD}" \
    --set=runtime_password="${POSTGRES_RUNTIME_PASSWORD}" <<'EOSQL'
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;

SELECT format(
    'CREATE ROLE jellyrin_migrator LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
    :'migrator_password'
)
WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_migrator')
\gexec

SELECT format(
    'ALTER ROLE jellyrin_migrator WITH LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
    :'migrator_password'
)
\gexec

SELECT format(
    'CREATE ROLE jellyrin_runtime LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
    :'runtime_password'
)
WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime')
\gexec

SELECT format(
    'ALTER ROLE jellyrin_runtime WITH LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
    :'runtime_password'
)
\gexec

ALTER DATABASE jellyrin OWNER TO jellyrin_migrator;
ALTER SCHEMA public OWNER TO jellyrin_migrator;

-- pg_trgm is a trusted extension. Creating it as the database owner lets future migration jobs
-- maintain it without handing the migrator superuser privileges.
SET ROLE jellyrin_migrator;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
RESET ROLE;

REVOKE ALL ON DATABASE jellyrin FROM PUBLIC;
REVOKE ALL ON SCHEMA public FROM PUBLIC;

GRANT CONNECT, TEMPORARY ON DATABASE jellyrin TO jellyrin_runtime;
GRANT USAGE ON SCHEMA public TO jellyrin_runtime;

-- Every persistent object is created by the one-shot migration job. Runtime receives the DML
-- permissions application tables need, plus identity-sequence access. The schema migration
-- explicitly reduces _sqlx_migrations to SELECT-only for runtime readiness checks.
ALTER DEFAULT PRIVILEGES FOR ROLE jellyrin_migrator IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO jellyrin_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE jellyrin_migrator IN SCHEMA public
    GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO jellyrin_runtime;

ALTER ROLE jellyrin_migrator SET search_path = public, pg_catalog;
ALTER ROLE jellyrin_runtime SET search_path = public, pg_catalog;
EOSQL
