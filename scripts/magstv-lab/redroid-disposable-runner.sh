#!/usr/bin/env bash
# Disposable, offline QEMU 8.2 + Ubuntu ARM64 + Redroid 8.1 runner.
#
# `boot` never installs an APK. `boot-benign` accepts only the pinned,
# source-auditable benign probe APK. `boot-magstv-offline` accepts only one
# pinned MAGSTV APK and observes it without credentials or network access.
# Every run creates a fresh QCOW2 overlay and UEFI VARS copy from the sealed
# baseline, then executes synchronously inside an isolated bubblewrap
# network/PID namespace.
set -Eeuo pipefail
set +x
IFS=$'\n\t'
umask 077

readonly LAB="${MAGSTV_REDROID_LAB:-/home/cdmonio/apk-work/arm64-redroid-lab}"
readonly IMMUTABLE="$LAB/immutable"
readonly RUNS="$LAB/runs"
readonly BASE="$IMMUTABLE/redroid-base.qcow2"
readonly VARS_TEMPLATE="$IMMUTABLE/AAVMF_VARS.template.fd"
readonly SEALED_MANIFEST="$IMMUTABLE/sealed-base.manifest"
readonly SSH_KEY="$LAB/lab_ed25519"
readonly SSH_KNOWN_HOSTS="$LAB/ssh_known_hosts"
readonly REDROID_INIT="$LAB/zz-magstv-lab.rc"
readonly LOCK_FILE="$LAB/disposable-runner.lock"
readonly QEMU_BIN="${MAGSTV_REDROID_QEMU:-/usr/bin/qemu-system-aarch64}"
readonly QEMU_IMG="${MAGSTV_REDROID_QEMU_IMG:-/usr/bin/qemu-img}"
readonly BWRAP_BIN="${MAGSTV_REDROID_BWRAP:-/usr/bin/bwrap}"
readonly AAVMF_CODE="${MAGSTV_REDROID_AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.fd}"
readonly SSH_PORT=2224
readonly SELF="$(readlink -f -- "$0")"
readonly SANDBOXED="${MAGSTV_REDROID_DISPOSABLE_SANDBOXED:-0}"
LAB_LOCK_FD=""
SANDBOX_QEMU_PID=""
SANDBOX_CLEANUP_DONE=0

readonly BASE_SHA256="71e90a67b0402d22376f3f7b654f3f29541c7a595a88d48fe8b4663e3bf43626"
readonly VARS_TEMPLATE_SHA256="4f8f77251200afd8850b6b35526c26ec11b25f255fd56626773f2194cdd1171d"
readonly SEALED_MANIFEST_SHA256="52359d91a938bc2f1b5da81ace085a77c895bcac9f45c3c74008687a9895155c"
readonly AAVMF_CODE_SHA256="4a4cb7f6d8106bb2a7dd8c763fab14b1810152136fc4304e5b728f0043e84f12"
readonly REDROID_INIT_SHA256="c6c28632167102d0234c604381dd9873f4f9ac82f1ad2824d8bdc6f493e0d563"
readonly SSH_KNOWN_HOSTS_SHA256="ba7363a3bc468cab97d1f80442fd95e72d9302b6d7e16059605b90532198e653"
readonly REDROID_MANIFEST_ID="sha256:8b95febfd6ef411bb73cad0b6f30ae3ec10f2216c8f8a58052417ef6792fc8b5"
readonly REDROID_CONFIG_ID="sha256:c38107720ad923a0aa1379412b4a53d2e5c5a192663cbd2bd0657e4d521b89f3"
readonly QEMU_VERSION_PREFIX="QEMU emulator version 8.2."
readonly BENIGN_APK_SHA256="1d0b01a9f6aedd91acfb0fbec65422f5677c339e8d162a9c5928c94fc88a7351"
readonly BENIGN_APK_SIZE="12769"
readonly BENIGN_PACKAGE="lab.jellyrin.benignprobe"
readonly BENIGN_COMPONENT="lab.jellyrin.benignprobe/.MainActivity"
readonly BENIGN_JNI_LIBRARY="libbenign_probe.so"
readonly BENIGN_JNI_MARKER="JELLYRIN_BENIGN_PROBE_OK"
readonly BENIGN_SO_SHA256="76718933721a1b919bca3e7adb2af52462ab64bac06410d31c621ac15ca83877"
readonly MAGSTV_APK_HOST_PATH="/home/cdmonio/mgstv-base.apk"
readonly MAGSTV_APK_SHA256="2b098adf19eab4ac0eaf11501ebf386561677b2c95cc1f0499811bb81a058bb5"
readonly MAGSTV_APK_SIZE="35272343"
readonly MAGSTV_PACKAGE="com.android.mgstv"
readonly MAGSTV_VERSION_NAME="4.34.5"
readonly MAGSTV_VERSION_CODE="43405"
readonly MAGSTV_COMPONENT="com.android.mgstv/com.interactive.brasiliptv.ui.activity.WelcomeActivity"
readonly MAGSTV_GOMEDIA_SERVICE="com.main.service.GoMediaService"
readonly MAGSTV_GOMEDIA_PROCESS="com.android.mgstv:gomediad"
readonly MAGSTV_IJIAMI_LIBEXEC_SHA256="512180fa7a5981837bf101474ea76168965dfa2bc367f141f750ac6e17fb7bae"
readonly MAGSTV_IJIAMI_LIBEXECMAIN_SHA256="a0864c7be8520aca7e76377cb87542c446fd1704214e6626ee715afedd2a1ee1"
readonly MAGSTV_RANGER_JNI_SHA256="be0bbd0bc7b09ff35141465721dde19e6b025483a1b88bf58ed9ef670bbd19db"
BENIGN_APK_FD=""
MAGSTV_APK_FD=""

log() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: redroid-disposable-runner.sh COMMAND [ARGUMENT]

Commands:
  doctor                Validate the sealed baseline and host prerequisites.
  boot                  Boot and validate Redroid without installing an APK.
  boot-benign APK_PATH  Run only the pinned harmless ARM64/JNI probe APK.
  boot-magstv-offline /home/cdmonio/mgstv-base.apk
                        Observe the one pinned MAGSTV APK strictly offline.

This runner refuses APK and credential variables. APK scenarios validate an
opened descriptor against one exact size and SHA-256 before sandboxing; no
host APK path or environment variable crosses into the sandbox. The MAGSTV
scenario launches only WelcomeActivity once, never starts GoMediaService
manually, and remains offline. The runner never reads .env, never mounts the
user's home, exposes no host-visible port, and keeps each run under the
external lab's runs/ directory for review.
EOF
}

require_command() {
    command -v "$1" >/dev/null 2>&1 ||
        die "required command not found: $1"
}

reject_forbidden_environment() {
    local variable

    for variable in \
        MAGSTV_APK \
        MAGSTV_ALLOW_APK_EXEC \
        MAGSTV_USERNAME \
        MAGSTV_PASSWORD; do
        if declare -p "$variable" >/dev/null 2>&1; then
            die "disposable runner refuses environment variable: $variable"
        fi
    done
}

sha256_of() {
    sha256sum "$1" | awk '{print $1}'
}

require_sha256() {
    local label="$1"
    local path="$2"
    local expected="$3"
    local actual

    [[ -f "$path" ]] || die "$label is missing: $path"
    actual="$(sha256_of "$path")"
    [[ "$actual" == "$expected" ]] ||
        die "$label sha256 mismatch: $actual"
    printf 'OK    %-24s sha256 %s\n' "$label" "$actual"
}

