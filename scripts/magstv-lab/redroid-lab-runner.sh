#!/usr/bin/env bash
# Reproducible runner for the current QEMU 8.2 + Ubuntu ARM64 + Redroid lab.
#
# This is a preparation runner. It opens setup.qcow2 directly and mutably, so
# it must never be used to execute an APK. The only host-forwarded port is SSH.
set -euo pipefail

readonly LAB="${MAGSTV_REDROID_LAB:-/home/cdmonio/apk-work/arm64-redroid-lab}"
readonly QEMU_BIN="${MAGSTV_REDROID_QEMU:-/usr/bin/qemu-system-aarch64}"
readonly QEMU_IMG="${MAGSTV_REDROID_QEMU_IMG:-/usr/bin/qemu-img}"
readonly AAVMF_CODE="${MAGSTV_REDROID_AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.fd}"
readonly AAVMF_VARS="$LAB/AAVMF_VARS.fd"
readonly DISK="$LAB/setup.qcow2"
readonly SSH_KEY="$LAB/lab_ed25519"
readonly SSH_KNOWN_HOSTS="$LAB/ssh_known_hosts"
readonly SSH_USER="${MAGSTV_REDROID_SSH_USER:-lab}"
readonly SSH_PORT="${MAGSTV_REDROID_SSH_PORT:-2224}"
readonly PID_FILE="$LAB/qemu.pid"
readonly MONITOR_SOCKET="$LAB/monitor.sock"
readonly SERIAL_LOG="$LAB/serial.log"
readonly LOCK_FILE="$LAB/runner.lock"
readonly UNIT="${MAGSTV_REDROID_UNIT:-magstv-redroid-lab.service}"
LAB_LOCK_FD=""

readonly QEMU_VERSION_PREFIX="QEMU emulator version 8.2."
readonly AAVMF_CODE_SHA256="4a4cb7f6d8106bb2a7dd8c763fab14b1810152136fc4304e5b728f0043e84f12"
readonly UBUNTU_IMAGE_SHA256="7df0201546f75b8bcc1044594c806c35749421ad3c9bc1be2a3ab806cfae39cc"
readonly SEED_ISO_SHA256="3fbc36d8842e20120ae4daf0ac1a8cefef8553e25ecf5ec64bc4540fdc65084e"
readonly REDROID_OCI_SHA256="1a4fa63a8b3ee2ba7a079e4b297113afa223061bb095e13227e0ac03e91581ac"
readonly REDROID_ISO_SHA256="881c214a42ab45155a3d3a571496c1db3632c6edfcf9d9a0a3305f309b31f832"
readonly REDROID_INIT_SHA256="c6c28632167102d0234c604381dd9873f4f9ac82f1ad2824d8bdc6f493e0d563"
readonly REDROID_MANIFEST_DIGEST="8b95febfd6ef411bb73cad0b6f30ae3ec10f2216c8f8a58052417ef6792fc8b5"
readonly REDROID_CONFIG_DIGEST="c38107720ad923a0aa1379412b4a53d2e5c5a192663cbd2bd0657e4d521b89f3"
readonly SSH_PUBLIC_KEY_SHA256="7bcd98cf3186eaf2ae8ec4883bfa83ff2537ce7f697a1981a3e0071505ee0a9c"
readonly SSH_KNOWN_HOSTS_SHA256="ba7363a3bc468cab97d1f80442fd95e72d9302b6d7e16059605b90532198e653"

readonly -a EXPECTED_QEMU_ARGV=(
  "$QEMU_BIN"
  -name magstv-arm64-redroid-lab
  -machine virt,gic-version=3,virtualization=on
  -cpu max
  -smp 4
  -m 10240
  -accel tcg,thread=single
  -drive "if=pflash,format=raw,unit=0,file=$AAVMF_CODE,readonly=on"
  -drive "if=pflash,format=raw,unit=1,file=$AAVMF_VARS"
  -drive "file=$DISK,if=virtio,format=qcow2,cache=none,aio=threads,discard=unmap"
  -device virtio-rng-pci
  -netdev "user,id=net0,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22"
  -device virtio-net-pci,netdev=net0,romfile=
  -display none
  -serial "file:$SERIAL_LOG"
  -monitor "unix:$MONITOR_SOCKET,server=on,wait=off"
  -pidfile "$PID_FILE"
)

