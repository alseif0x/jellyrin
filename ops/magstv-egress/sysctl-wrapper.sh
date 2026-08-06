#!/bin/sh

# Docker blocks this network-namespace sysctl even with NET_ADMIN. wg-quick
# treats it as a required command, although the tunnel and its policy routes
# are already configured. Do not mask any other sysctl operation.
if [ "$#" -eq 2 ] && [ "$1" = "-q" ] &&
    [ "$2" = "net.ipv4.conf.all.src_valid_mark=1" ]; then
    exit 0
fi

exec /usr/bin/sysctl "$@"