open_and_validate_benign_apk() {
    local apk_path="$1"
    local fd_path kind size hash path_identity fd_identity

    [[ "$apk_path" == /* ]] ||
        die "boot-benign requires an absolute APK path"
    [[ -f "$apk_path" && ! -L "$apk_path" ]] ||
        die "benign probe path must be a regular non-symlink file"
    path_identity="$(stat -c '%d:%i' -- "$apk_path")"
    if ! exec {BENIGN_APK_FD}<"$apk_path"; then
        die "could not open benign probe APK"
    fi
    fd_path="/proc/$$/fd/$BENIGN_APK_FD"
    fd_identity="$(stat -Lc '%d:%i' "$fd_path")"
    [[ "$path_identity" == "$fd_identity" && ! -L "$apk_path" ]] ||
        die "benign probe path changed while opening"
    kind="$(stat -Lc '%F' "$fd_path")"
    [[ "$kind" == "regular file" ]] ||
        die "benign probe input descriptor is not a regular file"
    size="$(stat -Lc '%s' "$fd_path")"
    [[ "$size" == "$BENIGN_APK_SIZE" ]] ||
        die "benign probe APK size mismatch: $size"
    hash="$(sha256_of "$fd_path")"
    [[ "$hash" == "$BENIGN_APK_SHA256" ]] ||
        die "benign probe APK sha256 mismatch: $hash"
    printf 'OK    %-24s sha256 %s, size %s\n' \
        "benign probe APK FD" "$hash" "$size"
}

close_benign_apk() {
    [[ -n "$BENIGN_APK_FD" ]] || return 0
    exec {BENIGN_APK_FD}<&-
    BENIGN_APK_FD=""
}

open_and_validate_magstv_apk() {
    local apk_path="$1"
    local fd_path kind size hash owner links mode
    local path_identity fd_identity path_identity_after fd_identity_after

    [[ "$apk_path" == "$MAGSTV_APK_HOST_PATH" ]] ||
        die "boot-magstv-offline accepts only the literal pinned APK path"
    [[ -f "$apk_path" && ! -L "$apk_path" ]] ||
        die "MAGSTV APK path must be a regular non-symlink file"
    owner="$(stat -c '%u' -- "$apk_path")"
    links="$(stat -c '%h' -- "$apk_path")"
    mode="$(stat -c '%a' -- "$apk_path")"
    [[ "$owner" == "$EUID" ]] ||
        die "MAGSTV APK is not owned by the current user"
    [[ "$links" == "1" ]] ||
        die "MAGSTV APK has additional hard links"
    (( (8#$mode & 0022) == 0 )) ||
        die "MAGSTV APK is group/world writable"

    path_identity="$(stat -Lc '%d:%i' -- "$apk_path")"
    if ! exec {MAGSTV_APK_FD}<"$apk_path"; then
        die "could not open MAGSTV APK"
    fi
    fd_path="/proc/$$/fd/$MAGSTV_APK_FD"
    fd_identity="$(stat -Lc '%d:%i' "$fd_path")"
    [[ "$path_identity" == "$fd_identity" && ! -L "$apk_path" ]] ||
        die "MAGSTV APK path changed while opening"
    kind="$(stat -Lc '%F' "$fd_path")"
    owner="$(stat -Lc '%u' "$fd_path")"
    links="$(stat -Lc '%h' "$fd_path")"
    mode="$(stat -Lc '%a' "$fd_path")"
    [[ "$kind" == "regular file" ]] ||
        die "MAGSTV APK input descriptor is not a regular file"
    [[ "$owner" == "$EUID" ]] ||
        die "MAGSTV APK descriptor is not owned by the current user"
    [[ "$links" == "1" ]] ||
        die "MAGSTV APK descriptor has additional hard links"
    (( (8#$mode & 0022) == 0 )) ||
        die "MAGSTV APK descriptor is group/world writable"
    size="$(stat -Lc '%s' "$fd_path")"
    [[ "$size" == "$MAGSTV_APK_SIZE" ]] ||
        die "MAGSTV APK size mismatch: $size"
    hash="$(sha256_of "$fd_path")"
    [[ "$hash" == "$MAGSTV_APK_SHA256" ]] ||
        die "MAGSTV APK sha256 mismatch: $hash"

    path_identity_after="$(stat -Lc '%d:%i' -- "$apk_path")"
    fd_identity_after="$(stat -Lc '%d:%i' "$fd_path")"
    owner="$(stat -Lc '%u' "$fd_path")"
    links="$(stat -Lc '%h' "$fd_path")"
    mode="$(stat -Lc '%a' "$fd_path")"
    [[ "$path_identity_after" == "$path_identity" &&
        "$fd_identity_after" == "$fd_identity" &&
        "$path_identity_after" == "$fd_identity_after" &&
        ! -L "$apk_path" ]] ||
        die "MAGSTV APK identity changed during descriptor validation"
    [[ "$owner" == "$EUID" && "$links" == "1" ]] ||
        die "MAGSTV APK metadata changed during descriptor validation"
    (( (8#$mode & 0022) == 0 )) ||
        die "MAGSTV APK became group/world writable during validation"
    printf 'OK    %-24s sha256 %s, size %s\n' \
        "MAGSTV APK FD" "$hash" "$size"
}

close_magstv_apk() {
    [[ -n "$MAGSTV_APK_FD" ]] || return 0
    exec {MAGSTV_APK_FD}<&-
    MAGSTV_APK_FD=""
}

inode_open_pids() (
    local path="$1"
    local target_identity fd_path fd_identity pid
    local -A seen=()

    [[ -e "$path" ]] || return 1
    target_identity="$(stat -Lc '%d:%i' "$path")" || return 1
    shopt -s nullglob
    for fd_path in /proc/[1-9]*/fd/*; do
        fd_identity="$(stat -Lc '%d:%i' "$fd_path" 2>/dev/null || true)"
        [[ "$fd_identity" == "$target_identity" ]] || continue
        pid="${fd_path#/proc/}"
        pid="${pid%%/*}"
        seen["$pid"]=1
    done
    ((${#seen[@]} == 0)) || printf '%s\n' "${!seen[@]}" | sort -n | paste -sd, -
)

acquire_lock() {
    local owner mode links path_identity fd_identity

    require_command flock
    [[ -d "$LAB" ]] || die "lab directory missing: $LAB"
    if [[ ! -e "$LOCK_FILE" && ! -L "$LOCK_FILE" ]]; then
        (
            umask 077
            set -o noclobber
            : >"$LOCK_FILE"
        ) 2>/dev/null || true
    fi
    [[ -f "$LOCK_FILE" && ! -L "$LOCK_FILE" ]] ||
        die "lock must be a regular non-symlink file"
    owner="$(stat -c '%u' "$LOCK_FILE")"
    mode="$(stat -c '%a' "$LOCK_FILE")"
    links="$(stat -c '%h' "$LOCK_FILE")"
    [[ "$owner" == "$EUID" ]] || die "lock is owned by another user"
    [[ "$links" == "1" ]] || die "lock has additional hard links"
    (( (8#$mode & 0022) == 0 )) || die "lock is group/world writable"
    path_identity="$(stat -Lc '%d:%i' "$LOCK_FILE")"
    exec {LAB_LOCK_FD}<>"$LOCK_FILE"
    fd_identity="$(stat -Lc '%d:%i' "/proc/$$/fd/$LAB_LOCK_FD")"
    [[ "$path_identity" == "$fd_identity" ]] ||
        die "lock inode changed while opening"
    flock -n "$LAB_LOCK_FD" ||
        die "another disposable runner operation is active"
}

release_lock() {
    [[ -n "$LAB_LOCK_FD" ]] || return 0
    flock -u "$LAB_LOCK_FD"
    exec {LAB_LOCK_FD}>&-
    LAB_LOCK_FD=""
}

host_resource_cleanup() {
    local status=$?

    trap - EXIT INT TERM HUP
    set +e
    close_benign_apk
    close_magstv_apk
    release_lock
    return "$status"
}

host_preflight() {
    local version open_pids info backing base_mode vars_mode immutable_mode key_mode

    for command in \
        awk \
        cmp \
        date \
        find \
        findmnt \
        flock \
        grep \
        head \
        install \
        ip \
        od \
        paste \
        readlink \
        sed \
        seq \
        sha256sum \
        sort \
        stat \
        ssh \
        tar \
        tail \
        tee \
        timeout \
        tr \
        uniq \
        wc; do
        require_command "$command"
    done
    [[ -x "$QEMU_BIN" ]] || die "QEMU is missing: $QEMU_BIN"
    [[ -x "$QEMU_IMG" ]] || die "qemu-img is missing: $QEMU_IMG"
    [[ -x "$BWRAP_BIN" ]] || die "bubblewrap is missing: $BWRAP_BIN"
    [[ -r "$AAVMF_CODE" ]] || die "AAVMF code is missing: $AAVMF_CODE"
    [[ -f "$SSH_KEY" ]] || die "SSH private key is missing: $SSH_KEY"
    [[ -f "$SELF" ]] || die "runner path is not a regular file: $SELF"

    version="$("$QEMU_BIN" --version | head -n 1)"
    [[ "$version" == "$QEMU_VERSION_PREFIX"* ]] ||
        die "expected QEMU 8.2.x, found: $version"
    printf 'OK    %-24s %s\n' "QEMU" "$version"
    printf 'OK    %-24s %s\n' "bubblewrap" "$("$BWRAP_BIN" --version)"

    immutable_mode="$(stat -c '%a' "$IMMUTABLE")"
    base_mode="$(stat -c '%a' "$BASE")"
    vars_mode="$(stat -c '%a' "$VARS_TEMPLATE")"
    key_mode="$(stat -c '%a' "$SSH_KEY")"
    [[ "$immutable_mode" == "555" ]] ||
        die "immutable directory mode is $immutable_mode, expected 555"
    [[ "$base_mode" == "444" ]] ||
        die "sealed base mode is $base_mode, expected 444"
    [[ "$vars_mode" == "444" ]] ||
        die "VARS template mode is $vars_mode, expected 444"
    [[ "$key_mode" == "600" ]] ||
        die "SSH private key mode is $key_mode, expected 600"

    require_sha256 "sealed base" "$BASE" "$BASE_SHA256"
    require_sha256 "VARS template" "$VARS_TEMPLATE" "$VARS_TEMPLATE_SHA256"
    require_sha256 "sealed manifest" "$SEALED_MANIFEST" "$SEALED_MANIFEST_SHA256"
    require_sha256 "AAVMF code" "$AAVMF_CODE" "$AAVMF_CODE_SHA256"
    require_sha256 "Redroid init" "$REDROID_INIT" "$REDROID_INIT_SHA256"
    require_sha256 "SSH known hosts" "$SSH_KNOWN_HOSTS" "$SSH_KNOWN_HOSTS_SHA256"

    open_pids="$(inode_open_pids "$BASE" 2>/dev/null || true)"
    [[ -z "$open_pids" ]] ||
        die "sealed base inode is open by PID(s): $open_pids"

    info="$("$QEMU_IMG" info --output=json "$BASE")"
    grep -Eq '"format"[[:space:]]*:[[:space:]]*"qcow2"' <<<"$info" ||
        die "sealed base is not QCOW2"
    backing="$(grep -E '"backing-filename"' <<<"$info" || true)"
    [[ -z "$backing" ]] || die "sealed base unexpectedly has a backing file"
    grep -Eq '"dirty-flag"[[:space:]]*:[[:space:]]*false' <<<"$info" ||
        die "sealed base has a dirty flag"
    "$QEMU_IMG" check "$BASE"
    printf 'OK    %-24s flattened, clean, mode 0444\n' "sealed QCOW2"

    install -d -m 0700 "$RUNS"
}

append_manifest() {
    local manifest="$1"
    shift
    printf '%s\n' "$@" >>"$manifest"
}

new_run_id() {
    local suffix
    suffix="$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
    [[ "$suffix" =~ ^[0-9a-f]{8}$ ]] || die "could not generate run suffix"
    printf '%s-%s\n' "$(date -u +%Y%m%dT%H%M%SZ)" "$suffix"
}

run_sandbox() {
    local run_id="$1"
    local run="$2"
    local scenario="$3"
    local apk_fd="${4:-}"
    local sandbox_run="/lab/runs/$run_id"
    local -a benign_apk_bind=()
    local -a magstv_apk_bind=()

    case "$scenario" in
        baseline)
            [[ -z "$apk_fd" ]] ||
                die "baseline sandbox must not receive an APK descriptor"
            ;;
        benign)
            [[ "$apk_fd" =~ ^[0-9]+$ ]] ||
                die "benign sandbox requires an open APK descriptor"
            benign_apk_bind=(
                --perms 0400
                --ro-bind-data "$apk_fd" /input/benign-probe.apk
            )
            ;;
        magstv-offline)
            [[ "$apk_fd" =~ ^[0-9]+$ ]] ||
                die "MAGSTV offline sandbox requires an open APK descriptor"
            magstv_apk_bind=(
                --perms 0400
                --ro-bind-data "$apk_fd" /input/magstv-base.apk
            )
            ;;
        *)
            die "invalid disposable scenario: $scenario"
            ;;
    esac

    (
        # The parent keeps the lab lock for the whole run. Do not leak its
        # writable descriptor through bubblewrap into QEMU or guest tooling.
        exec {LAB_LOCK_FD}>&-
        exec "$BWRAP_BIN" \
        --unshare-user \
        --disable-userns \
        --unshare-pid \
        --unshare-ipc \
        --unshare-uts \
        --unshare-cgroup \
        --unshare-net \
        --die-with-parent \
        --new-session \
        --hostname magstv-offline \
        --clearenv \
        --cap-drop ALL \
        --proc /proc \
        --dev /dev \
        --tmpfs /tmp \
        --tmpfs /run \
        --ro-bind /usr /usr \
        --dir /etc \
        --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
        --ro-bind /etc/passwd /etc/passwd \
        --ro-bind /etc/group /etc/group \
        --ro-bind /etc/nsswitch.conf /etc/nsswitch.conf \
        --dir /etc/alternatives \
        --ro-bind /etc/alternatives /etc/alternatives \
        --ro-bind /sys /sys \
        --symlink usr/bin /bin \
        --symlink usr/lib /lib \
        --symlink usr/lib64 /lib64 \
        --symlink usr/sbin /sbin \
        --dir /lab \
        --dir /lab/immutable \
        --dir /lab/runs \
        --dir "$sandbox_run" \
        --ro-bind "$IMMUTABLE" /lab/immutable \
        --bind "$run" "$sandbox_run" \
        --dir /secrets \
        --ro-bind "$SSH_KEY" /secrets/lab_ed25519 \
        --ro-bind "$SSH_KNOWN_HOSTS" /secrets/ssh_known_hosts \
        --dir /input \
        --ro-bind "$REDROID_INIT" /input/zz-magstv-lab.rc \
        "${benign_apk_bind[@]}" \
        "${magstv_apk_bind[@]}" \
        --dir /runner \
        --ro-bind "$SELF" /runner/redroid-disposable-runner.sh \
        --setenv MAGSTV_REDROID_DISPOSABLE_SANDBOXED 1 \
        --setenv MAGSTV_DISPOSABLE_RUN_ID "$run_id" \
        --setenv HOME /nonexistent \
        --setenv USER labuser \
        --setenv LOGNAME labuser \
        --setenv LANG C.UTF-8 \
        --setenv LC_ALL C.UTF-8 \
        --setenv TZ UTC \
        --setenv PATH /usr/sbin:/usr/bin \
            --chdir "$sandbox_run" \
            -- /runner/redroid-disposable-runner.sh __sandboxed_boot "$scenario"
    )
}

host_boot() {
    local scenario="$1"
    local apk_fd="${2:-}"
    local run_id run overlay vars manifest sandbox_status=0
    local base_after open_pids overlay_info full_backing

    case "$scenario" in
        baseline)
            [[ -z "$apk_fd" ]] ||
                die "baseline scenario must not receive an APK descriptor"
            ;;
        benign)
            [[ "$apk_fd" =~ ^[0-9]+$ ]] ||
                die "benign scenario requires an APK descriptor"
            ;;
        magstv-offline)
            [[ "$apk_fd" =~ ^[0-9]+$ ]] ||
                die "MAGSTV offline scenario requires an APK descriptor"
            ;;
        *)
            die "invalid disposable scenario: $scenario"
            ;;
    esac
    host_preflight
    run_id="$(new_run_id)"
    run="$RUNS/$run_id"
    overlay="$run/root.qcow2"
    vars="$run/AAVMF_VARS.fd"
    manifest="$run/manifest.txt"

    [[ ! -e "$run" && ! -L "$run" ]] ||
        die "refusing to reuse run path: $run"
    install -d -m 0700 "$run"
    "$QEMU_IMG" create \
        -q \
        -f qcow2 \
        -F qcow2 \
        -b ../../immutable/redroid-base.qcow2 \
        "$overlay"
    install -m 0600 "$VARS_TEMPLATE" "$vars"

    overlay_info="$("$QEMU_IMG" info --output=json "$overlay")"
    grep -Fq '"backing-filename": "../../immutable/redroid-base.qcow2"' <<<"$overlay_info" ||
        die "fresh overlay does not use the expected relative backing path"
    full_backing="$("$QEMU_IMG" info --output=json "$overlay" |
        sed -n 's/.*"full-backing-filename": "\([^"]*\)".*/\1/p')"
    [[ "$(readlink -f -- "$full_backing")" == "$(readlink -f -- "$BASE")" ]] ||
        die "fresh overlay resolves to an unexpected backing file"

    {
        printf 'manifest_version=1\n'
        printf 'run_id=%s\n' "$run_id"
        printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        case "$scenario" in
            baseline)
                printf 'scenario=redroid_disposable_offline_baseline\n'
                printf 'apk_input=none\n'
                printf 'apk_execution_authorized=no\n'
                ;;
            benign)
                printf 'scenario=redroid_disposable_offline_benign_jni\n'
                printf 'apk_input=pinned_benign_probe\n'
                printf 'apk_execution_authorized=yes\n'
                printf 'apk_expected_sha256=%s\n' "$BENIGN_APK_SHA256"
                printf 'apk_expected_size=%s\n' "$BENIGN_APK_SIZE"
                printf 'apk_expected_package=%s\n' "$BENIGN_PACKAGE"
                printf 'apk_expected_component=%s\n' "$BENIGN_COMPONENT"
                printf 'apk_expected_jni_marker=%s\n' "$BENIGN_JNI_MARKER"
                printf 'apk_expected_jni_so_sha256=%s\n' "$BENIGN_SO_SHA256"
                ;;
            magstv-offline)
                printf 'scenario=redroid_disposable_offline_magstv_observation\n'
                printf 'apk_input=pinned_magstv_base\n'
                printf 'apk_execution_authorized=offline_observation_only\n'
                printf 'apk_expected_sha256=%s\n' "$MAGSTV_APK_SHA256"
                printf 'apk_expected_size=%s\n' "$MAGSTV_APK_SIZE"
                printf 'apk_expected_package=%s\n' "$MAGSTV_PACKAGE"
                printf 'apk_expected_version_name=%s\n' "$MAGSTV_VERSION_NAME"
                printf 'apk_expected_version_code=%s\n' "$MAGSTV_VERSION_CODE"
                printf 'apk_expected_component=%s\n' "$MAGSTV_COMPONENT"
                printf 'apk_expected_gomedia_service=%s\n' \
                    "$MAGSTV_GOMEDIA_SERVICE"
                printf 'apk_expected_gomedia_process=%s\n' \
                    "$MAGSTV_GOMEDIA_PROCESS"
                printf 'apk_expected_libexec_sha256=%s\n' \
                    "$MAGSTV_IJIAMI_LIBEXEC_SHA256"
                printf 'apk_expected_libexecmain_sha256=%s\n' \
                    "$MAGSTV_IJIAMI_LIBEXECMAIN_SHA256"
                printf 'apk_expected_libranger_jni_sha256=%s\n' \
                    "$MAGSTV_RANGER_JNI_SHA256"
                printf 'gomedia_manual_start_authorized=no\n'
                printf 'gomedia_autostart_policy=passive_observation_only\n'
                ;;
        esac
        printf 'credentials_present=no\n'
        printf 'network_namespace=unshared\n'
        printf 'qemu_user_network=restrict_on\n'
        printf 'qemu_user_network_ipv6=off\n'
        printf 'host_forward_scope=sandbox_loopback_2224_to_guest_22_only\n'
        printf 'base_sha256=%s\n' "$BASE_SHA256"
        printf 'base_backing=none\n'
        printf 'overlay_backing=../../immutable/redroid-base.qcow2\n'
        printf 'vars_template_sha256=%s\n' "$VARS_TEMPLATE_SHA256"
        printf 'result=pending\n'
    } >"$manifest"

    log "run $run_id: starting isolated disposable $scenario scenario"
    if run_sandbox "$run_id" "$run" "$scenario" "$apk_fd"; then
        sandbox_status=0
    else
        sandbox_status=$?
    fi

    open_pids="$(inode_open_pids "$BASE" 2>/dev/null || true)"
    if [[ -n "$open_pids" ]]; then
        append_manifest "$manifest" \
            "postrun_base_open_pids=$open_pids" \
            "postrun_qemu_img_check=skipped"
        sandbox_status=1
    elif "$QEMU_IMG" check "$overlay" >"$run/qemu-img-check.txt" 2>&1; then
        append_manifest "$manifest" "postrun_qemu_img_check=no_errors"
    else
        append_manifest "$manifest" "postrun_qemu_img_check=failed"
        sandbox_status=1
    fi

    base_after="$(sha256_of "$BASE")"
    append_manifest "$manifest" \
        "postrun_base_sha256=$base_after" \
        "finished_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if [[ "$base_after" != "$BASE_SHA256" ]]; then
        append_manifest "$manifest" "base_unchanged=no"
        sandbox_status=1
    else
        append_manifest "$manifest" "base_unchanged=yes"
    fi

    if [[ "$sandbox_status" -eq 0 ]]; then
        append_manifest "$manifest" "result=pass"
        log "run $run_id: PASS; evidence retained at $run"
    else
        append_manifest "$manifest" "result=fail"
        log "run $run_id: FAIL; evidence retained at $run"
    fi
    return "$sandbox_status"
}

sandbox_run_dir() {
    local run_id="${MAGSTV_DISPOSABLE_RUN_ID:-}"

    [[ "$run_id" =~ ^[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$ ]] ||
        die "invalid sandbox run id"
    printf '/lab/runs/%s\n' "$run_id"
}

declare -a SSH_OPTIONS=(
    -F /dev/null
    -i /secrets/lab_ed25519
    -p "$SSH_PORT"
    -o BatchMode=yes
    -o IdentitiesOnly=yes
    -o StrictHostKeyChecking=yes
    -o UserKnownHostsFile=/secrets/ssh_known_hosts
    -o GlobalKnownHostsFile=/dev/null
    -o ConnectTimeout=10
    -o ConnectionAttempts=1
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=2
    -o LogLevel=ERROR
)

ssh_guest() {
    local duration="$1"
    shift
    timeout --foreground --kill-after=5 "$duration" \
        ssh "${SSH_OPTIONS[@]}" lab@127.0.0.1 "$@"
}

sandbox_bootstrap_redroid() {
    ssh_guest 2100 "bash -s" <<'REMOTE'
set -Eeuo pipefail
set +x

log() {
    printf '%s guest: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

remote_error() {
    local status=$?
    printf 'REMOTE_ERROR line=%s status=%s command=%q\n' \
        "$1" "$status" "$2" >&2
    exit "$status"
}
trap 'remote_error "$LINENO" "$BASH_COMMAND"' ERR

[[ "$(uname -m)" == "aarch64" ]]
services_ready=0
for attempt in $(seq 1 180); do
    if sudo timeout --foreground --kill-after=2 8 systemctl is-active --quiet docker &&
        sudo timeout --foreground --kill-after=2 8 systemctl is-active --quiet containerd &&
        sudo timeout --foreground --kill-after=2 8 systemctl is-active --quiet redroid-binderfs.service &&
        mountpoint -q /dev/binderfs &&
        [[ -c /dev/binderfs/binder ]] &&
        [[ -c /dev/binderfs/hwbinder ]] &&
        [[ -c /dev/binderfs/vndbinder ]]; then
        services_ready=1
        break
    fi
    if ((attempt % 12 == 0)); then
        log "waiting for Docker, containerd, and BinderFS (${attempt}/180)"
    fi
    sleep 5
done
[[ "$services_ready" == "1" ]]
log "Docker, containerd, and BinderFS are ready"
printf '%s\n' 'guest_ipv4_routes_begin'
ip -4 route show table all | sed -n '1,200p'
printf '%s\n' 'guest_ipv4_routes_end'
printf '%s\n' 'guest_ipv6_routes_begin'
ip -6 route show table all | sed -n '1,200p'
printf '%s\n' 'guest_ipv6_routes_end'
guest_ipv4_default_routes="$(
    ip -4 route show table all |
        awk '$1 == "default" {count++} END {print count + 0}'
)"
guest_ipv6_default_routes="$(
    ip -6 route show table all |
        awk '$1 == "default" {count++} END {print count + 0}'
)"
if [[ "$guest_ipv4_default_routes" != "0" ||
    "$guest_ipv6_default_routes" != "0" ]]; then
    printf 'ERROR: guest unexpectedly has a default route (IPv4=%s, IPv6=%s)\n' \
        "$guest_ipv4_default_routes" "$guest_ipv6_default_routes" >&2
    exit 1
fi
if timeout --foreground --kill-after=2 6 \
    bash -c '</dev/tcp/1.1.1.1/443' >/dev/null 2>&1; then
    guest_tcp_status=0
else
    guest_tcp_status=$?
fi
case "$guest_tcp_status" in
    1|124|137) ;;
    *) printf 'ERROR: invalid guest TCP isolation result: %s\n' "$guest_tcp_status" >&2; exit 1 ;;
esac
log "guest network isolation passed (TCP status $guest_tcp_status)"

image_id="$(sudo docker image inspect --format '{{.Id}}' redroid/redroid:8.1.0-latest)"
log "Docker reports pinned Redroid image ID $image_id"
case "$image_id" in
    sha256:8b95febfd6ef411bb73cad0b6f30ae3ec10f2216c8f8a58052417ef6792fc8b5|\
    sha256:c38107720ad923a0aa1379412b4a53d2e5c5a192663cbd2bd0657e4d521b89f3)
        ;;
    *)
        printf 'ERROR: unexpected Redroid Docker image ID: %s\n' "$image_id" >&2
        exit 1
        ;;
esac
image_arch="$(sudo docker image inspect --format '{{.Architecture}}' "$image_id")"
[[ "$image_arch" == "arm64" ]]
log "pinned Redroid image is present"
if sudo docker container inspect redroid-clean1 >/dev/null 2>&1; then
    printf 'ERROR: sealed baseline unexpectedly contains redroid-clean1\n' >&2
    exit 1
fi

if sudo docker network inspect redroid-isolated >/dev/null 2>&1; then
    [[ "$(sudo docker network inspect --format '{{.Internal}}' redroid-isolated)" == "true" ]]
    attached="$(
        sudo docker network inspect \
            --format '{{len .Containers}}' redroid-isolated
    )"
    [[ "$attached" == "0" ]]
    log "existing internal Docker network is clean"
else
    sudo docker network create \
        --driver bridge \
        --internal \
        --opt com.docker.network.bridge.enable_ip_masquerade=false \
        redroid-isolated >/dev/null
    log "created internal Docker network"
fi

sudo install -d -m 0700 /var/lib/redroid-data-disposable
source_hash="$(sudo sha256sum /var/lib/redroid-tools/zz-magstv-lab.rc | awk '{print $1}')"
[[ "$source_hash" == "c6c28632167102d0234c604381dd9873f4f9ac82f1ad2824d8bdc6f493e0d563" ]]
log "starting disposable Redroid container"

container_id="$(
    sudo docker run -d \
        --name redroid-clean1 \
        --pull never \
        --restart no \
        --network redroid-isolated \
        --privileged \
        --cpus 4 \
        --memory 8g \
        --pids-limit 4096 \
        --log-driver local \
        --log-opt max-size=20m \
        --log-opt max-file=2 \
        --mount type=bind,src=/var/lib/redroid-data-disposable,dst=/data \
        --mount type=bind,src=/var/lib/redroid-tools/zz-magstv-lab.rc,dst=/system/etc/init/zz-magstv-lab.rc,readonly \
        redroid/redroid:8.1.0-latest \
        androidboot.use_memfd=true \
        androidboot.redroid_gpu_mode=guest
)"
log "Redroid container created: ${container_id:0:12}"

android_container_pid() {
    sudo docker inspect -f '{{.State.Pid}}' redroid-clean1
}

