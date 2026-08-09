#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "${script_dir}/.." && pwd)

web_dir="${repo_root}/web"
compose_env="${repo_root}/.env"
runtime_env="${repo_root}/ops/jellyrin.env"
provider_keyring=
failures=0

usage() {
    cat <<'EOF'
Usage: ops/deployment-preflight.sh [options]

Checks local Compose deployment inputs using file metadata only; it never reads env/key contents.

Options:
  --web-dir PATH                    Jellyfin Web output (default: ./web)
  --compose-env PATH                Compose .env file (default: ./.env)
  --runtime-env PATH                Jellyrin runtime env (default: ./ops/jellyrin.env)
  --require-provider-keyring PATH   Require root:10001 mode 0440 keyring for the Compose overlay
  -h, --help                        Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --web-dir | --compose-env | --runtime-env | --require-provider-keyring)
            if [ "$#" -lt 2 ] || [ -z "$2" ]; then
                echo "$1 requires a path" >&2
                exit 64
            fi
            option=$1
            value=$2
            shift 2
            case "${option}" in
                --web-dir) web_dir=${value} ;;
                --compose-env) compose_env=${value} ;;
                --runtime-env) runtime_env=${value} ;;
                --require-provider-keyring) provider_keyring=${value} ;;
            esac
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 64
            ;;
    esac
done

pass() {
    printf 'ok: %s\n' "$1"
}

fail() {
    printf 'error: %s\n' "$1" >&2
    failures=$((failures + 1))
}

check_private_env() {
    path=$1
    label=$2
    if [ ! -f "${path}" ] || [ -L "${path}" ]; then
        fail "${label} must be an existing regular, non-symlink file: ${path}"
        return
    fi
    mode=$(stat -c '%a' -- "${path}")
    owner=$(stat -c '%u' -- "${path}")
    current_uid=$(id -u)
    case "${mode}" in
        400 | 600) ;;
        *)
            fail "${label} must use mode 0400 or 0600 (found ${mode}): ${path}"
            return
            ;;
    esac
    if [ "${owner}" != "0" ] && [ "${owner}" != "${current_uid}" ]; then
        fail "${label} must be owned by root or the invoking user: ${path}"
        return
    fi
    pass "${label} metadata"
}

if [ ! -d "${web_dir}" ] || [ -L "${web_dir}" ]; then
    fail "web output must be an existing real directory: ${web_dir}"
else
    if [ ! -f "${web_dir}/index.html" ] || [ -L "${web_dir}/index.html" ]; then
        fail "web output is missing regular index.html: ${web_dir}/index.html"
    elif [ -n "$(find "${web_dir}" -type f ! -perm -004 -print -quit)" ]; then
        fail "every web file must be world-readable for the fixed container identity"
    elif [ -n "$(find "${web_dir}" -type d ! -perm -001 -print -quit)" ]; then
        fail "every web directory must be world-traversable for the fixed container identity"
    elif [ -n "$(find "${web_dir}" -type l -print -quit)" ]; then
        fail "web output must not contain symbolic links"
    elif ! find "${web_dir}" -type f ! -name index.html -print -quit | grep -q .; then
        fail "web output contains index.html but no assets"
    else
        pass "web index and assets"
    fi
fi

check_private_env "${compose_env}" "Compose environment"
check_private_env "${runtime_env}" "Jellyrin runtime environment"

if [ -n "${provider_keyring}" ]; then
    if [ ! -f "${provider_keyring}" ] || [ -L "${provider_keyring}" ]; then
        fail "provider keyring must be an existing regular, non-symlink file: ${provider_keyring}"
    else
        keyring_mode=$(stat -c '%a' -- "${provider_keyring}")
        keyring_owner=$(stat -c '%u' -- "${provider_keyring}")
        keyring_group=$(stat -c '%g' -- "${provider_keyring}")
        if [ "${keyring_owner}" != "0" ]; then
            fail "provider keyring must be owned by root"
        elif [ "${keyring_group}" != "10001" ]; then
            fail "provider keyring group must be the container GID 10001"
        elif [ "${keyring_mode}" != "440" ]; then
            fail "provider keyring must use mode 0440 so container GID 10001 can read it (found ${keyring_mode})"
        else
            pass "provider keyring metadata"
        fi
    fi
else
    pass "provider keyring not requested"
fi

if [ "${failures}" -ne 0 ]; then
    printf 'Deployment preflight failed with %s error(s).\n' "${failures}" >&2
    exit 1
fi

echo "Deployment preflight passed."