log() {
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: redroid-lab-runner.sh COMMAND

Commands:
  doctor  Validate host tools and hashes; validate the QCOW2 when stopped.
  start   Start QEMU under a transient systemd user service.
  status  Report the systemd service and validated QEMU process state.
  stop    Gracefully stop only the validated, systemd-managed lab QEMU.
  check   Validate Ubuntu, Docker, BinderFS, Redroid, and Android over SSH.

PREPARATION ONLY: this runner opens setup.qcow2 mutably. It never authorizes or
installs an APK, reads an .env file, starts ADB, or attaches setup ISOs.
EOF
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

reject_forbidden_environment() {
  local variable

  for variable in \
    MAGSTV_APK \
    MAGSTV_ALLOW_APK_EXEC \
    MAGSTV_USERNAME \
    MAGSTV_PASSWORD; do
    if declare -p "$variable" >/dev/null 2>&1; then
      die "preparation runner refuses environment variable: $variable"
    fi
  done
}

pid_from_file() {
  local pid

  [[ -f "$PID_FILE" ]] || return 1
  IFS= read -r pid < "$PID_FILE" || return 1
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  printf '%s\n' "$pid"
}

unit_main_pid() {
  local pid

  pid="$(systemctl --user show "$UNIT" --property=MainPID --value 2>/dev/null || true)"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  printf '%s\n' "$pid"
}

pid_is_expected_qemu() {
  local pid="$1"
  local exe expected_exe index
  local -a arguments=()

  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  [[ -r "/proc/$pid/cmdline" ]] || return 1
  exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
  expected_exe="$(readlink -f "$QEMU_BIN" 2>/dev/null || true)"
  [[ -n "$expected_exe" && "$exe" == "$expected_exe" ]] || return 1
  mapfile -d '' -t arguments < "/proc/$pid/cmdline"

  [[ "${#arguments[@]}" -eq "${#EXPECTED_QEMU_ARGV[@]}" ]] || return 1
  for index in "${!EXPECTED_QEMU_ARGV[@]}"; do
    [[ "${arguments[$index]}" == "${EXPECTED_QEMU_ARGV[$index]}" ]] || return 1
  done
}

current_qemu_pid() {
  local pid

  pid="$(pid_from_file 2>/dev/null || true)"
  if [[ -n "$pid" ]] && pid_is_expected_qemu "$pid"; then
    printf '%s\n' "$pid"
    return 0
  fi

  pid="$(unit_main_pid 2>/dev/null || true)"
  if [[ -n "$pid" ]] && pid_is_expected_qemu "$pid"; then
    printf '%s\n' "$pid"
    return 0
  fi

  return 1
}

disk_open_pids() (
  local target_identity fd_path fd_identity pid
  local -A seen=()

  [[ -e "$DISK" ]] || return 1
  target_identity="$(stat -Lc '%d:%i' "$DISK")" || return 1
  shopt -s nullglob

  for fd_path in /proc/[1-9][0-9]*/fd/* /proc/[1-9]/fd/*; do
    fd_identity="$(stat -Lc '%d:%i' "$fd_path" 2>/dev/null || true)"
    [[ "$fd_identity" == "$target_identity" ]] || continue
    pid="${fd_path#/proc/}"
    pid="${pid%%/*}"
    seen["$pid"]=1
  done

  ((${#seen[@]} > 0)) || return 0
  printf '%s\n' "${!seen[@]}" | sort -n | paste -sd, -
)

unique_archive_name() {
  local requested="$1"
  local candidate="$requested"
  local suffix=1

  while [[ -e "$candidate" || -L "$candidate" ]]; do
    candidate="${requested}.${suffix}"
    suffix=$((suffix + 1))
  done
  printf '%s\n' "$candidate"
}

archive_stale_path() {
  local path="$1"
  local label="$2"
  local extension="$3"
  local timestamp destination

  [[ -e "$path" || -L "$path" ]] || return 0
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  destination="$(unique_archive_name "$LAB/${label}-stale-${timestamp}.${extension}")"
  mv -- "$path" "$destination"
  log "archived stale $(basename "$path") as $(basename "$destination")"
}

runtime_preflight() {
  local version code_sha

  require_command systemctl
  require_command systemd-run
  require_command sha256sum
  require_command stat
  [[ -x "$QEMU_BIN" ]] || die "QEMU executable missing: $QEMU_BIN"
  [[ -x "$QEMU_IMG" ]] || die "qemu-img executable missing: $QEMU_IMG"
  [[ -d "$LAB" && -w "$LAB" ]] || die "lab directory is missing or not writable: $LAB"
  [[ -r "$AAVMF_CODE" ]] || die "AAVMF code image missing: $AAVMF_CODE"
  [[ -r "$AAVMF_VARS" && -w "$AAVMF_VARS" ]] || die "writable AAVMF vars missing: $AAVMF_VARS"
  [[ -r "$DISK" && -w "$DISK" ]] || die "writable lab disk missing: $DISK"

  version="$("$QEMU_BIN" --version | head -n 1)"
  [[ "$version" == "$QEMU_VERSION_PREFIX"* ]] ||
    die "expected QEMU 8.2.x, found: $version"
  code_sha="$(sha256sum "$AAVMF_CODE" | awk '{print $1}')"
  [[ "$code_sha" == "$AAVMF_CODE_SHA256" ]] ||
    die "AAVMF code sha256 mismatch: $code_sha"

  systemctl --user show-environment >/dev/null 2>&1 ||
    die "the systemd user manager is not available"
}

sha_check() {
  local label="$1"
  local path="$2"
  local expected="$3"
  local required="$4"
  local actual

  if [[ ! -f "$path" ]]; then
    if [[ "$required" == "yes" ]]; then
      printf 'FAIL  %-22s missing: %s\n' "$label" "$path"
      return 1
    fi
    printf 'SKIP  %-22s optional artifact absent\n' "$label"
    return 0
  fi

  actual="$(sha256sum "$path" | awk '{print $1}')"
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL  %-22s sha256 %s (expected %s)\n' "$label" "$actual" "$expected"
    return 1
  fi
  printf 'OK    %-22s sha256 %s\n' "$label" "$actual"
}

oci_layout_check() {
  local archive="$LAB/redroid-8.1-arm64.oci.tar"
  local index manifest manifest_sha config_sha

  index="$(tar -xOf "$archive" index.json 2>/dev/null || true)"
  grep -Fq -- "\"digest\":\"sha256:$REDROID_MANIFEST_DIGEST\"" <<<"$index" ||
    return 1

  manifest="$(
    tar -xOf "$archive" "blobs/sha256/$REDROID_MANIFEST_DIGEST" 2>/dev/null ||
      true
  )"
  manifest_sha="$(printf '%s' "$manifest" | sha256sum | awk '{print $1}')"
  [[ "$manifest_sha" == "$REDROID_MANIFEST_DIGEST" ]] || return 1
  grep -Fq -- "\"digest\":\"sha256:$REDROID_CONFIG_DIGEST\"" <<<"$manifest" ||
    return 1

  config_sha="$(
    tar -xOf "$archive" "blobs/sha256/$REDROID_CONFIG_DIGEST" 2>/dev/null |
      sha256sum |
      awk '{print $1}' ||
      true
  )"
  [[ "$config_sha" == "$REDROID_CONFIG_DIGEST" ]]
}

doctor() {
  local failures=0
  local version open_pids format derived_public_key recorded_public_key

  runtime_preflight
  require_command ssh
  require_command ssh-keygen
  require_command tar

  version="$("$QEMU_BIN" --version | head -n 1)"
  printf 'OK    %-22s %s\n' "QEMU" "$version"
  printf 'OK    %-22s %s\n' "systemd user manager" "$(systemctl --user --version | head -n 1)"

  sha_check "AAVMF code" "$AAVMF_CODE" "$AAVMF_CODE_SHA256" yes || failures=$((failures + 1))
  sha_check "Ubuntu ARM64 image" "$LAB/noble-server-cloudimg-arm64.img" "$UBUNTU_IMAGE_SHA256" yes ||
    failures=$((failures + 1))
  sha_check "cloud-init seed" "$LAB/seed.iso" "$SEED_ISO_SHA256" no || failures=$((failures + 1))
  sha_check "Redroid OCI archive" "$LAB/redroid-8.1-arm64.oci.tar" "$REDROID_OCI_SHA256" yes ||
    failures=$((failures + 1))
  if oci_layout_check; then
    printf 'OK    %-22s manifest %s; config %s\n' \
      "Redroid OCI layout" "$REDROID_MANIFEST_DIGEST" "$REDROID_CONFIG_DIGEST"
  else
    printf 'FAIL  %-22s manifest/config digest mismatch\n' "Redroid OCI layout"
    failures=$((failures + 1))
  fi
  sha_check "Redroid transfer ISO" "$LAB/redroid-8.1-arm64.iso" "$REDROID_ISO_SHA256" no ||
    failures=$((failures + 1))
  sha_check "Redroid init config" "$LAB/zz-magstv-lab.rc" "$REDROID_INIT_SHA256" yes ||
    failures=$((failures + 1))
  sha_check "SSH public key" "$LAB/lab_ed25519.pub" "$SSH_PUBLIC_KEY_SHA256" yes ||
    failures=$((failures + 1))
  sha_check "SSH known hosts" "$SSH_KNOWN_HOSTS" "$SSH_KNOWN_HOSTS_SHA256" yes ||
    failures=$((failures + 1))

  if [[ ! -f "$SSH_KEY" ]]; then
    printf 'FAIL  %-22s missing: %s\n' "SSH private key" "$SSH_KEY"
    failures=$((failures + 1))
  elif [[ "$(stat -c '%a' "$SSH_KEY")" != "600" ]]; then
    printf 'FAIL  %-22s mode is %s (expected 600)\n' "SSH private key" "$(stat -c '%a' "$SSH_KEY")"
    failures=$((failures + 1))
  else
    printf 'OK    %-22s mode 600\n' "SSH private key"
    derived_public_key="$(
      ssh-keygen -y -f "$SSH_KEY" 2>/dev/null |
        awk '{print $1 " " $2}' ||
        true
    )"
    recorded_public_key="$(awk '{print $1 " " $2}' "$LAB/lab_ed25519.pub" 2>/dev/null || true)"
    if [[ "$derived_public_key" != "$recorded_public_key" ]]; then
      printf 'FAIL  %-22s private/public key mismatch\n' "SSH key pair"
      failures=$((failures + 1))
    else
      printf 'OK    %-22s private/public keys match\n' "SSH key pair"
    fi
  fi

  open_pids="$(disk_open_pids 2>/dev/null || true)"
  if [[ -n "$open_pids" ]]; then
    printf 'SKIP  %-22s disk inode open by PID(s) %s\n' "QCOW2 metadata" "$open_pids"
    printf 'SKIP  %-22s disk inode open by PID(s) %s\n' "qemu-img check" "$open_pids"
  else
    format="$("$QEMU_IMG" info --output=json "$DISK" 2>/dev/null || true)"
    if ! grep -Eq '"format"[[:space:]]*:[[:space:]]*"qcow2"' <<<"$format"; then
      printf 'FAIL  %-22s is not a readable QCOW2\n' "prepared disk"
      failures=$((failures + 1))
    else
      printf 'OK    %-22s format qcow2\n' "prepared disk"
    fi

    open_pids="$(disk_open_pids 2>/dev/null || true)"
    if [[ -n "$open_pids" ]]; then
      printf 'SKIP  %-22s disk opened meanwhile by PID(s) %s\n' "qemu-img check" "$open_pids"
    elif "$QEMU_IMG" check "$DISK"; then
      printf 'OK    %-22s no errors\n' "qemu-img check"
    else
      printf 'FAIL  %-22s disk check failed\n' "qemu-img check"
      failures=$((failures + 1))
    fi
  fi

  if ((failures > 0)); then
    die "doctor found $failures failure(s)"
  fi
  log "doctor: all required checks passed"
}

ssh_options() {
  printf '%s\0' \
    -i "$SSH_KEY" \
    -p "$SSH_PORT" \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=yes \
    -o "UserKnownHostsFile=$SSH_KNOWN_HOSTS" \
    -o GlobalKnownHostsFile=/dev/null \
    -o ConnectTimeout=15 \
    -o ConnectionAttempts=1 \
    -o ServerAliveInterval=5 \
    -o ServerAliveCountMax=3 \
    -o LogLevel=ERROR
}

ssh_guest_timed() {
  local duration="$1"
  local options=()
  shift

  require_command timeout
  while IFS= read -r -d '' option; do
    options+=("$option")
  done < <(ssh_options)
  timeout --foreground --kill-after=5 "$duration" \
    ssh "${options[@]}" "$SSH_USER@127.0.0.1" "$@"
}

port_is_busy() {
  if command -v ss >/dev/null 2>&1; then
    [[ -n "$(ss -H -ltn "sport = :$SSH_PORT" 2>/dev/null)" ]]
    return
  fi
  return 1
}

rollback_new_unit() {
  local main_pid

  main_pid="$(unit_main_pid 2>/dev/null || true)"
  if [[ -n "$main_pid" ]] && pid_is_expected_qemu "$main_pid"; then
    log "rolling back newly created QEMU unit (PID $main_pid)"
    systemctl --user stop "$UNIT" || true
  elif systemctl --user is-active --quiet "$UNIT"; then
    log "refusing automatic rollback: $UNIT has an unexpected MainPID" >&2
  fi
}

start_vm() {
  local running open_pids pid attempt

  runtime_preflight
  open_pids="$(disk_open_pids 2>/dev/null || true)"
  [[ -z "$open_pids" ]] ||
    die "prepared disk inode is already open by PID(s): $open_pids"
  running="$(current_qemu_pid 2>/dev/null || true)"
  [[ -z "$running" ]] || die "lab QEMU is already running as PID $running"

  if systemctl --user is-active --quiet "$UNIT"; then
    die "$UNIT is active but is not the expected lab QEMU; refusing to replace it"
  fi
  port_is_busy && die "loopback TCP port $SSH_PORT is already in use"

  archive_stale_path "$PID_FILE" qemu pid
  archive_stale_path "$MONITOR_SOCKET" monitor sock
  archive_stale_path "$SERIAL_LOG" serial log
  systemctl --user reset-failed "$UNIT" >/dev/null 2>&1 || true

  log "starting QEMU 8.2 ARM64 lab under $UNIT"
  if ! systemd-run --user \
      --unit="$UNIT" \
      --collect \
      --service-type=exec \
      --property=WorkingDirectory="$LAB" \
      --property=StandardOutput=journal \
      --property=StandardError=journal \
      --property=KillMode=mixed \
      --property=TimeoutStopSec=45s \
      -- "${EXPECTED_QEMU_ARGV[@]}"; then
    rollback_new_unit
    die "systemd-run failed to create the QEMU service"
  fi

  pid=""
  for attempt in $(seq 1 50); do
    pid="$(current_qemu_pid 2>/dev/null || true)"
    [[ -n "$pid" ]] && break
    systemctl --user is-failed --quiet "$UNIT" && break
    sleep 0.2
  done
  if [[ -z "$pid" ]]; then
    rollback_new_unit
    die "QEMU did not create a valid PID; inspect: journalctl --user -u $UNIT"
  fi

  log "started validated QEMU PID $pid; SSH will appear on 127.0.0.1:$SSH_PORT"
  log "logs: journalctl --user -u $UNIT -f"
}

status_vm() {
  local pid file_pid main_pid open_pids active sub ssh_state

  active="$(systemctl --user show "$UNIT" --property=ActiveState --value 2>/dev/null || echo unknown)"
  sub="$(systemctl --user show "$UNIT" --property=SubState --value 2>/dev/null || echo unknown)"
  main_pid="$(unit_main_pid 2>/dev/null || true)"
  file_pid="$(pid_from_file 2>/dev/null || true)"
  pid="$(current_qemu_pid 2>/dev/null || true)"
  open_pids="$(disk_open_pids 2>/dev/null || true)"
  ssh_state="not-listening"
  port_is_busy && ssh_state="listening"

  printf 'unit:       %s\n' "$UNIT"
  printf 'unit_state: %s/%s\n' "${active:-unknown}" "${sub:-unknown}"
  printf 'main_pid:   %s\n' "${main_pid:-none}"
  printf 'pid_file:   %s\n' "${file_pid:-none}"
  printf 'ssh:        127.0.0.1:%s (%s)\n' "$SSH_PORT" "$ssh_state"

  if [[ -n "$pid" ]]; then
    if [[ -z "$main_pid" || "$pid" != "$main_pid" ]]; then
      printf 'qemu:       exact argv found but unmanaged (PID %s)\n' "$pid"
      printf 'ownership:  not managed by %s\n' "$UNIT"
      return 4
    else
      printf 'qemu:       running and validated (PID %s)\n' "$pid"
      printf 'ownership:  systemd user service validated\n'
      return 0
    fi
  fi

  if [[ -n "$file_pid" ]]; then
    if [[ -n "$open_pids" ]]; then
      printf 'qemu:       unmanaged/unexpected process has disk open (PID(s) %s)\n' "$open_pids"
    else
      printf 'qemu:       stopped; PID file is stale or does not identify this lab\n'
    fi
    return 4
  fi
  if [[ -n "$open_pids" ]]; then
    printf 'qemu:       unmanaged/unexpected process has disk open (PID(s) %s)\n' "$open_pids"
    return 4
  fi
  printf 'qemu:       stopped\n'
  return 3
}

stop_vm() {
  local pid main_pid attempt replacement_pid

  require_command systemctl
  pid="$(current_qemu_pid 2>/dev/null || true)"
  [[ -n "$pid" ]] || die "no validated lab QEMU is running"
  pid_is_expected_qemu "$pid" || die "PID $pid is not the expected qemu-system-aarch64"

  main_pid="$(unit_main_pid 2>/dev/null || true)"
  [[ "$main_pid" == "$pid" ]] ||
    die "PID $pid is not owned by $UNIT; refusing to stop an unmanaged QEMU"

  log "requesting a graceful guest shutdown over SSH"
  if [[ -f "$SSH_KEY" ]]; then
    ssh_guest_timed 30 "sudo systemctl poweroff" >/dev/null 2>&1 || true
  fi

  for attempt in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 2
  done

  if kill -0 "$pid" 2>/dev/null; then
    pid_is_expected_qemu "$pid" ||
      die "PID $pid changed identity while waiting; refusing to signal it"
    main_pid="$(unit_main_pid 2>/dev/null || true)"
    [[ "$main_pid" == "$pid" ]] ||
      die "$UNIT changed MainPID while waiting; refusing to stop it"
    log "guest did not power off in time; stopping the validated systemd service"
    systemctl --user stop "$UNIT"
  fi
  if kill -0 "$pid" 2>/dev/null && pid_is_expected_qemu "$pid"; then
    die "validated QEMU PID $pid is still running"
  fi

  replacement_pid="$(current_qemu_pid 2>/dev/null || true)"
  [[ -z "$replacement_pid" ]] ||
    die "a replacement lab QEMU appeared as PID $replacement_pid; refusing to archive its files"
  archive_stale_path "$PID_FILE" qemu pid
  archive_stale_path "$MONITOR_SOCKET" monitor sock
  log "lab VM stopped; serial evidence remains at $SERIAL_LOG"
}

check_vm() {
  local pid main_pid

  require_command ssh
  [[ -f "$SSH_KEY" ]] || die "SSH key missing: $SSH_KEY"
  pid="$(current_qemu_pid 2>/dev/null || true)"
  [[ -n "$pid" ]] || die "no validated lab QEMU is running"
  main_pid="$(unit_main_pid 2>/dev/null || true)"
  [[ "$main_pid" == "$pid" ]] ||
    die "validated QEMU is not owned by $UNIT; refusing to inspect it"

  log "checking the guest over SSH; this does not use ADB or copy any file"
  ssh_guest_timed 180 "bash -s" <<'REMOTE_CHECK'
set -euo pipefail

printf 'ubuntu_arch: %s\n' "$(uname -m)"
[[ "$(uname -m)" == "aarch64" ]]
command -v timeout >/dev/null

docker_version="$(sudo docker version --format '{{.Server.Version}}')"
printf 'docker_server: %s\n' "$docker_version"
sudo systemctl is-active --quiet docker

sudo systemctl is-active --quiet redroid-binderfs.service
mountpoint -q /dev/binderfs
for device in binder hwbinder vndbinder; do
  [[ -c "/dev/binderfs/$device" ]]
done
printf 'binderfs: active (binder,hwbinder,vndbinder)\n'

grep -Eq '^[[:space:]]*/dev/vda16[[:space:]]+/boot[[:space:]]+ext4[[:space:]]+noauto,nofail[[:space:]]+0[[:space:]]+2([[:space:]]*#.*)?$' /etc/fstab
grep -Eq '^[[:space:]]*/dev/vda15[[:space:]]+/boot/efi[[:space:]]+vfat[[:space:]]+noauto,nofail,umask=0077[[:space:]]+0[[:space:]]+1([[:space:]]*#.*)?$' /etc/fstab
printf 'fstab_boot_mounts: /dev/vda16=noauto,nofail /dev/vda15=noauto,nofail\n'

for masked_unit in \
  boot.mount \
  boot-efi.mount \
  serial-getty@ttyAMA0.service \
  systemd-networkd-wait-online.service \
  snapd.service \
  snapd.socket; do
  masked_state="$(systemctl is-enabled "$masked_unit" 2>/dev/null || true)"
  [[ "$masked_state" == "masked" ]]
done
printf 'tcg_boot_masks: critical units masked\n'

for enabled_unit in \
  docker.service \
  containerd.service \
  redroid-binderfs.service \
  ssh.socket; do
  enabled_state="$(systemctl is-enabled "$enabled_unit" 2>/dev/null || true)"
  [[ "$enabled_state" == "enabled" ]]
done
printf 'required_units: docker containerd binderfs ssh.socket enabled\n'

container="redroid-clean1"
state="$(sudo docker inspect --format '{{.State.Status}}' "$container")"
[[ "$state" == "running" ]]
restarts="$(sudo docker inspect --format '{{.RestartCount}}' "$container")"
[[ "$restarts" == "0" ]]
image_reference="$(sudo docker inspect --format '{{.Config.Image}}' "$container")"
case "$image_reference" in
  redroid/redroid:8.1.0|redroid/redroid:8.1.0-latest)
    ;;
  *)
    exit 1
    ;;
esac
image_id="$(sudo docker inspect --format '{{.Image}}' "$container")"
case "$image_id" in
  sha256:8b95febfd6ef411bb73cad0b6f30ae3ec10f2216c8f8a58052417ef6792fc8b5|\
  sha256:c38107720ad923a0aa1379412b4a53d2e5c5a192663cbd2bd0657e4d521b89f3)
    ;;
  *)
    exit 1
    ;;
