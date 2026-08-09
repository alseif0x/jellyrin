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

image_ffmpeg_source_sha256="$(
    docker image inspect --format '{{index .Config.Labels "io.jellyrin.ffmpeg-source-sha256"}}' "${image_ref}"
)"
if [[ "${image_ffmpeg_source_sha256}" != "${FFMPEG_SOURCE_SHA256}" ]]; then
    echo "FFmpeg source digest drift" >&2
    exit 1
fi
printf '%s\n' "${image_ffmpeg_source_sha256}" > "${output_dir}/ffmpeg-source-sha256.txt"
docker run --rm --entrypoint ffmpeg "${image_ref}" -version > "${output_dir}/ffmpeg-version.txt" 2>&1
docker run --rm --entrypoint ffprobe "${image_ref}" -version > "${output_dir}/ffprobe-version.txt" 2>&1
grep -Fq "ffmpeg version ${FFMPEG_UPSTREAM_VERSION}" "${output_dir}/ffmpeg-version.txt"
grep -Fq "ffprobe version ${FFMPEG_UPSTREAM_VERSION}" "${output_dir}/ffprobe-version.txt"
docker run --rm --entrypoint ffmpeg "${image_ref}" -hide_banner -buildconf \
    > "${output_dir}/ffmpeg-build-configuration.txt" 2>&1
for listing in protocols demuxers muxers bsfs encoders decoders; do
    docker run --rm --entrypoint ffmpeg "${image_ref}" -hide_banner "-${listing}" \
        > "${output_dir}/ffmpeg-${listing}.txt" 2>&1
done
grep -o -- '--enable-parser=[^[:space:]]*' "${output_dir}/ffmpeg-build-configuration.txt" \
    | sed 's/^--enable-parser=//' \
    | tr -d "'\"" \
    | tr ',' '\n' \
    | LC_ALL=C sort -u \
    > "${output_dir}/ffmpeg-parsers.txt"
[[ -s "${output_dir}/ffmpeg-parsers.txt" ]]
expected_parsers="$(
    printf '%s\n' aac aac_latm ac3 av1 dca h264 hevc mpeg4video mpegaudio mpegvideo opus vorbis \
        | LC_ALL=C sort
)"
if [[ "$(<"${output_dir}/ffmpeg-parsers.txt")" != "${expected_parsers}" ]]; then
    echo "remux-only image parser allowlist drift" >&2
    exit 1
fi
if awk '$1 ~ /^[VAS]/ && length($1) == 6 && $2 != "=" { found=1 } END { exit !found }' \
    "${output_dir}/ffmpeg-encoders.txt"; then
    echo "remux-only image unexpectedly contains an encoder" >&2
    exit 1
fi
actual_decoders="$(
    awk '$1 ~ /^[VAS]/ && length($1) == 6 && $2 != "=" { print $2 }' \
        "${output_dir}/ffmpeg-decoders.txt" | LC_ALL=C sort -u
)"
if [[ "${actual_decoders}" != "aac" ]]; then
    echo "remux-only image decoder allowlist drift: ${actual_decoders:-<none>}" >&2
    exit 1
fi
if docker run --rm --entrypoint dpkg-query "${image_ref}" -W ffmpeg >/dev/null 2>&1; then
    echo "general-purpose Debian ffmpeg package is present in the remux-only image" >&2
    exit 1
fi
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

# Syft cannot infer an upstream package identity from a stripped native binary. Record the
# checksum-pinned FFmpeg source as an explicit SBOM component instead of pretending it is a dpkg.
ffmpeg_source_url="https://ffmpeg.org/releases/ffmpeg-${FFMPEG_UPSTREAM_VERSION}.tar.xz"
sbom_created_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
jq -n \
    --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    --arg sha256 "${FFMPEG_SOURCE_SHA256}" \
    --arg source_url "${ffmpeg_source_url}" \
    --arg created_at "${sbom_created_at}" \
    '{
      spdxVersion: "SPDX-2.3",
      dataLicense: "CC0-1.0",
      SPDXID: "SPDXRef-DOCUMENT",
      name: ("jellyrin-ffmpeg-source-" + $version),
      documentNamespace: ("https://jellyrin.invalid/spdx/ffmpeg/" + $version + "/" + $sha256),
      creationInfo: {created: $created_at, creators: ["Tool: jellyrin-generate-sbom"]},
      packages: [{
        name: "ffmpeg",
        SPDXID: "SPDXRef-Package-FFmpeg",
        versionInfo: $version,
        downloadLocation: $source_url,
        filesAnalyzed: false,
        checksums: [{algorithm: "SHA256", checksumValue: $sha256}],
        licenseConcluded: "NOASSERTION",
        licenseDeclared: "NOASSERTION",
        copyrightText: "NOASSERTION",
        externalRefs: [{
          referenceCategory: "PACKAGE-MANAGER",
          referenceType: "purl",
          referenceLocator: ("pkg:generic/ffmpeg@" + $version)
        }, {
          referenceCategory: "SECURITY",
          referenceType: "cpe23Type",
          referenceLocator: ("cpe:2.3:a:ffmpeg:ffmpeg:" + $version + ":*:*:*:*:*:*:*")
        }]
      }],
      relationships: [{
        spdxElementId: "SPDXRef-DOCUMENT",
        relationshipType: "DESCRIBES",
        relatedSpdxElement: "SPDXRef-Package-FFmpeg"
      }]
    }' > "${output_dir}/ffmpeg-source.spdx.json"
