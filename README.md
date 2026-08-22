# Jellyrin

Jellyrin is a Rust port of Jellyfin server behavior. The current milestone is a
compatibility-first backend that can serve the existing Jellyfin web client and
then grow feature-by-feature against golden behavior from upstream Jellyfin.

El stack obligatorio, los componentes opcionales y los perfiles de recursos están en
[`docs/minimum-stack.md`](docs/minimum-stack.md). Las decisiones de PostgreSQL, Redis y FFmpeg se
detallan en [`docs/transcode-optimization-plan.md`](docs/transcode-optimization-plan.md).

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
JELLYRIN_DB_DRIVER=postgresql \
  DATABASE_URL=postgresql://jellyrin_runtime:password@127.0.0.1/jellyrin \
  cargo run -p jellyrin-server -- --web-dir ./web
```

Las credenciales de proveedores externos usan un vault AEAD y una key externa; consulta
[docs/provider-secrets.md](docs/provider-secrets.md) antes de configurar Xtream.

The database boundary uses explicit dialect-native adapters. PostgreSQL is the
only production driver today. `sqlite` is an explicit public selector with a
real adapter limited to tests and historical migration, while MySQL is a
recognised but unavailable future adapter. See
[`docs/database-drivers.md`](docs/database-drivers.md) for the
manager/configuration boundary and the required conformance path.

Service-specific integrations are maintained as out-of-tree plugins. See
[`docs/plugin-boundary.md`](docs/plugin-boundary.md) for the public/private
boundary and the rules for extending the generic plugin SDK.

When Jellyfin is running on `8096` and Jellyrin is running on `8097`, run the
local compatibility acceptance gate with:

```bash
npm run qa:acceptance
```

The local development service is installed as `jellyrin-rust-dev.service` and
listens on port `8097` so it can run alongside the upstream .NET Jellyfin
development server on `8096`.

## Release Packaging

Release artifacts live under `ops/` plus the root Docker files:

- `Dockerfile` builds `jellyrin-server` plus the one-shot `jellyrin-migrate`
  binary in a non-root runtime image with `ffmpeg`.
- `docker-compose.yml` is the complete entrypoint: it starts private PostgreSQL,
  applies migrations with a DDL-only credential, then starts Jellyrin with the
  restricted runtime credential. The published HTTP port binds to host loopback
  by default. Redis remains optional and fail-open; when its profile is enabled it caches only
  shared, regenerable catalogue projections.
- `docker-compose.dlna.yml` is the optional DLNA/UPnP override. Use it with
  `docker compose -f docker-compose.yml -f docker-compose.dlna.yml up -d --build`
  when SSDP discovery must work from TVs or VLC on the LAN.
- `ops/jellyrin.service` depends on the separate `ops/jellyrin-migrate.service`;
  each reads a different root-owned PostgreSQL environment file.
- `ops/nginx-jellyrin.test.kode.live.conf.example` is the reverse-proxy template;
  its access log records `$uri` without query strings so Jellyfin `api_key`
  parameters are never logged.
- `ops/release-checklist.md` covers fresh install, upgrade, smoke checks and
  rollback.
- `ops/supply-chain.lock.env` pins base/infrastructure images, the Debian
  snapshot, FFmpeg, Jellyfin Web source, its official minimal Swiper security
  patch and the SBOM tool. `ops/supply-chain.md` documents the verified
  lock-refresh, build, SBOM and digest-promotion workflow.

For Compose, create untracked configuration and fill every required database
secret before the first start (empty database secrets fail configuration
validation). Then run `ops/bootstrap-compose.sh`; it creates the provider
keyring once, preserves it across restarts, applies the fixed `root:10001`
`0440` permissions and enables the provider-secrets overlay in `.env`.
The repository does not vendor generated Jellyfin Web assets. Build the exact
reviewed source and checksum from `ops/supply-chain.lock.env`; the builder
verifies and applies the official PR #7617 patch to Swiper 12.1.2, omits the
Node-only optional `canvas`/`node-pre-gyp`/`tar` chain, refuses to replace an
existing output directory and publishes the completed build atomically:

```bash
ops/build-jellyfin-web.sh ./web
cp ops/compose.env.example .env
cp ops/jellyrin.env.example ops/jellyrin.env
chmod 600 .env ops/jellyrin.env
ops/bootstrap-compose.sh
ops/deployment-preflight.sh
docker compose up -d --build
```

The offline preflight reads file metadata only. It requires `web/index.html`,
at least one asset, private environment-file permissions and, when the provider
overlay is enabled, run it with
`--require-provider-keyring /absolute/path/to/providers.keyring` to enforce the
fixed container ownership contract before Compose is invoked.

See [`ops/postgres/README.md`](ops/postgres/README.md) for role separation,
existing-volume handling, TLS guidance, and the optional Redis cache profile.
See [docs/provider-secrets.md](docs/provider-secrets.md) before enabling the
provider-keyring overlay; the image uses fixed UID/GID `10001:10001` and the
host file must be readable by that group without being world-readable.

Run `npm run qa:packaging-release` before cutting a release.
Run `node qa/supply-chain.js` as well; CI builds the locked image and uploads a
checksummed SPDX/CycloneDX bundle for the image and Cargo dependency manifests.
Rust dependencies can be audited without Docker using
`ops/audit-rustsec.sh rustsec-audit-artifacts`; the runner downloads the pinned
`cargo-audit`, checks the exact RustSec revision and produces checksummed evidence.
Generate the same bundle locally with
`ops/generate-sbom.sh jellyrin:release supply-chain-artifacts` after building
the release candidate as documented in [`ops/supply-chain.md`](ops/supply-chain.md).

## Recommended topology

For a node that indexes external providers, use the default Compose deployment:

- PostgreSQL is the only durable runtime database and stays on the private
  backend network.
- Redis stays disabled for small installations. Large shared catalogues may enable it for public
  facets and library counts; its measured limits and fail-open contract are recorded in
  [`docs/redis-decision.md`](docs/redis-decision.md).
- nginx is optional. Jellyrin can listen directly on a LAN/publication port and compresses normal
  JSON/web responses itself. Use a reverse proxy when TLS, a public hostname, ACME or perimeter
  controls are required.
- The default is `JELLYRIN_FFMPEG_MODE=enabled` because browser, Android and
  Android TV profiles can require AC3-to-AAC or H.264 transcoding. VOD and Live
  HLS still try direct play/remux first and make at most one encode fallback
  only when needed. Set it to `remux-only` only when every deployed client is
  known to support every source codec, or to `disabled` for direct-play-only
  installations.
- The container CPU limit includes FFmpeg children. Start with 1.5 CPU, one
  total FFmpeg job across all lanes, one remux, one auxiliary FFmpeg job, at
  most eight queued FFmpeg requests, one
  active remote probe, at most eight queued probes and
  `JELLYRIN_FFMPEG_NICE=10`; then raise limits only from measured demand.
- A video encode applies `JELLYRIN_TRANSCODE_THREADS` to the encoder and both
  simple and complex filter graphs. Remux, video-copy and audio-only work do not
  receive video-filter thread flags.
- HLS writers share atomic disk admission, a single usage monitor and a 64 MiB
  headroom reservation per active job. Tune it with
  `JELLYRIN_TRANSCODE_RESERVATION_BYTES`; a bounded volume remains the hard
  protection against growth between measurements.
- `ffprobe` inherits the same scheduler niceness, runs with one thread and has a
  15-second hard deadline. Timed-out or cancelled probes are killed and reaped.
- External provider plugins run from verified packages and receive only the
  environment variables declared by their manifest. They are trusted native
  processes, so install only reproducible packages from controlled sources.

Provider media should reach clients through Direct Play/direct proxy whenever
compatible. That route does not launch FFmpeg and is the largest CPU saving.

## Compatibility Notes

Jellyfin Web does not always call API routes with the same casing as the
canonical upstream route name. For example, the client has been observed calling
`/users/public`, `/Users/authenticatebyname`,
`/sessions/capabilities/full` and `/quickconnect/enabled`.

When adding Jellyfin-compatible endpoints, keep one handler implementation and
register the canonical route plus observed lowercase or mixed-case aliases. A
404 caused only by path casing is treated as a compatibility bug and should be
covered by Playwright or route-level regression tests.
