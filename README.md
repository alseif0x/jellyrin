# Jellyrin

Jellyrin is a Rust port of Jellyfin server behavior. The current milestone is a
compatibility-first backend that can serve the existing Jellyfin web client and
then grow feature-by-feature against golden behavior from upstream Jellyfin.

Planning lives outside this repository:

```text
/home/cdmonio/projects/jellyrin/plans
```

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
cargo run -p jellyrin-server -- --web-dir /home/cdmonio/dev/jellyfin-web/dist
```

### Database backend

SQLite is the operational backend and remains the default for local and lightweight
installations. Backend selection is explicit:

```bash
JELLYRIN_DATABASE_ENGINE=sqlite \
DATABASE_URL='sqlite:///var/lib/jellyrin/jellyrin.db?mode=rwc' \
cargo run -p jellyrin-server
```

`JELLYRIN_DATABASE_ENGINE` accepts `sqlite`, `postgres`, or `postgresql`, and Jellyrin validates
that it matches `DATABASE_URL`. The PostgreSQL contract and selection path exist, but its storage
adapter and migrations are not implemented yet; selecting it fails at startup instead of silently
falling back to SQLite. Switching an existing installation will require the planned export/import
cutover and a controlled restart—there is no live hot-swap or implicit fallback.

Run `./scripts/check-persistence-boundaries.sh` locally to verify that API and contract crates do
not acquire new SQLx or raw-pool dependencies.

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

- `Dockerfile` builds a release `jellyrin-server` image with `ffmpeg`, persistent
  volumes and a `/healthz` healthcheck.
- `docker-compose.yml` runs Jellyrin with persistent data/config/cache/log
  volumes and read-only Jellyfin Web/media mounts.
- `docker-compose.dlna.yml` is the optional DLNA/UPnP override. Use it with
  `docker compose -f docker-compose.yml -f docker-compose.dlna.yml up -d --build`
  when SSDP discovery must work from TVs or VLC on the LAN.
- `ops/jellyrin.service` is the production systemd unit; copy
  `ops/jellyrin.env.example` to `/etc/jellyrin/jellyrin.env` before enabling it.
- `ops/magstv-egress/` contains the optional isolated MX WireGuard sidecar for
  the MAGSTV provider. Its VPN profile stays outside Git and is never entered
  in the plugin UI.
- `ops/release-checklist.md` covers fresh install, upgrade, smoke checks and
  rollback.

Run `npm run qa:packaging-release` before cutting a release.

## Compatibility Notes

Jellyfin Web does not always call API routes with the same casing as the
canonical upstream route name. For example, the client has been observed calling
`/users/public`, `/Users/authenticatebyname`,
`/sessions/capabilities/full` and `/quickconnect/enabled`.

When adding Jellyfin-compatible endpoints, keep one handler implementation and
register the canonical route plus observed lowercase or mixed-case aliases. A
404 caused only by path casing is treated as a compatibility bug and should be
covered by Playwright or route-level regression tests.
