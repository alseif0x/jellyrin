#!/bin/sh
set -eu

config_path="${MAGSTV_WG_CONFIG:-/run/secrets/mx.conf}"
test -r "$config_path" || {
    echo "MAGSTV WireGuard profile is not readable" >&2
    exit 78
}

# Keep the mounted secret read-only and give wg-quick its own mode-0600 copy.
install -m 0600 "$config_path" /etc/wireguard/mx.conf

cleanup() {
    wg-quick down mx >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

wg-quick up mx

country="$(curl -4 -fsS --max-time 20 https://ipinfo.io/country 2>/dev/null | tr -d '\r\n' || true)"
if [ "$country" != MX ]; then
    echo "MAGSTV egress is not MX; refusing to serve the proxy" >&2
    exit 78
fi

exec /usr/local/bin/jellyrin-magstv-egress