esac
image_architecture="$(sudo docker image inspect --format '{{.Architecture}}' "$image_id")"
[[ "$image_architecture" == "arm64" ]]

network_set="$(
  sudo docker inspect \
    --format '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' \
    "$container" |
    sed '/^[[:space:]]*$/d' |
    LC_ALL=C sort
)"
[[ "$network_set" == "redroid-isolated" ]]
network_internal="$(
  sudo docker network inspect --format '{{.Internal}}' redroid-isolated
)"
[[ "$network_internal" == "true" ]]
network_gateway="$(
  sudo docker inspect \
    --format '{{with index .NetworkSettings.Networks "redroid-isolated"}}{{.Gateway}}{{end}}' \
    "$container"
)"
if [[ -n "$network_gateway" ]]; then
  gateway_metadata="present"
else
  gateway_metadata="absent"
fi
default_route_count="$(
  sudo docker exec "$container" /system/bin/toybox cat /proc/net/route |
    awk 'NR > 1 && $2 == "00000000" { count++ } END { print count + 0 }'
)"
[[ "$default_route_count" == "0" ]]

set +e
timeout --foreground --kill-after=2 6 \
  sudo docker exec "$container" \
    /system/bin/toybox nc -w 3 1.1.1.1 443 \
    </dev/null >/dev/null 2>&1
