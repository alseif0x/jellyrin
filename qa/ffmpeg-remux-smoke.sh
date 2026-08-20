#!/usr/bin/env bash
set -Eeuo pipefail

image_ref="${1:?usage: qa/ffmpeg-remux-smoke.sh IMAGE_REF}"
container_engine="${CONTAINER_ENGINE:-docker}"

for required_command in "${container_engine}" ffmpeg jq; do
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        echo "required FFmpeg smoke command is unavailable: ${required_command}" >&2
        exit 1
    fi
done

fixture_root="$(mktemp -d)"
cleanup() {
    find "${fixture_root}" -mindepth 1 -delete
    rmdir "${fixture_root}"
}
trap cleanup EXIT
chmod 0777 "${fixture_root}"

ffmpeg -hide_banner -loglevel error \
    -f lavfi -i 'testsrc=size=160x90:rate=10' \
    -f lavfi -i 'sine=frequency=1000:sample_rate=48000' \
    -t 1 -c:v libx264 -preset ultrafast -threads 1 -pix_fmt yuv420p -c:a aac \
    "${fixture_root}/source.mp4"
ffmpeg -hide_banner -loglevel error -i "${fixture_root}/source.mp4" -c copy \
    "${fixture_root}/source.mkv"
ffmpeg -hide_banner -loglevel error -i "${fixture_root}/source.mp4" -c copy \
    -bsf:v h264_mp4toannexb -f mpegts "${fixture_root}/source.ts"
# CI and hardened operator shells can use umask 0077. The image runs as the dedicated
# non-root Jellyrin user, so make the generated, non-sensitive fixtures explicitly readable.
chmod 0644 "${fixture_root}/source.mp4" "${fixture_root}/source.mkv" \
    "${fixture_root}/source.ts"

for input_name in source.mp4 source.mkv source.ts; do
    fixture_id="${input_name//./-}"
    output_dir="${fixture_root}/hls-${fixture_id}"
    mkdir "${output_dir}"
    chmod 0777 "${output_dir}"
    "${container_engine}" run --rm --entrypoint ffprobe \
        -v "${fixture_root}:/fixtures" "${image_ref}" \
        -v error -show_format -show_streams -of json "/fixtures/${input_name}" \
        > "${fixture_root}/${fixture_id}.probe.json"
    jq -e '
      (.streams | length == 2)
      and ([.streams[].codec_type] | sort == ["audio", "video"])
      and ([.streams[] | select(.codec_type == "audio")][0].sample_rate == "48000")
    ' "${fixture_root}/${fixture_id}.probe.json" >/dev/null
    "${container_engine}" run --rm --entrypoint ffmpeg \
        -v "${fixture_root}:/fixtures" "${image_ref}" \
        -hide_banner -loglevel error -i "/fixtures/${input_name}" \
        -map 0:v:0 -map 0:a:0 -c copy -f hls -hls_time 1 -hls_list_size 0 \
        "/fixtures/hls-${fixture_id}/index.m3u8"
    test -s "${output_dir}/index.m3u8"
    segment_path="$(find "${output_dir}" -type f -name '*.ts' -size +0c -print -quit)"
    [[ -n "${segment_path}" ]]
    segment_name="$(basename -- "${segment_path}")"
    "${container_engine}" run --rm --entrypoint ffprobe \
        -v "${fixture_root}:/fixtures" "${image_ref}" \
        -v error -show_streams -of json "/fixtures/hls-${fixture_id}/${segment_name}" \
        > "${fixture_root}/${fixture_id}.segment-probe.json"
    jq -e '(.streams | length == 2)' \
        "${fixture_root}/${fixture_id}.segment-probe.json" >/dev/null
done

encoder_names="$(
    "${container_engine}" run --rm --entrypoint ffmpeg "${image_ref}" \
        -hide_banner -encoders 2>/dev/null \
        | awk '$1 ~ /^[VAS]/ && length($1) == 6 && $2 != "=" { print $2 }' \
        | LC_ALL=C sort -u
)"
decoder_names="$(
    "${container_engine}" run --rm --entrypoint ffmpeg "${image_ref}" \
        -hide_banner -decoders 2>/dev/null \
        | awk '$1 ~ /^[VAS]/ && length($1) == 6 && $2 != "=" { print $2 }' \
        | LC_ALL=C sort -u
)"
reviewed_runtime_ffmpeg_encoders='aac,libx264,mjpeg,subrip,webvtt'
reviewed_runtime_ffmpeg_decoders='aac,ac3,ass,av1,dvbsub,dvdsub,eac3,flac,h263,h264,hevc,mjpeg,mp3,mp3float,mpeg2video,mpeg4,opus,pgssub,srt,ssa,subrip,vc1,vorbis,vp8,vp9,webvtt,wmv3'
expected_encoder_names="$(
    printf '%s\n' "${reviewed_runtime_ffmpeg_encoders}" | tr ',' '\n' | LC_ALL=C sort
)"
expected_decoder_names="$(
    printf '%s\n' "${reviewed_runtime_ffmpeg_decoders}" | tr ',' '\n' | LC_ALL=C sort
)"
if [[ "${encoder_names}" != "${expected_encoder_names}" ]]; then
    echo "enabled image encoder allowlist drift in FFmpeg smoke test" >&2
    exit 1
fi
if [[ "${decoder_names}" != "${expected_decoder_names}" ]]; then
    echo "enabled image decoder allowlist drift in FFmpeg smoke test" >&2
    exit 1
fi

printf 'verified minimal FFmpeg probe/remux corpus: %s\n' "${image_ref}"
