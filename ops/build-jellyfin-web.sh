#!/bin/sh
set -eu
umask 022

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "${script_dir}/.." && pwd)
lock_file="${script_dir}/supply-chain.lock.env"
output_dir=${1:-"${repo_root}/web"}

if [ ! -f "${lock_file}" ] || [ -L "${lock_file}" ]; then
    echo "missing regular supply-chain lock: ${lock_file}" >&2
    exit 66
fi

# shellcheck disable=SC1090 -- qa/supply-chain.js validates this repository-owned public lock.
. "${lock_file}"

: "${JELLYFIN_WEB_VERSION:?missing JELLYFIN_WEB_VERSION in supply-chain lock}"
: "${JELLYFIN_WEB_COMMIT:?missing JELLYFIN_WEB_COMMIT in supply-chain lock}"
: "${JELLYFIN_WEB_TARBALL_SHA256:?missing JELLYFIN_WEB_TARBALL_SHA256 in supply-chain lock}"
: "${JELLYFIN_WEB_SWIPER_VERSION:?missing JELLYFIN_WEB_SWIPER_VERSION in supply-chain lock}"
: "${JELLYFIN_WEB_SWIPER_PATCH_COMMIT:?missing JELLYFIN_WEB_SWIPER_PATCH_COMMIT in supply-chain lock}"
: "${JELLYFIN_WEB_SWIPER_PATCH_SHA256:?missing JELLYFIN_WEB_SWIPER_PATCH_SHA256 in supply-chain lock}"

case "${JELLYFIN_WEB_VERSION}" in
    *[!0-9.]* | .* | *.)
        echo "invalid Jellyfin Web version in supply-chain lock" >&2
        exit 65
        ;;
esac
printf '%s\n' "${JELLYFIN_WEB_COMMIT}" | grep -Eq '^[a-f0-9]{40}$' || {
    echo "Jellyfin Web commit must be exactly 40 lowercase hexadecimal characters" >&2
    exit 65
}
printf '%s\n' "${JELLYFIN_WEB_TARBALL_SHA256}" | grep -Eq '^[a-f0-9]{64}$' || {
    echo "Jellyfin Web tarball checksum must be exactly 64 lowercase hexadecimal characters" >&2
    exit 65
}
case "${JELLYFIN_WEB_SWIPER_VERSION}" in
    *[!0-9.]* | .* | *.)
        echo "invalid hardened Swiper version in supply-chain lock" >&2
        exit 65
        ;;
esac
printf '%s\n' "${JELLYFIN_WEB_SWIPER_PATCH_COMMIT}" | grep -Eq '^[a-f0-9]{40}$' || {
    echo "Swiper patch commit must be exactly 40 lowercase hexadecimal characters" >&2
    exit 65
}
printf '%s\n' "${JELLYFIN_WEB_SWIPER_PATCH_SHA256}" | grep -Eq '^[a-f0-9]{64}$' || {
    echo "Swiper patch checksum must be exactly 64 lowercase hexadecimal characters" >&2
    exit 65
}

for command_name in awk cp curl find grep mktemp mv node npm patch sed sha256sum sort tar tr; do
    command -v "${command_name}" >/dev/null 2>&1 || {
        echo "${command_name} is required" >&2
        exit 69
    }
done

if [ -e "${output_dir}" ] || [ -L "${output_dir}" ]; then
    echo "refusing to overwrite existing Jellyfin Web output: ${output_dir}" >&2
    exit 73
fi

output_parent=$(dirname -- "${output_dir}")
output_name=$(basename -- "${output_dir}")
case "${output_name}" in
    '' | . | ..)
        echo "invalid Jellyfin Web output directory" >&2
        exit 64
        ;;
esac
if [ ! -d "${output_parent}" ] || [ -L "${output_parent}" ]; then
    echo "Jellyfin Web output parent must be an existing real directory: ${output_parent}" >&2
    exit 73
fi

work_dir=$(mktemp -d "${output_parent}/.jellyfin-web-build.XXXXXX")
cleanup() {
    if [ -n "${work_dir:-}" ] && [ -d "${work_dir}" ]; then
        rm -rf -- "${work_dir}"
    fi
}
trap cleanup EXIT HUP INT TERM

archive="${work_dir}/jellyfin-web.tar.gz"
swiper_patch="${work_dir}/jellyfin-web-swiper-security.patch"
source_dir="${work_dir}/source"
publish_dir="${work_dir}/publish"
archive_url="https://github.com/jellyfin/jellyfin-web/archive/${JELLYFIN_WEB_COMMIT}.tar.gz"
swiper_patch_url="https://github.com/jellyfin/jellyfin-web/commit/${JELLYFIN_WEB_SWIPER_PATCH_COMMIT}.patch"

mkdir -m 0755 "${source_dir}"
curl --fail --silent --show-error --location --retry 3 \
    --proto '=https' --tlsv1.2 \
    --output "${archive}" "${archive_url}"
printf '%s  %s\n' "${JELLYFIN_WEB_TARBALL_SHA256}" "${archive}" | sha256sum --check --strict -
tar -xzf "${archive}" --strip-components=1 --directory "${source_dir}"

