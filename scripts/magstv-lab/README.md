# MAGSTV RE lab — hardened ARM64 boot runner

Tooling for the MAGSTV protocol reverse-engineering lab (plan `0037`). It boots a
**clean, unmodified AOSP ARM64 (API 27) system image** under QEMU TCG inside a
locked-down sandbox, so the environment can be studied reproducibly.

It does **not** install or run the MAGSTV APK, and never touches the network or
any credential. Running the APK (plan phase L3+) is intentionally out of scope
here and stays behind the plan's decision gates.

## Files

- `magstv-lab-runner.sh` — the runner. `boot [N]` boots the image `N` times, each
  in a fresh disposable AVD copy, and reports whether each reached
  `sys.boot_completed=1`. Exit status is `0` iff every requested boot succeeded.
- `patch_audio_hal.py` — builds the minimal audio-HAL patch the runner applies
  (see below).

## What the runner guarantees

- **Isolation:** minimal read-only mounts (the user's home is *not* visible),
  `--unshare-net/pid/ipc/uts/cgroup` + `--die-with-parent`. No external route
  reaches the guest; QEMU/ADB/crash-service die with the sandbox (no orphans).
- **Reproducibility:** an explicit, sha-pinned profile (Emulator 28.0.25 +
  `ranchu-api27-fixed.dtb` + API 27 google_apis image). Verified: `boot 3` = 3/3
  to `sys.boot_completed=1`.
- **Evidence:** each run writes `manifest.txt` (versions, hashes, host+guest
  network-isolation proof, home listing), `runner.log`, `emulator.log`,
  `logcat.txt`, `properties.txt`, `tombstones.txt` under `runs/<ts>-…-bootN/`.

## The audio-HAL patch (why boot needs it)

On this image, `StreamOut::getPresentationPositionImpl` in
`/vendor/lib64/hw/android.hardware.audio@2.0-impl.so` SIGSEGVs under TCG, killing
the audio HAL; `audioserver` then aborts in a loop and `system_server` never
finishes, so `sys.boot_completed` never reaches `1`. `patch_audio_hal.py`
replaces that function's prologue with a 24-byte safe early-return
(`*frames=0; *timestamp={0,0}; return 0`). The runner pushes the patched library
into the disposable AVD copy at runtime via `-writable-system` — the base image
and the APK are never modified.

The patch **must** be built from the exact guest library it targets: the script
verifies the prologue bytes, and the runner refuses to proceed unless the guest
HAL's sha matches the patch base (`GUEST_HAL_SHA_EXPECT`).

## External artifacts (not in the repo)

The runner references large, host-local lab artifacts that are deliberately kept
out of version control (system images, emulator, AVDs, the APK). They default to
`/home/cdmonio/apk-work` and `/home/cdmonio/android-sdk` and are overridable via
env: `MAGSTV_SDK`, `MAGSTV_LAB`, `MAGSTV_EMU_HOME`, `MAGSTV_SYSIMG`, `MAGSTV_DTB`,
`MAGSTV_PATCHED_HAL`, `MAGSTV_AVD_SRC_HOME`, `MAGSTV_AVD_NAME`.

Required, with pinned hashes checked by the runner's preflight:
- Emulator `28.0.25`; API 27 arm64 `google_apis` system image.
- `ranchu-api27-fixed.dtb` — sha256 `ccc2f279…135e57` (fixes the system partition
  path `a003800`→`a003600`).
- patched HAL — sha256 `be184a34…1998e`, built by `patch_audio_hal.py` from the
  guest HAL sha256 `220d1868…5203`.

## L3 — offline APK execution (`run-apk`, gated)

`run-apk` boots+patches, then installs the APK offline, launches it once without
credentials, and collects loader-focused artifacts (install/verify output,
`ps -A`, the app's `/proc/<pid>/maps`, logcat, tombstones, guest sockets) plus
containment evidence (guest external reachability before and after). The
disposable AVD is destroyed afterward.

It is **gated off by default**: without `MAGSTV_ALLOW_APK_EXEC=1` it boots,
patches, records `apk_executed: no (gated)`, and stops — nothing untrusted runs.
The network stays isolated the whole time; the run aborts if the guest can reach
outside before the install.

```sh
# harness only (no APK executed):
MAGSTV_APK=/path/to/com.android.mgstv.apk \
  scripts/magstv-lab/magstv-lab-runner.sh run-apk

# actually execute the APK in the isolated lab (opt-in):
MAGSTV_APK=/path/to/com.android.mgstv.apk MAGSTV_ALLOW_APK_EXEC=1 \
  scripts/magstv-lab/magstv-lab-runner.sh run-apk
```

## Known limitation — this emulator can't run the APK's native code

Empirically (plan `0037`, L3), the APK does **not** install or run on this
QEMU-TCG ARM64 setup. The install helper `DefaultContainerService`, plus
`omx@1.0-service` and the audio HAL, all SIGSEGV with the same non-canonical
`0xffffff8…` garbage-pointer signature — a **systemic TCG codegen bug**, not
anything specific to MAGSTV. Because TCG translates the offending instruction
the same way regardless of `-cpu`, the APK's own protected native loader would
crash the same way. Dynamic execution/capture therefore needs a **native ARM64
runtime** (a physical device or an ARM host with KVM), not this x86-hosted TCG
emulator. Containment held throughout (no network egress).

## Usage

```sh
scripts/magstv-lab/magstv-lab-runner.sh boot 3
```

Requires `bwrap` (bubblewrap) and the external artifacts above. The disposable
AVD copy is destroyed after each run by default (`MAGSTV_KEEP_AVD=1` to keep it).
