# Supply-chain lock, SBOM and vulnerability policy

`ops/supply-chain.lock.env` is the reviewed, public lock for container inputs. It fixes the Rust
builder, Debian runtime, PostgreSQL and the dormant Redis scaffold by tag plus OCI manifest digest.
The runtime apt sources use a dated Debian snapshot. FFmpeg is compiled in a separate stage from
the official versioned source archive after verifying its locked SHA-256; the runtime receives only
the two stripped binaries and their small TLS/zlib closure. `cargo build --locked` makes Cargo
refuse dependency-lock drift.
Jellyfin Web is independently pinned to version `10.11.11`, its immutable
upstream commit and the SHA-256 of that commit archive. On top of that unchanged
base, the official minimal PR #7617 commit patch and its SHA-256 are pinned to
move Swiper from `11.2.8` to the first corrected release, `12.1.2`. Generated
browser assets remain outside version control.

The current manifest digests were resolved from Docker Hub and the returned manifest bytes were
independently SHA-256 checked on 2026-08-08. The official FFmpeg 8.1.2 archive is locked by SHA-256
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`; it is compiled with
`--disable-everything` and an explicit remux/probe capability set, without encoders and with AAC
as its sole decoder. MPEG-TS requires inspection of an AAC frame to recover the sample rate before
the HLS stream-copy muxer can write a valid header; Jellyrin still rejects every encode command in
`remux-only` mode.
The input contract covers the allowlisted Xtream extensions plus MAGSTV MPEG-TS. HTTP(S), local
file/pipe, UDP and encrypted-HLS (`crypto`) input are retained deliberately; outputs are restricted
to HLS/MPEG-TS plus the MOV/MP4 muxer required by FFmpeg's HLS implementation at link time. Xtream rejects an
explicit container extension outside the same finite contract instead of silently admitting a
format absent from the runtime image.
These facts describe the current lock; they are not permission to update only one duplicate
reference.

## Build and verify a release candidate

Build the browser client into a new output directory before packaging:

```bash
ops/build-jellyfin-web.sh ./web
```

`ops/build-jellyfin-web.sh` downloads the commit-addressed archive over HTTPS,
checks the locked SHA-256, verifies the package version, then downloads and
checksum-verifies the official Swiper patch. It rejects a patch that touches
anything except `package.json` and `package-lock.json`, applies it, and confirms
both manifests lock Swiper `12.1.2`. It runs `npm ci --omit=optional` and fails
if the Node-only `canvas`, `node-pre-gyp` or `tar` chain is installed before the
production build. Finally it validates `index.html` plus assets and renames the
completed tree into place on the same filesystem. It refuses to overwrite an
existing path, so an interrupted or stale deployment cannot be silently mixed
with new assets.

Review and update the base archive fields and the Swiper patch fields as two
explicit provenance units. Do not replace the patch with a branch, mutable PR
URL or an unreviewed dependency override. Because Swiper 12 is a major upgrade,
browser E2E for slideshow and comics remains required before deployment; the
local packaging build does not provide that interaction evidence.

From a clean checkout with Rust/Cargo, Docker, curl, git, jq, Node and standard checksum tools:

```bash
node qa/supply-chain.js
set -a
. ops/supply-chain.lock.env
set +a
docker build --pull \
  --build-arg "RUST_IMAGE=${RUST_IMAGE}" \
  --build-arg "RUNTIME_IMAGE=${RUNTIME_IMAGE}" \
  --build-arg "DEBIAN_SNAPSHOT=${DEBIAN_SNAPSHOT}" \
  --build-arg "FFMPEG_SOURCE_SHA256=${FFMPEG_SOURCE_SHA256}" \
  --build-arg "FFMPEG_UPSTREAM_VERSION=${FFMPEG_UPSTREAM_VERSION}" \
  --build-arg "VCS_REF=$(git rev-parse HEAD)" \
  --tag jellyrin:release \
  .