jq -n \
    --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    --arg sha256 "${FFMPEG_SOURCE_SHA256}" \
    --arg source_url "${ffmpeg_source_url}" \
    --arg created_at "${sbom_created_at}" \
    '{
      bomFormat: "CycloneDX",
      specVersion: "1.6",
      serialNumber: ("urn:uuid:" + ($sha256[0:8]) + "-" + ($sha256[8:12]) + "-4" + ($sha256[13:16]) + "-8" + ($sha256[17:20]) + "-" + ($sha256[20:32])),
      version: 1,
      metadata: {timestamp: $created_at, tools: {components: [{type: "application", name: "jellyrin-generate-sbom"}]}},
      components: [{
        type: "application",
        name: "ffmpeg",
        version: $version,
        purl: ("pkg:generic/ffmpeg@" + $version),
        cpe: ("cpe:2.3:a:ffmpeg:ffmpeg:" + $version + ":*:*:*:*:*:*:*"),
        hashes: [{alg: "SHA-256", content: $sha256}],
        externalReferences: [{type: "distribution", url: $source_url}]
      }]
    }' > "${output_dir}/ffmpeg-source.cyclonedx.json"

# Make the statically linked FFmpeg component part of both image SBOMs. Retain the standalone
# source documents as provenance evidence and as the explicit input to the vulnerability scanner.
jq --slurpfile ffmpeg "${output_dir}/ffmpeg-source.spdx.json" '
    .packages = ((.packages // []) | map(select((.name | ascii_downcase) != "ffmpeg")))
      + $ffmpeg[0].packages
    | .relationships = (.relationships // []) + [{
        spdxElementId: "SPDXRef-DOCUMENT",
        relationshipType: "DESCRIBES",
        relatedSpdxElement: "SPDXRef-Package-FFmpeg"
      }]
  ' "${output_dir}/jellyrin-image.spdx.json" > "${temp_root}/jellyrin-image.spdx.json"
mv "${temp_root}/jellyrin-image.spdx.json" "${output_dir}/jellyrin-image.spdx.json"
jq --slurpfile ffmpeg "${output_dir}/ffmpeg-source.cyclonedx.json" '
    .components = ((.components // [])
      | map(select((.name | ascii_downcase) != "ffmpeg")))
      + $ffmpeg[0].components
  ' "${output_dir}/jellyrin-image.cyclonedx.json" \
    > "${temp_root}/jellyrin-image.cyclonedx.json"
mv "${temp_root}/jellyrin-image.cyclonedx.json" "${output_dir}/jellyrin-image.cyclonedx.json"

jq -e '.spdxVersion == "SPDX-2.3" and (.packages | length > 0)' \
    "${output_dir}/jellyrin-image.spdx.json" >/dev/null
jq -e '.bomFormat == "CycloneDX" and (.components | length > 0)' \
    "${output_dir}/jellyrin-image.cyclonedx.json" >/dev/null
jq -e --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    '[.packages[] | select(.name == "ffmpeg" and .versionInfo == $version)] | length == 1' \
    "${output_dir}/ffmpeg-source.spdx.json" >/dev/null
jq -e --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    '[.components[] | select(.name == "ffmpeg" and .version == $version)] | length == 1' \
    "${output_dir}/ffmpeg-source.cyclonedx.json" >/dev/null
jq -e --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    '([.packages[] | select(.name == "ffmpeg" and .versionInfo == $version)
      | select(any(.externalRefs[]?; .referenceType == "cpe23Type"))] | length == 1)
      and ([.packages[] | select((.name | ascii_downcase) == "ffmpeg")] | length == 1)
      and ([.relationships[] | select(.relatedSpdxElement == "SPDXRef-Package-FFmpeg")] | length == 1)' \
    "${output_dir}/jellyrin-image.spdx.json" >/dev/null
jq -e --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    '([.components[] | select(.name == "ffmpeg" and .version == $version and (.cpe | startswith("cpe:2.3:a:ffmpeg:ffmpeg:")))] | length == 1)
      and ([.components[] | select((.name | ascii_downcase) == "ffmpeg")] | length == 1)' \
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
        ffmpeg-bsfs.txt \
        ffmpeg-build-configuration.txt \
        ffmpeg-decoders.txt \
        ffmpeg-demuxers.txt \
        ffmpeg-encoders.txt \
        ffmpeg-muxers.txt \
        ffmpeg-parsers.txt \
        ffmpeg-protocols.txt \
        ffmpeg-source-sha256.txt \
        ffmpeg-source.cyclonedx.json \
        ffmpeg-source.spdx.json \
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