android_nsenter() {
    local duration="$1"
    shift
    local container_pid
    container_pid="$(android_container_pid)"
    [[ "$container_pid" =~ ^[0-9]+$ && "$container_pid" != "0" ]]
    sudo timeout --foreground --kill-after=5 "$duration" \
        nsenter -t "$container_pid" -m -p -u -i -n -- "$@"
}

boot_completed=""
for attempt in $(seq 1 180); do
    boot_completed="$(
        android_nsenter 20 /system/bin/getprop sys.boot_completed \
            2>/dev/null |
            tr -d '\r' ||
            true
    )"
    [[ "$boot_completed" == "1" ]] && break
    state="$(sudo docker inspect --format '{{.State.Status}}' redroid-clean1)"
    [[ "$state" == "running" ]]
    if ((attempt % 12 == 0)); then
        log "waiting for Android boot (${attempt}/180)"
    fi
    sleep 5
done
[[ "$boot_completed" == "1" ]]

state="$(sudo docker inspect --format '{{.State.Status}}' redroid-clean1)"
restarts="$(sudo docker inspect --format '{{.RestartCount}}' redroid-clean1)"
[[ "$state" == "running" ]]
[[ "$restarts" == "0" ]]
container_image_id="$(
    sudo docker inspect --format '{{.Image}}' redroid-clean1
)"
case "$container_image_id" in
    sha256:8b95febfd6ef411bb73cad0b6f30ae3ec10f2216c8f8a58052417ef6792fc8b5|\
    sha256:c38107720ad923a0aa1379412b4a53d2e5c5a192663cbd2bd0657e4d521b89f3)
        ;;
    *)
        printf 'ERROR: unexpected container image ID: %s\n' \
            "$container_image_id" >&2
        exit 1
        ;;
esac
network_set="$(
    sudo docker inspect \
        --format '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' \
        redroid-clean1 |
        sed '/^[[:space:]]*$/d' |
        LC_ALL=C sort
)"
[[ "$network_set" == "redroid-isolated" ]]
gateway="$(
    sudo docker inspect \
        --format '{{with index .NetworkSettings.Networks "redroid-isolated"}}{{.Gateway}}{{end}}' \
        redroid-clean1
)"
if [[ -n "$gateway" ]]; then
    gateway_metadata=present
else
    gateway_metadata=absent
fi
printf '%s\n' 'redroid_ipv4_route_table_begin'
android_nsenter 30 /system/bin/sh -c \
    '[ -r /proc/net/route ] && /system/bin/toybox cat /proc/net/route || printf "unreadable\n"' |
    sed -n '1,200p'
printf '%s\n' 'redroid_ipv4_route_table_end'
printf '%s\n' 'redroid_ipv6_route_table_begin'
android_nsenter 30 /system/bin/sh -c \
    '[ -r /proc/net/ipv6_route ] && /system/bin/toybox cat /proc/net/ipv6_route || printf "unreadable\n"' |
    sed -n '1,200p'
printf '%s\n' 'redroid_ipv6_route_table_end'
default_routes="$(
    android_nsenter 30 /system/bin/toybox cat /proc/net/route |
        awk 'NR > 1 && $2 == "00000000" { count++ } END { print count + 0 }'
)"
[[ "$default_routes" == "0" ]]

if timeout --foreground --kill-after=2 6 \
    android_nsenter 6 /system/bin/toybox nc -w 3 1.1.1.1 443 \
        </dev/null >/dev/null 2>&1; then
    android_tcp_status=0
else
    android_tcp_status=$?
fi
case "$android_tcp_status" in
    1|124|137) ;;
    *) printf 'ERROR: invalid Android TCP isolation result: %s\n' "$android_tcp_status" >&2; exit 1 ;;
esac

container_init_hash="$(
    android_nsenter 30 /system/bin/toybox sha256sum \
        /system/etc/init/zz-magstv-lab.rc |
        awk '{print $1}'
)"
[[ "$container_init_hash" == "$source_hash" ]]
mmap_bits="$(
    android_nsenter 30 /system/bin/toybox cat \
        /proc/sys/vm/mmap_rnd_compat_bits |
        tr -d '\r'
)"
aslr="$(
    android_nsenter 30 /system/bin/toybox cat \
        /proc/sys/kernel/randomize_va_space |
        tr -d '\r'
)"
dalvik_opts="$(
    android_nsenter 30 /system/bin/getprop dalvik.vm.extra-opts |
        tr -d '\r'
)"
release="$(
    android_nsenter 30 /system/bin/getprop ro.build.version.release |
        tr -d '\r'
)"
sdk="$(
    android_nsenter 30 /system/bin/getprop ro.build.version.sdk |
        tr -d '\r'
)"
abi="$(
    android_nsenter 30 /system/bin/getprop ro.product.cpu.abilist |
        tr -d '\r'
)"
[[ "$mmap_bits" == "15" ]]
[[ "$aslr" == "2" ]]
[[ "$dalvik_opts" == "-Xnorelocate" ]]
[[ "$release" == "8.1.0" ]]
[[ "$sdk" == "27" ]]
[[ "$abi" == "arm64-v8a,armeabi-v7a,armeabi" ]]

printf 'ubuntu_arch=aarch64\n'
printf 'guest_ipv4_default_route_count=%s\n' "$guest_ipv4_default_routes"
printf 'guest_ipv6_default_route_count=%s\n' "$guest_ipv6_default_routes"
printf 'guest_tcp_probe_status=%s\n' "$guest_tcp_status"
printf 'redroid_image_id=%s\n' "$image_id"
printf 'redroid_container_image_id=%s\n' "$container_image_id"
printf 'redroid_image_arch=%s\n' "$image_arch"
printf 'redroid_state=%s\n' "$state"
printf 'redroid_restarts=%s\n' "$restarts"
printf 'redroid_network=%s\n' "$network_set"
printf 'redroid_network_internal=true\n'
printf 'redroid_gateway_metadata=%s\n' "$gateway_metadata"
printf 'redroid_default_route_count=%s\n' "$default_routes"
printf 'redroid_tcp_probe_status=%s\n' "$android_tcp_status"
printf 'redroid_init_sha256=%s\n' "$container_init_hash"
printf 'android_release=%s\n' "$release"
printf 'android_sdk=%s\n' "$sdk"
printf 'android_abi=%s\n' "$abi"
printf 'android_boot_completed=%s\n' "$boot_completed"
printf 'mmap_rnd_compat_bits=%s\n' "$mmap_bits"
printf 'randomize_va_space=%s\n' "$aslr"
printf 'dalvik_vm_extra_opts=%s\n' "$dalvik_opts"
REMOTE
}

sandbox_egress_gate() {
    local phase="$1"
    local route_count route6_count sandbox_tcp_status remote_status=0 status=0

    printf 'phase=%s\n' "$phase"
    printf '%s\n' 'sandbox_ipv4_routes_begin'
    ip -4 route show table all | sed -n '1,200p'
    printf '%s\n' 'sandbox_ipv4_routes_end'
    printf '%s\n' 'sandbox_ipv6_routes_begin'
    ip -6 route show table all | sed -n '1,200p'
    printf '%s\n' 'sandbox_ipv6_routes_end'
    route_count="$(
        ip -4 route show table all |
            awk '$1 == "default" {count++} END {print count + 0}'
    )"
    printf 'sandbox_default_route_count=%s\n' "$route_count"
    if [[ "$route_count" != "0" ]]; then
        status=1
    fi
    route6_count="$(
        ip -6 route show table all |
            awk '$1 == "default" {count++} END {print count + 0}'
    )"
    printf 'sandbox_ipv6_default_route_count=%s\n' "$route6_count"
    if [[ "$route6_count" != "0" ]]; then
        status=1
    fi

    if timeout --foreground --kill-after=2 6 \
        bash -c '</dev/tcp/1.1.1.1/443' >/dev/null 2>&1; then
        sandbox_tcp_status=0
    else
        sandbox_tcp_status=$?
    fi
    printf 'sandbox_tcp_probe_status=%s\n' "$sandbox_tcp_status"
    case "$sandbox_tcp_status" in
        1|124|137) ;;
        *) status=1 ;;
    esac

    if ssh_guest 180 "bash -s" <<'REMOTE'
set -Eeuo pipefail
set +x

printf '%s\n' 'guest_ipv4_routes_begin'
ip -4 route show table all | sed -n '1,200p'
printf '%s\n' 'guest_ipv4_routes_end'
printf '%s\n' 'guest_ipv6_routes_begin'
ip -6 route show table all | sed -n '1,200p'
printf '%s\n' 'guest_ipv6_routes_end'
guest_default_routes="$(
    ip -4 route show table all |
        awk '$1 == "default" {count++} END {print count + 0}'
)"
[[ "$guest_default_routes" == "0" ]]
guest_ipv6_default_routes="$(
    ip -6 route show table all |
        awk '$1 == "default" {count++} END {print count + 0}'
)"
[[ "$guest_ipv6_default_routes" == "0" ]]
if timeout --foreground --kill-after=2 6 \
    bash -c '</dev/tcp/1.1.1.1/443' >/dev/null 2>&1; then
    guest_tcp_status=0
else
    guest_tcp_status=$?
fi
case "$guest_tcp_status" in
    1|124|137) ;;
    *) exit 1 ;;
esac

[[ "$(sudo docker inspect --format '{{.State.Status}}' redroid-clean1)" == "running" ]]
[[ "$(sudo docker network inspect --format '{{.Internal}}' redroid-isolated)" == "true" ]]
network_set="$(
    sudo docker inspect \
        --format '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' \
        redroid-clean1 |
        sed '/^[[:space:]]*$/d' |
        LC_ALL=C sort
)"
[[ "$network_set" == "redroid-isolated" ]]
gateway="$(
    sudo docker inspect \
        --format '{{with index .NetworkSettings.Networks "redroid-isolated"}}{{.Gateway}}{{end}}' \
        redroid-clean1
)"
if [[ -n "$gateway" ]]; then
    gateway_metadata=present
else
    gateway_metadata=absent
fi
printf '%s\n' 'redroid_ipv4_route_table_begin'
container_pid="$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1)"
sudo nsenter -t "$container_pid" -m -p -u -i -n -- /system/bin/sh -c \
    '[ -r /proc/net/route ] && /system/bin/toybox cat /proc/net/route || printf "unreadable\n"' |
    sed -n '1,200p'
printf '%s\n' 'redroid_ipv4_route_table_end'
printf '%s\n' 'redroid_ipv6_route_table_begin'
sudo nsenter -t "$container_pid" -m -p -u -i -n -- /system/bin/sh -c \
    '[ -r /proc/net/ipv6_route ] && /system/bin/toybox cat /proc/net/ipv6_route || printf "unreadable\n"' |
    sed -n '1,200p'
printf '%s\n' 'redroid_ipv6_route_table_end'
android_default_routes="$(
    sudo nsenter -t "$container_pid" -m -p -u -i -n -- /system/bin/toybox cat /proc/net/route |
        awk 'NR > 1 && $2 == "00000000" {count++} END {print count + 0}'
)"
[[ "$android_default_routes" == "0" ]]
if timeout --foreground --kill-after=2 6 \
    sudo nsenter -t "$container_pid" -m -p -u -i -n -- \
        /system/bin/toybox nc -w 3 1.1.1.1 443 \
        </dev/null >/dev/null 2>&1; then
    android_tcp_status=0
else
    android_tcp_status=$?
fi
case "$android_tcp_status" in
    1|124|137) ;;
    *) exit 1 ;;
esac

printf 'guest_default_route_count=%s\n' "$guest_default_routes"
printf 'guest_ipv6_default_route_count=%s\n' "$guest_ipv6_default_routes"
printf 'guest_tcp_probe_status=%s\n' "$guest_tcp_status"
printf 'redroid_network=%s\n' "$network_set"
printf 'redroid_network_internal=true\n'
printf 'redroid_gateway_metadata=%s\n' "$gateway_metadata"
printf 'redroid_default_route_count=%s\n' "$android_default_routes"
printf 'redroid_tcp_probe_status=%s\n' "$android_tcp_status"
REMOTE
    then
        remote_status=0
    else
        remote_status=$?
        status=1
    fi
    printf 'remote_gate_status=%s\n' "$remote_status"
    return "$status"
}

sandbox_run_benign_probe() {
    local run="$1"
    local manifest="$run/manifest.txt"
    local status=0 can_continue=1 launch_attempted=0
    local tombstones_before_valid=0
    local input_hash input_size container_hash package_path resolved_component
    local launch_status=0 launch_output pid_raw first_pid pid_after so_path so_hash
    local container_state container_restarts boot_completed
    local preinstall_status=0 pid_deadline
    local pid_query_state=unknown pidof_status=""
    local pid_after_state=unknown pid_after_status=""

    append_manifest "$manifest" \
        "benign_started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "benign_egress_before_attempted=yes"
    if sandbox_egress_gate before >"$run/egress-before.txt" 2>&1; then
        append_manifest "$manifest" \
            "benign_egress_before=pass" \
            "egress_before=pass"
    else
        append_manifest "$manifest" \
            "benign_egress_before=fail" \
            "egress_before=fail"
        status=1
        can_continue=0
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_preinstall_check_attempted=yes"
        if ssh_guest 180 "bash -s" \
            >"$run/apk-preinstall-path.txt" 2>&1 <<'REMOTE'
set -Eeuo pipefail
set +x
set +e
output="$(
    sudo docker exec redroid-clean1 \
        /system/bin/sh /system/bin/pm \
        list packages --user 0 lab.jellyrin.benignprobe 2>&1
)"
pm_status=$?
set -e
printf '%s\n' "$output"
printf 'pm_exit_status=%s\n' "$pm_status"
[[ "$pm_status" -eq 0 ]] || exit "$pm_status"
if [[ "$output" == "package:lab.jellyrin.benignprobe" ]]; then
    exit 10
fi
[[ -z "$output" ]] || exit 11
exit 0
REMOTE
        then
            append_manifest "$manifest" "benign_preinstall_package_absent=yes"
        else
            preinstall_status=$?
            if [[ "$preinstall_status" -eq 10 ]]; then
                append_manifest "$manifest" \
                    "benign_preinstall_package_absent=no" \
                    "benign_preinstall_package_present=yes" \
                    "benign_preinstall_probe_status=$preinstall_status"
            else
                append_manifest "$manifest" \
                    "benign_preinstall_package_absent=unknown" \
                    "benign_preinstall_package_present=unknown" \
                    "benign_preinstall_probe_status=$preinstall_status"
            fi
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        input_hash="$(sha256_of /input/benign-probe.apk)"
        input_size="$(stat -c '%s' /input/benign-probe.apk)"
        {
            printf 'sha256=%s\n' "$input_hash"
            printf 'size=%s\n' "$input_size"
        } >"$run/apk-sandbox-revalidation.txt"
        if [[ "$input_hash" == "$BENIGN_APK_SHA256" &&
            "$input_size" == "$BENIGN_APK_SIZE" ]]; then
            append_manifest "$manifest" \
                "benign_pretransfer_sha256=$input_hash" \
                "benign_pretransfer_size=$input_size" \
                "benign_pretransfer_revalidation=pass"
        else
            append_manifest "$manifest" "benign_pretransfer_revalidation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_transfer_attempted=yes"
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox mkdir -p /data/local/tmp && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox rm -f /data/local/tmp/benign-probe.apk" \
            >"$run/apk-staging-prepare.txt" 2>&1 &&
            tar \
                --format=ustar \
                --owner=0 \
                --group=0 \
                --numeric-owner \
                -C /input \
                -cf - benign-probe.apk |
                ssh_guest 180 \
                    "sudo docker cp - redroid-clean1:/data/local/tmp" \
                    >"$run/apk-copy.txt" 2>&1; then
            append_manifest "$manifest" "benign_transfer=pass"
        else
            append_manifest "$manifest" "benign_transfer=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_container_hash_attempted=yes"
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox chmod 0644 /data/local/tmp/benign-probe.apk && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox sha256sum /data/local/tmp/benign-probe.apk" \
            >"$run/apk-container-sha256.txt" 2>&1; then
            container_hash="$(
                awk 'NR == 1 {print $1}' "$run/apk-container-sha256.txt"
            )"
            if [[ "$container_hash" == "$BENIGN_APK_SHA256" ]]; then
                append_manifest "$manifest" \
                    "benign_container_sha256=$container_hash" \
                    "benign_container_hash=pass"
            else
                append_manifest "$manifest" \
                    "benign_container_sha256=$container_hash" \
                    "benign_container_hash=fail"
                status=1
                can_continue=0
            fi
        else
            append_manifest "$manifest" "benign_container_hash=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_install_attempted=yes"
        if ssh_guest 900 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo timeout --foreground --kill-after=10 840 nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/sh /system/bin/pm install --user 0 /data/local/tmp/benign-probe.apk" \
            >"$run/apk-install.txt" 2>&1 &&
            [[ "$(tr -d '\r' <"$run/apk-install.txt")" == "Success" ]]; then
            append_manifest "$manifest" "benign_install_command=pass"
        else
            # A transport timeout does not prove that Package Manager failed
            # before committing the install.  Keep the state unknown until
            # the independent `pm path` observation below.
            append_manifest "$manifest" "benign_install_command=fail_or_unknown"
            status=1
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_package_validation_attempted=yes"
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/sh /system/bin/pm path --user 0 $BENIGN_PACKAGE" \
            >"$run/apk-package-path.txt" 2>&1; then
            package_path="$(tr -d '\r' <"$run/apk-package-path.txt")"
            if [[ "$package_path" != *$'\n'* &&
                "$package_path" =~ ^package:/data/app/.+/base\.apk$ ]]; then
                append_manifest "$manifest" \
                    "apk_installed=yes" \
                    "benign_installed_package=$BENIGN_PACKAGE" \
                    "benign_package_validation=pass"
            else
                append_manifest "$manifest" \
                    "apk_installed=unknown" \
                    "benign_package_validation=fail"
                status=1
                can_continue=0
            fi
        else
            append_manifest "$manifest" \
                "apk_installed=unknown" \
                "benign_package_validation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_package_dump_attempted=yes"
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/dumpsys package $BENIGN_PACKAGE" \
            >"$run/apk-package-dump.txt" 2>&1 &&
            grep -Eq 'versionCode=1([[:space:]]|$)' "$run/apk-package-dump.txt" &&
            grep -Eq 'versionName=1\.0([[:space:]]|$)' "$run/apk-package-dump.txt" &&
            grep -Eq '^[[:space:]]*primaryCpuAbi=arm64-v8a([[:space:]]|$)' \
                "$run/apk-package-dump.txt"; then
            append_manifest "$manifest" \
                "benign_version_code=1" \
                "benign_version_name=1.0" \
                "benign_primary_cpu_abi=arm64-v8a" \
                "benign_package_dump_validation=pass"
        else
            append_manifest "$manifest" "benign_package_dump_validation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "benign_component_validation_attempted=yes"
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/cmd package resolve-activity --brief --user 0 -a android.intent.action.MAIN -c android.intent.category.LAUNCHER $BENIGN_PACKAGE" \
            >"$run/apk-resolved-component.txt" 2>&1; then
            resolved_component="$(
                tr -d '\r' <"$run/apk-resolved-component.txt" |
                    awk 'NF {value=$0} END {print value}'
            )"
            if [[ "$resolved_component" == "$BENIGN_COMPONENT" ]]; then
                append_manifest "$manifest" \
                    "benign_resolved_component=$resolved_component" \
                    "benign_component_validation=pass"
            else
                append_manifest "$manifest" \
                    "benign_resolved_component=$resolved_component" \
                    "benign_component_validation=fail"
                status=1
                can_continue=0
            fi
        else
            append_manifest "$manifest" "benign_component_validation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        if ! ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox ls -lan /data/tombstones | LC_ALL=C sort" \
            >"$run/tombstones-before.txt" 2>"$run/tombstones-before.stderr"; then
            append_manifest "$manifest" "benign_tombstones_before_collection=fail"
            status=1
        else
            tombstones_before_valid=1
            append_manifest "$manifest" "benign_tombstones_before_collection=pass"
        fi
        append_manifest "$manifest" \
            "benign_launch_attempted=yes" \
            "apk_launch_attempted=yes"
        launch_attempted=1
        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/am force-stop --user 0 $BENIGN_PACKAGE && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/logcat -c" \
            >"$run/apk-pre-launch.txt" 2>&1; then
            append_manifest "$manifest" "benign_pre_launch=pass"
        else
            append_manifest "$manifest" "benign_pre_launch=fail"
            status=1
        fi
        if ssh_guest 360 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo timeout --foreground --kill-after=10 300 nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/am start -W --user 0 -n $BENIGN_COMPONENT" \
            >"$run/apk-launch.txt" 2>&1; then
            launch_status=0
        else
            launch_status=$?
        fi
        launch_output="$(tr -d '\r' <"$run/apk-launch.txt")"
        if [[ "$launch_status" -eq 0 ]] &&
            grep -Fxq "Status: ok" <<<"$launch_output" &&
            grep -Fxq "Activity: $BENIGN_COMPONENT" <<<"$launch_output"; then
            append_manifest "$manifest" "benign_launch_command=pass"
        else
            append_manifest "$manifest" \
                "benign_launch_command_status=$launch_status" \
                "benign_launch_command=fail"
            status=1
        fi

        pid_raw=""
        pid_deadline=$((SECONDS + 120))
        while ((SECONDS < pid_deadline)); do
            if ssh_guest 15 "bash -s" \
                >"$run/apk-pid.txt" 2>"$run/apk-pid.stderr" <<'REMOTE'