tcp_probe_status=$?
set -e
case "$tcp_probe_status" in
  1|124|137)
    ;;
  0)
    printf 'ERROR: isolated Redroid unexpectedly reached 1.1.1.1:443\n' >&2
    exit 1
    ;;
  *)
    printf 'ERROR: TCP isolation probe was not valid (status %s)\n' "$tcp_probe_status" >&2
    exit 1
    ;;
esac

mount_record="$(
  sudo docker inspect \
    --format '{{range .Mounts}}{{if eq .Destination "/system/etc/init/zz-magstv-lab.rc"}}{{.Source}}|{{.RW}}|{{.Type}}{{end}}{{end}}' \
    "$container"
)"
IFS='|' read -r mount_source mount_rw mount_type <<<"$mount_record"
[[ -n "$mount_source" && "$mount_rw" == "false" && "$mount_type" == "bind" ]]
mount_source_sha="$(sudo sha256sum "$mount_source" | awk '{print $1}')"
container_init_sha="$(
  sudo docker exec "$container" /system/bin/toybox sha256sum \
    /system/etc/init/zz-magstv-lab.rc |
    awk '{print $1}'
)"
[[ "$mount_source_sha" == "c6c28632167102d0234c604381dd9873f4f9ac82f1ad2824d8bdc6f493e0d563" ]]
[[ "$container_init_sha" == "$mount_source_sha" ]]

