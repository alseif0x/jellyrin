#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

: "${JELLYRIN_POSTGRES_BACKUP_DIR:?set JELLYRIN_POSTGRES_BACKUP_DIR to an absolute path outside the PostgreSQL data volume}"
: "${JELLYRIN_POSTGRES_BACKUP_AGE_RECIPIENTS:?set JELLYRIN_POSTGRES_BACKUP_AGE_RECIPIENTS to an age recipients file}"

POSTGRES_SERVICE="${JELLYRIN_POSTGRES_BACKUP_SERVICE:-jellyrin-backup}"
DAILY_RETENTION_DAYS="${JELLYRIN_POSTGRES_DAILY_RETENTION_DAYS:-14}"
WEEKLY_RETENTION_DAYS="${JELLYRIN_POSTGRES_WEEKLY_RETENTION_DAYS:-63}"
MONTHLY_RETENTION_DAYS="${JELLYRIN_POSTGRES_MONTHLY_RETENTION_DAYS:-400}"

if [[ "${JELLYRIN_POSTGRES_BACKUP_DIR}" != /* ]]; then
    echo "JELLYRIN_POSTGRES_BACKUP_DIR must be absolute" >&2
    exit 1
fi
if [[ ! -r "${JELLYRIN_POSTGRES_BACKUP_AGE_RECIPIENTS}" ]]; then
    echo "age recipients file is not readable" >&2
    exit 1
fi
if [[ ! "${POSTGRES_SERVICE}" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo "invalid PostgreSQL service name" >&2
    exit 1
fi
for value in "${DAILY_RETENTION_DAYS}" "${WEEKLY_RETENTION_DAYS}" "${MONTHLY_RETENTION_DAYS}"; do
    if [[ ! "${value}" =~ ^[1-9][0-9]*$ ]]; then
        echo "retention values must be positive whole days" >&2
        exit 1
    fi
done

for command in age flock pg_dump pg_restore psql sha256sum; do
    command -v "${command}" >/dev/null || {
        echo "required command is missing: ${command}" >&2
        exit 1
    }
done

mkdir -p -- "${JELLYRIN_POSTGRES_BACKUP_DIR}"/{daily,weekly,monthly,.staging}
exec 9>"${JELLYRIN_POSTGRES_BACKUP_DIR}/.backup.lock"
if ! flock -n 9; then
    echo "another PostgreSQL backup is already running" >&2
    exit 1
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WEEKDAY="$(date -u +%u)"
MONTHDAY="$(date -u +%d)"
STAGING_DIR="$(mktemp -d "${JELLYRIN_POSTGRES_BACKUP_DIR}/.staging/${STAMP}.XXXXXX")"
PLAIN_DUMP="${STAGING_DIR}/jellyrin.dump"
SNAPSHOT_DIR="${JELLYRIN_POSTGRES_BACKUP_DIR}/daily/${STAMP}"

cleanup() {
    find "${STAGING_DIR}" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT

if [[ -e "${SNAPSHOT_DIR}" ]]; then
    echo "backup destination already exists: ${SNAPSHOT_DIR}" >&2
    exit 1
fi

PGSERVICE="${POSTGRES_SERVICE}" pg_dump \
    --format=custom \
    --compress=9 \
    --no-owner \
    --no-privileges \
    --file="${PLAIN_DUMP}"

if [[ ! -s "${PLAIN_DUMP}" ]]; then
    echo "pg_dump produced an empty backup" >&2
    exit 1
fi
pg_restore --list "${PLAIN_DUMP}" >/dev/null

PGSERVICE="${POSTGRES_SERVICE}" psql \
    --no-psqlrc --no-align --tuples-only --set=ON_ERROR_STOP=1 \
    --command='SELECT current_database(), current_setting('"'"'server_version'"'"')' \
    >"${STAGING_DIR}/database.txt"
pg_dump --version >"${STAGING_DIR}/pg_dump-version.txt"

age --encrypt \
    --recipients-file "${JELLYRIN_POSTGRES_BACKUP_AGE_RECIPIENTS}" \
    --output "${STAGING_DIR}/jellyrin.dump.age" \
    "${PLAIN_DUMP}"
find "${PLAIN_DUMP}" -delete

(
    cd "${STAGING_DIR}"
    sha256sum jellyrin.dump.age database.txt pg_dump-version.txt >SHA256SUMS
    sha256sum --check --strict SHA256SUMS >/dev/null
)

# Moving the complete directory makes the dump, metadata and checksum visible as one snapshot.
mv -- "${STAGING_DIR}" "${SNAPSHOT_DIR}"
STAGING_DIR="${JELLYRIN_POSTGRES_BACKUP_DIR}/.staging/.completed-${STAMP}"

# Sunday and first-of-month copies use hard links: deleting a daily tier never invalidates them.
if [[ "${WEEKDAY}" == "7" ]]; then
    cp -al -- "${SNAPSHOT_DIR}" "${JELLYRIN_POSTGRES_BACKUP_DIR}/weekly/${STAMP}"
fi
if [[ "${MONTHDAY}" == "01" ]]; then
    cp -al -- "${SNAPSHOT_DIR}" "${JELLYRIN_POSTGRES_BACKUP_DIR}/monthly/${STAMP}"
fi

prune_tier() {
    local tier="$1"
    local retention_days="$2"
    find "${JELLYRIN_POSTGRES_BACKUP_DIR}/${tier}" \
        -mindepth 1 -maxdepth 1 -type d -mtime "+${retention_days}" \
        -exec sh -c 'for snapshot do find "$snapshot" -depth -delete; done' sh {} +
}

prune_tier daily "${DAILY_RETENTION_DAYS}"
prune_tier weekly "${WEEKLY_RETENTION_DAYS}"
prune_tier monthly "${MONTHLY_RETENTION_DAYS}"

echo "PostgreSQL backup completed: ${SNAPSHOT_DIR}"
