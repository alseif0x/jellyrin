#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "$#" -ne 1 || -z "${1}" ]]; then
    echo "usage: $0 <jellyrin-image>" >&2
    exit 64
fi

image_ref="$1"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
postgres_image="$(sed -n 's/^POSTGRES_IMAGE=//p' "${repo_root}/ops/supply-chain.lock.env" | head -n 1)"
if [[ -z "${postgres_image}" || "${postgres_image}" != *@sha256:* ]]; then
    echo "POSTGRES_IMAGE is missing or not digest-pinned" >&2
    exit 1
fi
docker image inspect "${image_ref}" >/dev/null

smoke_id="jellyrin-runtime-smoke-$$"
network_name="${smoke_id}-network"
postgres_name="${smoke_id}-postgres"
server_name="${smoke_id}-server"
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/jellyrin-runtime-smoke.XXXXXX")"
admin_password="runtime-smoke-admin"
migrator_password="runtime-smoke-migrator"
runtime_password="runtime-smoke-runtime"

cleanup() {
    docker stop --time 10 "${server_name}" >/dev/null 2>&1 || true
    docker stop --time 10 "${postgres_name}" >/dev/null 2>&1 || true
    docker container rm --volumes "${server_name}" >/dev/null 2>&1 || true
    docker container rm --volumes "${postgres_name}" >/dev/null 2>&1 || true
    docker network rm "${network_name}" >/dev/null 2>&1 || true
    rm -rf -- "${smoke_root:?}"
}
trap cleanup EXIT

# Use a traversable fixture without weakening checkout permissions on the public init directory.
install -d -m 0755 "${smoke_root}/init" "${smoke_root}/web"
install -m 0755 "${repo_root}/ops/postgres/init/001-bootstrap.sh" \
    "${smoke_root}/init/001-bootstrap.sh"
printf '%s\n' '<!doctype html><title>Jellyrin runtime smoke</title>' \
    > "${smoke_root}/web/index.html"
chmod 0644 "${smoke_root}/web/index.html"

docker network create "${network_name}" >/dev/null
docker run --detach --name "${postgres_name}" --network "${network_name}" \
    --env POSTGRES_DB=jellyrin \
    --env POSTGRES_USER=postgres \
    --env "POSTGRES_PASSWORD=${admin_password}" \
    --env "POSTGRES_MIGRATOR_PASSWORD=${migrator_password}" \
    --env "POSTGRES_RUNTIME_PASSWORD=${runtime_password}" \
    --env 'POSTGRES_INITDB_ARGS=--data-checksums --auth-host=scram-sha-256 --auth-local=trust' \
    --volume "${smoke_root}/init:/docker-entrypoint-initdb.d:ro" \
    "${postgres_image}" -c shared_preload_libraries=pg_stat_statements >/dev/null

for attempt in $(seq 1 90); do
    if docker exec --env "PGPASSWORD=${runtime_password}" "${postgres_name}" \
        psql --host 127.0.0.1 --username jellyrin_runtime --dbname jellyrin \
        --tuples-only --no-align --command 'SELECT 1' 2>/dev/null | grep -qx '1'; then
        break
    fi
    if [[ "${attempt}" -eq 90 ]]; then
        docker logs "${postgres_name}" >&2
        echo "PostgreSQL runtime role did not become ready" >&2
        exit 1
    fi
    sleep 1
done

docker run --rm --network "${network_name}" --read-only \
    --tmpfs /tmp:size=32m,mode=1777 --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --entrypoint /usr/local/bin/jellyrin-migrate \
    --env "DATABASE_URL=postgresql://jellyrin_migrator:${migrator_password}@${postgres_name}:5432/jellyrin?sslmode=disable" \
    "${image_ref}" schema > "${smoke_root}/migration-report.json"
jq --exit-status \
    '.status == "schema_migrated" and .schema_version_after != null and .applied_migrations > 0' \
    "${smoke_root}/migration-report.json" >/dev/null

docker run --detach --name "${server_name}" --network "${network_name}" \
    --publish 127.0.0.1::8096 --read-only --init \
    --tmpfs /tmp:size=64m,mode=1777 --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --env JELLYRIN_HOST=0.0.0.0 \
    --env JELLYRIN_PORT=8096 \
    --env JELLYRIN_DB_DRIVER=postgresql \
    --env "DATABASE_URL=postgresql://jellyrin_runtime:${runtime_password}@${postgres_name}:5432/jellyrin?sslmode=disable" \
    --env JELLYRIN_FFMPEG_MODE=remux-only \
    --volume "${smoke_root}/web:/srv/jellyrin/web:ro" \
    "${image_ref}" >/dev/null

published_port="$(docker port "${server_name}" 8096/tcp | sed -n '1s/.*://p')"
if [[ ! "${published_port}" =~ ^[0-9]+$ ]]; then
    echo "could not resolve the runtime smoke host port" >&2
    exit 1
fi
for attempt in $(seq 1 90); do
    if curl --fail --silent --show-error \
        "http://127.0.0.1:${published_port}/readyz" \
        > "${smoke_root}/ready.json" 2>/dev/null; then
        break
    fi
    if [[ "${attempt}" -eq 90 ]]; then
        docker logs "${server_name}" >&2
        echo "Jellyrin did not become ready" >&2
        exit 1
    fi
    sleep 1
done

docker exec "${server_name}" /usr/local/bin/jellyrin-server --healthcheck
curl --fail --silent --show-error "http://127.0.0.1:${published_port}/healthz" \
    > "${smoke_root}/health.json"
jq --exit-status '.Status == "Ready"' "${smoke_root}/ready.json" >/dev/null
jq --exit-status '.Status == "Healthy"' "${smoke_root}/health.json" >/dev/null

runtime_user="$(docker inspect "${server_name}" --format '{{.Config.User}}')"
restart_count="$(docker inspect "${server_name}" --format '{{.RestartCount}}')"
if [[ "${runtime_user}" != "10001:10001" || "${restart_count}" != "0" ]]; then
    echo "unexpected runtime identity/restarts: user=${runtime_user} restarts=${restart_count}" >&2
    exit 1
fi

docker stop --time 45 "${server_name}" >/dev/null
server_state="$(docker inspect "${server_name}" --format '{{.State.Status}}')"
server_exit_code="$(docker inspect "${server_name}" --format '{{.State.ExitCode}}')"
if [[ "${server_state}" != "exited" || "${server_exit_code}" != "0" ]]; then
    docker logs "${server_name}" >&2
    echo "runtime did not shut down cleanly: state=${server_state} exit=${server_exit_code}" >&2
    exit 1
fi

echo "verified distroless migrator/server runtime smoke: ${image_ref}"
