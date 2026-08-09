#!/usr/bin/env bash
set -u -o pipefail

umask 077

migrate_bin=${JELLYRIN_MIGRATE_BIN:-/usr/local/bin/jellyrin-migrate}
journalctl_bin=${JELLYRIN_JOURNALCTL_BIN:-journalctl}
systemctl_bin=${JELLYRIN_SYSTEMCTL_BIN:-systemctl}
proc_root=${JELLYRIN_PROC_ROOT:-/proc}
cgroup_root=${JELLYRIN_CGROUP_ROOT:-/sys/fs/cgroup}
unit=jellyrin.service
since=
relay_port=
report=
use_default_logs=1
declare -a explicit_logs=()
declare -a log_directories=()

usage() {
    cat <<'EOF'
Usage: sudo ops/audit-runtime-hygiene.sh --since TIME --relay-port PORT --report PATH [options]

Takes a bounded journal snapshot, scans regular Jellyrin/Nginx logs and snapshots every current
process argv in the Jellyrin cgroup. Output and the report contain counts only.

Options:
  --since TIME          Required journal lower bound (prefer the recorded rollout RFC3339 time)
  --relay-port PORT     Required internal Jellyrin HTTP port
  --report PATH         Required new counts-only JSON evidence path
  --unit UNIT           systemd unit to inspect (default: jellyrin.service)
  --log PATH            Additional required regular log file (repeatable)
  --log-dir PATH        Additional required real log directory (repeatable)
  --no-default-logs     Do not require /var/log/jellyrin and the two Jellyrin Nginx logs
  -h, --help            Show this help
EOF
}

fail_usage() {
    printf 'error: invalid runtime hygiene audit arguments\n' >&2
    usage >&2
    exit 64
}

while (($#)); do
    case "$1" in
        --since | --relay-port | --report | --unit | --log | --log-dir)
            (($# >= 2)) && [[ -n $2 ]] || fail_usage
            option=$1
            value=$2
            shift 2
            case "$option" in
                --since) since=$value ;;
                --relay-port) relay_port=$value ;;
                --report) report=$value ;;
                --unit) unit=$value ;;
                --log) explicit_logs+=("$value") ;;
                --log-dir) log_directories+=("$value") ;;
            esac
            ;;
        --no-default-logs)
            use_default_logs=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) fail_usage ;;
    esac
done

[[ -n $since && -n $relay_port && -n $report ]] || fail_usage
[[ $relay_port =~ ^[0-9]+$ ]] && ((relay_port >= 1 && relay_port <= 65535)) || fail_usage
[[ ! -e $report && ! -L $report ]] || {
    printf 'error: runtime hygiene report path must be new\n' >&2
    exit 3
}
[[ -x $migrate_bin ]] || {
    printf 'error: runtime hygiene scanner is unavailable\n' >&2
    exit 3
}

audit_tmp=$(mktemp -d "${TMPDIR:-/tmp}/jellyrin-runtime-hygiene.XXXXXXXX") || {
    printf 'error: cannot create private audit workspace\n' >&2
    exit 3
}
cleanup() {
    find "$audit_tmp" -type f -exec shred -u -- {} + 2>/dev/null || true
    rm -rf -- "$audit_tmp"
}
trap cleanup EXIT HUP INT TERM

declare -a scanner_args=(audit-runtime-hygiene --relay-port "$relay_port" --report "$report")
incomplete=0
journal_snapshot=$audit_tmp/journal.log
if "$journalctl_bin" --unit "$unit" --since "$since" --output=cat --no-pager --quiet \
    >"$journal_snapshot" 2>/dev/null; then
    scanner_args+=(--log "$journal_snapshot")
else
    incomplete=1
    scanner_args+=(--log "$audit_tmp/journal-incomplete")
fi

if ((use_default_logs)); then
    log_directories+=(/var/log/jellyrin)
    explicit_logs+=(/var/log/nginx/jellyrin.access.log /var/log/nginx/jellyrin.error.log)
fi

directory_index=0
for directory in "${log_directories[@]}"; do
    directory_index=$((directory_index + 1))
    symlink_marker=$audit_tmp/log-symlinks-$directory_index
    file_listing=$audit_tmp/log-files-$directory_index
    if [[ ! -d $directory || ! -r $directory || ! -x $directory || -L $directory ]] ||
        ! find "$directory" -type l -print -quit >"$symlink_marker" 2>/dev/null ||
        [[ -s $symlink_marker ]] ||
        ! find "$directory" -type f -print0 >"$file_listing" 2>/dev/null; then
        incomplete=1
        scanner_args+=(--log "$audit_tmp/log-directory-incomplete")
        continue
    fi
    mapfile -d '' directory_files <"$file_listing"
    for path in "${directory_files[@]}"; do
        scanner_args+=(--log "$path")
    done
done
for path in "${explicit_logs[@]}"; do
    scanner_args+=(--log "$path")
done

control_group=$($systemctl_bin show "$unit" --property=ControlGroup --value 2>/dev/null) || control_group=
if [[ ! $control_group =~ ^/[A-Za-z0-9_.@:/-]+$ || $control_group == *..* ]]; then
    incomplete=1
    scanner_args+=(--argv "$audit_tmp/cgroup-incomplete")
else
    cgroup_procs=${cgroup_root}${control_group}/cgroup.procs
    if [[ ! -f $cgroup_procs || ! -r $cgroup_procs || -L $cgroup_procs ]]; then
        incomplete=1
        scanner_args+=(--argv "$audit_tmp/cgroup-incomplete")
    else
        process_count=0
        while IFS= read -r pid; do
            [[ $pid =~ ^[1-9][0-9]*$ ]] || {
                incomplete=1
                continue
            }
            source_cmdline=$proc_root/$pid/cmdline
            snapshot=$audit_tmp/cmdline-$pid
            if [[ -L $source_cmdline ]]; then
                incomplete=1
            elif cp -- "$source_cmdline" "$snapshot" 2>/dev/null; then
                scanner_args+=(--argv "$snapshot")
                process_count=$((process_count + 1))
            elif [[ -e $source_cmdline ]]; then
                incomplete=1
            fi
        done <"$cgroup_procs"
        if ((process_count == 0)); then
            incomplete=1
            scanner_args+=(--argv "$audit_tmp/cgroup-empty")
        fi
    fi
fi

if ((incomplete)); then
    scanner_args+=(--log "$audit_tmp/preflight-incomplete")
fi

set +e
"$migrate_bin" "${scanner_args[@]}" >"$audit_tmp/stdout.json" 2>/dev/null
status=$?
set -e
cat "$audit_tmp/stdout.json"
case $status in
    0) printf 'Runtime hygiene audit passed.\n' ;;
    2) printf 'Runtime hygiene audit found credential-bearing material.\n' >&2 ;;
    *)
        status=3
        printf 'Runtime hygiene audit was incomplete.\n' >&2
        ;;
esac
exit "$status"