mmap_rnd_compat_bits="$(
  sudo docker exec "$container" /system/bin/toybox cat \
    /proc/sys/vm/mmap_rnd_compat_bits |
    tr -d '\r'
)"
randomize_va_space="$(
  sudo docker exec "$container" /system/bin/toybox cat \
    /proc/sys/kernel/randomize_va_space |
    tr -d '\r'
)"
dalvik_extra_opts="$(
  sudo docker exec "$container" /system/bin/getprop dalvik.vm.extra-opts |
    tr -d '\r'
)"
[[ "$mmap_rnd_compat_bits" == "15" ]]
[[ "$randomize_va_space" == "2" ]]
[[ "$dalvik_extra_opts" == "-Xnorelocate" ]]

boot_completed="$(
  sudo docker exec "$container" /system/bin/getprop sys.boot_completed |
    tr -d '\r'
)"
abi="$(
  sudo docker exec "$container" /system/bin/getprop ro.product.cpu.abilist |
    tr -d '\r'
)"
android_release="$(
  sudo docker exec "$container" /system/bin/getprop ro.build.version.release |
    tr -d '\r'
)"
android_sdk="$(
  sudo docker exec "$container" /system/bin/getprop ro.build.version.sdk |
    tr -d '\r'
)"
[[ "$android_release" == "8.1.0" ]]
[[ "$android_sdk" == "27" ]]
[[ "$abi" == "arm64-v8a,armeabi-v7a,armeabi" ]]
printf 'redroid_container: %s (%s, image=%s, arch=%s, restarts=%s)\n' \
  "$container" "$state" "$image_reference" "$image_architecture" "$restarts"
