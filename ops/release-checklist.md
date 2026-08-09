# Jellyrin Release Checklist

## Fresh Install

- Build release binaries: `cargo build --locked --release -p jellyrin-server -p jellyrin-migrate`.
- Verify `node qa/supply-chain.js` passes and record the exact
  `ops/supply-chain.lock.env` shipped with the release.
- Run `ops/audit-rustsec.sh rustsec-audit-artifacts` even on a host without
  Docker, verify its `SHA256SUMS`, and retain the standalone evidence. The full
  Docker gate below remains mandatory for release.
- On a Docker-capable release host, require a passing `ops/scan-vulnerabilities.sh` run and retain
  `cargo-audit.json`, `trivy-image.json`, `nvd-ffmpeg.json`,
  `ffmpeg-security-baseline.txt`, `scan-status.json` and its verified
  `SHA256SUMS`. A local policy-only QA pass is not vulnerability scan evidence.
- Run `qa/ffmpeg-remux-smoke.sh` against the exact release image; retain a passing MP4, Matroska and
  MPEG-TS probe/stream-copy result before promotion.
- Run `qa/runtime-container-smoke.sh` against the exact release image; require migrations,
  non-root read-only startup, health/readiness and graceful shutdown to pass against disposable
  PostgreSQL before promotion.
- Install both binaries under `/usr/local/bin/`.
- Install FFmpeg and verify both `ffmpeg -version` and `ffprobe -version` as the
  service user; startup requires a usable `ffprobe` even when FFmpeg jobs are disabled.
- Create `jellyrin` system user and group.
- Create `/var/lib/jellyrin`, `/var/cache/jellyrin`, `/var/log/jellyrin`,
  `/etc/jellyrin` and the parent of the future Jellyfin Web directory.
- Build Jellyfin Web from the locked commit and checksum with
  `ops/build-jellyfin-web.sh <new-web-output-directory>`; verify the published
  output contains `index.html` and non-index assets. The output path must not
  already exist.
- Confirm the build log verifies the official Swiper patch, reports Swiper
  `12.1.2`, and installs with `--omit=optional`; `canvas`, `node-pre-gyp` and
  Node `tar` must not enter the build environment.
- Before deploying this hardened Web build, run browser E2E for both image
  slideshow and comics reader. This local packaging gate does not replace
  those pending interaction tests.
- Provision PostgreSQL with separate administrator, `jellyrin_migrator`, and
  `jellyrin_runtime` roles; verify `pg_trgm` is installed.
- Copy `ops/jellyrin.env.example` to `/etc/jellyrin/jellyrin.env`; set the
  runtime `DATABASE_URL` and `JELLYRIN_WEB_DIR`.
- Copy `ops/jellyrin-migrate.env.example` to `/etc/jellyrin-migrate.env`; set
  only the migrator `DATABASE_URL`.
- Set both environment files to owner `root`, group `root`, mode `0600`.
- Create `/etc/jellyrin-secrets` as `root:root` mode `0700`, then generate the
  versioned provider keyring described in `docs/provider-secrets.md` directly
  into `/etc/jellyrin-secrets/provider-secret-keyring.json`; set it to
  `root:root` mode `0400`. Never stage it under `/etc/jellyrin`: the supplied
  unit reads the root-only source with `LoadCredential` and gives Jellyrin an
  immutable copy.
- Install `ops/jellyrin.service` and `ops/jellyrin-migrate.service` under
  `/etc/systemd/system/`.
- Review `CPUQuota`/`MemoryMax` in a systemd drop-in if the target is not the
  four-core host used for the supplied defaults.
- The supplied unit is software-only and uses `PrivateDevices=true`. Hardware
  acceleration requires a reviewed drop-in that relaxes this sandbox and
  exposes only the required render devices, for example `PrivateDevices=false`,
  `DevicePolicy=closed` and a host-specific
  `DeviceAllow=/dev/dri/renderD128 rw`; never expose all of `/dev` by default.
