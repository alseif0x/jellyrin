#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
lock_file="${script_dir}/supply-chain.lock.env"
image_ref="${1:-jellyrin:local}"
output_dir="${2:-${repo_root}/supply-chain-artifacts}"

if [[ ! -f "${lock_file}" ]]; then
    echo "missing supply-chain lock: ${lock_file}" >&2
    exit 1
fi
if [[ -e "${output_dir}" ]]; then
    echo "refusing to overwrite existing SBOM output: ${output_dir}" >&2
    exit 1
fi

set -a
# shellcheck disable=SC1090 -- the repository-owned lock is validated by qa/supply-chain.js.
source "${lock_file}"
set +a

for required_command in curl docker jq node sha256sum tar; do
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        echo "required SBOM command is unavailable: ${required_command}" >&2
        exit 1
    fi
done

# Do not allow standalone use to bypass lock, CI-contract or exception-policy validation.
node "${repo_root}/qa/supply-chain.js" >/dev/null

case "$(uname -m)" in
    x86_64)
        syft_arch="amd64"
        syft_sha256="${SYFT_LINUX_AMD64_SHA256}"
        ;;
    aarch64 | arm64)
        syft_arch="arm64"
        syft_sha256="${SYFT_LINUX_ARM64_SHA256}"
        ;;
    *)
        echo "unsupported SBOM host architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

temp_root="$(mktemp -d)"
container_id=""
cleanup() {
    if [[ -n "${container_id}" ]]; then
        docker rm -f "${container_id}" >/dev/null 2>&1 || true
    fi
    rm -rf -- "${temp_root}"
}
trap cleanup EXIT

syft_archive="${temp_root}/syft.tar.gz"
syft_root="${temp_root}/syft"
mkdir -p "${syft_root}" "${output_dir}"
curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail --silent --show-error --location \
    "https://github.com/anchore/syft/releases/download/v${SYFT_VERSION}/syft_${SYFT_VERSION}_linux_${syft_arch}.tar.gz" \
    --output "${syft_archive}"
printf '%s  %s\n' "${syft_sha256}" "${syft_archive}" | sha256sum --check --strict
tar -xzf "${syft_archive}" -C "${syft_root}" syft
"${syft_root}/syft" version > "${output_dir}/syft-version.txt"

installed_ffmpeg_version="$(
    docker run --rm --entrypoint dpkg-query "${image_ref}" -W -f='${Version}' ffmpeg
)"
if [[ "${installed_ffmpeg_version}" != "${FFMPEG_PACKAGE_VERSION}" ]]; then
    echo "FFmpeg package drift: expected ${FFMPEG_PACKAGE_VERSION}, found ${installed_ffmpeg_version}" >&2
    exit 1
fi
printf '%s\n' "${installed_ffmpeg_version}" > "${output_dir}/ffmpeg-package-version.txt"
docker run --rm --entrypoint ffmpeg "${image_ref}" -version > "${output_dir}/ffmpeg-version.txt" 2>&1
docker run --rm --entrypoint ffprobe "${image_ref}" -version > "${output_dir}/ffprobe-version.txt" 2>&1
grep -Fq "ffmpeg version ${FFMPEG_UPSTREAM_VERSION}" "${output_dir}/ffmpeg-version.txt"
grep -Fq "ffprobe version ${FFMPEG_UPSTREAM_VERSION}" "${output_dir}/ffprobe-version.txt"
docker run --rm --entrypoint dpkg-query "${image_ref}" \
    -W -f='${binary:Package}\t${Version}\n' \
    | LC_ALL=C sort > "${output_dir}/runtime-packages.txt"

image_revision="$(
    docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "${image_ref}"
)"
if [[ ! "${image_revision}" =~ ^[0-9a-f]{40,64}$ ]]; then
    echo "release image has no immutable VCS revision label: ${image_revision:-<missing>}" >&2
    exit 1
fi
printf '%s\n' "${image_revision}" > "${output_dir}/source-revision.txt"

"${syft_root}/syft" scan "docker:${image_ref}" \
    --output "spdx-json=${output_dir}/jellyrin-image.spdx.json"
"${syft_root}/syft" scan "docker:${image_ref}" \
    --output "cyclonedx-json=${output_dir}/jellyrin-image.cyclonedx.json"

# Scan Cargo manifests separately so statically linked Rust dependencies are represented even when
# an image scanner cannot recover their package metadata from optimized native binaries.
source_root="${temp_root}/source-manifests"
mkdir -p "${source_root}/crates"
cp "${repo_root}/Cargo.toml" "${repo_root}/Cargo.lock" "${source_root}/"
while IFS= read -r -d '' manifest; do
    relative_manifest="${manifest#${repo_root}/}"
    mkdir -p "${source_root}/$(dirname -- "${relative_manifest}")"
    cp "${manifest}" "${source_root}/${relative_manifest}"
done < <(find "${repo_root}/crates" -name Cargo.toml -type f -print0)
"${syft_root}/syft" scan "dir:${source_root}" \
    --output "spdx-json=${output_dir}/jellyrin-source.spdx.json"
"${syft_root}/syft" scan "dir:${source_root}" \
    --output "cyclonedx-json=${output_dir}/jellyrin-source.cyclonedx.json"

jq -e '.spdxVersion == "SPDX-2.3" and (.packages | length > 0)' \
    "${output_dir}/jellyrin-image.spdx.json" >/dev/null
jq -e '.bomFormat == "CycloneDX" and (.components | length > 0)' \
    "${output_dir}/jellyrin-image.cyclonedx.json" >/dev/null
jq -e '[.packages[] | select(.name == "ffmpeg")] | length > 0' \
    "${output_dir}/jellyrin-image.spdx.json" >/dev/null
jq -e '[.components[] | select(.name == "ffmpeg")] | length > 0' \
    "${output_dir}/jellyrin-image.cyclonedx.json" >/dev/null
jq -e '.spdxVersion == "SPDX-2.3" and (.packages | length > 0)' \
    "${output_dir}/jellyrin-source.spdx.json" >/dev/null
jq -e '.bomFormat == "CycloneDX" and (.components | length > 0)' \
    "${output_dir}/jellyrin-source.cyclonedx.json" >/dev/null

container_id="$(docker create "${image_ref}")"
docker cp "${container_id}:/usr/local/bin/jellyrin-server" "${output_dir}/jellyrin-server"
docker cp "${container_id}:/usr/local/bin/jellyrin-migrate" "${output_dir}/jellyrin-migrate"
docker rm "${container_id}" >/dev/null
container_id=""

docker image inspect "${image_ref}" > "${output_dir}/image-inspect.json"
docker image inspect --format '{{.Id}}' "${image_ref}" > "${output_dir}/image-id.txt"
cp "${lock_file}" "${output_dir}/supply-chain.lock.env"

(
    cd "${output_dir}"
    sha256sum jellyrin-server jellyrin-migrate > binaries.sha256
    sha256sum \
        binaries.sha256 \
        ffmpeg-package-version.txt \
        ffmpeg-version.txt \
        ffprobe-version.txt \
        image-id.txt \
        image-inspect.json \
        jellyrin-migrate \
        jellyrin-server \
        jellyrin-image.cyclonedx.json \
        jellyrin-image.spdx.json \
        jellyrin-source.cyclonedx.json \
        jellyrin-source.spdx.json \
        runtime-packages.txt \
        source-revision.txt \
        supply-chain.lock.env \
        syft-version.txt \
        > SHA256SUMS
    sha256sum --check --strict SHA256SUMS
    sha256sum --check --strict binaries.sha256
)

printf 'verified SBOM bundle: %s\n' "${output_dir}"