ops/generate-sbom.sh jellyrin:release supply-chain-artifacts
ops/scan-vulnerabilities.sh jellyrin:release vulnerability-artifacts
(cd supply-chain-artifacts && sha256sum --check --strict SHA256SUMS)
(cd vulnerability-artifacts && sha256sum --check --strict SHA256SUMS)
```

The bundle contains SPDX JSON and CycloneDX JSON for the runtime image and Cargo dependency
manifests, the exact dpkg inventory, FFmpeg source digest, build configuration, capability listings
and FFmpeg/ffprobe versions, image inspection metadata, both release binaries, the public lock and
checksums. It rejects a Debian `ffmpeg` package, every encoder, or any decoder other than AAC in the
remux-only image. The
generator downloads Syft only after selecting
the lock's amd64/arm64 checksum and refuses to overwrite an existing output directory.

The vulnerability bundle contains the exact cargo-audit version, pinned RustSec database commit,
Trivy versions and database metadata, machine-readable Rust and image findings, the generated
ignore policy, scanner exit codes and checksums. The runner downloads the cargo-audit crate and
Trivy archive into an isolated temporary directory and verifies both before execution. It does not
assume either scanner is installed globally and does not modify the developer's Cargo tools.

Rust dependencies can also be gated independently on a host without Docker:

```bash
ops/audit-rustsec.sh rustsec-audit-artifacts
(cd rustsec-audit-artifacts && sha256sum --check --strict SHA256SUMS)
```

The standalone runner uses the same public lock and governed RustSec exceptions as the complete
image gate. It retains `Cargo.lock`, scanner output and stderr, the exact advisory database commit,
the rendered ignore list, status JSON and checksums. It refuses to overwrite an existing evidence
directory. This is real RustSec evidence, but it does not replace the Docker-dependent Trivy scan.

The current standalone result remains red only for `RUSTSEC-2023-0071` on `rsa 0.9.10`. That crate
is retained by Cargo's lock resolution through SQLx's optional MySQL backend, while both the
production tree and `cargo tree --workspace --all-features --target all -i rsa` are empty and no
workspace manifest enables SQLx's `mysql` feature. The advisory declares no patched release. Do
not silently ignore it or upgrade SQLx as a side effect: either keep the gate red, approve a
time-bounded reviewed `crate:rsa@0.9.10` exception for the unreachable backend, or first analyze a
backend-specific SQLx dependency layout that removes it from the lock without weakening PostgreSQL
and SQLite conformance. No such exception is currently present.

## Runtime surface decisions

The release image does not install `curl`. Its OCI healthcheck invokes
`jellyrin-server --healthcheck`, a localhost-only HTTP probe with fixed connect, read and write
timeouts that runs before argument parsing, database initialization or secret loading. On the
locked arm64 Bookworm image, removing `curl` also makes eight exclusive automatic dependencies
unnecessary (`libcurl4`, `libldap-2.5-0`, `libnghttp2-14`, `libpsl5`, `librtmp1`, `libsasl2-2`,
`libsasl2-modules-db` and `libssh2-1`). An `apt-get --simulate autoremove --purge curl` comparison
measured 295 packages before and 286 after. In the 2026-08-09 Trivy evidence this removes 12
HIGH/CRITICAL package findings representing seven unique CVEs. These are measured reductions, not
an assertion that the remaining image is vulnerability-free; rebuild and rescan evidence is still
required before promotion.

The migrator remains in the image for now. Removing its 7.6 MiB binary without publishing and
pinning a distinct migration image would complicate the mandatory pre-start schema gate while not
removing any Debian package from the server runtime. Splitting it is therefore a packaging option,
not a demonstrated CVE reduction.

### Evaluated packaged FFmpeg alternative (not adopted)

The official `jellyfin-ffmpeg7` `7.1.4-3-bookworm` release was evaluated on 2026-08-09. Its GitHub
release assets report SHA-256
`0cf62bc2423822c9ec7a38dbc8f526d9a58671bd01843daa7817ece35619fc1c` for amd64 and
`9de240f98cc49db7ebc649c72c37d3ef154170d37ec2fbc68483cc84744804bb` for arm64. The downloaded
arm64 asset matched that digest. Installed with `--no-install-recommends` on the same locked Debian
base, it reduced the package count from 295 to 135 and exposed FFmpeg/ffprobe `7.1.4-Jellyfin`, the
HLS muxer, libx264 and AAC encoders required by Jellyrin.

That package deliberately bundles many private shared libraries below
`/usr/lib/jellyfin-ffmpeg/lib`. A Debian-package-only Trivy result would see the wrapper package but
can miss CVEs in those bundled FFmpeg and codec libraries, so the lower dpkg count is not yet proof
of lower residual risk. Do not adopt it until all of the following are retained as release evidence:

- immutable per-architecture asset URLs and checksums, release/source provenance and license review;
- an SBOM that catalogs the bundled ELF libraries, plus vulnerability results that map those exact
  library versions instead of relying only on Debian package metadata;
- the complete playback/remux/transcode compatibility matrix and comparative CPU measurements on
  amd64 and arm64;
- the normal image scan with no unreviewed HIGH/CRITICAL findings or invented exceptions.

Jellyrin therefore does not adopt that package. The release candidate instead builds the locked
FFmpeg 8.1.2 source with the finite capability set above, links only its small TLS/zlib closure,
records FFmpeg as a CPE-addressable component in the image SBOM, submits that component to Trivy,
and runs MP4/Matroska/MPEG-TS probe plus stream-copy HLS smoke tests inside the final image. Trivy
0.70 does not currently prove inventory for this generic native component, so the gate detects an
empty/unmatched report and fails closed; promotion requires a validated FFmpeg-aware matcher.

CI performs the same native-platform build and verification after the Rust/PostgreSQL gates, then
uploads `jellyrin-supply-chain-<commit>` for 90 days. Tag pushes matching `v*` also run this workflow.
An amd64 CI artifact does not attest an arm64 image: build and run the generator once per published
platform, then retain both bundles beside the release.

## Vulnerability gate

The release gate has deliberately different database semantics for the scanners:

- cargo-audit checks `Cargo.lock` against the exact `RUSTSEC_ADVISORY_DB_REVISION` in the public
  lock, without fetching inside the audit. Known vulnerabilities and RustSec `unsound` advisories
  fail the gate. A database older than cargo-audit's freshness limit also fails, which forces a
  reviewed lock update. Yank state is not queried because crates.io's live index would make this
  otherwise pinned verdict time-dependent; dependency source checksums and the locked build remain
  separate integrity controls.
- Trivy scans OS and language packages in the built runtime image and fails on every `HIGH` or
  `CRITICAL` finding, including findings without an upstream fix. Its vulnerability database is
  intentionally refreshed at scan time so a scheduled scan can discover new CVEs. Therefore its
  security verdict is time-dependent even though the Trivy executable is checksum-pinned. The
  evidence bundle records the scan timestamp and Trivy database metadata needed to reproduce and
  explain that verdict; it must not be described as a permanently reproducible result.

CI runs the gate for pull requests, pushes, release tags and every Monday. PR, push and tag runs
still wait for all Rust/PostgreSQL prerequisites to pass; the Monday run uses an explicit
`always()` job condition so an unrelated failed prerequisite cannot silently suppress the scheduled
security build and scan. The artifact upload also uses an always-run step, so reports survive a
finding-triggered failure. A green local policy check does not mean the image was scanned:
`node qa/supply-chain.js` validates configuration only. The complete gate requires network access
and a Docker-capable host; only the standalone RustSec runner can execute without Docker. This
development host is not assumed to provide Docker, cargo-audit, Trivy or Syft globally.

### Exceptions

`ops/vulnerability-exceptions.json` is the only reviewed exception source and currently contains
no exceptions. Do not add raw cargo-audit arguments or a hand-written `.trivyignore`. Each future
entry must contain exactly:

```json
{
  "scanner": "trivy",
  "id": "CVE-YYYY-NNNN",
  "components": ["pkg:deb/debian/package@version"],
  "reason": "Why the vulnerable code is unreachable or why temporary risk is accepted.",
  "owner": "@responsible-team",
  "tracking_issue": "https://github.com/alseif0x/jellyrin/issues/123",
  "approved_on": "2026-08-08",
  "expires_on": "2026-08-22"
}
```

Use `scanner: "rustsec"`, an ID such as `RUSTSEC-YYYY-NNNN`, and components such as
`crate:name@version` for RustSec. Trivy components must be exact package URLs (purls), which the
runner turns into package-scoped ignore rules. Every exception requires a named owner, HTTPS
tracking issue, substantive reason and expiry 1–30 days after approval. QA rejects duplicate,
future, expired, malformed or longer-lived entries before the scanners are downloaded. Removing or
upgrading the affected component is preferred; renewal requires a new reviewed risk decision.

The generated Trivy file is retained with `--show-suppressed`, so accepted findings remain visible
in the JSON evidence. A RustSec ignore is advisory-wide (a RustSec advisory identifies a specific
crate); the declared component records the exact dependency that justified the acceptance.

## Refresh the lock

Resolve a candidate tag through the official registry. This prints the manifest-list digest; it
does not edit the lock:

```bash
resolve_docker_hub_ref() {
  image_ref="$1"
  image_repo="${image_ref%%:*}"
  image_tag="${image_ref#*:}"
  registry_token="$(
    curl -fsSL "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/${image_repo}:pull" \
      | jq -er '.token'
  )"
  digest="$(
    curl -fsSI \
      -H "Authorization: Bearer ${registry_token}" \
      -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json' \
      "https://registry-1.docker.io/v2/library/${image_repo}/manifests/${image_tag}" \
      | tr -d '\r' \
      | awk 'tolower($1)=="docker-content-digest:" {print $2}' \
      | tail -n 1
  )"
  printf '%s@%s\n' "${image_ref}" "${digest}"
}

