# Jellyrin Release Checklist

## Fresh Install

- Build release binary: `cargo build --release -p jellyrin-server`.
- Install binary to `/usr/local/bin/jellyrin-server`.
- Create `jellyrin` system user and group.
- Create `/var/lib/jellyrin`, `/var/cache/jellyrin`, `/var/log/jellyrin`, `/etc/jellyrin` and the Jellyfin Web directory.
- Copy `ops/jellyrin.env.example` to `/etc/jellyrin/jellyrin.env` and set `JELLYRIN_WEB_DIR`.
- Install `ops/jellyrin.service` to `/etc/systemd/system/jellyrin.service`.
- Run `systemctl daemon-reload`, `systemctl enable --now jellyrin`.
- Verify `curl -fsS http://127.0.0.1:8096/healthz`.

## Docker/Compose

- Place Jellyfin Web assets in `./web`.
- Mount media read-only under `./media` or adjust the compose media volume.
- Run `docker compose up -d --build`.
- Verify `docker compose ps` shows a healthy `jellyrin` service.
- For DLNA/UPnP device discovery, run with the host-network override:
  `docker compose -f docker-compose.yml -f docker-compose.dlna.yml up -d --build`.
- For systemd/bare-metal DLNA/UPnP, allow TCP `8096` and UDP `1900` on the LAN
  firewall, then verify a control point can fetch `/dlna/{serverId}/description.xml`
  from the SSDP `LOCATION`.

## Upgrade

- Stop the service or container.
- Back up `/var/lib/jellyrin/jellyrin.db`, `jellyrin.db-wal`, `jellyrin.db-shm`, `/etc/jellyrin`, and any external media path mapping.
- Install the new binary or image.
- Start Jellyrin; embedded SQLx migrations run automatically on startup before serving traffic.
- Verify `/healthz`, `/System/Info/Public`, login, library browse, playback, scheduled tasks, backup/restore, and migration dry-run.

## Rollback

- Stop the upgraded service or container.
- Restore the previous binary/image and the database/config backup taken before upgrade.
- Start the previous version.
- Verify `/healthz`, login and playback.
- Keep failed upgrade logs from `/var/log/jellyrin` for diagnosis.
