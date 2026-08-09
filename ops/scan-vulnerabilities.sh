#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
lock_file="${script_dir}/supply-chain.lock.env"
exceptions_file="${script_dir}/vulnerability-exceptions.json"
image_ref="${1:-jellyrin:local}"
output_dir="${2:-${repo_root}/vulnerability-artifacts}"

for required_file in "${lock_file}" "${exceptions_file}" "${repo_root}/Cargo.lock"; do
    if [[ ! -f "${required_file}" ]]; then
        echo "missing vulnerability scan input: ${required_file}" >&2
        exit 1
    fi
done
if [[ -e "${output_dir}" ]]; then
    echo "refusing to overwrite existing vulnerability output: ${output_dir}" >&2
    exit 1
fi

set -a
# shellcheck disable=SC1090 -- the repository-owned lock is validated before any download.
source "${lock_file}"
set +a

for required_command in cargo curl docker git jq node sha256sum tar; do
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        echo "required vulnerability scan command is unavailable: ${required_command}" >&2
        exit 1
    fi
done

node "${repo_root}/qa/supply-chain.js" >/dev/null

case "$(uname -m)" in
    x86_64)
        trivy_arch="64bit"
        trivy_sha256="${TRIVY_LINUX_AMD64_SHA256}"
        ;;
    aarch64 | arm64)
        trivy_arch="ARM64"
        trivy_sha256="${TRIVY_LINUX_ARM64_SHA256}"
        ;;
    *)
        echo "unsupported vulnerability scanner host architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

temp_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temp_root}"
}
trap cleanup EXIT
mkdir -p "${output_dir}"

cargo_audit_archive="${temp_root}/cargo-audit-${CARGO_AUDIT_VERSION}.crate"
cargo_audit_source="${temp_root}/cargo-audit-source"
cargo_audit_root="${temp_root}/cargo-audit-root"
mkdir -p "${cargo_audit_source}" "${cargo_audit_root}"
curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail --silent --show-error --location \
    --user-agent 'jellyrin-supply-chain/1' \
    "https://crates.io/api/v1/crates/cargo-audit/${CARGO_AUDIT_VERSION}/download" \
    --output "${cargo_audit_archive}"
printf '%s  %s\n' "${CARGO_AUDIT_CRATE_SHA256}" "${cargo_audit_archive}" \
    | sha256sum --check --strict
tar -xzf "${cargo_audit_archive}" -C "${cargo_audit_source}"
cargo install --locked \
    --path "${cargo_audit_source}/cargo-audit-${CARGO_AUDIT_VERSION}" \
    --root "${cargo_audit_root}"
cargo_audit_bin="${cargo_audit_root}/bin/cargo-audit"
"${cargo_audit_bin}" --version > "${output_dir}/cargo-audit-version.txt"
grep -Fq "${CARGO_AUDIT_VERSION}" "${output_dir}/cargo-audit-version.txt"

rustsec_db="${temp_root}/rustsec-advisory-db"
git init --quiet "${rustsec_db}"
git -C "${rustsec_db}" remote add origin https://github.com/RustSec/advisory-db.git
git -C "${rustsec_db}" fetch --quiet --depth=1 origin "${RUSTSEC_ADVISORY_DB_REVISION}"
git -C "${rustsec_db}" checkout --quiet --detach FETCH_HEAD
resolved_rustsec_revision="$(git -C "${rustsec_db}" rev-parse HEAD)"
if [[ "${resolved_rustsec_revision}" != "${RUSTSEC_ADVISORY_DB_REVISION}" ]]; then
    echo "RustSec advisory database revision drift" >&2
    exit 1
fi
printf '%s\n' "${resolved_rustsec_revision}" \
    > "${output_dir}/rustsec-advisory-db-revision.txt"

rustsec_args=(
    audit
    --db "${rustsec_db}"
    --no-fetch
    --no-yanked
    --deny unsound
    --file "${repo_root}/Cargo.lock"
    --json
)
while IFS= read -r advisory_id; do
    [[ -n "${advisory_id}" ]] || continue
    rustsec_args+=(--ignore "${advisory_id}")
done < <(node "${script_dir}/render-vulnerability-ignores.js" rustsec-ids)

set +e
"${cargo_audit_bin}" "${rustsec_args[@]}" \
    > "${output_dir}/cargo-audit.json" \
    2> "${output_dir}/cargo-audit.stderr.txt"
rustsec_status=$?
set -e
if ! jq -e . "${output_dir}/cargo-audit.json" >/dev/null; then
    echo "cargo-audit did not produce valid JSON" >&2
    rustsec_status=99
fi

trivy_archive="${temp_root}/trivy-${TRIVY_VERSION}.tar.gz"
trivy_root="${temp_root}/trivy"
trivy_cache="${temp_root}/trivy-cache"
mkdir -p "${trivy_root}" "${trivy_cache}"
curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail --silent --show-error --location \
    "https://github.com/aquasecurity/trivy/releases/download/v${TRIVY_VERSION}/trivy_${TRIVY_VERSION}_Linux-${trivy_arch}.tar.gz" \
    --output "${trivy_archive}"
printf '%s  %s\n' "${trivy_sha256}" "${trivy_archive}" | sha256sum --check --strict
tar -xzf "${trivy_archive}" -C "${trivy_root}" trivy
trivy_bin="${trivy_root}/trivy"
"${trivy_bin}" --cache-dir "${trivy_cache}" --version \
    > "${output_dir}/trivy-version-before-scan.txt"
