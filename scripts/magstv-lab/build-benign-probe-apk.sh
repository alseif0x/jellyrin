#!/usr/bin/env bash
set -Eeuo pipefail
set +x
IFS=$'\n\t'
umask 077

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SOURCE_DIR="${SCRIPT_DIR}/benign-probe"
readonly DEFAULT_SDK_ROOT="/home/cdmonio/android-sdk"
readonly SDK_ROOT="${ANDROID_SDK_ROOT:-${DEFAULT_SDK_ROOT}}"
readonly OUTPUT_DIR="${1:-/tmp/jellyrin-benign-probe-$(date -u +%Y%m%dT%H%M%SZ)}"
readonly PACKAGE_NAME="lab.jellyrin.benignprobe"
readonly MIN_SDK="27"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_file() {
    [[ -f "$1" ]] || die "required file is missing: $1"
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

latest_version_dir() {
    local parent="$1"
    local selected

    [[ -d "${parent}" ]] || die "required directory is missing: ${parent}"
    selected="$(
        find "${parent}" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' \
            | sort -V \
            | tail -n 1
    )"
    [[ -n "${selected}" ]] || die "no installed version found below: ${parent}"
    printf '%s/%s\n' "${parent}" "${selected}"
}

require_command date
require_command find
require_command git
require_command javac
require_command keytool
require_command openssl
require_command realpath
require_command sha256sum
require_command sort
require_command tail
require_command unzip

require_file "${SOURCE_DIR}/AndroidManifest.xml"
require_file "${SOURCE_DIR}/res/values/strings.xml"
require_file "${SOURCE_DIR}/src/lab/jellyrin/benignprobe/MainActivity.java"
require_file "${SOURCE_DIR}/jni/benign_probe.c"

readonly BUILD_TOOLS_DIR="$(latest_version_dir "${SDK_ROOT}/build-tools")"
readonly NDK_DIR="$(latest_version_dir "${SDK_ROOT}/ndk")"
readonly PLATFORM_DIR="$(latest_version_dir "${SDK_ROOT}/platforms")"
readonly ANDROID_JAR="${PLATFORM_DIR}/android.jar"
readonly AAPT="${BUILD_TOOLS_DIR}/aapt"
readonly APKSIGNER="${BUILD_TOOLS_DIR}/apksigner"
readonly D8="${BUILD_TOOLS_DIR}/d8"
readonly ZIPALIGN="${BUILD_TOOLS_DIR}/zipalign"
readonly CLANG="${NDK_DIR}/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android${MIN_SDK}-clang"

require_file "${ANDROID_JAR}"
require_file "${AAPT}"
require_file "${APKSIGNER}"
require_file "${D8}"
require_file "${ZIPALIGN}"
require_file "${CLANG}"

readonly GIT_ROOT="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
readonly GIT_ROOT_REAL="$(realpath -e -- "${GIT_ROOT}")"
readonly OUTPUT_REAL_PARENT="$(realpath -m -- "$(dirname -- "${OUTPUT_DIR}")")"
case "${OUTPUT_REAL_PARENT}/$(basename -- "${OUTPUT_DIR}")/" in
    "${GIT_ROOT_REAL}/"*)
        die "output directory must be outside the Git worktree"
        ;;
esac

[[ ! -e "${OUTPUT_DIR}" ]] || die "output path already exists: ${OUTPUT_DIR}"
mkdir -p -- \
    "${OUTPUT_DIR}/build/classes" \
    "${OUTPUT_DIR}/build/dex" \
    "${OUTPUT_DIR}/build/package/lib/arm64-v8a" \
    "${OUTPUT_DIR}/signing"

readonly CLASSES_DIR="${OUTPUT_DIR}/build/classes"
readonly DEX_DIR="${OUTPUT_DIR}/build/dex"
readonly PACKAGE_DIR="${OUTPUT_DIR}/build/package"
readonly RESOURCES_APK="${OUTPUT_DIR}/build/resources.ap_"
readonly UNALIGNED_APK="${OUTPUT_DIR}/build/benign-probe-unaligned.apk"
readonly ALIGNED_APK="${OUTPUT_DIR}/build/benign-probe-aligned.apk"
readonly FINAL_APK="${OUTPUT_DIR}/jellyrin-benign-probe.apk"
readonly KEYSTORE="${OUTPUT_DIR}/signing/benign-probe.p12"

"${AAPT}" package \
    -f \
    -m \
    -M "${SOURCE_DIR}/AndroidManifest.xml" \
    -S "${SOURCE_DIR}/res" \
    -I "${ANDROID_JAR}" \
    -F "${RESOURCES_APK}"