set -Eeuo pipefail
set +x
set +e
pid="$(
    container_pid="$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1)"
    sudo nsenter -t "$container_pid" -m -p -u -i -n -- \
        /system/bin/toybox pidof lab.jellyrin.benignprobe 2>&1
)"
pidof_status=$?
set -e
printf 'pidof_status=%s\n' "$pidof_status"
printf 'pid=%s\n' "$pid"
case "$pidof_status" in
    0|1) exit 0 ;;
    *) exit "$pidof_status" ;;
esac
REMOTE
            then
                pidof_status="$(
                    sed -n 's/^pidof_status=//p' "$run/apk-pid.txt" |
                        tail -n 1
                )"
                pid_raw="$(
                    sed -n 's/^pid=//p' "$run/apk-pid.txt" |
                        tail -n 1 |
                        tr -d '\r\n'
                )"
                if [[ "$pidof_status" == "0" &&
                    "$pid_raw" =~ ^[1-9][0-9]*$ ]]; then
                    pid_query_state=yes
                    break
                elif [[ "$pidof_status" == "1" && -z "$pid_raw" ]]; then
                    pid_query_state=no
                    pid_raw=""
                else
                    pid_query_state=unknown
                    pid_raw=""
                fi
            else
                pid_query_state=unknown
                pid_raw=""
            fi
            ((SECONDS < pid_deadline)) || break
            sleep 5
        done

        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/logcat -d -v threadtime" \
            >"$run/apk-logcat.txt" 2>"$run/apk-logcat.stderr"; then
            if grep -Fq "$BENIGN_JNI_MARKER" "$run/apk-logcat.txt" &&
                grep -Fq "JellyrinBenignProbe" "$run/apk-logcat.txt"; then
                append_manifest "$manifest" \
                    "benign_jni_marker=$BENIGN_JNI_MARKER" \
                    "benign_jni_marker_validation=pass" \
                    "jni_marker_seen=yes"
            else
                append_manifest "$manifest" \
                    "benign_jni_marker_validation=fail" \
                    "jni_marker_seen=no"
                status=1
            fi
            if grep -Eq \
                'UnsatisfiedLinkError|dlopen failed|FATAL EXCEPTION|Fatal signal [0-9]+|SIGSEGV|SIGABRT|ANR in ' \
                "$run/apk-logcat.txt"; then
                append_manifest "$manifest" "benign_logcat_crash_markers=present"
                status=1
            else
                append_manifest "$manifest" "benign_logcat_crash_markers=absent"
            fi
        else
            append_manifest "$manifest" \
                "benign_logcat_collection=fail" \
                "jni_marker_seen=unknown"
            status=1
        fi

        if [[ "$pid_raw" =~ ^[1-9][0-9]*$ ]]; then
            first_pid="$pid_raw"
            append_manifest "$manifest" \
                "benign_process_pid=$first_pid" \
                "benign_process_running=yes"
            if ! ssh_guest 180 \
                "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/ps -A" \
                >"$run/apk-processes.txt" 2>"$run/apk-processes.stderr"; then
                append_manifest "$manifest" "benign_process_list_collection=fail"
                status=1
            fi
            if ssh_guest 180 \
                "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox cat /proc/$first_pid/maps" \
                >"$run/apk-maps.txt" 2>"$run/apk-maps.stderr" &&
                grep -Fq "$BENIGN_JNI_LIBRARY" "$run/apk-maps.txt" &&
                grep -Fq "/arm64/$BENIGN_JNI_LIBRARY" "$run/apk-maps.txt"; then
                append_manifest "$manifest" \
                    "benign_jni_library=$BENIGN_JNI_LIBRARY" \
                    "benign_jni_maps_validation=pass"
                so_path="$(
                    awk -v library="$BENIGN_JNI_LIBRARY" \
                        '$NF ~ ("/arm64/" library "$") {print $NF; exit}' \
                        "$run/apk-maps.txt"
                )"
                if [[ "$so_path" =~ ^/data/app/[A-Za-z0-9._=+/-]+/lib/arm64/libbenign_probe\.so$ ]] &&
                    ssh_guest 180 \
                        "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox sha256sum $so_path" \
                        >"$run/apk-jni-so-sha256.txt" 2>&1; then
                    so_hash="$(awk 'NR == 1 {print $1}' "$run/apk-jni-so-sha256.txt")"
                else
                    so_hash=""
                fi
                if [[ "$so_hash" == "$BENIGN_SO_SHA256" ]]; then
                    append_manifest "$manifest" \
                        "benign_jni_so_sha256=$so_hash" \
                        "benign_jni_so_hash_validation=pass"
                else
                    append_manifest "$manifest" "benign_jni_so_hash_validation=fail"
                    status=1
                fi
            else
                append_manifest "$manifest" "benign_jni_maps_validation=fail"
                status=1
            fi
            if ssh_guest 180 "bash -s" \
                >"$run/apk-pid-after.txt" 2>"$run/apk-pid-after.stderr" <<'REMOTE'
set -Eeuo pipefail
set +x
set +e
pid="$(
    container_pid="$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1)"
    sudo nsenter -t "$container_pid" -m -p -u -i -n -- \
        /system/bin/toybox pidof lab.jellyrin.benignprobe 2>&1
)"
pidof_status=$?
set -e
printf 'pidof_status=%s\n' "$pidof_status"
printf 'pid=%s\n' "$pid"
case "$pidof_status" in
    0|1) exit 0 ;;
    *) exit "$pidof_status" ;;
esac
REMOTE
            then
                pid_after_status="$(
                    sed -n 's/^pidof_status=//p' "$run/apk-pid-after.txt" |
                        tail -n 1
                )"
                pid_after="$(
                    sed -n 's/^pid=//p' "$run/apk-pid-after.txt" |
                        tail -n 1 |
                        tr -d '\r\n'
                )"
                if [[ "$pid_after_status" == "0" &&
                    "$pid_after" =~ ^[1-9][0-9]*$ ]]; then
                    if [[ "$pid_after" == "$first_pid" ]]; then
                        pid_after_state=yes
                    else
                        pid_after_state=replaced
                    fi
                elif [[ "$pid_after_status" == "1" && -z "$pid_after" ]]; then
                    pid_after_state=no
                else
                    pid_after_state=unknown
                fi
            else
                pid_after_state=unknown
            fi
            case "$pid_after_state" in
                yes)
                    append_manifest "$manifest" "benign_process_still_running=yes"
                    ;;
                no)
                    append_manifest "$manifest" "benign_process_still_running=no"
                    status=1
                    ;;
                replaced)
                    append_manifest "$manifest" \
                        "benign_process_still_running=no" \
                        "benign_process_replaced_pid=$pid_after"
                    status=1
                    ;;
                *)
                    append_manifest "$manifest" "benign_process_still_running=unknown"
                    status=1
                    ;;
            esac
        else
            append_manifest "$manifest" "benign_process_running=$pid_query_state"
            status=1
        fi

        if ssh_guest 180 \
            "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/toybox ls -lan /data/tombstones | LC_ALL=C sort" \
            >"$run/tombstones-after.txt" 2>"$run/tombstones-after.stderr" &&
            [[ "$tombstones_before_valid" -eq 1 ]] &&
            cmp -s "$run/tombstones-before.txt" "$run/tombstones-after.txt"; then
            append_manifest "$manifest" "benign_tombstones_changed=no"
        else
            append_manifest "$manifest" "benign_tombstones_changed=yes_or_unknown"
            status=1
        fi

        if ssh_guest 180 "bash -s" \
            >"$run/apk-runtime-state.txt" 2>"$run/apk-runtime-state.stderr" <<'REMOTE'
set -Eeuo pipefail
state="$(sudo docker inspect --format '{{.State.Status}}' redroid-clean1)"
restarts="$(sudo docker inspect --format '{{.RestartCount}}' redroid-clean1)"
boot_completed="$(
    container_pid="$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1)"
    sudo nsenter -t "$container_pid" -m -p -u -i -n -- /system/bin/getprop sys.boot_completed |
        tr -d '\r'
)"
printf 'state=%s\n' "$state"
printf 'restarts=%s\n' "$restarts"
printf 'boot_completed=%s\n' "$boot_completed"
[[ "$state" == "running" ]]
[[ "$restarts" == "0" ]]
[[ "$boot_completed" == "1" ]]
REMOTE
        then
            container_state="$(
                sed -n 's/^state=//p' "$run/apk-runtime-state.txt"
            )"
            container_restarts="$(
                sed -n 's/^restarts=//p' "$run/apk-runtime-state.txt"
            )"
            boot_completed="$(
                sed -n 's/^boot_completed=//p' "$run/apk-runtime-state.txt"
            )"
            append_manifest "$manifest" \
                "benign_redroid_state=$container_state" \
                "benign_redroid_restarts=$container_restarts" \
                "benign_android_boot_completed=$boot_completed" \
                "benign_runtime_state_validation=pass"
        else
            append_manifest "$manifest" "benign_runtime_state_validation=fail"
            status=1
        fi
    fi

    if [[ "$launch_attempted" -eq 0 ]]; then
        append_manifest "$manifest" \
            "benign_launch_attempted=no" \
            "apk_launch_attempted=no" \
            "jni_marker_seen=not_attempted"
    fi
    append_manifest "$manifest" "benign_egress_after_attempted=yes"
    if sandbox_egress_gate after >"$run/egress-after.txt" 2>&1; then
        append_manifest "$manifest" \
            "benign_egress_after=pass" \
            "egress_after=pass"
    else
        append_manifest "$manifest" \
            "benign_egress_after=fail" \
            "egress_after=fail"
        status=1
    fi
    append_manifest "$manifest" \
        "benign_finished_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    return "$status"
}

sandbox_run_magstv_offline() {
    local run="$1"
    local manifest="$run/manifest.txt"
    local status=0 can_continue=1 remote_status=0 evidence_status=0
    local input_hash input_size input_mode input_mount_options container_hash
    local result_file="$run/magstv-result.txt"
    local observer_dir="$run/magstv-observer"
    local evidence_tar="$run/magstv-observer.tar"
    local evidence_members="$run/magstv-observer-members.txt"
    local evidence_verbose="$run/magstv-observer-members-verbose.txt"
    local evidence_tar_limit=268435456
    local tar_source_status=1 tar_sink_status=1
    local evidence_tar_size=0 evidence_member_count=0
    local remote_complete=no remote_quality=fail
    local observer_evidence=unknown evidence_archive=unknown
    local install_attempted=no install_command_status=not_attempted
    local launch_attempted=no launch_command_status=not_attempted
    local apk_installed=unknown installed_base_hash_matches=unknown
    local package_version_name_matches=unknown package_version_code_matches=unknown
    local package_primary_abi_arm64=unknown resolved_welcome_activity=unknown
    local welcome_activity_started=unknown
    local ijiami_assets_pinned=unknown main_process_seen=unknown
    local main_process_survived_window=unknown process_abi_arm64=unknown
    local map_evidence_complete=unknown
    local ijiami_libexec_extracted=unknown ijiami_libexec_hash_matches=unknown
    local ijiami_libexecmain_extracted=unknown
    local ijiami_libexecmain_hash_matches=unknown
    local ijiami_libexec_mapped=unknown libranger_jni_mapped=unknown
    local libranger_jni_hash_matches=unknown ijiami_jni_registration=unknown
    local gomedia_declared=unknown gomedia_process_seen=unknown
    local gomedia_active_seen=unknown app_fatal_seen=unknown
    local tombstones_changed=unknown package_socket_seen=unknown
    local logcat_evidence_complete=unknown
    local observer_completed=unknown subject_outcome=inconclusive
    local summary_valid=0 key value
    local -a tristate_keys=(
        apk_installed
        installed_base_hash_matches
        package_version_name_matches
        package_version_code_matches
        package_primary_abi_arm64
        resolved_welcome_activity
        welcome_activity_started
        ijiami_assets_pinned
        main_process_seen
        main_process_survived_window
        process_abi_arm64
        map_evidence_complete
        ijiami_libexec_extracted
        ijiami_libexec_hash_matches
        ijiami_libexecmain_extracted
        ijiami_libexecmain_hash_matches
        ijiami_libexec_mapped
        libranger_jni_mapped
        libranger_jni_hash_matches
        ijiami_jni_registration
        gomedia_declared
        gomedia_process_seen
        gomedia_active_seen
        app_fatal_seen
        logcat_evidence_complete
        tombstones_changed
        package_socket_seen
        observer_completed
        observer_evidence
    )

    # Subject failures and timeouts are evidence, not reasons to skip the
    # mandatory post-observation egress gate. Every critical command below
    # is captured explicitly and this function returns the accumulated state.
    set +e

    summary_value() {
        local wanted="$1"
        local count

        count="$(grep -c "^${wanted}=" "$result_file" 2>/dev/null || true)"
        [[ "$count" == "1" ]] || return 1
        sed -n "s/^${wanted}=//p" "$result_file"
    }

    append_manifest "$manifest" \
        "magstv_started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "magstv_credentials_supplied=no" \
        "gomedia_manual_start_attempted=no" \
        "gomedia_autostart_observation=passive_only" \
        "magstv_egress_before_attempted=yes"
    if sandbox_egress_gate before >"$run/magstv-egress-before.txt" 2>&1; then
        append_manifest "$manifest" \
            "magstv_egress_before=pass" \
            "egress_before=pass"
    else
        append_manifest "$manifest" \
            "magstv_egress_before=fail" \
            "egress_before=fail"
        status=1
        can_continue=0
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        input_hash="$(sha256_of /input/magstv-base.apk)"
        input_size="$(stat -c '%s' /input/magstv-base.apk)"
        input_mode="$(stat -c '%a' /input/magstv-base.apk)"
        input_mount_options="$(findmnt -n -o OPTIONS -T /input/magstv-base.apk)"
        {
            printf 'sha256=%s\n' "$input_hash"
            printf 'size=%s\n' "$input_size"
            printf 'mode=%s\n' "$input_mode"
            printf 'mount_options=%s\n' "$input_mount_options"
        } >"$run/magstv-sandbox-revalidation.txt"
        if [[ "$input_hash" == "$MAGSTV_APK_SHA256" &&
            "$input_size" == "$MAGSTV_APK_SIZE" &&
            "$input_mode" == "400" &&
            ",$input_mount_options," == *,ro,* ]]; then
            append_manifest "$manifest" "magstv_pretransfer_revalidation=pass"
        else
            append_manifest "$manifest" "magstv_pretransfer_revalidation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        append_manifest "$manifest" "magstv_transfer_attempted=yes"
        if ssh_guest 180 \
            "sudo docker exec redroid-clean1 /system/bin/toybox mkdir -p /data/local/tmp && sudo docker exec redroid-clean1 /system/bin/toybox rm -f /data/local/tmp/jellyrin-magstv-pinned.apk" \
            >"$run/magstv-staging-prepare.txt" 2>&1 &&
            tar \
                --format=ustar \
                --owner=0 \
                --group=0 \
                --numeric-owner \
                -C /input \
                -cf - magstv-base.apk |
                ssh_guest 300 \
                    "sudo docker cp - redroid-clean1:/data/local/tmp" \
                    >"$run/magstv-copy.txt" 2>&1 &&
            ssh_guest 180 "bash -s" \
                >"$run/magstv-staging-rename.txt" 2>&1 <<'REMOTE'
set -Eeuo pipefail
sudo docker exec redroid-clean1 /system/bin/toybox \
    mv /data/local/tmp/magstv-base.apk /data/local/tmp/jellyrin-magstv-pinned.apk
sudo docker exec redroid-clean1 /system/bin/toybox \
    chmod 0644 /data/local/tmp/jellyrin-magstv-pinned.apk
REMOTE
        then
            append_manifest "$manifest" "magstv_transfer=pass"
        else
            append_manifest "$manifest" "magstv_transfer=fail"
            status=1
            can_continue=0
        fi
    else
        append_manifest "$manifest" "magstv_transfer_attempted=no"
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        if ssh_guest 180 "bash -s" \
            >"$run/magstv-container-revalidation.txt" 2>&1 <<'REMOTE'
set -Eeuo pipefail
path=/data/local/tmp/jellyrin-magstv-pinned.apk
hash="$(
    sudo docker exec redroid-clean1 /system/bin/toybox sha256sum "$path" |
        awk '{print $1}'
)"
size="$(
    sudo docker exec redroid-clean1 /system/bin/toybox stat -c '%s' "$path" |
        tr -d '\r'
)"
printf 'sha256=%s\n' "$hash"
printf 'size=%s\n' "$size"
[[ "$hash" == "2b098adf19eab4ac0eaf11501ebf386561677b2c95cc1f0499811bb81a058bb5" ]]
[[ "$size" == "35272343" ]]
REMOTE
        then
            container_hash="$(
                sed -n 's/^sha256=//p' "$run/magstv-container-revalidation.txt"
            )"
            input_hash="$(sha256_of /input/magstv-base.apk)"
            input_size="$(stat -c '%s' /input/magstv-base.apk)"
            if [[ "$container_hash" == "$MAGSTV_APK_SHA256" &&
                "$input_hash" == "$MAGSTV_APK_SHA256" &&
                "$input_size" == "$MAGSTV_APK_SIZE" ]]; then
                append_manifest "$manifest" \
                    "magstv_container_revalidation=pass" \
                    "magstv_posttransfer_input_revalidation=pass"
            else
                append_manifest "$manifest" \
                    "magstv_container_revalidation=fail" \
                    "magstv_posttransfer_input_revalidation=fail"
                status=1
                can_continue=0
            fi
        else
            append_manifest "$manifest" "magstv_container_revalidation=fail"
            status=1
            can_continue=0
        fi
    fi

    if [[ "$can_continue" -eq 1 ]]; then
        if ssh_guest 2700 "sudo bash -s" \
            >"$result_file" 2>"$run/magstv-result.stderr" <<'REMOTE'