printf 'redroid_network: %s (internal=%s, gateway-metadata=%s, default_routes=%s)\n' \
  "$network_set" "$network_internal" "$gateway_metadata" "$default_route_count"
printf 'tcp_egress_probe: blocked (status=%s)\n' "$tcp_probe_status"
printf 'redroid_init_bind: read-only sha256=%s\n' "$container_init_sha"
printf 'aslr: randomize_va_space=%s mmap_rnd_compat_bits=%s\n' \
  "$randomize_va_space" "$mmap_rnd_compat_bits"
printf 'dalvik.vm.extra-opts: %s\n' "$dalvik_extra_opts"
printf 'android_version: %s (SDK %s)\n' "$android_release" "$android_sdk"
printf 'android_abi: %s\n' "$abi"
printf 'sys.boot_completed: %s\n' "$boot_completed"
[[ "$boot_completed" == "1" ]]
REMOTE_CHECK
  log "check: Docker, BinderFS, boot prerequisites, Redroid, and Android are healthy"
}

acquire_lab_lock() {
  local path_identity fd_identity path_identity_after owner mode links fd_links

  require_command flock
  [[ -d "$LAB" ]] || die "lab directory is missing: $LAB"

  if [[ ! -e "$LOCK_FILE" && ! -L "$LOCK_FILE" ]]; then
    (
      umask 077
      set -o noclobber
      : > "$LOCK_FILE"
    ) 2>/dev/null || true
  fi

  [[ -f "$LOCK_FILE" && ! -L "$LOCK_FILE" ]] ||
    die "lock must be a regular, non-symlink file: $LOCK_FILE"
  owner="$(stat -c '%u' "$LOCK_FILE")"
  [[ "$owner" == "$EUID" ]] || die "lock is not owned by the current user"
  links="$(stat -c '%h' "$LOCK_FILE")"
  [[ "$links" == "1" ]] || die "lock must not have additional hard links"
  mode="$(stat -c '%a' "$LOCK_FILE")"
  (( (8#$mode & 0022) == 0 )) ||
    die "lock must not be group/world writable (mode $mode)"

  path_identity="$(stat -Lc '%d:%i' "$LOCK_FILE")"
  exec {LAB_LOCK_FD}<>"$LOCK_FILE"
  fd_identity="$(stat -Lc '%d:%i' "/proc/$$/fd/$LAB_LOCK_FD")"
  path_identity_after="$(stat -Lc '%d:%i' "$LOCK_FILE" 2>/dev/null || true)"
  fd_links="$(stat -Lc '%h' "/proc/$$/fd/$LAB_LOCK_FD")"
  if [[ -L "$LOCK_FILE" ||
        "$fd_identity" != "$path_identity" ||
        "$path_identity_after" != "$path_identity" ||
        "$fd_links" != "1" ]]; then
    exec {LAB_LOCK_FD}>&-
    LAB_LOCK_FD=""
    die "lock path changed while it was being opened"
  fi

  flock -n "$LAB_LOCK_FD" ||
    die "another redroid-lab-runner operation holds $LOCK_FILE"

  path_identity_after="$(stat -Lc '%d:%i' "$LOCK_FILE" 2>/dev/null || true)"
  fd_links="$(stat -Lc '%h' "/proc/$$/fd/$LAB_LOCK_FD")"
  if [[ -L "$LOCK_FILE" ||
        "$path_identity_after" != "$fd_identity" ||
        "$fd_links" != "1" ]]; then
    flock -u "$LAB_LOCK_FD"
    exec {LAB_LOCK_FD}>&-
    LAB_LOCK_FD=""
    die "lock path changed while acquiring the lock"
  fi
}

release_lab_lock() {
  [[ -n "$LAB_LOCK_FD" ]] || return 0
  flock -u "$LAB_LOCK_FD"
  exec {LAB_LOCK_FD}>&-
  LAB_LOCK_FD=""
}

main() {
  local command="${1:-}"

  reject_forbidden_environment

  case "$command" in
    doctor)
      [[ $# -eq 1 ]] || die "doctor takes no arguments"
      acquire_lab_lock
      doctor
      release_lab_lock
      ;;
    start)
      [[ $# -eq 1 ]] || die "start takes no arguments"
      acquire_lab_lock
      start_vm
      release_lab_lock
      ;;
    status)
      [[ $# -eq 1 ]] || die "status takes no arguments"
      status_vm
      ;;
    stop)
      [[ $# -eq 1 ]] || die "stop takes no arguments"
      acquire_lab_lock
      stop_vm
      release_lab_lock
      ;;
    check)
      [[ $# -eq 1 ]] || die "check takes no arguments"
      acquire_lab_lock
      check_vm
      release_lab_lock
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
