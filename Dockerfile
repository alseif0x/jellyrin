ARG RUST_IMAGE=rust:1.93.0-bookworm@sha256:d0a4aa3ca2e1088ac0c81690914a0d810f2eee188197034edf366ed010a2b382
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

FROM ${RUST_IMAGE} AS builder

ARG CARGO_BUILD_JOBS=2
WORKDIR /src
COPY . .
RUN CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} cargo build --locked --release -p jellyrin-server -p jellyrin-migrate

FROM ${RUNTIME_IMAGE}

ARG RUNTIME_IMAGE
ARG DEBIAN_SNAPSHOT=20260808T000000Z
ARG FFMPEG_PACKAGE_VERSION=7:5.1.9-0+deb12u1
ARG FFMPEG_UPSTREAM_VERSION=5.1.9
ARG VCS_REF=unknown

# A dated, signed Debian snapshot fixes FFmpeg and its complete dependency closure. The top-level
# package is pinned separately so a bad lock update fails instead of silently selecting a new build.
# The fixed 10001 runtime identity lets a root-owned host keyring grant group-read access without
# making the secret world-readable.
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/" \
      'Suites: bookworm bookworm-updates' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      '' \
      'Types: deb' \
      "URIs: http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/" \
      'Suites: bookworm-security' \
      'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
      'Check-Valid-Until: no' \
      > /etc/apt/sources.list.d/jellyrin-snapshot.sources \
    && apt-get -o Acquire::Check-Valid-Until=false update \
    && apt-get install -y --no-install-recommends ca-certificates "ffmpeg=${FFMPEG_PACKAGE_VERSION}" \
    && test "$(dpkg-query -W ffmpeg | awk '{print $2}')" = "${FFMPEG_PACKAGE_VERSION}" \
    && ffmpeg -version | head -n 1 | grep -F "ffmpeg version ${FFMPEG_UPSTREAM_VERSION}" \
    && ffprobe -version | head -n 1 | grep -F "ffprobe version ${FFMPEG_UPSTREAM_VERSION}" \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 jellyrin \
    && useradd --uid 10001 --gid jellyrin --home-dir /var/lib/jellyrin --no-create-home --shell /usr/sbin/nologin jellyrin \
    && install -d -o jellyrin -g jellyrin /var/lib/jellyrin /var/lib/jellyrin/plugins/packages /var/cache/jellyrin /var/cache/jellyrin/transcodes /var/log/jellyrin /etc/jellyrin /srv/jellyrin/web

COPY --from=builder /src/target/release/jellyrin-server /usr/local/bin/jellyrin-server
COPY --from=builder /src/target/release/jellyrin-migrate /usr/local/bin/jellyrin-migrate

LABEL org.opencontainers.image.source="https://github.com/alseif0x/jellyrin" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.base.name="${RUNTIME_IMAGE}" \
      io.jellyrin.debian-snapshot="${DEBIAN_SNAPSHOT}" \
      io.jellyrin.ffmpeg-package="${FFMPEG_PACKAGE_VERSION}"

USER jellyrin
EXPOSE 8096
VOLUME ["/var/lib/jellyrin", "/var/cache/jellyrin", "/var/log/jellyrin", "/etc/jellyrin", "/srv/jellyrin/web"]
ENV JELLYRIN_HOST=0.0.0.0 \
    JELLYRIN_PORT=8096 \
    JELLYRIN_DATA_DIR=/var/lib/jellyrin \
    JELLYRIN_CONFIG_DIR=/etc/jellyrin \
    JELLYRIN_CACHE_DIR=/var/cache/jellyrin \
    JELLYRIN_LOG_DIR=/var/log/jellyrin \
    JELLYRIN_WEB_DIR=/srv/jellyrin/web \
    JELLYRIN_FFMPEG_MODE=remux-only \
    RUST_LOG=jellyrin=info,tower_http=info
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/jellyrin-server", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/jellyrin-server"]
