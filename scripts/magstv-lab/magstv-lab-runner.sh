#!/usr/bin/env bash
# =============================================================================
# magstv-lab-runner.sh  --  hardened, reproducible ARM64 lab runner (L1/L2)
#
# Boots a CLEAN AOSP ARM64 (API 27) system image under QEMU TCG with:
#   * an explicit, baked-in profile (Emulator 28.0.25 + fixed DTB + API 27),
#     so nothing depends on variables typed in by hand;
#   * the minimal audio-HAL binary patch applied automatically at runtime so
#     the guest reaches sys.boot_completed=1 (the patch lives only in the
#     disposable AVD copy's writable overlay -> reversible);
#   * a hardened sandbox: no external network, no access to the user's home,
#     PID/IPC/UTS/cgroup namespaces, die-with-parent, guaranteed cleanup of
#     QEMU/ADB and every temp artifact even on interruption;
#   * a per-run disposable AVD copy and a complete run manifest.
#
# This runner does NOT install or execute the MAGSTV APK. It only prepares and
# validates the isolated boot environment (plan phases L1 and L2). Running the
# APK (L3+) is gated behind approval of L1 and is intentionally out of scope
# here.
#
# Usage:
#   magstv-lab-runner.sh boot [N]     boot N times (default 1), each a fresh
#                                     disposable AVD copy; report per-boot
#                                     sys.boot_completed and write a manifest.
#
# Exit status: 0 iff every requested boot reached sys.boot_completed=1.
# =============================================================================
set -euo pipefail

# ---------------------------------------------------------------------------
# Profile (explicit; override via env only if you must, but defaults stand
# alone so a clean checkout reproduces the verified boot).
# ---------------------------------------------------------------------------
readonly SDK="${MAGSTV_SDK:-/home/cdmonio/android-sdk}"
readonly LAB="${MAGSTV_LAB:-/home/cdmonio/apk-work}"
readonly EMU_HOME="${MAGSTV_EMU_HOME:-$LAB/emulators/28.0.25/emulator}"
readonly SYSIMG="${MAGSTV_SYSIMG:-$SDK/system-images/android-27/google_apis/arm64-v8a}"
readonly DTB="${MAGSTV_DTB:-$LAB/ranchu-api27-fixed.dtb}"
readonly PATCHED_HAL="${MAGSTV_PATCHED_HAL:-$LAB/android.hardware.audio@2.0-impl.api27.arm64.patched.so}"
readonly AVD_SRC_HOME="${MAGSTV_AVD_SRC_HOME:-$HOME/.android/avd}"
readonly AVD_NAME="${MAGSTV_AVD_NAME:-magstv-re-lab-arm64-tcg-api27}"
readonly CPU_MODEL="${MAGSTV_CPU_MODEL:-cortex-a57}"
readonly GUEST_HAL=/vendor/lib64/hw/android.hardware.audio@2.0-impl.so

# Expected artifact hashes (pin the reproducible base).
readonly DTB_SHA_EXPECT=ccc2f27979fad576877cc9cdfe746d71e5d618980840c149073207f5d0135e57
readonly PATCHED_HAL_SHA_EXPECT=be184a3474ecbd6bf7ae166e753c180f19e8269fd2c990c0fd5c346f08a1998e
# Pristine guest HAL this patch was built from (must equal the image's HAL).
readonly GUEST_HAL_SHA_EXPECT=220d1868f779fbe151d7bc2fd49b3e1d97a23877fed21675bf4472417c1a5203

readonly SENTINEL="${MAGSTV_SANDBOXED:-0}"