set -uo pipefail
set +x
umask 077

readonly OBS="/var/lib/jellyrin-magstv-observation"
readonly PACKAGE="com.android.mgstv"
readonly COMPONENT="com.android.mgstv/com.interactive.brasiliptv.ui.activity.WelcomeActivity"
readonly GOMEDIA_SERVICE="com.main.service.GoMediaService"
readonly GOMEDIA_PROCESS="com.android.mgstv:gomediad"
readonly APK="/data/local/tmp/jellyrin-magstv-pinned.apk"
readonly APK_SHA256="2b098adf19eab4ac0eaf11501ebf386561677b2c95cc1f0499811bb81a058bb5"
readonly APK_SIZE="35272343"
readonly VERSION_NAME="4.34.5"
readonly VERSION_CODE="43405"
readonly LIBEXEC_SHA256="512180fa7a5981837bf101474ea76168965dfa2bc367f141f750ac6e17fb7bae"
readonly LIBEXECMAIN_SHA256="a0864c7be8520aca7e76377cb87542c446fd1704214e6626ee715afedd2a1ee1"
readonly RANGER_SHA256="be0bbd0bc7b09ff35141465721dde19e6b025483a1b88bf58ed9ef670bbd19db"

quality=pass
evidence_complete=yes
apk_install_attempted=no
apk_install_command_status=not_attempted
apk_launch_attempted=no
apk_launch_command_status=not_attempted
apk_installed=unknown
installed_base_hash_matches=unknown
package_version_name_matches=unknown
package_version_code_matches=unknown
package_primary_abi_arm64=unknown
resolved_welcome_activity=unknown
welcome_activity_started=unknown
ijiami_assets_pinned=unknown
main_process_seen=unknown
main_process_survived_window=unknown
process_abi_arm64=unknown
map_evidence_complete=unknown
ijiami_libexec_extracted=unknown
ijiami_libexec_hash_matches=unknown
ijiami_libexecmain_extracted=unknown
ijiami_libexecmain_hash_matches=unknown
ijiami_libexec_mapped=unknown
libranger_jni_mapped=unknown
libranger_jni_hash_matches=unknown
ijiami_jni_registration=unknown
gomedia_declared=unknown
gomedia_process_seen=unknown
gomedia_active_seen=unknown
app_fatal_seen=unknown
logcat_evidence_complete=unknown
tombstones_changed=unknown
package_socket_seen=unknown
observer_completed=unknown
observer_evidence=unknown
subject_outcome=inconclusive
package_valid=no
preinstall_clean=no
base_path=""
package_uid=""
install_status=not_attempted
launch_status=not_attempted

quality_fail() {
    quality=fail
}

capture_or_mark() {
    local output="$1"
    local capture_status
    shift
    if "$@" >"$OBS/$output" 2>"$OBS/$output.stderr"; then
        return 0
    else
        capture_status=$?
    fi
    printf '%s capture_failed=%s\n' "$output" "$capture_status" \
        >>"$OBS/capture-failures.txt"
    evidence_complete=no
    quality_fail
    return 1
}

snapshot_sockets() {
    local output="$1"
    docker exec redroid-clean1 /system/bin/sh -c '
        for table in /proc/net/tcp /proc/net/tcp6 /proc/net/udp /proc/net/udp6; do
            printf "===== %s =====\n" "$table"
            if [ -r "$table" ]; then
                /system/bin/toybox cat "$table"
            else
                printf "unreadable\n"
            fi
        done
    ' >"$OBS/$output" 2>"$OBS/$output.stderr"
}

snapshot_tombstones() {
    local output="$1"
    docker exec redroid-clean1 /system/bin/sh -c '
        found=0
        for file in /data/tombstones/tombstone_*; do
            [ -f "$file" ] || continue
            found=1
            /system/bin/toybox sha256sum "$file"
        done
        [ "$found" -eq 1 ] || printf "none\n"
    ' | LC_ALL=C sort >"$OBS/$output" 2>"$OBS/$output.stderr"
}

if [[ -e "$OBS" || -L "$OBS" ]]; then
    printf 'ERROR: observation directory already exists\n' >&2
    exit 70
fi
install -d -m 0700 "$OBS"

capture_or_mark pre-packages.txt \
    docker exec redroid-clean1 \
    /system/bin/sh /system/bin/pm list packages --user 0 || true
capture_or_mark pre-processes.txt \
    docker exec redroid-clean1 /system/bin/ps -A || true
capture_or_mark pre-services.txt \
    docker exec redroid-clean1 /system/bin/dumpsys activity services || true
if ! snapshot_sockets pre-sockets.txt; then
    evidence_complete=no
    quality_fail
fi
if ! snapshot_tombstones tombstones-before-install.txt; then
    evidence_complete=no
    quality_fail
fi

if docker exec redroid-clean1 \
    /system/bin/sh /system/bin/pm list packages --user 0 "$PACKAGE" \
    >"$OBS/pre-package-filter.txt" 2>"$OBS/pre-package-filter.stderr"; then
    pre_package="$(
        tr -d '\r' <"$OBS/pre-package-filter.txt" |
            sed '/^[[:space:]]*$/d'
    )"
    if [[ -z "$pre_package" ]]; then
        pre_package_absent=yes
    elif [[ "$pre_package" == "package:$PACKAGE" ]]; then
        pre_package_absent=no
        quality_fail
    else
        pre_package_absent=unknown
        quality_fail
    fi
else
    pre_package_absent=unknown
    quality_fail
fi
if docker exec redroid-clean1 /system/bin/sh -c \
    'test ! -e /data/user/0/com.android.mgstv && test ! -e /data/data/com.android.mgstv' \
    >"$OBS/pre-data-check.txt" 2>"$OBS/pre-data-check.stderr"; then
    pre_data_absent=yes
else
    pre_data_absent=no
    quality_fail
fi
if [[ "$pre_package_absent" == "yes" && "$pre_data_absent" == "yes" ]]; then
    preinstall_clean=yes
fi
printf 'pre_package_absent=%s\npre_data_absent=%s\n' \
    "$pre_package_absent" "$pre_data_absent" >"$OBS/preinstall-state.txt"

if docker exec redroid-clean1 /system/bin/toybox sha256sum "$APK" \
    >"$OBS/staged-hash.txt" 2>"$OBS/staged-hash.stderr"; then
    staged_hash_status=0
else
    staged_hash_status=$?
fi
staged_hash="$(awk 'NR == 1 {print $1}' "$OBS/staged-hash.txt")"
if docker exec redroid-clean1 /system/bin/toybox stat -c '%s' "$APK" \
    >"$OBS/staged-size.txt" 2>"$OBS/staged-size.stderr"; then
    staged_size_status=0
else
    staged_size_status=$?
fi
staged_size="$(tr -d '\r' <"$OBS/staged-size.txt")"
printf 'sha256=%s\nsize=%s\n' "$staged_hash" "$staged_size" \
    >"$OBS/staged-apk-identity.txt"
if [[ "$staged_hash_status" -ne 0 ||
    "$staged_size_status" -ne 0 ||
    "$staged_hash" != "$APK_SHA256" ||
    "$staged_size" != "$APK_SIZE" ]]; then
    quality_fail
fi

if [[ "$preinstall_clean" == "yes" &&
    "$staged_hash" == "$APK_SHA256" &&
    "$staged_size" == "$APK_SIZE" ]]; then
    apk_install_attempted=yes
    timeout --foreground --kill-after=30 1800 \
        docker exec redroid-clean1 \
        /system/bin/sh /system/bin/pm install --user 0 "$APK" \
        >"$OBS/install.txt" 2>"$OBS/install.stderr"
    install_status=$?
    apk_install_command_status=$install_status
    printf '%s\n' "$install_status" >"$OBS/install.status"
    sleep 5
fi

observe_package_state() {
    local suffix="$1"
    local path_status list_status path_output list_output

    docker exec redroid-clean1 \
        /system/bin/sh /system/bin/pm path --user 0 "$PACKAGE" \
        >"$OBS/pm-path-$suffix.txt" 2>"$OBS/pm-path-$suffix.stderr"
    path_status=$?
    docker exec redroid-clean1 \
        /system/bin/sh /system/bin/pm list packages --user 0 "$PACKAGE" \
        >"$OBS/pm-list-$suffix.txt" 2>"$OBS/pm-list-$suffix.stderr"
    list_status=$?
    path_output="$(
        tr -d '\r' <"$OBS/pm-path-$suffix.txt" |
            sed '/^[[:space:]]*$/d'
    )"
    list_output="$(
        tr -d '\r' <"$OBS/pm-list-$suffix.txt" |
            sed '/^[[:space:]]*$/d'
    )"
    printf 'path_status=%s\nlist_status=%s\npath=%s\nlist=%s\n' \
        "$path_status" "$list_status" "$path_output" "$list_output" \
        >"$OBS/package-state-$suffix.txt"
    if [[ "$path_status" -eq 0 &&
        "$list_status" -eq 0 &&
        "$list_output" == "package:$PACKAGE" &&
        "$path_output" != *$'\n'* &&
        "$path_output" =~ ^package:/data/app/[A-Za-z0-9._=+/-]+/base\.apk$ ]]; then
        base_path="${path_output#package:}"
        apk_installed=yes
        return 0
    fi
    if [[ "$path_status" -eq 0 &&
        "$list_status" -eq 0 &&
        -z "$path_output" &&
        -z "$list_output" ]]; then
        apk_installed=no
        return 1
    fi
    apk_installed=unknown
    return 2
}

observe_package_state first
package_state_status=$?
if [[ "$apk_installed" == "no" ]]; then
    sleep 10
    observe_package_state second
    package_state_status=$?
fi

if [[ "$apk_installed" == "yes" ]]; then
    if docker exec redroid-clean1 /system/bin/toybox sha256sum "$base_path" \
        >"$OBS/installed-base-hash.txt" \
        2>"$OBS/installed-base-hash.stderr"; then
        installed_hash_status=0
    else
        installed_hash_status=$?
    fi
    installed_hash="$(
        awk 'NR == 1 {print $1}' "$OBS/installed-base-hash.txt"
    )"
    if docker exec redroid-clean1 /system/bin/toybox stat -c '%s' "$base_path" \
        >"$OBS/installed-base-size.txt" \
        2>"$OBS/installed-base-size.stderr"; then
        installed_size_status=0
    else
        installed_size_status=$?
    fi
    installed_size="$(tr -d '\r' <"$OBS/installed-base-size.txt")"
    printf 'path=%s\nsha256=%s\nsize=%s\n' \
        "$base_path" "$installed_hash" "$installed_size" \
        >"$OBS/installed-base-identity.txt"
    if [[ "$installed_hash_status" -eq 0 &&
        "$installed_size_status" -eq 0 &&
        "$installed_hash" == "$APK_SHA256" &&
        "$installed_size" == "$APK_SIZE" ]]; then
        installed_base_hash_matches=yes
        ijiami_assets_pinned=yes
    elif [[ "$installed_hash_status" -eq 0 &&
        "$installed_size_status" -eq 0 ]]; then
        installed_base_hash_matches=no
        ijiami_assets_pinned=no
        quality_fail
    else
        installed_base_hash_matches=unknown
        ijiami_assets_pinned=unknown
        quality_fail
    fi

    if docker exec redroid-clean1 /system/bin/dumpsys package "$PACKAGE" \
        >"$OBS/package-dump.txt" 2>"$OBS/package-dump.stderr"; then
        version_name_actual="$(
            sed -n 's/^[[:space:]]*versionName=//p' \
                "$OBS/package-dump.txt" |
                head -n 1
        )"
        version_code_actual="$(
            sed -n \
                's/.*versionCode=\([0-9][0-9]*\).*/\1/p' \
                "$OBS/package-dump.txt" |
                head -n 1
        )"
        if [[ "$version_name_actual" == "$VERSION_NAME" ]]; then
            package_version_name_matches=yes
        else
            package_version_name_matches=no
        fi
        if [[ "$version_code_actual" == "$VERSION_CODE" ]]; then
            package_version_code_matches=yes
        else
            package_version_code_matches=no
        fi
        if grep -Eq '^[[:space:]]*primaryCpuAbi=arm64-v8a([[:space:]]|$)' \
            "$OBS/package-dump.txt"; then
            package_primary_abi_arm64=yes
        else
            package_primary_abi_arm64=no
        fi
        if grep -Fq "$GOMEDIA_SERVICE" "$OBS/package-dump.txt"; then
            gomedia_declared=yes
        else
            gomedia_declared=no
        fi
        package_uid="$(
            sed -n 's/^[[:space:]]*userId=\([0-9][0-9]*\).*$/\1/p' \
                "$OBS/package-dump.txt" |
                head -n 1
        )"
        if [[ ! "$package_uid" =~ ^[0-9]+$ ]]; then
            package_uid="$(
                docker exec redroid-clean1 /system/bin/toybox \
                    stat -c '%u' "/data/user/0/$PACKAGE" 2>/dev/null |
                    tr -d '\r'
            )"
        fi
    else
        package_version_name_matches=unknown
        package_version_code_matches=unknown
        package_primary_abi_arm64=unknown
        gomedia_declared=unknown
        quality_fail
    fi

    if docker exec redroid-clean1 /system/bin/cmd package resolve-activity \
        --brief --user 0 \
        -a android.intent.action.MAIN \
        -c android.intent.category.LEANBACK_LAUNCHER \
        "$PACKAGE" \
        >"$OBS/resolved-component.txt" 2>"$OBS/resolved-component.stderr"; then
        resolved_component="$(
            tr -d '\r' <"$OBS/resolved-component.txt" |
                awk 'NF {value=$0} END {print value}'
        )"
        if [[ "$resolved_component" == "$COMPONENT" ]]; then
            resolved_welcome_activity=yes
        else
            resolved_welcome_activity=no
        fi
    else
        resolved_welcome_activity=unknown
    fi

    if [[ "$preinstall_clean" == "yes" &&
        "$apk_install_attempted" == "yes" &&
        "$apk_installed" == "yes" &&
        "$installed_base_hash_matches" == "yes" &&
        "$package_version_name_matches" == "yes" &&
        "$package_version_code_matches" == "yes" &&
        "$package_primary_abi_arm64" == "yes" &&
        "$resolved_welcome_activity" == "yes" ]]; then
        package_valid=yes
    else
        package_valid=no
        quality_fail
    fi
    if [[ ! "$package_uid" =~ ^[0-9]+$ ]]; then
        quality_fail
    fi
elif [[ "$apk_installed" == "no" ]]; then
    installed_base_hash_matches=unknown
    package_version_name_matches=unknown
    package_version_code_matches=unknown
    package_primary_abi_arm64=unknown
    resolved_welcome_activity=unknown
    ijiami_assets_pinned=unknown
    gomedia_declared=unknown
    if [[ "$apk_install_attempted" == "yes" &&
        "$install_status" =~ ^[0-9]+$ &&
        "$install_status" -ne 0 &&
        "$install_status" -ne 124 &&
        "$install_status" -ne 137 ]]; then
        subject_outcome=install_failed_definitive
    else
        quality_fail
    fi
else
    quality_fail
fi

