ARG RUST_IMAGE=docker.io/library/rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f
ARG RUNTIME_IMAGE=docker.io/library/debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
ARG DISTROLESS_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:d97bc0a941b8d4be647dc0ee75b264ddbb772f1ac5ba690a4309c00723b23775

FROM ${RUNTIME_IMAGE} AS ffmpeg-builder

ARG DEBIAN_SNAPSHOT=20260808T000000Z
ARG FFMPEG_SOURCE_REVISION=1e0279143db99d7324b17f9784b3229122269b38
ARG FFMPEG_SOURCE_VERSION=8.2-dev-git-1e0279143db9
ARG FFMPEG_NVD_BASELINE_VERSION=8.1.2
ARG FFMPEG_SOURCE_SHA256=2eb566ff9b41802220974bf9457da9bdbda078b1f56d1f008525b7b7cd71ca40
ARG FFMPEG_BUILD_JOBS=2

# Build the current upstream release without the very large optional desktop, hardware and codec
# dependency closure pulled in by Debian's general-purpose ffmpeg package. Jellyrin's production
# default is remux-only, so the image contains only the network, container and parser surface used
# by the finite Jellyrin media contract. It contains no encoder, device or filter plugin. The AAC
# decoder is the sole decoder: MPEG-TS does not carry the sample rate in its container metadata, so
# FFmpeg must inspect an AAC frame before a stream-copy HLS mux can write a valid header.
# External TLS and zlib are the only optional libraries enabled deliberately.
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
    && apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git libssl-dev pkg-config zlib1g-dev

COPY ops/ffmpeg-security-baseline.txt /tmp/ffmpeg-security-baseline.txt
RUN curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail \
      --silent --show-error --location \
      "https://code.ffmpeg.org/FFmpeg/FFmpeg/archive/${FFMPEG_SOURCE_REVISION}.tar.gz" \
      --output /tmp/ffmpeg.tar.gz \
    && printf '%s  %s\n' "${FFMPEG_SOURCE_SHA256}" /tmp/ffmpeg.tar.gz \
      | sha256sum --check --strict \
    && mkdir /tmp/ffmpeg-source \
    && tar --extract --gzip --file /tmp/ffmpeg.tar.gz --directory /tmp/ffmpeg-source \
      --strip-components=1 \
    && printf '%s\n' "${FFMPEG_SOURCE_VERSION}" > /tmp/ffmpeg-source/VERSION \
    && git -C /tmp/ffmpeg-source init --quiet \
    && while read -r cve fix_commit patch_sha256; do \
      case "${cve}" in \#*|'') continue ;; esac; \
      patch_file="/tmp/${cve}.patch"; \
      curl --proto '=https' --tlsv1.2 --retry 5 --retry-all-errors --fail \
        --silent --show-error --location \
        "https://code.ffmpeg.org/FFmpeg/FFmpeg/commit/${fix_commit}.patch" \
        --output "${patch_file}"; \
      printf '%s  %s\n' "${patch_sha256}" "${patch_file}" | sha256sum --check --strict; \
      git -C /tmp/ffmpeg-source apply --reverse --check "${patch_file}"; \
    done < /tmp/ffmpeg-security-baseline.txt \
    && rm -rf /tmp/ffmpeg-source/.git

RUN cd /tmp/ffmpeg-source \
    && export SOURCE_DATE_EPOCH=1781668800 \
    && ./configure \
      --prefix=/opt/jellyrin-ffmpeg \
      --disable-autodetect \
      --disable-everything \
      --disable-debug \
      --disable-doc \
      --disable-avdevice \
      --disable-indevs \
      --disable-outdevs \
      --disable-sdl2 \
      --disable-xlib \
      --enable-ffmpeg \
      --enable-ffprobe \
      --enable-avcodec \
      --enable-avformat \
      --enable-avutil \
      --enable-network \
      --enable-openssl \
      --enable-zlib \
      --enable-protocol=file,pipe,http,https,tcp,tls,udp,crypto \
      --enable-demuxer=mpegts,mov,matroska,avi,asf,mp3,flac,aac,ogg,wav,flv,mpeg,hls \
      --enable-muxer=hls,mpegts,mov,mp4 \
      --enable-decoder=aac \
      --enable-parser=h264,hevc,mpeg4video,mpegvideo,aac,aac_latm,ac3,dca,mpegaudio,opus,vorbis,av1 \
      --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,extract_extradata,aac_adtstoasc \
      --cpu=generic \
      --enable-pic \
      --extra-cflags='-O2 -pipe -fstack-protector-strong -D_FORTIFY_SOURCE=2' \
      --extra-ldflags='-Wl,-z,relro,-z,now' \
    && make -j"${FFMPEG_BUILD_JOBS}" \
    && make install \
    && strip /opt/jellyrin-ffmpeg/bin/ffmpeg /opt/jellyrin-ffmpeg/bin/ffprobe \
    && /opt/jellyrin-ffmpeg/bin/ffmpeg -version | head -n 1 \
      | grep -F "ffmpeg version ${FFMPEG_SOURCE_VERSION}" \
    && /opt/jellyrin-ffmpeg/bin/ffprobe -version | head -n 1 \
      | grep -F "ffprobe version ${FFMPEG_SOURCE_VERSION}"