- Keep `JELLYRIN_HOST=127.0.0.1` behind a reverse proxy. Install the supplied
  Nginx template only after replacing its certificate paths; do not use an
  access-log format containing `$request`, `$request_uri`, `$args` or referrers.
- Run `systemctl daemon-reload`, `systemctl enable --now jellyrin`.
- Verify the migration unit succeeded, then check both `/healthz` and `/readyz`.
- For first TLS issuance, install the ACME bootstrap vhost, create
  `/var/www/letsencrypt/.well-known/acme-challenge`, run Certbot with the
  webroot authenticator, then atomically replace the bootstrap with the final
  vhost. Install `ops/certbot-reload-nginx` as executable under
  `/etc/letsencrypt/renewal-hooks/deploy/`, run `certbot renew --dry-run`, and
  verify the deploy hook with `nginx -t` before declaring renewal operational.

## Staging E2E (`jellyrin.test.kode.live`)

- Record the deployed binary and Web-tree SHA-256 evidence, PostgreSQL schema
  migration versions, FFmpeg/ffprobe versions and effective systemd properties.
- Confirm ports `8096` and `5432` listen only on loopback, HTTPS presents the
  expected certificate, HTTP redirects while preserving the request URI, and
  Nginx access/error logs contain no query strings or provider credentials.
- Check `/healthz`, `/readyz` and `/System/Info/Public` locally and through HTTPS;
  verify a PostgreSQL outage makes readiness fail closed without starting SQLite.
- Create the first administrator through the Web setup flow. Enter Xtream and
  MAGSTV credentials only in the authenticated UI; never paste them into shell
  history, fixtures, service environments, tickets or chat.
- Before MAGSTV credentials are entered, install the reproducible plugin package,
  verify its SHA-256/ABI and exact public Jellyrin SDK/RPC commit pin, then confirm
  its granted permissions and one-shot runtime boundary.
- Import controlled Xtream and MAGSTV catalogues. Verify database rows, logs,
  diagnostics and FFmpeg/ffprobe argv contain provider references/relay URLs and
  no authenticated upstream URL, username, password, token or grant canary.
- Run `jellyrin-migrate audit-source-hygiene --report <root-only-json>` with the
  runtime PostgreSQL URL after reindex. Require exit `0`; retain its counts-only
  report. Exit `2` requires reimport and exit `3` means the audit is incomplete.
- Exercise Jellyfin Web slideshow/comics, catalogue browse/filter/search, direct
  proxy, stream-copy remux, seek, disconnect, Live TV and one incompatible sample.
  Keep `remux-only`: the incompatible sample must fail closed rather than encode.
- During playback record selected delivery mode/reasons, FFmpeg process count,
  cgroup CPU/RSS, queue waits, `speed`, first-segment time and PostgreSQL pool
  waits. Stop Jellyrin mid-session and confirm no FFmpeg/plugin descendants or
  HLS reservations/files survive beyond the documented grace/retention bounds.
- Run `pg_dump`, restore into an isolated database, apply migrations/readiness
  there, and complete login/catalogue read-only smokes before accepting rollback.

## Docker/Compose

- Run `ops/build-jellyfin-web.sh ./web`; confirm `web/index.html` and non-index
  assets exist. `./web` is generated, ignored and must not already exist.
- Confirm the builder applied the locked official Swiper `12.1.2` security
  patch and omitted optional Node-only dependencies. Keep slideshow and comics
  browser E2E as a deployment gate for this major dependency backport.
- Copy `ops/compose.env.example` to `.env`, fill every database secret with
  an independently generated URL-safe value, and set the file to mode `0600`.
- Copy `ops/jellyrin.env.example` to the ignored `ops/jellyrin.env` and review
  the resource/transcode limits. Keep `JELLYRIN_MAX_FFMPEG_JOBS=1` on the
  low-resource index/proxy node until measurements justify more concurrency.
- Keep `JELLYRIN_PUBLISH_ADDRESS=127.0.0.1`; an intentional LAN-facing DLNA
  deployment must use the separate host-network override and firewall review.
