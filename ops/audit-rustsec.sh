#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
lock_file="${script_dir}/supply-chain.lock.env"
exceptions_file="${script_dir}/vulnerability-exceptions.json"
output_dir="${1:-${repo_root}/rustsec-audit-artifacts}"

for required_file in "${lock_file}" "${exceptions_file}" "${repo_root}/Cargo.lock"; do
    if [[ ! -f "${required_file}" ]]; then
        echo "missing RustSec audit input: ${required_file}" >&2
        exit 1
    fi
done
if [[ -e "${output_dir}" ]]; then
    echo "refusing to overwrite existing RustSec output: ${output_dir}" >&2
    exit 1
fi

for required_command in cargo curl git jq node sha256sum tar; do
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        echo "required RustSec audit command is unavailable: ${required_command}" >&2
        exit 1
    fi
done

# Validate the lock and governed exception policy before downloading or executing tooling.
node "${repo_root}/qa/supply-chain.js" >/dev/null

set -a
# shellcheck disable=SC1090 -- the repository-owned lock was validated immediately above.
source "${lock_file}"
set +a

temp_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temp_root}"
}
trap cleanup EXIT

cargo_audit_archive="${temp_root}/cargo-audit-${CARGO_AUDIT_VERSION}.crate"
cargo_audit_source="${temp_root}/cargo-audit-source"
cargo_audit_root="${temp_root}/cargo-audit-root"
cargo_home="${temp_root}/cargo-home"
mkdir -p "${cargo_audit_source}" "${cargo_audit_root}" "${cargo_home}"
curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail --silent --show-error --location \
    --user-agent 'jellyrin-supply-chain/1' \
    "https://crates.io/api/v1/crates/cargo-audit/${CARGO_AUDIT_VERSION}/download" \
    --output "${cargo_audit_archive}"
printf '%s  %s\n' "${CARGO_AUDIT_CRATE_SHA256}" "${cargo_audit_archive}" \
    | sha256sum --check --strict
tar -xzf "${cargo_audit_archive}" -C "${cargo_audit_source}"
CARGO_HOME="${cargo_home}" cargo install --locked \
    --path "${cargo_audit_source}/cargo-audit-${CARGO_AUDIT_VERSION}" \
    --root "${cargo_audit_root}"
cargo_audit_bin="${cargo_audit_root}/bin/cargo-audit"

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

# Only create the evidence directory after all pinned tooling and database inputs are ready. A
# scanner finding still leaves a complete, checksummed bundle; setup failures leave no partial one.
mkdir -p "${output_dir}"
"${cargo_audit_bin}" --version > "${output_dir}/cargo-audit-version.txt"
grep -Fq "${CARGO_AUDIT_VERSION}" "${output_dir}/cargo-audit-version.txt"
printf '%s\n' "${resolved_rustsec_revision}" \
    > "${output_dir}/rustsec-advisory-db-revision.txt"
node "${script_dir}/render-vulnerability-ignores.js" rustsec-ids \
    > "${output_dir}/rustsec-ignores.txt"

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
done < "${output_dir}/rustsec-ignores.txt"

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

cp "${repo_root}/Cargo.lock" "${output_dir}/Cargo.lock"
cp "${lock_file}" "${output_dir}/supply-chain.lock.env"
cp "${exceptions_file}" "${output_dir}/vulnerability-exceptions.json"
jq -n \
    --arg rustsec_revision "${resolved_rustsec_revision}" \
    --arg scanned_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson rustsec_exit_code "${rustsec_status}" \
    '{
      mode: "rustsec-standalone",
      rustsec_advisory_db_revision: $rustsec_revision,
      scanned_at: $scanned_at,
      rustsec_exit_code: $rustsec_exit_code,
      passed: ($rustsec_exit_code == 0)
    }' > "${output_dir}/rustsec-status.json"

(
    cd "${output_dir}"
    sha256sum \
        Cargo.lock \
        cargo-audit-version.txt \
        cargo-audit.json \
        cargo-audit.stderr.txt \
        rustsec-advisory-db-revision.txt \
        rustsec-ignores.txt \
        rustsec-status.json \
        supply-chain.lock.env \
        vulnerability-exceptions.json \
        > SHA256SUMS
    sha256sum --check --strict SHA256SUMS
)

if (( rustsec_status != 0 )); then
    echo "RustSec gate failed (exit=${rustsec_status}); evidence: ${output_dir}" >&2
    exit 1
fi

printf 'verified standalone RustSec evidence: %s\n' "${output_dir}"