actual_version=$(cd "${source_dir}" && npm pkg get version | tr -d '"[:space:]')
if [ "${actual_version}" != "${JELLYFIN_WEB_VERSION}" ]; then
    echo "Jellyfin Web source version ${actual_version} does not match lock ${JELLYFIN_WEB_VERSION}" >&2
    exit 65
fi

curl --fail --silent --show-error --location --retry 3 \
    --proto '=https' --tlsv1.2 \
    --output "${swiper_patch}" "${swiper_patch_url}"
printf '%s  %s\n' "${JELLYFIN_WEB_SWIPER_PATCH_SHA256}" "${swiper_patch}" | \
    sha256sum --check --strict -

patch_paths=$(awk '$1 == "diff" && $2 == "--git" { print substr($3, 3); print substr($4, 3) }' \
    "${swiper_patch}" | sort -u)
expected_patch_paths=$(printf '%s\n' package-lock.json package.json)
if [ "${patch_paths}" != "${expected_patch_paths}" ]; then
    echo "Swiper security patch must modify only package.json and package-lock.json" >&2
    exit 65
fi
if ! grep -Fqx \
    "From ${JELLYFIN_WEB_SWIPER_PATCH_COMMIT} Mon Sep 17 00:00:00 2001" "${swiper_patch}"; then
    echo "Swiper security patch does not identify the locked official commit" >&2
    exit 65
fi
patch --batch --forward --strip=1 --directory "${source_dir}" < "${swiper_patch}"

SOURCE_DIR="${source_dir}" EXPECTED_SWIPER_VERSION="${JELLYFIN_WEB_SWIPER_VERSION}" node <<'NODE'
const fs = require('node:fs');
const path = require('node:path');

const sourceDir = process.env.SOURCE_DIR;
const expected = process.env.EXPECTED_SWIPER_VERSION;
const manifest = JSON.parse(fs.readFileSync(path.join(sourceDir, 'package.json'), 'utf8'));
const lock = JSON.parse(fs.readFileSync(path.join(sourceDir, 'package-lock.json'), 'utf8'));
const lockedRoot = lock.packages?.['']?.dependencies?.swiper;
const lockedPackage = lock.packages?.['node_modules/swiper']?.version;

if (manifest.dependencies?.swiper !== expected || lockedRoot !== expected || lockedPackage !== expected) {
  throw new Error(`Swiper patch did not lock every manifest to ${expected}`);
}
NODE

(
    cd "${source_dir}"
    npm ci --omit=optional
    for forbidden_optional in \
        node_modules/pdfjs-dist/node_modules/canvas \
        node_modules/@mapbox/node-pre-gyp \
        node_modules/tar
    do
        if [ -e "${forbidden_optional}" ] || [ -L "${forbidden_optional}" ]; then
            echo "optional Node-only dependency must not be installed: ${forbidden_optional}" >&2
            exit 70
        fi
    done
    npm run build:production
)

if [ ! -f "${source_dir}/dist/index.html" ] || [ -L "${source_dir}/dist/index.html" ]; then
    echo "Jellyfin Web build did not produce a regular dist/index.html" >&2
    exit 70
fi
if ! find "${source_dir}/dist" -type f ! -name index.html -print -quit | grep -q .; then
    echo "Jellyfin Web build did not produce any assets" >&2
    exit 70
fi

# Jellyfin Web only consumes complete External subtitle documents. Jellyrin's adapter preserves
# that contract while fetching remote embedded text tracks in bounded windows around currentTime.
subtitle_adapter="${script_dir}/web/jellyrin-segmented-subtitles.js"
if [ ! -f "${subtitle_adapter}" ] || [ -L "${subtitle_adapter}" ]; then
    echo "missing regular Jellyrin segmented subtitle adapter: ${subtitle_adapter}" >&2
    exit 66
fi
cp -- "${subtitle_adapter}" "${source_dir}/dist/jellyrin-segmented-subtitles.js"
WEB_INDEX="${source_dir}/dist/index.html" node <<'NODE'
const fs = require('node:fs');

const indexPath = process.env.WEB_INDEX;
const source = fs.readFileSync(indexPath, 'utf8');
const marker = '<script defer="defer" src="runtime.bundle.js';
const offset = source.indexOf(marker);
if (offset < 0) {
  throw new Error('Jellyfin Web index does not contain the runtime bundle marker');
}
const adapter = '<script defer="defer" src="jellyrin-segmented-subtitles.js?v=3"></script>';
if (source.includes(adapter)) {
  throw new Error('Jellyrin segmented subtitle adapter was already injected');
}
fs.writeFileSync(indexPath, source.slice(0, offset) + adapter + source.slice(offset));
NODE

mv -- "${source_dir}/dist" "${publish_dir}"
if [ -e "${output_dir}" ] || [ -L "${output_dir}" ]; then
    echo "refusing to overwrite Jellyfin Web output created during build: ${output_dir}" >&2
    exit 73
fi
mv -T -- "${publish_dir}" "${output_dir}"
trap - EXIT HUP INT TERM
cleanup

echo "Published hardened Jellyfin Web ${JELLYFIN_WEB_VERSION} (${JELLYFIN_WEB_COMMIT}, Swiper ${JELLYFIN_WEB_SWIPER_VERSION}) to ${output_dir}"
