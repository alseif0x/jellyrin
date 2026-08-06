FROM rust:1-bookworm AS builder

WORKDIR /src
COPY ops/magstv-egress/Cargo.toml ./Cargo.toml
COPY crates/jellyrin-magstv-egress/src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl iproute2 iptables openresolv procps wireguard-tools \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -m 0700 /etc/wireguard /run/secrets

COPY --from=builder /src/target/release/jellyrin-magstv-egress /usr/local/bin/jellyrin-magstv-egress
COPY ops/magstv-egress/entrypoint.sh /usr/local/bin/jellyrin-magstv-egress-entrypoint
COPY ops/magstv-egress/sysctl-wrapper.sh /usr/local/sbin/sysctl
RUN chmod 0755 /usr/local/bin/jellyrin-magstv-egress-entrypoint /usr/local/sbin/sysctl

ENV MAGSTV_EGRESS_LISTEN=0.0.0.0:18080 \
    MAGSTV_WG_CONFIG=/run/secrets/mx.conf

ENTRYPOINT ["/usr/local/bin/jellyrin-magstv-egress-entrypoint"]
