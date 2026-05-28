FROM rust:1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release -p jellyrin-server

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/jellyrin --shell /usr/sbin/nologin jellyrin \
    && install -d -o jellyrin -g jellyrin /var/lib/jellyrin /var/cache/jellyrin /var/log/jellyrin /etc/jellyrin /srv/jellyrin/web

COPY --from=builder /src/target/release/jellyrin-server /usr/local/bin/jellyrin-server

USER jellyrin
EXPOSE 8096
VOLUME ["/var/lib/jellyrin", "/var/cache/jellyrin", "/var/log/jellyrin", "/etc/jellyrin", "/srv/jellyrin/web", "/media"]
ENV JELLYRIN_HOST=0.0.0.0 \
    JELLYRIN_PORT=8096 \
    JELLYRIN_DATA_DIR=/var/lib/jellyrin \
    JELLYRIN_CONFIG_DIR=/etc/jellyrin \
    JELLYRIN_CACHE_DIR=/var/cache/jellyrin \
    JELLYRIN_LOG_DIR=/var/log/jellyrin \
    JELLYRIN_WEB_DIR=/srv/jellyrin/web \
    RUST_LOG=jellyrin=info,tower_http=info
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${JELLYRIN_PORT}/healthz" || exit 1

ENTRYPOINT ["/usr/local/bin/jellyrin-server"]