FROM ${RUST_IMAGE} AS builder

ARG CARGO_BUILD_JOBS=2
WORKDIR /src
# Keep documentation, deployment and security-policy edits from invalidating the expensive Rust
# layer. The workspace build needs only the locked root manifests and crate sources.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} cargo build --locked --release -p jellyrin-server -p jellyrin-migrate

FROM ${RUNTIME_IMAGE} AS runtime-layout

# Prepare only Jellyrin-owned empty directories. No Debian package or dpkg database from this
# helper stage is copied into the final distroless runtime.
RUN install -d -o 10001 -g 10001 \
      /jellyrin-root/var/lib/jellyrin/plugins/packages \
      /jellyrin-root/var/cache/jellyrin/transcodes \
      /jellyrin-root/var/log/jellyrin \
      /jellyrin-root/etc/jellyrin \
      /jellyrin-root/srv/jellyrin/web

FROM ${DISTROLESS_IMAGE} AS runtime-smoke

COPY --from=builder /src/target/release/jellyrin-server /usr/local/bin/jellyrin-server
COPY --from=builder /src/target/release/jellyrin-migrate /usr/local/bin/jellyrin-migrate
COPY --from=ffmpeg-builder /opt/jellyrin-ffmpeg/bin/ffmpeg /usr/local/bin/ffmpeg
COPY --from=ffmpeg-builder /opt/jellyrin-ffmpeg/bin/ffprobe /usr/local/bin/ffprobe

# Exec-form RUN works without a shell and proves the distroless glibc/OpenSSL/zlib closure can
# load both media binaries. Exact versions and capabilities were already verified in the builder.
RUN ["/usr/local/bin/ffmpeg", "-version"]
RUN ["/usr/local/bin/ffprobe", "-version"]

FROM runtime-smoke

ARG DISTROLESS_IMAGE
ARG RUNTIME_IMAGE
ARG DEBIAN_SNAPSHOT=20260808T000000Z
ARG FFMPEG_SOURCE_REVISION=1e0279143db99d7324b17f9784b3229122269b38
ARG FFMPEG_SOURCE_VERSION=8.2-dev-git-1e0279143db9
ARG FFMPEG_NVD_BASELINE_VERSION=8.1.2
ARG FFMPEG_SOURCE_SHA256=2eb566ff9b41802220974bf9457da9bdbda078b1f56d1f008525b7b7cd71ca40
ARG VCS_REF=unknown

COPY --from=runtime-layout --chown=10001:10001 /jellyrin-root/ /

LABEL org.opencontainers.image.source="https://github.com/alseif0x/jellyrin" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.base.name="${DISTROLESS_IMAGE}" \
      io.jellyrin.build-base.name="${RUNTIME_IMAGE}" \
      io.jellyrin.build-debian-snapshot="${DEBIAN_SNAPSHOT}" \
      io.jellyrin.ffmpeg-version="${FFMPEG_SOURCE_VERSION}" \
      io.jellyrin.ffmpeg-source-revision="${FFMPEG_SOURCE_REVISION}" \
      io.jellyrin.ffmpeg-nvd-baseline-version="${FFMPEG_NVD_BASELINE_VERSION}" \
      io.jellyrin.ffmpeg-source-sha256="${FFMPEG_SOURCE_SHA256}"

USER 10001:10001
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
