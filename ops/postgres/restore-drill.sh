#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 /absolute/path/to/backup-snapshot" >&2
    exit 2
fi

SNAPSHOT_DIR="$1"
: "${JELLYRIN_POSTGRES_RESTORE_AGE_IDENTITY:?set JELLYRIN_POSTGRES_RESTORE_AGE_IDENTITY to an age identity file}"
POSTGRES_SERVICE="${JELLYRIN_POSTGRES_RESTORE_SERVICE:-jellyrin-restore-admin}"

if [[ "${SNAPSHOT_DIR}" != /* || ! -d "${SNAPSHOT_DIR}" ]]; then
    echo "snapshot path must be an existing absolute directory" >&2
    exit 1
fi
if [[ ! -r "${JELLYRIN_POSTGRES_RESTORE_AGE_IDENTITY}" ]]; then
    echo "age identity file is not readable" >&2
    exit 1
fi
if [[ ! "${POSTGRES_SERVICE}" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo "invalid PostgreSQL service name" >&2
    exit 1
fi
for command in age createdb dropdb pg_restore psql sha256sum; do
    command -v "${command}" >/dev/null || {
        echo "required command is missing: ${command}" >&2
        exit 1
    }
done

(
    cd "${SNAPSHOT_DIR}"
    sha256sum --check --strict SHA256SUMS
)

WORK_DIR="$(mktemp -d)"
PLAIN_DUMP="${WORK_DIR}/jellyrin.dump"
RESTORE_DATABASE="jellyrin_restore_$(date -u +%Y%m%dT%H%M%SZ)_${BASHPID}"
DATABASE_CREATED=0

cleanup() {
    if [[ "${DATABASE_CREATED}" == "1" ]]; then
        PGSERVICE="${POSTGRES_SERVICE}" dropdb --if-exists --force "${RESTORE_DATABASE}" || true
    fi
    find "${WORK_DIR}" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT

age --decrypt \
    --identity "${JELLYRIN_POSTGRES_RESTORE_AGE_IDENTITY}" \
    --output "${PLAIN_DUMP}" \
    "${SNAPSHOT_DIR}/jellyrin.dump.age"
pg_restore --list "${PLAIN_DUMP}" >/dev/null

# The service must point to the same cluster and have CREATEDB. It is never used by Jellyrin.
PGSERVICE="${POSTGRES_SERVICE}" createdb \
    --maintenance-db=postgres \
    --template=template0 \
    --encoding=UTF8 \
    "${RESTORE_DATABASE}"
DATABASE_CREATED=1

# pg_stat_statements is present in supported Jellyrin clusters and requires an administrative
# role to install. Pre-creating it makes the dump's idempotent extension entry restorable while
# keeping application migrations and runtime roles unprivileged.
PGSERVICE="${POSTGRES_SERVICE}" psql \
    --no-psqlrc --set=ON_ERROR_STOP=1 \
    --dbname="${RESTORE_DATABASE}" \
    --command='CREATE EXTENSION IF NOT EXISTS pg_stat_statements'

PGSERVICE="${POSTGRES_SERVICE}" pg_restore \
    --exit-on-error \
    --no-owner \
    --no-privileges \
    --dbname="${RESTORE_DATABASE}" \
    "${PLAIN_DUMP}"

query_scalar() {
    PGSERVICE="${POSTGRES_SERVICE}" psql \
        --no-psqlrc --no-align --tuples-only --set=ON_ERROR_STOP=1 \
        --dbname="${RESTORE_DATABASE}" \
        --command="$1"
}

MIGRATION_FAILURES="$(query_scalar "SELECT count(*) FROM public._sqlx_migrations WHERE NOT success")"
INVALID_CONSTRAINTS="$(query_scalar "SELECT count(*) FROM pg_catalog.pg_constraint WHERE connamespace = 'public'::regnamespace AND NOT convalidated")"
APPLICATION_TABLES="$(query_scalar "SELECT count(*) FROM pg_catalog.pg_class WHERE relnamespace = 'public'::regnamespace AND relkind IN ('r','p')")"

if [[ "${MIGRATION_FAILURES}" != "0" || "${INVALID_CONSTRAINTS}" != "0" ]]; then
    echo "restore integrity checks failed: migrations=${MIGRATION_FAILURES}, invalid_constraints=${INVALID_CONSTRAINTS}" >&2
    exit 1
fi
if [[ ! "${APPLICATION_TABLES}" =~ ^[1-9][0-9]*$ ]]; then
    echo "restore contains no application tables" >&2
    exit 1
fi

echo "Restore drill passed: database=${RESTORE_DATABASE} tables=${APPLICATION_TABLES} failed_migrations=0 invalid_constraints=0"