- If enabling the provider-keyring overlay, make its host file `root:10001`
  mode `0440` before startup and confirm it is readable as container UID/GID
  `10001:10001`.
- Run `ops/deployment-preflight.sh` before `docker compose config` or `up`. If
  enabling the provider-keyring overlay, pass
  `--require-provider-keyring <absolute-host-path>`; the check uses metadata
  only and does not read secret contents.
- Verify the HTTP-to-HTTPS Nginx redirect preserves the complete request URI,
  including query parameters, while the access-log format continues to record
  `$uri` only and never query strings.
- Run `docker compose up -d --build`.
- For a release candidate, build from the locked inputs and run
  `ops/generate-sbom.sh jellyrin:release supply-chain-artifacts`; verify
  `SHA256SUMS`, `jellyrin-image.spdx.json`, `jellyrin-image.cyclonedx.json` and
  both source SBOMs before promotion. Confirm the runtime inventory remains at
  or below the reviewed 25-entry bound and contains no packaged `ffmpeg`.
- Confirm the exact production image uses the digest-pinned distroless base,
  runs as `10001:10001`, has no shell/package manager, and still executes both
  FFmpeg binaries plus the complete remux corpus.
- Run `ops/scan-vulnerabilities.sh jellyrin:release vulnerability-artifacts`; require both scanner
  exit codes to be zero and verify its `SHA256SUMS`. Review suppressed findings as well as active
  findings; every suppression must still be valid in `ops/vulnerability-exceptions.json`.
- Run `docker compose config` first in CI or on a Docker-capable staging host;
  this development host does not have the Docker CLI.
- Verify PostgreSQL is healthy, `jellyrin-migrate` exited with status `0`, and
  Jellyrin is healthy on `/readyz`.
- Production deployment must set `JELLYRIN_IMAGE` to the promoted registry
  reference including `@sha256:<verified-manifest-digest>`, then use
  `docker compose up -d --no-build`; never promote a mutable tag alone.
- Do not enable Redis until the application has a measured cache use for it;
  when needed, set `REDIS_PASSWORD` and use `--profile distributed-cache`.
- For DLNA/UPnP device discovery, run with the host-network override:
  `docker compose -f docker-compose.yml -f docker-compose.dlna.yml up -d --build`.
- For systemd/bare-metal DLNA/UPnP, allow TCP `8096` and UDP `1900` on the LAN
  firewall, then verify a control point can fetch `/dlna/{serverId}/description.xml`
  from the SSDP `LOCATION`.

## SQLite to PostgreSQL Cutover

- Announce a maintenance window and record its start time, the source host,
  SQLite path, Jellyrin binary version, and intended PostgreSQL target. Keep the
  old SQLite file until the cutover has been accepted and independently backed up.
- Stop every Jellyrin replica, worker, scheduled task, plugin host, and other
  process that can write the SQLite database. Block ingress and verify no process
  still has the database or its `-wal`/`-shm` files open. The no-write window
  begins here and lasts until PostgreSQL validation is complete.
- Create a transactionally consistent SQLite backup with SQLite's backup API or
  `VACUUM INTO`; do not copy only the main file while WAL mode is active. Run
  `PRAGMA integrity_check` and `PRAGMA foreign_key_check` against the backup,
  then record its size and SHA-256. Use this immutable backup as the migration
  source rather than the live file.
- Provision an empty PostgreSQL 16+ database with `pg_trgm`, separate migrator
  and runtime credentials, verified backups, and enough free space for the import
  plus indexes. Keep all Jellyrin runtimes stopped.
- Apply the embedded schema with the DDL credential:
  `DATABASE_URL="$MIGRATOR_DATABASE_URL" jellyrin-migrate schema`.
- Run a locked dry run and retain its JSON report:
  `DATABASE_URL="$MIGRATOR_DATABASE_URL" jellyrin-migrate data --source /path/to/jellyrin-cutover.db --dry-run --report /path/to/dry-run.json`.
  Resolve every schema-history, integrity, case-insensitive plugin-ID, reference,
  row-count, or digest error; never deduplicate or overwrite target rows implicitly.