grep -Fq "Version: ${TRIVY_VERSION}" "${output_dir}/trivy-version-before-scan.txt"

trivy_ignore="${output_dir}/trivy-ignore.generated.yaml"
node "${script_dir}/render-vulnerability-ignores.js" trivy-yaml > "${trivy_ignore}"
jq -n \
    --arg version "${FFMPEG_UPSTREAM_VERSION}" \
    --arg sha256 "${FFMPEG_SOURCE_SHA256}" \
    '{
      bomFormat: "CycloneDX",
      specVersion: "1.6",
      version: 1,
      components: [{
        type: "application",
        name: "ffmpeg",
        version: $version,
        purl: ("pkg:generic/ffmpeg@" + $version),
        cpe: ("cpe:2.3:a:ffmpeg:ffmpeg:" + $version + ":*:*:*:*:*:*:*"),
        hashes: [{alg: "SHA-256", content: $sha256}]
      }]
    }' > "${output_dir}/ffmpeg-source.cyclonedx.json"
set +e
"${trivy_bin}" image \
    --cache-dir "${trivy_cache}" \
    --image-src docker \
    --scanners vuln \
    --vuln-type os,library \
    --severity HIGH,CRITICAL \
    --ignorefile "${trivy_ignore}" \
    --show-suppressed \
    --exit-code 1 \
    --format json \
    --output "${output_dir}/trivy-image.json" \
    "${image_ref}" \
    2> "${output_dir}/trivy.stderr.txt"
trivy_status=$?
"${trivy_bin}" sbom \
    --cache-dir "${trivy_cache}" \
    --scanners vuln \
    --severity HIGH,CRITICAL \
    --ignorefile "${trivy_ignore}" \
    --show-suppressed \
    --exit-code 1 \
    --format json \
    --output "${output_dir}/trivy-ffmpeg.json" \
    "${output_dir}/ffmpeg-source.cyclonedx.json" \
    2> "${output_dir}/trivy-ffmpeg.stderr.txt"
trivy_ffmpeg_status=$?
set -e
"${trivy_bin}" --cache-dir "${trivy_cache}" --version > "${output_dir}/trivy-version.txt"
if ! jq -e . "${output_dir}/trivy-image.json" >/dev/null; then
    echo "Trivy did not produce valid JSON" >&2
    trivy_status=99
fi
if ! jq -e . "${output_dir}/trivy-ffmpeg.json" >/dev/null; then
    echo "Trivy did not produce valid FFmpeg SBOM JSON" >&2
    trivy_ffmpeg_status=99
elif ! jq -e '
    [.Results[]? | select(
      ((.Target // "") | ascii_downcase | contains("ffmpeg"))
      or ([.Vulnerabilities[]?.PkgName // ""] | map(ascii_downcase) | any(. == "ffmpeg"))
    )] | length > 0
  ' "${output_dir}/trivy-ffmpeg.json" >/dev/null; then
    echo "Trivy did not prove that the FFmpeg component was inventoried; failing closed" >&2
    trivy_ffmpeg_status=98
fi

cp "${lock_file}" "${output_dir}/supply-chain.lock.env"
cp "${exceptions_file}" "${output_dir}/vulnerability-exceptions.json"
jq -n \
    --arg image_ref "${image_ref}" \
    --arg rustsec_revision "${resolved_rustsec_revision}" \
    --arg scanned_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson rustsec_exit_code "${rustsec_status}" \
    --argjson trivy_exit_code "${trivy_status}" \
    --argjson trivy_ffmpeg_exit_code "${trivy_ffmpeg_status}" \
    '{
      image_ref: $image_ref,
      rustsec_advisory_db_revision: $rustsec_revision,
      scanned_at: $scanned_at,
      rustsec_exit_code: $rustsec_exit_code,
      trivy_exit_code: $trivy_exit_code,
      trivy_ffmpeg_exit_code: $trivy_ffmpeg_exit_code,
      passed: ($rustsec_exit_code == 0 and $trivy_exit_code == 0 and $trivy_ffmpeg_exit_code == 0)
    }' > "${output_dir}/scan-status.json"

(
    cd "${output_dir}"
    sha256sum \
        cargo-audit-version.txt \
        cargo-audit.json \
        cargo-audit.stderr.txt \
        ffmpeg-source.cyclonedx.json \
        rustsec-advisory-db-revision.txt \
        scan-status.json \
        supply-chain.lock.env \
        trivy-ignore.generated.yaml \
        trivy-ffmpeg.json \
        trivy-ffmpeg.stderr.txt \
        trivy-image.json \
        trivy-version-before-scan.txt \
        trivy-version.txt \
        trivy.stderr.txt \
        vulnerability-exceptions.json \
        > SHA256SUMS
    sha256sum --check --strict SHA256SUMS
)

if (( rustsec_status != 0 || trivy_status != 0 || trivy_ffmpeg_status != 0 )); then
    echo "vulnerability gate failed (RustSec=${rustsec_status}, Trivy-image=${trivy_status}, Trivy-FFmpeg=${trivy_ffmpeg_status}); evidence: ${output_dir}" >&2
    exit 1
fi

printf 'verified vulnerability evidence: %s\n' "${output_dir}"