observer() {
    local start now elapsed sample=0 sleep_for process_rows row pid cmd
    local process_scan_fail=0 service_scan_fail=0 socket_scan_fail=0
    local tombstone_scan_fail=0 map_attempts=0 map_successes=0
    local map_limit_hit=0 observation_timeout=0
    local launch_finished_seen=0 launch_finished_at=0 post_launch_elapsed=0
    local services_file sockets_file maps_file
    declare -A first_maps=()

    printf 'started_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        >"$OBS/observer-state.txt"
    : >"$OBS/process-samples.txt"
    : >"$OBS/service-samples.txt"
    : >"$OBS/socket-samples.txt"
    : >"$OBS/tombstone-samples.txt"
    : >"$OBS/maps-relevant-samples.txt"
    : >"$OBS/observer-ready"
    start="$(date +%s)"
    while :; do
        now="$(date +%s)"
        elapsed=$((now - start))
        if [[ "$launch_finished_seen" -eq 0 &&
            -f "$OBS/observer-launch-finished" ]]; then
            launch_finished_seen=1
            launch_finished_at="$now"
        fi
        if [[ "$launch_finished_seen" -eq 1 ]]; then
            post_launch_elapsed=$((now - launch_finished_at))
            ((post_launch_elapsed < 180)) || break
        fi
        if ((elapsed >= 390)); then
            observation_timeout=1
            break
        fi
        sample=$((sample + 1))
        printf 'sample=%s elapsed=%s utc=%s\n' \
            "$sample" "$elapsed" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            >>"$OBS/process-samples.txt"
        process_rows="$(
            docker exec redroid-clean1 /system/bin/sh -c '
                for proc in /proc/[0-9]*; do
                    [ -r "$proc/cmdline" ] || continue
                    cmd="$(
                        /system/bin/toybox tr "\000" " " <"$proc/cmdline" \
                            2>/dev/null
                    )"
                    cmd="${cmd%% *}"
                    case "$cmd" in
                        com.android.mgstv|com.android.mgstv:*)
                            printf "%s|%s\n" "${proc##*/}" "$cmd"
                            ;;
                    esac
                done
            ' 2>"$OBS/process-scan-$sample.stderr"
        )"
        process_status=$?
        if [[ "$process_status" -ne 0 ]]; then
            process_scan_fail=1
        fi
        if [[ -n "$process_rows" ]]; then
            printf '%s\n' "$process_rows" >>"$OBS/process-samples.txt"
        fi
        while IFS='|' read -r pid cmd; do
            [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
            [[ "$cmd" == "$PACKAGE" || "$cmd" == "$PACKAGE":* ]] || continue
            : >"$OBS/flag-package-process-seen"
            [[ "$cmd" == "$PACKAGE" ]] && : >"$OBS/flag-main-process-seen"
            [[ "$cmd" == "$GOMEDIA_PROCESS" ]] &&
                : >"$OBS/flag-gomedia-process-seen"
            if [[ -z "${first_maps[$pid]+x}" ]]; then
                if ((${#first_maps[@]} < 128)); then
                    first_maps["$pid"]=1
                    docker exec redroid-clean1 /system/bin/toybox \
                        cat "/proc/$pid/maps" \
                        2>"$OBS/maps-$pid-first.stderr" |
                        head -c 1048576 >"$OBS/maps-$pid-first.txt"
                else
                    map_limit_hit=1
                fi
            fi
            map_attempts=$((map_attempts + 1))
            maps_file="$OBS/maps-$pid-relevant-$sample.txt"
            if docker exec redroid-clean1 /system/bin/toybox \
                cat "/proc/$pid/maps" 2>"$maps_file.stderr" |
                grep -E 'libexec(main)?\.so|libranger-jni\.so|/system/lib64/|/system/bin/linker64' \
                >"$maps_file"; then
                map_successes=$((map_successes + 1))
                {
                    printf 'sample=%s pid=%s cmd=%s\n' "$sample" "$pid" "$cmd"
                    cat "$maps_file"
                } >>"$OBS/maps-relevant-samples.txt"
            else
                map_status=${PIPESTATUS[0]}
                if [[ "$map_status" -eq 0 ]]; then
                    map_successes=$((map_successes + 1))
                fi
            fi
        done <<<"$process_rows"

        services_file="$OBS/services-$sample.txt"
        if docker exec redroid-clean1 /system/bin/dumpsys activity services "$PACKAGE" \
            >"$services_file" 2>"$services_file.stderr"; then
            {
                printf 'sample=%s elapsed=%s\n' "$sample" "$elapsed"
                cat "$services_file"
            } >>"$OBS/service-samples.txt"
            if grep -Fq "$GOMEDIA_SERVICE" "$services_file"; then
                : >"$OBS/flag-gomedia-active-seen"
            fi
        else
            service_scan_fail=1
        fi

        sockets_file="$OBS/sockets-$sample.txt"
        if docker exec redroid-clean1 /system/bin/sh -c '
            for table in /proc/net/tcp /proc/net/tcp6 /proc/net/udp /proc/net/udp6; do
                printf "===== %s =====\n" "$table"
                [ -r "$table" ] && /system/bin/toybox cat "$table"
            done
        ' >"$sockets_file" 2>"$sockets_file.stderr"; then
            {
                printf 'sample=%s elapsed=%s\n' "$sample" "$elapsed"
                cat "$sockets_file"
            } >>"$OBS/socket-samples.txt"
            if awk -v uid="$package_uid" '
                /^===== / {next}
                NR > 1 && $8 == uid {found=1}
                END {exit(found ? 0 : 1)}
            ' "$sockets_file"; then
                : >"$OBS/flag-package-socket-seen"
            fi
        else
            socket_scan_fail=1
        fi

        if docker exec redroid-clean1 /system/bin/sh -c '
            for file in /data/tombstones/tombstone_*; do
                [ -f "$file" ] || continue
                /system/bin/toybox sha256sum "$file"
            done
        ' >"$OBS/tombstones-$sample.txt" 2>"$OBS/tombstones-$sample.stderr"; then
            {
                printf 'sample=%s elapsed=%s\n' "$sample" "$elapsed"
                cat "$OBS/tombstones-$sample.txt"
            } >>"$OBS/tombstone-samples.txt"
        else
            tombstone_scan_fail=1
        fi

        if ((elapsed < 20 ||
            (launch_finished_seen == 1 && post_launch_elapsed < 20) )); then
            sleep_for=1
        else
            sleep_for=5
        fi
        sleep "$sleep_for"
    done

    if docker exec redroid-clean1 /system/bin/sh -c '
        for proc in /proc/[0-9]*; do
            [ -r "$proc/cmdline" ] || continue
            cmd="$(
                /system/bin/toybox tr "\000" " " <"$proc/cmdline" 2>/dev/null
            )"
            cmd="${cmd%% *}"
            case "$cmd" in
                com.android.mgstv|com.android.mgstv:*)
                    printf "%s|%s\n" "${proc##*/}" "$cmd"
                    ;;
            esac
        done
    ' >"$OBS/processes-final-package.txt" \
        2>"$OBS/processes-final-package.stderr"; then
        if awk -F'|' -v package="$PACKAGE" '$2 == package {found=1} END {exit(found ? 0 : 1)}' \
            "$OBS/processes-final-package.txt"; then
            : >"$OBS/flag-main-process-survived"
        fi
    else
        process_scan_fail=1
    fi

    printf 'samples=%s\nmap_attempts=%s\nmap_successes=%s\n' \
        "$sample" "$map_attempts" "$map_successes" \
        >>"$OBS/observer-state.txt"
    printf 'process_scan_fail=%s\nservice_scan_fail=%s\nsocket_scan_fail=%s\n' \
        "$process_scan_fail" "$service_scan_fail" "$socket_scan_fail" \
        >>"$OBS/observer-state.txt"
    printf 'tombstone_scan_fail=%s\nfinished_at=%s\n' \
        "$tombstone_scan_fail" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        >>"$OBS/observer-state.txt"
    printf 'map_limit_hit=%s\n' "$map_limit_hit" \
        >>"$OBS/observer-state.txt"
    printf 'launch_finished_seen=%s\npost_launch_observation_seconds=%s\n' \
        "$launch_finished_seen" "$post_launch_elapsed" \
        >>"$OBS/observer-state.txt"
    printf 'observation_timeout=%s\n' "$observation_timeout" \
        >>"$OBS/observer-state.txt"
    if [[ "$process_scan_fail" -eq 0 &&
        "$service_scan_fail" -eq 0 &&
        "$socket_scan_fail" -eq 0 &&
        "$tombstone_scan_fail" -eq 0 &&
        "$map_limit_hit" -eq 0 &&
        "$launch_finished_seen" -eq 1 &&
        "$post_launch_elapsed" -ge 180 &&
        "$observation_timeout" -eq 0 &&
        "$sample" -gt 0 ]]; then
        printf 'observer_completed=yes\n' >>"$OBS/observer-state.txt"
        return 0
    fi
    printf 'observer_completed=no\n' >>"$OBS/observer-state.txt"
    return 1
}

if [[ "$package_valid" == "yes" ]]; then
    if docker exec redroid-clean1 /system/bin/sh -c '
        for proc in /proc/[0-9]*; do
            [ -r "$proc/cmdline" ] || continue
            cmd="$(
                /system/bin/toybox tr "\000" " " <"$proc/cmdline" 2>/dev/null
            )"
            cmd="${cmd%% *}"
            case "$cmd" in
                com.android.mgstv|com.android.mgstv:*)
                    printf "%s|%s\n" "${proc##*/}" "$cmd"
                    ;;
            esac
        done
    ' >"$OBS/processes-before-launch.txt" \
        2>"$OBS/processes-before-launch.stderr"; then
        if [[ -s "$OBS/processes-before-launch.txt" ]]; then
            quality_fail
        fi
    else
        quality_fail
    fi
    if ! docker exec redroid-clean1 /system/bin/am force-stop --user 0 "$PACKAGE" \
        >"$OBS/force-stop-before.txt" 2>"$OBS/force-stop-before.stderr"; then
        quality_fail
    fi
    if ! docker exec redroid-clean1 /system/bin/logcat -c \
        >"$OBS/logcat-clear.txt" 2>"$OBS/logcat-clear.stderr"; then
        quality_fail
    fi
    if ! snapshot_tombstones tombstones-before-launch.txt; then
        evidence_complete=no
        quality_fail
    fi

    (
        set +e
        set -o pipefail
        timeout --foreground --kill-after=10 390 \
            docker exec redroid-clean1 /system/bin/logcat -v threadtime \
            2>"$OBS/logcat-stream.stderr" |
            head -c 33554432
        stream_statuses=("${PIPESTATUS[@]}")
        printf 'source_status=%s\nsink_status=%s\n' \
            "${stream_statuses[0]}" "${stream_statuses[1]}" \
            >"$OBS/logcat-stream.status"
        exit 0
    ) >"$OBS/logcat-stream.txt" &
    logcat_pid=$!
    observer &
    observer_pid=$!
    observer_ready=no
    for _ in $(seq 1 40); do
        if [[ -f "$OBS/observer-ready" ]]; then
            observer_ready=yes
            break
        fi
        kill -0 "$observer_pid" 2>/dev/null || break
        sleep 0.5
    done
    if [[ "$observer_ready" == "yes" ]]; then
        apk_launch_attempted=yes
        timeout --foreground --kill-after=10 180 \
            docker exec redroid-clean1 /system/bin/am start -W \
            --user 0 -n "$COMPONENT" \
            >"$OBS/launch.txt" 2>"$OBS/launch.stderr"
        launch_status=$?
        apk_launch_command_status=$launch_status
        printf '%s\n' "$launch_status" >"$OBS/launch.status"
        tr -d '\r' <"$OBS/launch.txt" >"$OBS/launch-normalized.txt"
        if [[ "$launch_status" -eq 0 ]] &&
            grep -Fxq 'Status: ok' "$OBS/launch-normalized.txt" &&
            grep -Fxq "Activity: $COMPONENT" \
                "$OBS/launch-normalized.txt"; then
            welcome_activity_started=yes
        elif [[ "$launch_status" -eq 124 || "$launch_status" -eq 137 ]]; then
            welcome_activity_started=unknown
        else
            welcome_activity_started=no
        fi
    else
        quality_fail
    fi
    printf '%s\n' "$(date +%s)" >"$OBS/observer-launch-finished"
    wait "$observer_pid"
    observer_status=$?
    if [[ "$observer_status" -eq 0 ]]; then
        observer_completed=yes
        observer_evidence=yes
    else
        observer_completed=no
        observer_evidence=no
        quality_fail
    fi
    wait "$logcat_pid" 2>/dev/null || true
    logcat_source_status="$(
        sed -n 's/^source_status=//p' "$OBS/logcat-stream.status" |
            tail -n 1
    )"
    logcat_sink_status="$(
        sed -n 's/^sink_status=//p' "$OBS/logcat-stream.status" |
            tail -n 1
    )"
    logcat_evidence_complete=yes
    case "$logcat_source_status" in
        0|124|137) ;;
        *)
            logcat_evidence_complete=no
            evidence_complete=no
            quality_fail
            ;;
    esac
    if [[ "$logcat_sink_status" != "0" ]]; then
        logcat_evidence_complete=no
        evidence_complete=no
        quality_fail
    fi
    if ! docker exec redroid-clean1 /system/bin/logcat -d -v threadtime \
        >"$OBS/logcat-final.txt" 2>"$OBS/logcat-final.stderr"; then
        logcat_evidence_complete=no
        evidence_complete=no
        quality_fail
    fi
    if ! docker exec redroid-clean1 /system/bin/am force-stop --user 0 "$PACKAGE" \
        >"$OBS/force-stop-after.txt" 2>"$OBS/force-stop-after.stderr"; then
        quality_fail
    fi

    if [[ -f "$OBS/flag-main-process-seen" ]]; then
        main_process_seen=yes
    elif [[ "$observer_completed" == "yes" ]]; then
        main_process_seen=no
    fi
    if [[ -f "$OBS/flag-main-process-survived" ]]; then
        main_process_survived_window=yes
    elif [[ "$observer_completed" == "yes" ]]; then
        main_process_survived_window=no
    fi
    if [[ -f "$OBS/flag-gomedia-process-seen" ]]; then
        gomedia_process_seen=yes
    elif [[ "$observer_completed" == "yes" ]]; then
        gomedia_process_seen=no
    fi
    if [[ -f "$OBS/flag-gomedia-active-seen" ]]; then
        gomedia_active_seen=yes
    elif [[ "$observer_completed" == "yes" ]]; then
        gomedia_active_seen=no
    fi
    if [[ -f "$OBS/flag-package-socket-seen" ]]; then
        package_socket_seen=yes
    elif [[ "$observer_completed" == "yes" &&
        "$package_uid" =~ ^[0-9]+$ ]]; then
        package_socket_seen=no
    fi

    map_attempt_count="$(
        sed -n 's/^map_attempts=//p' "$OBS/observer-state.txt" |
            tail -n 1
    )"
    map_success_count="$(
        sed -n 's/^map_successes=//p' "$OBS/observer-state.txt" |
            tail -n 1
    )"
    [[ "$map_attempt_count" =~ ^[0-9]+$ ]] || map_attempt_count=0
    [[ "$map_success_count" =~ ^[0-9]+$ ]] || map_success_count=0
    if [[ "$observer_completed" == "yes" &&
        ! -f "$OBS/flag-package-process-seen" ]]; then
        map_evidence_complete=yes
    elif [[ "$observer_completed" == "yes" &&
        "$map_attempt_count" -gt 0 &&
        "$map_attempt_count" -eq "$map_success_count" ]]; then
        map_evidence_complete=yes
    elif [[ "$observer_completed" == "yes" ]]; then
        map_evidence_complete=no
    fi
    if [[ "$map_evidence_complete" == "no" ]]; then
        evidence_complete=no
        quality_fail
    fi
    if grep -Eq '/system/lib64/|/system/bin/linker64' \
        "$OBS"/maps-*-first.txt "$OBS/maps-relevant-samples.txt" \
        2>/dev/null; then
        process_abi_arm64=yes
    elif [[ -f "$OBS/flag-package-process-seen" &&
        "$map_evidence_complete" == "yes" ]]; then
        process_abi_arm64=no
    fi
    if grep -Eq '(^|/)libexec(main)?\.so([[:space:]]|$)' \
        "$OBS"/maps-*-first.txt "$OBS/maps-relevant-samples.txt" \
        2>/dev/null; then
        ijiami_libexec_mapped=yes
    elif [[ "$observer_completed" == "yes" &&
        "$map_evidence_complete" == "yes" ]]; then
        ijiami_libexec_mapped=no
    fi
    if grep -Eq '(^|/)libranger-jni\.so([[:space:]]|$)' \
        "$OBS"/maps-*-first.txt "$OBS/maps-relevant-samples.txt" \
        2>/dev/null; then
        libranger_jni_mapped=yes
    elif [[ "$observer_completed" == "yes" &&
        "$map_evidence_complete" == "yes" ]]; then
        libranger_jni_mapped=no
    fi

    if docker exec redroid-clean1 /system/bin/sh -c '
        for file in \
            /data/user/0/com.android.mgstv/files/libexec.so \
            /data/user/0/com.android.mgstv/files/*/libexec.so; do
            [ -f "$file" ] || continue
            /system/bin/toybox sha256sum "$file"
        done
    ' >"$OBS/ijiami-libexec-hashes.txt" \
        2>"$OBS/ijiami-libexec-hashes.stderr"; then
        if grep -Eq "^${LIBEXEC_SHA256}[[:space:]]" \
            "$OBS/ijiami-libexec-hashes.txt"; then
            ijiami_libexec_extracted=yes
            ijiami_libexec_hash_matches=yes
        elif [[ -s "$OBS/ijiami-libexec-hashes.txt" ]]; then
            ijiami_libexec_extracted=yes
            ijiami_libexec_hash_matches=no
        else
            ijiami_libexec_extracted=no
            ijiami_libexec_hash_matches=no
        fi
    fi
    if docker exec redroid-clean1 /system/bin/sh -c '
        for file in \
            /data/user/0/com.android.mgstv/files/libexecmain.so \
            /data/user/0/com.android.mgstv/files/*/libexecmain.so; do
            [ -f "$file" ] || continue
            /system/bin/toybox sha256sum "$file"
        done
    ' >"$OBS/ijiami-libexecmain-hashes.txt" \
        2>"$OBS/ijiami-libexecmain-hashes.stderr"; then
        if grep -Eq "^${LIBEXECMAIN_SHA256}[[:space:]]" \
            "$OBS/ijiami-libexecmain-hashes.txt"; then
            ijiami_libexecmain_extracted=yes
            ijiami_libexecmain_hash_matches=yes
        elif [[ -s "$OBS/ijiami-libexecmain-hashes.txt" ]]; then
            ijiami_libexecmain_extracted=yes
            ijiami_libexecmain_hash_matches=no
        else
            ijiami_libexecmain_extracted=no
            ijiami_libexecmain_hash_matches=no
        fi
    fi
    base_dir="$(dirname -- "$base_path")"
    if docker exec redroid-clean1 /system/bin/sh -c \
        "find '$base_dir' -type f -name libranger-jni.so -exec /system/bin/toybox sha256sum {} \\;" \
        >"$OBS/libranger-jni-hashes.txt" \
        2>"$OBS/libranger-jni-hashes.stderr"; then
        if grep -Eq "^${RANGER_SHA256}[[:space:]]" \
            "$OBS/libranger-jni-hashes.txt"; then
            libranger_jni_hash_matches=yes
        elif [[ -s "$OBS/libranger-jni-hashes.txt" ]]; then
            libranger_jni_hash_matches=no
        else
            libranger_jni_hash_matches=no
        fi
    fi

    combined_log="$OBS/logcat-combined.txt"
    {
        cat "$OBS/logcat-stream.txt" 2>/dev/null || true
        cat "$OBS/logcat-final.txt" 2>/dev/null || true
        cat "$OBS/launch.txt" 2>/dev/null || true
        cat "$OBS/launch.stderr" 2>/dev/null || true
    } | tail -c 33554432 >"$combined_log"
    if { grep -Fq 'FATAL EXCEPTION' "$combined_log" &&
            grep -Eq 'Process: com\.android\.mgstv([:,]|$)' "$combined_log"; } ||
        { grep -Eq 'Fatal signal [0-9]+|SIGSEGV|SIGABRT' "$combined_log" &&
            grep -Eq '>>> com\.android\.mgstv(:[^[:space:]]+)? <<<' \
                "$combined_log"; } ||
        grep -Eq 'ANR in com\.android\.mgstv([[:space:]:]|$)' \
            "$combined_log"; then
        app_fatal_seen=yes
    elif [[ "$observer_completed" == "yes" &&
        "$logcat_evidence_complete" == "yes" ]]; then
        app_fatal_seen=no
    fi
    if grep -Fq 'UnsatisfiedLinkError' "$combined_log" &&
        grep -Fq 's.h.e.l.l.N.al' "$combined_log"; then
        ijiami_jni_registration=no
    else
        ijiami_jni_registration=unknown
    fi

    if snapshot_tombstones tombstones-after-launch.txt; then
        if cmp -s "$OBS/tombstones-before-launch.txt" \
            "$OBS/tombstones-after-launch.txt"; then
            tombstones_changed=no
        else
            tombstones_changed=yes
        fi
    else
        tombstones_changed=unknown
        evidence_complete=no
        quality_fail
    fi

    if [[ "$package_primary_abi_arm64" == "no" ]]; then
        subject_outcome=abi_selection_mismatch
    elif [[ "$ijiami_jni_registration" == "no" ]]; then
        subject_outcome=installed_ijiami_jni_failed
    elif [[ ( "$ijiami_libexec_mapped" == "yes" ||
            "$ijiami_libexec_extracted" == "yes" ) &&
        "$ijiami_libexec_hash_matches" == "yes" ]]; then
        subject_outcome=installed_loader_reached
    elif [[ "$app_fatal_seen" == "yes" ]]; then
        subject_outcome=crashed_before_loader_proof
    else
        subject_outcome=inconclusive
    fi