javac \
    -encoding UTF-8 \
    -source 8 \
    -target 8 \
    -bootclasspath "${ANDROID_JAR}" \
    -classpath "${ANDROID_JAR}" \
    -d "${CLASSES_DIR}" \
    "${SOURCE_DIR}/src/lab/jellyrin/benignprobe/MainActivity.java"

"${D8}" \
    --min-api "${MIN_SDK}" \
    --lib "${ANDROID_JAR}" \
    --output "${DEX_DIR}" \
    "${CLASSES_DIR}/lab/jellyrin/benignprobe/MainActivity.class"

"${CLANG}" \
    -std=c11 \
    -Oz \
    -fPIC \
    -fvisibility=hidden \
    -ffunction-sections \
    -fdata-sections \
    -Wall \
    -Wextra \
    -Werror \
    -shared \
    -Wl,--gc-sections \
    -Wl,-z,relro \
    -Wl,-z,now \
    -Wl,-soname,libbenign_probe.so \
    -o "${PACKAGE_DIR}/lib/arm64-v8a/libbenign_probe.so" \
    "${SOURCE_DIR}/jni/benign_probe.c" \
    -llog

cp -- "${DEX_DIR}/classes.dex" "${PACKAGE_DIR}/classes.dex"
cp -- "${RESOURCES_APK}" "${UNALIGNED_APK}"
(
    cd -- "${PACKAGE_DIR}"
    "${AAPT}" add "${UNALIGNED_APK}" \
        classes.dex \
        lib/arm64-v8a/libbenign_probe.so >/dev/null
)

"${ZIPALIGN}" -f 4 "${UNALIGNED_APK}" "${ALIGNED_APK}"

BENIGN_PROBE_KEY_PASS="$(openssl rand -hex 16)"
[[ "${#BENIGN_PROBE_KEY_PASS}" -eq 32 ]] || die "could not generate ephemeral signing password"
export BENIGN_PROBE_KEY_PASS
trap 'unset BENIGN_PROBE_KEY_PASS' EXIT

keytool \
    -genkeypair \
    -noprompt \
    -storetype PKCS12 \
    -keystore "${KEYSTORE}" \
    -storepass:env BENIGN_PROBE_KEY_PASS \
    -keypass:env BENIGN_PROBE_KEY_PASS \
    -alias benign-probe \
    -keyalg RSA \
    -keysize 2048 \
    -validity 2 \
    -dname "CN=Jellyrin Benign Probe,OU=Ephemeral Lab,O=Jellyrin" \
    >/dev/null 2>&1

"${APKSIGNER}" sign \
    --ks "${KEYSTORE}" \
    --ks-key-alias benign-probe \
    --ks-pass env:BENIGN_PROBE_KEY_PASS \
    --key-pass env:BENIGN_PROBE_KEY_PASS \
    --out "${FINAL_APK}" \
    "${ALIGNED_APK}"

unset BENIGN_PROBE_KEY_PASS
trap - EXIT

"${APKSIGNER}" verify --verbose --print-certs "${FINAL_APK}" >/dev/null
"${ZIPALIGN}" -c -v 4 "${FINAL_APK}" >/dev/null

readonly BADGING="$("${AAPT}" dump badging "${FINAL_APK}")"
grep -Fq "package: name='${PACKAGE_NAME}'" <<<"${BADGING}" \
    || die "unexpected package name in built APK"
grep -Fq "sdkVersion:'${MIN_SDK}'" <<<"${BADGING}" \
    || die "unexpected minimum SDK in built APK"
grep -Fq "native-code: 'arm64-v8a'" <<<"${BADGING}" \
    || die "APK does not declare exactly the arm64-v8a native ABI"
if grep -Fq "uses-permission:" <<<"${BADGING}"; then
    die "built APK unexpectedly declares an Android permission"
fi

readonly ARCHIVE_LIST="$(unzip -Z1 "${FINAL_APK}")"
grep -Fxq "classes.dex" <<<"${ARCHIVE_LIST}" \
    || die "classes.dex is missing from built APK"
grep -Fxq "lib/arm64-v8a/libbenign_probe.so" <<<"${ARCHIVE_LIST}" \
    || die "ARM64 JNI library is missing from built APK"
if grep -E '^lib/[^/]+/' <<<"${ARCHIVE_LIST}" \
        | grep -Fvx "lib/arm64-v8a/libbenign_probe.so" >/dev/null; then
    die "built APK contains an unexpected native library or ABI"
fi

printf 'APK_PATH=%s\n' "${FINAL_APK}"
printf 'APK_SHA256=%s\n' "$(sha256sum "${FINAL_APK}" | cut -d ' ' -f 1)"