resolve_docker_hub_ref rust:1.93.0-bookworm
resolve_docker_hub_ref debian:bookworm-slim
resolve_docker_hub_ref postgres:17.10-bookworm
resolve_docker_hub_ref redis:7.2.14-bookworm
```

Before accepting a result, fetch the manifest by digest and confirm that `sha256sum` of the raw
response equals that digest. Check the proposed FFmpeg release archive independently against its
reviewed SHA-256. If detached signature verification is added later, pin and validate the exact
signing-key fingerprint rather than treating an unverified signature file as evidence:

```bash
curl --proto '=https' --tlsv1.2 --fail --location \
  "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_UPSTREAM_VERSION}.tar.xz" \
  --output ffmpeg-source.tar.xz
printf '%s  %s\n' "${FFMPEG_SOURCE_SHA256}" ffmpeg-source.tar.xz \
  | sha256sum --check --strict
```

For a Syft or Trivy update, obtain the release checksum file from the official project, verify the
amd64 and arm64 archive hashes independently, then update the version and both values together.
For cargo-audit, download the exact crate from crates.io, verify its package version and record its
SHA-256. Resolve `RUSTSEC_ADVISORY_DB_REVISION` and GitHub Actions from their official repositories
with `git ls-remote`; database and workflow references must remain full 40-character commit IDs.
Review the new RustSec commit range and run the real scan before accepting any database refresh.

After any update, change the lock, Dockerfile, Compose defaults and CI service reference in one
review, then run:

```bash
node qa/supply-chain.js
node --check qa/supply-chain.js
node --check ops/render-vulnerability-ignores.js
bash -n ops/audit-rustsec.sh
bash -n ops/generate-sbom.sh
bash -n ops/scan-vulnerabilities.sh
git diff --check
```

On the Docker-capable release host, also build the image and run both evidence generators. A lock
refresh is incomplete until the two scanners pass or every remaining finding has a valid reviewed
exception. Never copy exceptions forward without revalidating their component, reachability and
expiry.

## Promote and deploy

Push the tested image, obtain its registry digest, and put the full promoted reference in the
deployment `.env`:

```bash
docker push registry.example/jellyrin:<release>
docker buildx imagetools inspect registry.example/jellyrin:<release>
# JELLYRIN_IMAGE=registry.example/jellyrin:<release>@sha256:<verified-manifest-digest>
docker compose pull jellyrin jellyrin-migrate
docker compose up -d --no-build
```

Checksums detect accidental artifact drift but do not replace signing, and vulnerability scanners
cannot prove an image is safe. Publishing to the real registry, producing each target-platform
SBOM and vulnerability bundle, signing the promoted digest with the release identity, attaching
provenance, and testing pull-by-digest remain release-environment gates.