- Confirm the PostgreSQL application tables are still empty, then run the commit:
  `DATABASE_URL="$MIGRATOR_DATABASE_URL" jellyrin-migrate data --source /path/to/jellyrin-cutover.db --report /path/to/committed.json`.
  The migrator serializes against schema changes and holds exclusive locks on
  application tables before the emptiness check through commit or dry-run rollback.
- Compare the dry-run and committed overall digests, review every table/omission
  count, and perform direct read-only SQL checks for users, libraries, plugins,
  playlists, playback state, and Live TV tuner configuration. Take and verify a
  PostgreSQL backup now, before any runtime is allowed to write.
- Record this pre-runtime backup and validation timestamp as the rollback boundary.
  Before that boundary, rollback is lossless: discard the PostgreSQL target and
  restart the previous release against the immutable SQLite source. Starting any
  Jellyrin runtime against PostgreSQL crosses the write boundary because background
  work may write even while ingress remains blocked.
- Record authorization to end the no-write window, then start one PostgreSQL-backed
  runtime with ingress still blocked. Verify
  `/healthz`, `/readyz`, login, library browse, playback, scheduled tasks, plugins,
  backup/restore, and provider synchronization; then admit traffic, scale out, and
  record the promotion time.
- After the write boundary, never point the previous runtime at SQLite unless the
  operator explicitly accepts losing all PostgreSQL-side changes. Roll back the
  binary against PostgreSQL and restore the pre-runtime PostgreSQL backup only when
  schema compatibility permits; otherwise stop writers and export/reconcile the
  PostgreSQL delta before any SQLite fallback.

## Upgrade

- Stop the service or container.
- Take and verify a PostgreSQL backup; also back up `/etc/jellyrin` and provider
  configuration. Record the image/binary and schema versions.
- Compare the new `ops/supply-chain.lock.env` and SBOM bundle with the promoted
  release, verify every `SHA256SUMS` entry, and retain the previous image digest
  for rollback.
- Verify cargo-audit used the locked RustSec revision, Trivy recorded current database metadata,
  and no vulnerability exception is expired or carried forward without a reviewed tracking issue.
- Diff the newly shipped environment example against the installed environment
  instead of reusing it blindly. Preserve `JELLYRIN_FFMPEG_MODE=remux-only` (or
  `disabled`), the aggregate FFmpeg cap and the queue/per-lane limits unless
  encode has been explicitly approved from measured client requirements and
  host capacity.
- Install the new binary or image.
- For each published architecture, retain its own image/source SPDX and
  CycloneDX bundle. The native amd64 CI artifact does not attest arm64.
- Start Jellyrin. The exclusive migration job/unit must finish before the
  runtime starts; the runtime never performs DDL.
- Verify `/healthz`, `/readyz`, `/System/Info/Public`, login, library browse,
  playback, scheduled tasks, backup/restore, and migration validation.
- Build the MAGSTV package reproducibly from its separate repository, verify its
  SHA-256 and ABI, and update its Jellyrin SDK/RPC git pin only after the target
  Jellyrin commit is public. Exercise catalogue import and JIT playback with a
  controlled account before promoting either release.
- For large external catalogues, capture `EXPLAIN (ANALYZE, BUFFERS)` and p95
  latency for the actual `/Items` query shapes at 10k, 100k and 500k rows; do
  not claim the scale gate from repository unit tests alone.

## Rollback

- Stop the upgraded service or container.
- Determine whether this is an ordinary PostgreSQL upgrade rollback or a
  SQLite-to-PostgreSQL cutover. For a cutover, use the recorded pre-runtime write
  boundary above; do not assume the old SQLite database is current after that point.
- Restore the previous binary/image and, when the schema is not backward
  compatible, the PostgreSQL/config backup taken before upgrade.
- Start the previous version.
- Verify `/healthz`, login and playback.
- Keep failed upgrade logs from `/var/log/jellyrin` for diagnosis.