log(){ printf '%s %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die(){ printf 'ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Host phase: validate inputs, then re-exec inside a hardened bwrap sandbox.
# Nothing here touches the network or the APK.
# ---------------------------------------------------------------------------
host_preflight(){
  command -v bwrap >/dev/null || die "bubblewrap (bwrap) not found"
  [[ -x "$EMU_HOME/emulator" ]] || die "emulator launcher missing: $EMU_HOME/emulator"
  [[ -d "$SYSIMG" ]]            || die "system image missing: $SYSIMG"
  [[ -f "$DTB" ]]              || die "DTB missing: $DTB"
  [[ -f "$PATCHED_HAL" ]]      || die "patched HAL missing: $PATCHED_HAL"
  [[ -f "$AVD_SRC_HOME/$AVD_NAME.avd/config.ini" ]] || die "source AVD missing: $AVD_NAME"

  local got
  got=$(sha256sum "$DTB" | cut -d' ' -f1)
  [[ "$got" == "$DTB_SHA_EXPECT" ]] || die "DTB sha mismatch: $got"
  got=$(sha256sum "$PATCHED_HAL" | cut -d' ' -f1)
  [[ "$got" == "$PATCHED_HAL_SHA_EXPECT" ]] || die "patched HAL sha mismatch: $got"
}

# Prepare a disposable AVD copy for one run under $RUN/sbxhome/.android/avd.
# Only config.ini is copied; the emulator regenerates userdata and the
# writable system/vendor overlays from the read-only system image.
prepare_avd_copy(){
  local run="$1"
  local dst_home="$run/sbxhome/.android"
  local dst_avd="$dst_home/avd/$AVD_NAME.avd"
  mkdir -p "$dst_avd"
  cp "$AVD_SRC_HOME/$AVD_NAME.avd/config.ini" "$dst_avd/config.ini"
  # Fresh .ini pointing at the in-sandbox path (/sbx == $run).
  cat > "$dst_home/avd/$AVD_NAME.ini" <<EOF
avd.ini.encoding=UTF-8
path=/sbx/sbxhome/.android/avd/$AVD_NAME.avd
path.rel=avd/$AVD_NAME.avd
target=android-27
EOF
}

host_main(){
  local cmd="${1:-boot}"; shift || true
  [[ "$cmd" == "boot" ]] || die "unknown command: $cmd (only 'boot')"
  local count="${1:-1}"

  host_preflight

  local session="$(date -u +%Y%m%dT%H%M%SZ)-magstv-lab"
  local pass=0
  for n in $(seq 1 "$count"); do
    local run="$LAB/runs/${session}-boot${n}"
    mkdir -p "$run/sbxhome"
    prepare_avd_copy "$run"
    log "=== boot $n/$count -> $run ==="

    # Re-exec this script inside the hardened sandbox. Only the paths listed
    # below exist inside; the user's home is NOT among them.
    if bwrap \
        --unshare-net --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
        --die-with-parent --new-session \
        --proc /proc --dev /dev --tmpfs /tmp --tmpfs /run \
        --ro-bind /usr /usr \
        --ro-bind /etc /etc \
        --ro-bind /sys /sys \
        --symlink usr/bin /bin \
        --symlink usr/lib /lib \
        --symlink usr/lib64 /lib64 \
        --symlink usr/sbin /sbin \
        --ro-bind "$SDK/platform-tools" "$SDK/platform-tools" \
        --ro-bind "$SDK/platforms" "$SDK/platforms" \
        --ro-bind "$SYSIMG" "$SYSIMG" \
        --ro-bind "$EMU_HOME" "$EMU_HOME" \
        --ro-bind "$DTB" "$DTB" \
        --ro-bind "$PATCHED_HAL" "$PATCHED_HAL" \
        --ro-bind "$0" "$0" \
        --bind "$run" /sbx \
        --setenv MAGSTV_SANDBOXED 1 \
        --setenv HOME /sbx/sbxhome \
        --setenv USER labuser \
        --setenv LOGNAME labuser \
        --setenv TMPDIR /tmp \
        --setenv PATH "$SDK/platform-tools:/usr/sbin:/usr/bin" \
        --setenv ANDROID_SDK_ROOT "$SDK" \
        --setenv ANDROID_HOME "$SDK" \
        --setenv ANDROID_AVD_HOME /sbx/sbxhome/.android/avd \
        --setenv ANDROID_EMULATOR_HOME /sbx/sbxhome/.android \
        --setenv ANDROID_EMULATOR_LAUNCHER_DIR "$EMU_HOME" \
        --setenv ANDROID_ADB_SERVER_PORT 5044 \
        --setenv LD_LIBRARY_PATH "$EMU_HOME/lib64:$EMU_HOME/lib64/qt/lib" \
        -- "$0" __sandboxed_boot "$n" "$count"; then
      pass=$((pass+1))
      log "boot $n: PASS"
    else
      log "boot $n: FAIL"
    fi
  done

  log "=== summary: $pass/$count boots reached sys.boot_completed=1 ==="
  [[ "$pass" -eq "$count" ]]
}

# ---------------------------------------------------------------------------
# Sandbox phase: runs inside bwrap. No external route reachable, home absent.
# ---------------------------------------------------------------------------
sandbox_boot(){
  local n="$1" count="$2"
  # NOTE: these are intentionally NOT 'local' -- the EXIT trap's cleanup()
  # runs after sandbox_boot returns, so it needs them at global scope.
  run=/sbx
  port=5562; serial="emulator-5562"; adb_port=5044
  emu_pid=""; boot_ok=0
  export ANDROID_ADB_SERVER_PORT="$adb_port"

  exec > >(tee -a "$run/runner.log") 2>&1
  manifest="$run/manifest.txt"

  # Timeout lives INSIDE the helper so it wraps the real `adb` binary. Never
  # write `timeout N adb_lab ...`: `timeout` cannot invoke a shell function
  # (fails with "No such file or directory"), which silently breaks the call.
  adb_lab(){ timeout "${ADB_TO:-90}" adb -P "$adb_port" -s "$serial" "$@"; }

  # --- prove network is denied inside the sandbox (host side) ---
  if ip route show default 2>/dev/null | grep -q .; then
    die "sandbox has an external default route; refusing to boot"
  fi
  {
    echo "# magstv lab boot manifest"
    echo "boot_index: $n of $count"
    echo "utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "emulator: $("$EMU_HOME/emulator" -version 2>/dev/null | head -1)"
    echo "adb: $(adb --version 2>/dev/null | head -1)"
    echo "cpu_model: $CPU_MODEL"
    echo "dtb_sha: $(sha256sum "$DTB" | cut -d' ' -f1)"
    echo "patched_hal_sha: $(sha256sum "$PATCHED_HAL" | cut -d' ' -f1)"
    echo "host_default_route: $(ip route show default 2>/dev/null | grep -c . ) lines (expect 0)"
    echo "host_addrs: $(ip -o addr show 2>/dev/null | awk '{print $2}' | sort -u | paste -sd, -)"
    echo "home_listing: $(ls -A /home/cdmonio 2>/dev/null | paste -sd, - || echo '<none>')"
  } > "$manifest"

  log "starting emulator (TCG, headless, network-isolated)"
  adb -P "$adb_port" start-server >/dev/null 2>&1 || true

  "$EMU_HOME/emulator" -avd "$AVD_NAME" -port "$port" \
    -accel off -cores 1 \
    -wipe-data -no-cache -no-window -no-audio -no-boot-anim \
    -no-snapshot -gpu swiftshader_indirect -writable-system \
    -show-kernel -verbose \
    -qemu -cpu "$CPU_MODEL" -dtb "$DTB" \
    > "$run/emulator.log" 2>&1 &
  emu_pid=$!

  # Every adb call below is time-bounded so a wedged transport can never hang
  # the run; teardown SIGKILLs and relies on the PID namespace to reap the rest.
  cleanup(){
    adb_lab logcat -d -b all -v threadtime > "$run/logcat.txt" 2>&1 || true
    adb_lab shell getprop > "$run/properties.txt" 2>&1 || true
    adb_lab shell 'ls -l /data/tombstones' > "$run/tombstones.txt" 2>&1 || true
    { echo "boot_completed: $boot_ok"
      echo "post_patch_hal_crashes: $(grep -c 'HAL server crashed' "$run/logcat.txt" 2>/dev/null || echo NA)"
    } >> "$manifest"
    adb_lab emu kill >/dev/null 2>&1 || true
    [[ -n "${emu_pid:-}" ]] && kill -9 "$emu_pid" 2>/dev/null || true
    pkill -9 -f "qemu-system-aarch64 -avd $AVD_NAME" 2>/dev/null || true
    timeout 10 adb -P "$adb_port" kill-server >/dev/null 2>&1 || true
  }
  trap cleanup EXIT INT TERM

  # --- wait for adb ---
  # Bare start-server/get-state: the proven-reliable form. The emulator<->adb
  # registration is timing-sensitive; `timeout`-interrupted adb calls or a
  # kill-server here can drop a slow-but-progressing registration and wedge the
  # boot. Do NOT reintroduce timeouts/kicks in THIS loop (the patch phase below
  # keeps its own timeouts, which is where a real hang was observed).
  local i
  for i in $(seq 1 600); do
    kill -0 "$emu_pid" 2>/dev/null || { log "emulator exited before adb"; tail -n 60 "$run/emulator.log"; return 1; }
    [[ "$(adb_lab get-state 2>/dev/null)" == "device" ]] && { log "adb device after $((i*2))s"; break; }
    sleep 2
  done
  [[ "$(adb_lab get-state 2>/dev/null)" == "device" ]] || { log "no adb within 20 min"; return 1; }

  # --- apply audio HAL patch (automatic, reversible: only the disposable copy) ---
  # Whole apply is retried once (loop below) to ride out an adbd bounce.
  # adb_lab carries its own internal timeout; never prefix it with the `timeout`
  # binary (it cannot invoke a shell function).
  apply_patch(){
    local k ready=0 gsha w=0
    adb_lab root >/dev/null 2>&1 || true
    sleep 3                                     # let adbd begin restarting as root
    for k in $(seq 1 45); do                    # bounded wait for the device to return
      [[ "$(adb_lab get-state 2>/dev/null)" == "device" ]] && { ready=1; break; }
      sleep 2
    done
    [[ "$ready" == 1 ]] || { log "device did not return after adb root"; return 1; }
    sleep 2                                      # settle as root before remount
    adb_lab shell 'setenforce 0' >/dev/null 2>&1 || true
    # Ensure /vendor is actually writable before pushing (remount can lag root).
    for k in 1 2 3 4 5; do
      adb_lab remount >/dev/null 2>&1 || adb_lab shell 'mount -o rw,remount /vendor' >/dev/null 2>&1 || true
      [[ "$(adb_lab shell 'touch /vendor/.wtest 2>/dev/null && rm -f /vendor/.wtest && echo ok' 2>/dev/null | tr -d '\r')" == "ok" ]] && { w=1; break; }
      sleep 2
    done
    [[ "$w" == 1 ]] || { log "/vendor not writable after remount"; return 1; }
    gsha=$(adb_lab shell "sha256sum $GUEST_HAL 2>/dev/null" | tr -d '\r' | cut -d' ' -f1)
    echo "guest_hal_sha_before: ${gsha:-unknown}" >> "$manifest"
    if [[ -n "$gsha" && "$gsha" != "$GUEST_HAL_SHA_EXPECT" ]]; then
      log "WARNING: guest HAL sha ($gsha) != patch base ($GUEST_HAL_SHA_EXPECT)"; return 2
    fi
    adb_lab push "$PATCHED_HAL" /vendor/lib64/hw/.audio-impl.patched >/dev/null 2>&1 || return 1
    adb_lab shell "mv /vendor/lib64/hw/.audio-impl.patched $GUEST_HAL && chmod 644 $GUEST_HAL && (chcon u:object_r:vendor_file:s0 $GUEST_HAL 2>/dev/null || restorecon $GUEST_HAL 2>/dev/null || true)" >/dev/null 2>&1 || return 1
    adb_lab shell 'setprop ctl.restart audioserver' >/dev/null 2>&1 || true
    return 0
  }
  local attempt patch_rc=1
  for attempt in 1 2; do
    apply_patch && patch_rc=0 || patch_rc=$?
    [[ "$patch_rc" == 0 ]] && break
    [[ "$patch_rc" == 2 ]] && { log "aborting: guest audio HAL differs from patch base"; return 1; }
    # Do NOT kill-server here (it drops the emulator registration and wedges
    # the boot). Just wait for the device to settle and retry apply_patch.
    log "patch attempt $attempt failed; waiting before retry"
    sleep 5
    for k in $(seq 1 45); do
      [[ "$(adb_lab get-state 2>/dev/null)" == "device" ]] && break
      sleep 2
    done
  done
  [[ "$patch_rc" == 0 ]] || { log "audio HAL patch failed after retries"; return 1; }
  log "audio HAL patch applied; waiting for sys.boot_completed"

  # --- capture guest network isolation evidence ---
  {
    echo "guest_ip_route: $(adb_lab shell 'ip route 2>/dev/null' | tr '\r\n' '; ')"
    echo "guest_external_ping: $(adb_lab shell 'ping -c1 -W3 8.8.8.8 >/dev/null 2>&1 && echo REACHABLE || echo blocked')"
  } >> "$manifest"

  # --- wait for boot_completed (30 min past patch) ---
  for i in $(seq 1 360); do
    if [[ "$(adb_lab shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" == "1" ]]; then
      boot_ok=1
      log "sys.boot_completed=1 after $((i*5))s past patch"
      return 0
    fi
    (( i % 12 == 0 )) && log "still booting: $((i*5))s past patch"
    sleep 5
  done
  log "boot NOT completed within 30 min past patch"
  return 1
}

# ---------------------------------------------------------------------------
if [[ "${1:-}" == "__sandboxed_boot" ]]; then
  shift
  sandbox_boot "$@"
  exit $?
fi
host_main "$@"