fi

capture_or_mark post-packages.txt \
    docker exec redroid-clean1 \
    /system/bin/sh /system/bin/pm list packages --user 0 || true
capture_or_mark post-processes.txt \
    docker exec redroid-clean1 /system/bin/ps -A || true
capture_or_mark post-services.txt \
    docker exec redroid-clean1 /system/bin/dumpsys activity services || true
if ! snapshot_sockets post-sockets.txt; then
    evidence_complete=no
    quality_fail
fi
if ! snapshot_tombstones tombstones-final.txt; then
    evidence_complete=no
    quality_fail
fi

printf 'remote_complete=yes\n'
printf 'remote_quality=%s\n' "$quality"
printf 'observer_evidence=%s\n' "$observer_evidence"
printf 'apk_install_attempted=%s\n' "$apk_install_attempted"
printf 'apk_install_command_status=%s\n' "$apk_install_command_status"
printf 'apk_launch_attempted=%s\n' "$apk_launch_attempted"
printf 'apk_launch_command_status=%s\n' "$apk_launch_command_status"
printf 'apk_installed=%s\n' "$apk_installed"
printf 'installed_base_hash_matches=%s\n' "$installed_base_hash_matches"
printf 'package_version_name_matches=%s\n' "$package_version_name_matches"
printf 'package_version_code_matches=%s\n' "$package_version_code_matches"
printf 'package_primary_abi_arm64=%s\n' "$package_primary_abi_arm64"
printf 'resolved_welcome_activity=%s\n' "$resolved_welcome_activity"
printf 'welcome_activity_started=%s\n' "$welcome_activity_started"
printf 'ijiami_assets_pinned=%s\n' "$ijiami_assets_pinned"
printf 'main_process_seen=%s\n' "$main_process_seen"
printf 'main_process_survived_window=%s\n' "$main_process_survived_window"
printf 'process_abi_arm64=%s\n' "$process_abi_arm64"
printf 'map_evidence_complete=%s\n' "$map_evidence_complete"
printf 'ijiami_libexec_extracted=%s\n' "$ijiami_libexec_extracted"
printf 'ijiami_libexec_hash_matches=%s\n' "$ijiami_libexec_hash_matches"
printf 'ijiami_libexecmain_extracted=%s\n' "$ijiami_libexecmain_extracted"
printf 'ijiami_libexecmain_hash_matches=%s\n' "$ijiami_libexecmain_hash_matches"
printf 'ijiami_libexec_mapped=%s\n' "$ijiami_libexec_mapped"
printf 'libranger_jni_mapped=%s\n' "$libranger_jni_mapped"
printf 'libranger_jni_hash_matches=%s\n' "$libranger_jni_hash_matches"
printf 'ijiami_jni_registration=%s\n' "$ijiami_jni_registration"
printf 'gomedia_declared=%s\n' "$gomedia_declared"
printf 'gomedia_process_seen=%s\n' "$gomedia_process_seen"
printf 'gomedia_active_seen=%s\n' "$gomedia_active_seen"
printf 'app_fatal_seen=%s\n' "$app_fatal_seen"
printf 'logcat_evidence_complete=%s\n' "$logcat_evidence_complete"
printf 'tombstones_changed=%s\n' "$tombstones_changed"
printf 'package_socket_seen=%s\n' "$package_socket_seen"
printf 'observer_completed=%s\n' "$observer_completed"
printf 'magstv_subject_outcome=%s\n' "$subject_outcome"
printf 'evidence_complete=%s\n' "$evidence_complete"
exit 0
REMOTE
        then
            remote_status=0
        else
            remote_status=$?
            status=1
        fi
        append_manifest "$manifest" \
            "magstv_remote_experiment_attempted=yes" \
            "magstv_remote_experiment_status=$remote_status"

        ssh_guest 600 \
            "sudo tar --format=ustar -C /var/lib/jellyrin-magstv-observation -cf - ." \
            2>"$run/magstv-observer-tar.stderr" |
            head -c "$((evidence_tar_limit + 1))" >"$evidence_tar"
        tar_statuses=("${PIPESTATUS[@]}")
        tar_source_status="${tar_statuses[0]:-1}"
        tar_sink_status="${tar_statuses[1]:-1}"
        evidence_tar_size="$(
            stat -c '%s' "$evidence_tar" 2>/dev/null ||
                printf '0\n'
        )"
        {
            printf 'source_status=%s\n' "$tar_source_status"
            printf 'sink_status=%s\n' "$tar_sink_status"
            printf 'archive_size=%s\n' "$evidence_tar_size"
            printf 'archive_limit=%s\n' "$evidence_tar_limit"
        } >"$run/magstv-observer-transfer.txt"

        evidence_status=0
        if [[ "$tar_source_status" -ne 0 ||
            "$tar_sink_status" -ne 0 ||
            "$evidence_tar_size" -le 0 ||
            "$evidence_tar_size" -gt "$evidence_tar_limit" ]]; then
            evidence_status=1
        elif ! tar -tf "$evidence_tar" >"$evidence_members" 2>&1 ||
            ! tar --numeric-owner -tvf "$evidence_tar" \
                >"$evidence_verbose" 2>&1; then
            evidence_status=1
        else
            evidence_member_count="$(
                wc -l <"$evidence_members" |
                    tr -d '[:space:]'
            )"
            if [[ ! "$evidence_member_count" =~ ^[0-9]+$ ||
                "$evidence_member_count" -le 0 ||
                "$evidence_member_count" -gt 4096 ]]; then
                evidence_status=1
            elif ! awk '
                $0 == "." || $0 == "./" {next}
                $0 !~ /^\.\// {exit 1}
                $0 ~ /\/\.\.?($|\/)/ {exit 1}
                $0 ~ /\/\// {exit 1}
                $0 !~ /^\.[/][A-Za-z0-9._/-]+$/ {exit 1}
            ' "$evidence_members"; then
                evidence_status=1
            elif LC_ALL=C sort "$evidence_members" |
                uniq -d |
                grep -q .; then
                evidence_status=1
            elif ! awk '
                {
                    type=substr($1, 1, 1)
                    if (type != "-" && type != "d") {
                        exit 1
                    }
                    if ($3 !~ /^[0-9]+$/ || $3 > 67108864) {
                        exit 1
                    }
                    total += $3
                }
                END {
                    if (total > 268435456) {
                        exit 1
                    }
                }
            ' "$evidence_verbose"; then
                evidence_status=1
            fi
        fi

        if [[ "$evidence_status" -eq 0 ]]; then
            sha256sum "$evidence_tar" >"$run/magstv-observer-tar-sha256.txt"
            install -d -m 0700 "$observer_dir"
            if tar \
                --no-same-owner \
                --no-same-permissions \
                --no-overwrite-dir \
                -C "$observer_dir" \
                -xf "$evidence_tar" &&
                ! find "$observer_dir" -type l -print -quit | grep -q . &&
                ! find "$observer_dir" \
                    ! -type f \
                    ! -type d \
                    -print -quit |
                    grep -q .; then
                evidence_archive=yes
                (
                    cd "$observer_dir"
                    find . -type f -exec sha256sum {} + |
                        LC_ALL=C sort
                ) >"$run/magstv-observer-sha256.txt"
            else
                evidence_archive=no
                evidence_status=1
            fi
        else
            evidence_archive=no
        fi
        append_manifest "$manifest" \
            "magstv_evidence_tar_size=$evidence_tar_size" \
            "magstv_evidence_member_count=$evidence_member_count" \
            "magstv_evidence_tar_validation=$(
                [[ "$evidence_status" -eq 0 ]] && printf pass || printf fail
            )"
        if [[ "$evidence_status" -ne 0 ]]; then
            status=1
        fi

        if [[ "$remote_status" -eq 0 && -s "$result_file" ]]; then
            summary_valid=1
            if value="$(summary_value remote_complete)" &&
                [[ "$value" == "yes" ]]; then
                remote_complete=yes
            else
                summary_valid=0
            fi
            if value="$(summary_value remote_quality)" &&
                [[ "$value" == "pass" || "$value" == "fail" ]]; then
                remote_quality="$value"
            else
                summary_valid=0
            fi
            if value="$(summary_value apk_install_attempted)" &&
                [[ "$value" == "yes" || "$value" == "no" ]]; then
                install_attempted="$value"
            else
                summary_valid=0
            fi
            if value="$(summary_value apk_install_command_status)" &&
                [[ "$value" == "not_attempted" || "$value" =~ ^[0-9]+$ ]]; then
                install_command_status="$value"
            else
                summary_valid=0
            fi
            if value="$(summary_value apk_launch_attempted)" &&
                [[ "$value" == "yes" || "$value" == "no" ]]; then
                launch_attempted="$value"
            else
                summary_valid=0
            fi
            if value="$(summary_value apk_launch_command_status)" &&
                [[ "$value" == "not_attempted" || "$value" =~ ^[0-9]+$ ]]; then
                launch_command_status="$value"
            else
                summary_valid=0
            fi
            if value="$(summary_value magstv_subject_outcome)" &&
                [[ "$value" =~ ^(installed_loader_reached|installed_ijiami_jni_failed|crashed_before_loader_proof|install_failed_definitive|abi_selection_mismatch|inconclusive)$ ]]; then
                subject_outcome="$value"
            else
                summary_valid=0
            fi
            for key in "${tristate_keys[@]}"; do
                if value="$(summary_value "$key")" &&
                    [[ "$value" == "yes" ||
                        "$value" == "no" ||
                        "$value" == "unknown" ]]; then
                    printf -v "$key" '%s' "$value"
                else
                    summary_valid=0
                fi
            done
            if value="$(summary_value evidence_complete)" &&
                [[ "$value" == "yes" ||
                    "$value" == "no" ||
                    "$value" == "unknown" ]]; then
                if [[ "$value" != "yes" ]]; then
                    remote_quality=fail
                fi
            else
                summary_valid=0
            fi
        fi
        if [[ "$summary_valid" -ne 1 ]]; then
            status=1
            remote_quality=fail
            append_manifest "$manifest" "magstv_remote_summary_validation=fail"
        else
            append_manifest "$manifest" "magstv_remote_summary_validation=pass"
        fi
    else
        append_manifest "$manifest" "magstv_remote_experiment_attempted=no"
    fi

    append_manifest "$manifest" "magstv_egress_after_attempted=yes"
    if sandbox_egress_gate after >"$run/magstv-egress-after.txt" 2>&1; then
        append_manifest "$manifest" \
            "magstv_egress_after=pass" \
            "egress_after=pass"
    else
        append_manifest "$manifest" \
            "magstv_egress_after=fail" \
            "egress_after=fail"
        status=1
    fi

    if [[ "$remote_complete" == "yes" &&
        "$remote_quality" == "pass" &&
        "$evidence_archive" == "yes" &&
        "$summary_valid" -eq 1 &&
        "$status" -eq 0 ]]; then
        observation_result=pass
    else
        observation_result=fail
        status=1
    fi
    append_manifest "$manifest" \
        "magstv_remote_complete=$remote_complete" \
        "magstv_remote_experiment_quality=$remote_quality" \
        "magstv_observer_evidence=$observer_evidence" \
        "magstv_evidence_archive=$evidence_archive" \
        "apk_install_attempted=$install_attempted" \
        "apk_install_command_status=$install_command_status" \
        "apk_launch_attempted=$launch_attempted" \
        "apk_launch_command_status=$launch_command_status" \
        "apk_installed=$apk_installed" \
        "installed_base_hash_matches=$installed_base_hash_matches" \
        "package_version_name_matches=$package_version_name_matches" \
        "package_version_code_matches=$package_version_code_matches" \
        "package_primary_abi_arm64=$package_primary_abi_arm64" \
        "resolved_welcome_activity=$resolved_welcome_activity" \
        "welcome_activity_started=$welcome_activity_started" \
        "ijiami_assets_pinned=$ijiami_assets_pinned" \
        "main_process_seen=$main_process_seen" \
        "main_process_survived_window=$main_process_survived_window" \
        "process_abi_arm64=$process_abi_arm64" \
        "map_evidence_complete=$map_evidence_complete" \
        "ijiami_libexec_extracted=$ijiami_libexec_extracted" \
        "ijiami_libexec_hash_matches=$ijiami_libexec_hash_matches" \
        "ijiami_libexecmain_extracted=$ijiami_libexecmain_extracted" \
        "ijiami_libexecmain_hash_matches=$ijiami_libexecmain_hash_matches" \
        "ijiami_libexec_mapped=$ijiami_libexec_mapped" \
        "libranger_jni_mapped=$libranger_jni_mapped" \
        "libranger_jni_hash_matches=$libranger_jni_hash_matches" \
        "ijiami_jni_registration=$ijiami_jni_registration" \
        "gomedia_declared=$gomedia_declared" \
        "gomedia_process_seen=$gomedia_process_seen" \
        "gomedia_active_seen=$gomedia_active_seen" \
        "app_fatal_seen=$app_fatal_seen" \
        "logcat_evidence_complete=$logcat_evidence_complete" \
        "tombstones_changed=$tombstones_changed" \
        "package_socket_seen=$package_socket_seen" \
        "observer_completed=$observer_completed" \
        "magstv_observation_result=$observation_result" \
        "magstv_subject_outcome=$subject_outcome" \
        "magstv_finished_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    return "$status"
}

sandbox_collect_evidence() {
    local run="$1"
    local ssh_timeout="${2:-180}"
    local status=0

    if ! ssh_guest "$ssh_timeout" \
        "sudo docker inspect redroid-clean1" \
        >"$run/redroid-inspect.json" 2>"$run/redroid-inspect.stderr"; then
        status=1
    fi
    if ! ssh_guest "$ssh_timeout" \
        "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo timeout --foreground --kill-after=5 30 nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/getprop" \
        >"$run/android-properties.txt" 2>"$run/android-properties.stderr"; then
        status=1
    fi
    if ! ssh_guest "$ssh_timeout" \
        "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo timeout --foreground --kill-after=5 30 nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/ps -A" \
        >"$run/android-processes.txt" 2>"$run/android-processes.stderr"; then
        status=1
    fi
    ssh_guest "$ssh_timeout" \
        "pid=\$(sudo docker inspect -f '{{.State.Pid}}' redroid-clean1) && sudo timeout --foreground --kill-after=5 30 nsenter -t \"\$pid\" -m -p -u -i -n -- /system/bin/logcat -d -v threadtime" \
        >"$run/android-logcat.txt" 2>"$run/android-logcat.stderr" || true
    ssh_guest "$ssh_timeout" \
        "sudo dmesg --ctime" \
        >"$run/guest-dmesg.txt" 2>"$run/guest-dmesg.stderr" || true
    return "$status"
}

