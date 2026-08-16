#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "${script_dir}/.." && pwd)
compose_env=${JELLYRIN_COMPOSE_ENV:-${repo_root}/.env}
keyring=${JELLYRIN_PROVIDER_KEYRING_FILE:-${repo_root}/.runtime-secrets/provider-secret-keyring.json}

usage() {
    cat <<'EOF'
Usage: ops/bootstrap-compose.sh

Creates the local provider-secret keyring once and configures the Compose
overlay in .env. Existing key material is never replaced.

Environment overrides:
  JELLYRIN_COMPOSE_ENV             Compose environment file (default: ./.env)
  JELLYRIN_PROVIDER_KEYRING_FILE   Absolute keyring path
EOF
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi
if [ "$#" -ne 0 ]; then
    usage >&2
    exit 64
fi

case "${keyring}" in
    /*) ;;
    *) echo "provider keyring path must be absolute: ${keyring}" >&2; exit 64 ;;
esac

command -v openssl >/dev/null 2>&1 || {
    echo "openssl is required to generate the provider keyring" >&2
    exit 69
}

keyring_dir=$(dirname -- "${keyring}")
mkdir -p -- "${keyring_dir}"
chmod 700 -- "${keyring_dir}"

if [ -e "${keyring}" ] || [ -L "${keyring}" ]; then
    if [ ! -f "${keyring}" ] || [ -L "${keyring}" ]; then
        echo "provider keyring must be a regular, non-symlink file: ${keyring}" >&2
        exit 1
    fi
    mode=$(stat -c '%a' -- "${keyring}")
    owner=$(stat -c '%u' -- "${keyring}")
    group=$(stat -c '%g' -- "${keyring}")
    if [ "${mode}" != 440 ] || [ "${owner}" != 0 ] || [ "${group}" != 10001 ]; then
        echo "existing provider keyring must be root:10001 mode 0440 (found ${owner}:${group} ${mode})" >&2
        exit 1
    fi
    echo "provider keyring already exists; preserving it"
else
    key=$(openssl rand -base64 32 | tr -d '\n')
    key_id=$(date -u +%Y-%m)
    temporary=$(mktemp "${keyring}.tmp.XXXXXX")
    cleanup() { rm -f -- "${temporary}"; }
    trap cleanup EXIT HUP INT TERM
    umask 077
    cat >"${temporary}" <<EOF
{
  "active_key_id": "${key_id}",
  "keys": {
    "${key_id}": "${key}"
  }
}
EOF
    if [ "$(id -u)" -eq 0 ]; then
        install -o 0 -g 10001 -m 0440 -- "${temporary}" "${keyring}"
    else
        command -v sudo >/dev/null 2>&1 || {
            echo "sudo is required to install the keyring as root:10001" >&2
            exit 69
        }
        sudo install -o 0 -g 10001 -m 0440 -- "${temporary}" "${keyring}"
    fi
    rm -f -- "${temporary}"
    echo "created provider keyring: ${keyring}"
fi

compose_dir=$(dirname -- "${compose_env}")
mkdir -p -- "${compose_dir}"
if [ ! -e "${compose_env}" ]; then
    umask 077
    : >"${compose_env}"
    chmod 600 -- "${compose_env}"
fi

temporary_env=$(mktemp "${compose_env}.tmp.XXXXXX")
cleanup_env() { rm -f -- "${temporary_env}"; }
trap cleanup_env EXIT HUP INT TERM
awk '
    index($0, "COMPOSE_FILE=") == 1 { next }
    index($0, "JELLYRIN_PROVIDER_SECRET_KEYRING_HOST_FILE=") == 1 { next }
    { print }
' "${compose_env}" >"${temporary_env}"
printf '%s\n' \
    'COMPOSE_FILE=docker-compose.yml:docker-compose.provider-secrets.yml' \
    "JELLYRIN_PROVIDER_SECRET_KEYRING_HOST_FILE=${keyring}" >>"${temporary_env}"
chmod 600 -- "${temporary_env}"
mv -f -- "${temporary_env}" "${compose_env}"
trap - EXIT HUP INT TERM

echo "Compose provider-secret overlay configured in ${compose_env}"
echo "Next: docker compose --profile cache up -d --build"
