# MAGSTV MX egress

MAGSTV traffic must leave through the same Mexican WireGuard profile used by
the authorized APK run. The egress is intentionally separate from the
Jellyrin process: the sidecar owns WireGuard and exposes only an HTTP CONNECT
listener on the local Docker network (or `127.0.0.1:18080` for a host-run
development server).

The WireGuard profile is runtime secret material. Keep it outside Git, for
example at `/etc/jellyrin/mx.conf` with mode `0600`; do not put it in the
plugin configuration page or an image layer.

For the development server running on the host:

```sh
MAGSTV_WG_CONFIG_PATH=/etc/jellyrin/mx.conf \
  docker compose -f docker-compose.magstv-egress.yml up -d --build magstv-egress
```

The sidecar refuses to serve until `https://ipinfo.io/country` reports `MX`.
The provider defaults to `http://127.0.0.1:18080`, and therefore fails closed
when the sidecar is absent instead of falling back to the host's ordinary
egress.

For the full Docker deployment, use the base compose file together with the
override so Jellyrin uses the service name rather than the host loopback:

```sh
MAGSTV_WG_CONFIG_PATH=/etc/jellyrin/mx.conf \
  docker compose \
    -f docker-compose.yml \
    -f docker-compose.magstv-egress.yml \
    -f docker-compose.magstv-egress.override.yml \
    up -d --build
```