sandbox_stop() {
    local qemu_pid="$1"
    local attempt stop_status poweroff_status qemu_disappeared=no
    local guest_powerdown_marker=no run

    run="$(sandbox_run_dir)"

    log "stopping Redroid and requesting clean guest poweroff"
    if ssh_guest 180 "sudo docker stop --timeout 30 redroid-clean1" \
        >"$run/redroid-stop.txt" 2>"$run/redroid-stop.stderr"; then
        stop_status=0
    else
        stop_status=$?
    fi
    if ssh_guest 60 "sudo systemctl poweroff --no-block" \
        >"$run/guest-poweroff.txt" 2>"$run/guest-poweroff.stderr"; then
        poweroff_status=0
    else
        poweroff_status=$?
    fi
    for attempt in $(seq 1 180); do
        if ! kill -0 "$qemu_pid" 2>/dev/null; then
            qemu_disappeared=yes
            break
        fi
        sleep 1
    done
    if [[ "$qemu_disappeared" == "yes" ]] &&
        grep -Fq 'reboot: Power down' "$run/serial.log"; then
        guest_powerdown_marker=yes
    fi
    append_manifest "$run/manifest.txt" \
        "redroid_stop_command_status=$stop_status" \
        "guest_poweroff_request_status=$poweroff_status" \
        "qemu_disappeared_after_poweroff_request=$qemu_disappeared" \
        "guest_kernel_powerdown_marker=$guest_powerdown_marker"
    # A Redroid PID can refuse Docker's TERM/KILL sequence under slow TCG and
    # make `docker stop` return 1 even though systemd subsequently stops
    # containerd and powers the whole disposable guest down cleanly.  Preserve
    # that command status as evidence, but prove teardown with the stronger
    # whole-guest invariants: an accepted poweroff request, QEMU disappearance,
    # the kernel power-down marker, and (in the caller) QEMU exit status zero.
    [[ "$poweroff_status" -eq 0 &&
        "$qemu_disappeared" == "yes" &&
        "$guest_powerdown_marker" == "yes" ]]
}

sandbox_main() {
    local scenario="$1"
    local run attempt caps route_count route6_count qemu_status
    local benign_mount_options magstv_mount_options
    local scenario_status=0 bootstrap_ready=0 evidence_status=0 benign_status=0
    local magstv_status=0
    local ssh_ready=0 evidence_attempted=0
    local clean_poweroff=no shutdown_sequence_ok=0

    run="$(sandbox_run_dir)"
    exec > >(tee -a "$run/runner.log") 2>&1

    cleanup() {
        local status=$?
        trap - EXIT INT TERM HUP
        set +e
        append_manifest "$run/manifest.txt" \
            "unexpected_exit_status=$status" || true
        if [[ "$SANDBOX_CLEANUP_DONE" -eq 0 && -n "$SANDBOX_QEMU_PID" ]] &&
            kill -0 "$SANDBOX_QEMU_PID" 2>/dev/null; then
            if [[ "$ssh_ready" -eq 1 && "$evidence_attempted" -eq 0 ]]; then
                append_manifest "$run/manifest.txt" \
                    "emergency_evidence_collection_attempted=yes" || true
                if sandbox_collect_evidence "$run" 30; then
                    append_manifest "$run/manifest.txt" \
                        "evidence_collection=emergency_pass" || true
                else
                    append_manifest "$run/manifest.txt" \
                        "evidence_collection=emergency_partial" || true
                fi
            elif [[ "$evidence_attempted" -eq 1 ]]; then
                append_manifest "$run/manifest.txt" \
                    "evidence_collection=interrupted_or_incomplete" || true
            else
                append_manifest "$run/manifest.txt" \
                    "evidence_collection=unavailable_before_ssh" || true
            fi
            ssh_guest 60 "sudo docker stop --timeout 30 redroid-clean1" \
                >/dev/null 2>&1 || true
            ssh_guest 30 "sudo systemctl poweroff" >/dev/null 2>&1 || true
            for _ in $(seq 1 60); do
                kill -0 "$SANDBOX_QEMU_PID" 2>/dev/null || break
                sleep 1
            done
            kill -TERM "$SANDBOX_QEMU_PID" 2>/dev/null || true
            for _ in $(seq 1 15); do
                kill -0 "$SANDBOX_QEMU_PID" 2>/dev/null || break
                sleep 1
            done
            kill -KILL "$SANDBOX_QEMU_PID" 2>/dev/null || true
            wait "$SANDBOX_QEMU_PID" 2>/dev/null || true
        elif [[ "$evidence_attempted" -eq 0 ]]; then
            append_manifest "$run/manifest.txt" \
                "evidence_collection=unavailable_qemu_not_running" || true
        fi
        return "$status"
    }
    trap cleanup EXIT
    trap 'exit 130' INT TERM HUP

    [[ ! -e /home/cdmonio ]] || die "host home is visible inside sandbox"
    [[ ! -e /lab/setup.qcow2 ]] || die "mutable preparation disk is visible"
    [[ ! -e /lab/.env ]] || die ".env is visible inside sandbox"
    caps="$(awk '/^CapEff:/ {print $2}' /proc/self/status)"
    [[ "$caps" == "0000000000000000" ]] || die "sandbox retains Linux capabilities"
    printf '%s\n' 'sandbox_initial_ipv4_routes_begin'
    ip -4 route show table all | sed -n '1,200p'
    printf '%s\n' 'sandbox_initial_ipv4_routes_end'
    printf '%s\n' 'sandbox_initial_ipv6_routes_begin'
    ip -6 route show table all | sed -n '1,200p'
    printf '%s\n' 'sandbox_initial_ipv6_routes_end'
    route_count="$(
        ip -4 route show table all |
            awk '$1 == "default" {count++} END {print count + 0}'
    )"
    [[ "$route_count" == "0" ]] || die "sandbox network namespace has a default route"
    route6_count="$(
        ip -6 route show table all |
            awk '$1 == "default" {count++} END {print count + 0}'
    )"
    [[ "$route6_count" == "0" ]] ||
        die "sandbox network namespace has an IPv6 default route"
    [[ "$(sha256_of /lab/immutable/redroid-base.qcow2)" == "$BASE_SHA256" ]] ||
        die "sandbox sees an unexpected sealed base"
    [[ "$(sha256_of /input/zz-magstv-lab.rc)" == "$REDROID_INIT_SHA256" ]] ||
        die "sandbox sees an unexpected Redroid init"
    if [[ "$scenario" == "baseline" ]]; then
        [[ ! -e /input/benign-probe.apk ]] ||
            die "baseline sandbox unexpectedly received an APK"
        [[ ! -e /input/magstv-base.apk ]] ||
            die "baseline sandbox unexpectedly received the MAGSTV APK"
    elif [[ "$scenario" == "benign" ]]; then
        [[ ! -e /input/magstv-base.apk ]] ||
            die "benign sandbox unexpectedly received the MAGSTV APK"
        [[ -f /input/benign-probe.apk && ! -L /input/benign-probe.apk ]] ||
            die "benign probe is not a regular sandbox input"
        [[ "$(stat -c '%a' /input/benign-probe.apk)" == "400" ]] ||
            die "benign probe sandbox input mode is not 0400"
        [[ "$(stat -c '%s' /input/benign-probe.apk)" == "$BENIGN_APK_SIZE" ]] ||
            die "benign probe sandbox input size mismatch"
        [[ "$(sha256_of /input/benign-probe.apk)" == "$BENIGN_APK_SHA256" ]] ||
            die "benign probe sandbox input sha256 mismatch"
        benign_mount_options="$(findmnt -n -o OPTIONS -T /input/benign-probe.apk)"
        [[ ",$benign_mount_options," == *,ro,* ]] ||
            die "benign probe sandbox input is not mounted read-only"
        append_manifest "$run/manifest.txt" \
            "sandbox_benign_apk_sha256=$BENIGN_APK_SHA256" \
            "sandbox_benign_apk_size=$BENIGN_APK_SIZE" \
            "sandbox_benign_apk_mode=400" \
            "sandbox_benign_apk_mount_readonly=yes" \
            "apk_actual_sha256=$BENIGN_APK_SHA256" \
            "apk_actual_size=$BENIGN_APK_SIZE" \
            "apk_input_validated=yes"
    elif [[ "$scenario" == "magstv-offline" ]]; then
        [[ ! -e /input/benign-probe.apk ]] ||
            die "MAGSTV sandbox unexpectedly received the benign APK"
        [[ -f /input/magstv-base.apk && ! -L /input/magstv-base.apk ]] ||
            die "MAGSTV APK is not a regular sandbox input"
        [[ "$(stat -c '%a' /input/magstv-base.apk)" == "400" ]] ||
            die "MAGSTV sandbox input mode is not 0400"
        [[ "$(stat -c '%s' /input/magstv-base.apk)" == "$MAGSTV_APK_SIZE" ]] ||
            die "MAGSTV sandbox input size mismatch"
        [[ "$(sha256_of /input/magstv-base.apk)" == "$MAGSTV_APK_SHA256" ]] ||
            die "MAGSTV sandbox input sha256 mismatch"
        magstv_mount_options="$(
            findmnt -n -o OPTIONS -T /input/magstv-base.apk
        )"
        [[ ",$magstv_mount_options," == *,ro,* ]] ||
            die "MAGSTV sandbox input is not mounted read-only"
        append_manifest "$run/manifest.txt" \
            "sandbox_magstv_apk_sha256=$MAGSTV_APK_SHA256" \
            "sandbox_magstv_apk_size=$MAGSTV_APK_SIZE" \
            "sandbox_magstv_apk_mode=400" \
            "sandbox_magstv_apk_mount_readonly=yes" \
            "apk_actual_sha256=$MAGSTV_APK_SHA256" \
            "apk_actual_size=$MAGSTV_APK_SIZE" \
            "apk_input_validated=yes"
    else
        die "invalid sandbox scenario: $scenario"
    fi
    log "sandbox verified: no home, no capabilities, no default route"

    "$QEMU_BIN" \
        -name magstv-redroid-disposable \
        -machine virt,gic-version=3,virtualization=on \
        -cpu max \
        -smp 4 \
        -m 10240 \
        -accel tcg,thread=single \
        -sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny \
        -drive "if=pflash,format=raw,unit=0,file=$AAVMF_CODE,readonly=on" \
        -drive "if=pflash,format=raw,unit=1,file=$run/AAVMF_VARS.fd" \
        -drive "file=$run/root.qcow2,if=virtio,format=qcow2,cache=none,aio=threads,discard=unmap" \
        -device virtio-rng-pci \
        -netdev "user,id=net0,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22" \
        -device virtio-net-pci,netdev=net0,romfile= \
        -display none \
        -serial "file:$run/serial.log" \
        -monitor none \
        -pidfile "$run/qemu.pid" \
        -no-reboot \
        >"$run/qemu.stdout" 2>"$run/qemu.stderr" &
    SANDBOX_QEMU_PID=$!
    log "QEMU started inside isolated PID/network namespaces (PID $SANDBOX_QEMU_PID)"

    for attempt in $(seq 1 90); do
        kill -0 "$SANDBOX_QEMU_PID" 2>/dev/null ||
            die "QEMU exited before SSH became available"
        if ssh_guest 90 "true" >/dev/null 2>&1; then
            break
        fi
        if ((attempt % 6 == 0)); then
            log "waiting for Ubuntu SSH (${attempt}/90)"
        fi
        sleep 10
    done
    ((attempt <= 90)) || die "Ubuntu SSH did not become available"
    # The first successful connection can be the socket-activation handshake
    # for sshd itself.  Require a short, consecutive run of real commands so
    # a one-off activation success cannot be mistaken for a usable guest.
    local stable_probe
    for stable_probe in $(seq 1 3); do
        if ! ssh_guest 120 "true"; then
            die "Ubuntu SSH did not pass stability probe ${stable_probe}/3"
        fi
        sleep 2
    done
    ssh_ready=1
    log "Ubuntu SSH is ready inside the isolated namespace"

    if sandbox_bootstrap_redroid | tee "$run/guest-validation.txt"; then
        bootstrap_ready=1
        append_manifest "$run/manifest.txt" "guest_validation=pass"
        log "Redroid baseline validation passed"
    else
        scenario_status=$?
        append_manifest "$run/manifest.txt" \
            "guest_validation=fail" \
            "guest_validation_status=$scenario_status"
        log "Redroid baseline validation failed; proceeding to evidence and shutdown"
    fi

    if [[ "$bootstrap_ready" -eq 1 && "$scenario" == "benign" ]]; then
        # Do not invoke the function as an `if` condition: Bash otherwise
        # suppresses errexit throughout its body.  The subshell restores
        # fail-fast behavior while the parent deliberately captures status.
        set +e
        (
            trap - EXIT INT TERM HUP
            set -Eeuo pipefail
            sandbox_run_benign_probe "$run"
        )
        benign_status=$?
        set -e
        if [[ "$benign_status" -eq 0 ]]; then
            append_manifest "$run/manifest.txt" "benign_validation=pass"
            log "benign ARM64/JNI probe validation passed"
        else
            scenario_status=$benign_status
            append_manifest "$run/manifest.txt" \
                "benign_validation=fail" \
                "benign_validation_status=$scenario_status"
            log "benign probe validation failed; proceeding to evidence and shutdown"
        fi
    fi

    if [[ "$bootstrap_ready" -eq 1 && "$scenario" == "magstv-offline" ]]; then
        # The MAGSTV subject is run only inside the already validated offline
        # baseline. Capture its factual outcome without allowing an app crash
        # or definitive install rejection to masquerade as containment loss.
        set +e
        (
            trap - EXIT INT TERM HUP
            set -Eeuo pipefail
            sandbox_run_magstv_offline "$run"
        )
        magstv_status=$?
        set -e
        if [[ "$magstv_status" -eq 0 ]]; then
            append_manifest "$run/manifest.txt" "magstv_validation=pass"
            log "MAGSTV offline observation completed with valid evidence"
        else
            scenario_status=$magstv_status
            append_manifest "$run/manifest.txt" \
                "magstv_validation=fail" \
                "magstv_validation_status=$scenario_status"
            log "MAGSTV offline observation failed its experiment-quality gate"
        fi
    fi

    evidence_attempted=1
    if sandbox_collect_evidence "$run"; then
        append_manifest "$run/manifest.txt" "evidence_collection=pass"
        log "run evidence collected"
    else
        evidence_status=$?
        append_manifest "$run/manifest.txt" \
            "evidence_collection=partial" \
            "evidence_collection_status=$evidence_status"
        scenario_status=1
        log "evidence collection was partial; proceeding to shutdown"
    fi

    if sandbox_stop "$SANDBOX_QEMU_PID"; then
        shutdown_sequence_ok=1
    else
        scenario_status=1
        shutdown_sequence_ok=0
        kill -TERM "$SANDBOX_QEMU_PID" 2>/dev/null || true
        for _ in $(seq 1 15); do
            kill -0 "$SANDBOX_QEMU_PID" 2>/dev/null || break
            sleep 1
        done
        kill -KILL "$SANDBOX_QEMU_PID" 2>/dev/null || true
    fi
    if wait "$SANDBOX_QEMU_PID"; then
        qemu_status=0
    else
        qemu_status=$?
    fi
    if [[ "$qemu_status" -ne 0 ]]; then
        scenario_status=1
    fi
    if [[ "$shutdown_sequence_ok" -eq 1 && "$qemu_status" -eq 0 ]]; then
        clean_poweroff=yes
    else
        clean_poweroff=no
    fi
    SANDBOX_QEMU_PID=""
    SANDBOX_CLEANUP_DONE=1
    trap - EXIT INT TERM HUP
    append_manifest "$run/manifest.txt" \
        "sandbox_ipv4_default_route=absent" \
        "sandbox_ipv6_default_route=absent" \
        "sandbox_cap_eff=0000000000000000" \
        "qemu_exit_status=$qemu_status" \
        "clean_poweroff=$clean_poweroff"
    if [[ "$scenario_status" -eq 0 ]]; then
        append_manifest "$run/manifest.txt" "sandbox_scenario_result=pass"
        log "sandbox scenario completed with clean poweroff"
    else
        append_manifest "$run/manifest.txt" "sandbox_scenario_result=fail"
        die "sandbox scenario failed; evidence and shutdown steps completed"
    fi
}

main() {
    local command="${1:-}"
    local status=0

    reject_forbidden_environment
    if [[ "$SANDBOXED" == "1" ]]; then
        [[ "$command" == "__sandboxed_boot" && $# -eq 2 ]] ||
            die "invalid sandbox invocation"
        [[ "$2" == "baseline" ||
            "$2" == "benign" ||
            "$2" == "magstv-offline" ]] ||
            die "invalid sandbox scenario"
        sandbox_main "$2"
        return
    fi
    trap host_resource_cleanup EXIT
    trap 'exit 130' INT TERM HUP

    case "$command" in
        doctor)
            [[ $# -eq 1 ]] || die "doctor takes no arguments"
            acquire_lock
            host_preflight
            release_lock
            log "doctor: sealed baseline and prerequisites passed"
            ;;
        boot)
            [[ $# -eq 1 ]] || die "boot takes no arguments"
            acquire_lock
            set +e
            (
                set -Eeuo pipefail
                host_boot baseline
            )
            status=$?
            set -e
            release_lock
            return "$status"
            ;;
        boot-magstv-offline)
            [[ $# -eq 2 ]] ||
                die "boot-magstv-offline requires exactly the pinned APK path"
            acquire_lock
            open_and_validate_magstv_apk "$2"
            set +e
            (
                set -Eeuo pipefail
                host_boot magstv-offline "$MAGSTV_APK_FD"
            )
            status=$?
            set -e
            close_magstv_apk
            release_lock
            return "$status"
            ;;
        boot-benign)
            [[ $# -eq 2 ]] || die "boot-benign requires exactly one APK path"
            acquire_lock
            open_and_validate_benign_apk "$2"
            set +e
            (
                set -Eeuo pipefail
                host_boot benign "$BENIGN_APK_FD"
            )
            status=$?
            set -e
            close_benign_apk
            release_lock
            return "$status"
            ;;
        -h|--help|help)
            usage
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
