#!/usr/bin/env bash

# Synthetic lower-bound benchmark for the dormant Redis reevaluation profile.
#
# This does not decide whether Jellyrin should cache a route: it measures the
# extra process memory and the best-case cost of 1 KiB key/value lookups. Run an
# end-to-end endpoint benchmark before enabling Redis in production.

set -euo pipefail

redis_server_bin="${JELLYRIN_REDIS_SERVER_BIN:-$(command -v redis-server || true)}"
redis_cli_bin="${JELLYRIN_REDIS_CLI_BIN:-$(command -v redis-cli || true)}"
redis_benchmark_bin="${JELLYRIN_REDIS_BENCHMARK_BIN:-$(command -v redis-benchmark || true)}"

for required_bin in "$redis_server_bin" "$redis_cli_bin" "$redis_benchmark_bin"; do
  if [[ -z "$required_bin" || ! -x "$required_bin" ]]; then
    echo "redis-server, redis-cli and redis-benchmark are required" >&2
    exit 2
  fi
done

if [[ -n "${JELLYRIN_REDIS_LIBRARY_PATH:-}" ]]; then
  export LD_LIBRARY_PATH="$JELLYRIN_REDIS_LIBRARY_PATH"
fi

keyspace="${JELLYRIN_REDIS_EVAL_KEYSPACE:-50000}"
prefill_requests="${JELLYRIN_REDIS_EVAL_PREFILL_REQUESTS:-500000}"
read_requests="${JELLYRIN_REDIS_EVAL_READ_REQUESTS:-250000}"
value_bytes="${JELLYRIN_REDIS_EVAL_VALUE_BYTES:-1024}"
maxmemory_mb="${JELLYRIN_REDIS_EVAL_MAXMEMORY_MB:-96}"
redis_port="${JELLYRIN_REDIS_EVAL_PORT:-16379}"
saturation_requests="${JELLYRIN_REDIS_EVAL_SATURATION_REQUESTS:-1500000}"

for numeric_value in \
  "$keyspace" "$prefill_requests" "$read_requests" "$value_bytes" \
  "$maxmemory_mb" "$redis_port" "$saturation_requests"
do
  if [[ ! "$numeric_value" =~ ^[0-9]+$ ]]; then
    echo "benchmark numeric settings must contain only decimal digits" >&2
    exit 2
  fi
done
if ((
  keyspace < 1 || prefill_requests < 1 || read_requests < 1 ||
    value_bytes < 1 || saturation_requests < 1
)); then
  echo "keyspace, request counts and value size must be positive" >&2
  exit 2
fi
if (( maxmemory_mb < 16 || maxmemory_mb > 4096 )); then
  echo "maxmemory must be between 16 and 4096 MiB" >&2
  exit 2
fi
if (( redis_port < 1024 || redis_port > 65535 )); then
  echo "benchmark port must be between 1024 and 65535" >&2
  exit 2
fi

benchmark_dir="$(mktemp -d "${TMPDIR:-/tmp}/jellyrin-redis-eval.XXXXXX")"
redis_log="$benchmark_dir/redis.log"
benchmark_password="jellyrin-eval-${PPID}-${RANDOM}-${RANDOM}"
redis_pid=""
pg_schema=""
postgres_url="${JELLYRIN_REDIS_EVAL_POSTGRES_URL:-}"

cleanup() {
  set +e
  if [[ -n "$redis_pid" ]]; then
    REDISCLI_AUTH="$benchmark_password" "$redis_cli_bin" \
      -h 127.0.0.1 -p "$redis_port" --no-auth-warning shutdown nosave >/dev/null 2>&1
    wait "$redis_pid" >/dev/null 2>&1
  fi
  if [[ "$pg_schema" =~ ^jellyrin_redis_eval_[0-9]+_[0-9]+$ && -n "$postgres_url" ]]; then
    PGCONNECT_TIMEOUT=5 psql -X -v ON_ERROR_STOP=1 -q "$postgres_url" \
      -c "DROP SCHEMA $pg_schema CASCADE" >/dev/null
  fi
  rm -f -- "$benchmark_dir/redis.sock" "$redis_log"
  rmdir -- "$benchmark_dir" >/dev/null 2>&1
}
trap cleanup EXIT INT TERM

"$redis_server_bin" \
  --bind 127.0.0.1 \
  --protected-mode yes \
  --port "$redis_port" \
  --save "" \
  --appendonly no \
  --maxmemory "${maxmemory_mb}mb" \
  --maxmemory-policy allkeys-lru \
  --requirepass "$benchmark_password" \
  --dir "$benchmark_dir" \
  --logfile "$redis_log" &
redis_pid=$!

redis_ready=0
for _ in $(seq 1 100); do
  if REDISCLI_AUTH="$benchmark_password" "$redis_cli_bin" \
    -h 127.0.0.1 -p "$redis_port" --no-auth-warning ping >/dev/null 2>&1; then
    redis_ready=1
    break
  fi
  if ! kill -0 "$redis_pid" >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done
if (( redis_ready == 0 )); then
  echo "isolated Redis did not start; the port may already be occupied" >&2
  sed -n '1,80p' "$redis_log" >&2
  exit 1
fi

echo "benchmark_timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "host_arch=$(uname -m)"
echo "host_cpus=$(getconf _NPROCESSORS_ONLN)"
echo "redis_version=$("$redis_server_bin" --version)"
echo "redis_transport=tcp-loopback"
echo "redis_keyspace=$keyspace"
echo "redis_value_bytes=$value_bytes"
echo "redis_maxmemory_mb=$maxmemory_mb"
awk '/^VmRSS:/ { print "redis_idle_process_rss_kib=" $2 }' "/proc/$redis_pid/status"

echo "redis_prefill_csv:"
"$redis_benchmark_bin" \
  -h 127.0.0.1 -p "$redis_port" -a "$benchmark_password" --csv \
  -n "$prefill_requests" -c 32 -P 8 -r "$keyspace" -d "$value_bytes" -t set

echo "redis_read_csv:"
"$redis_benchmark_bin" \
  -h 127.0.0.1 -p "$redis_port" -a "$benchmark_password" --csv \
  -n "$read_requests" -c 6 -P 1 -r "$keyspace" -d "$value_bytes" -t get

awk '/^VmRSS:/ { print "redis_loaded_process_rss_kib=" $2 }' "/proc/$redis_pid/status"
REDISCLI_AUTH="$benchmark_password" "$redis_cli_bin" \
  -h 127.0.0.1 -p "$redis_port" --no-auth-warning info memory stats \
  | grep -E '^(used_memory|used_memory_rss|maxmemory|mem_fragmentation_ratio|evicted_keys|keyspace_hits|keyspace_misses):'

if [[ "${JELLYRIN_REDIS_EVAL_SATURATE:-0}" == "1" ]]; then
  saturation_keyspace=$((keyspace * 2))
  echo "redis_saturation_prefill_csv:"
  "$redis_benchmark_bin" \
    -h 127.0.0.1 -p "$redis_port" -a "$benchmark_password" --csv \
    -n "$saturation_requests" -c 32 -P 8 -r "$saturation_keyspace" \
    -d "$value_bytes" -t set
  echo "redis_saturation_read_csv:"
  "$redis_benchmark_bin" \
    -h 127.0.0.1 -p "$redis_port" -a "$benchmark_password" --csv \
    -n "$read_requests" -c 6 -P 1 -r "$saturation_keyspace" \
    -d "$value_bytes" -t get
  awk '/^VmRSS:/ { print "redis_saturated_process_rss_kib=" $2 }' "/proc/$redis_pid/status"
  REDISCLI_AUTH="$benchmark_password" "$redis_cli_bin" \
    -h 127.0.0.1 -p "$redis_port" --no-auth-warning info memory stats \
    | grep -E '^(used_memory|used_memory_rss|maxmemory|mem_fragmentation_ratio|evicted_keys|keyspace_hits|keyspace_misses):'
fi

if [[ -z "$postgres_url" ]]; then
  echo "postgres_comparison=skipped (set JELLYRIN_REDIS_EVAL_POSTGRES_URL)"
  exit 0
fi
for pg_bin in psql pgbench; do
  if ! command -v "$pg_bin" >/dev/null 2>&1; then
    echo "$pg_bin is required for the optional PostgreSQL comparison" >&2
    exit 2
  fi
done

pg_schema="jellyrin_redis_eval_${PPID}_${RANDOM}"
PGCONNECT_TIMEOUT=5 psql -X -v ON_ERROR_STOP=1 -q "$postgres_url" -c \
  "CREATE SCHEMA $pg_schema; CREATE TABLE $pg_schema.cache_eval (id integer PRIMARY KEY, payload text NOT NULL); INSERT INTO $pg_schema.cache_eval SELECT id, repeat('x', $value_bytes) FROM generate_series(1, $keyspace) AS id; ANALYZE $pg_schema.cache_eval;"

echo "postgres_relation_bytes=$(PGCONNECT_TIMEOUT=5 psql -X -Atq "$postgres_url" -c "SELECT pg_total_relation_size('$pg_schema.cache_eval')")"
printf '\\set id random(1, %s)\nSELECT payload FROM %s.cache_eval WHERE id = :id;\n' \
  "$keyspace" "$pg_schema" \
  | PGCONNECT_TIMEOUT=5 pgbench -n -M prepared -c 6 -j 4 -T 3 -f - "$postgres_url" >/dev/null
echo "postgres_read:"
printf '\\set id random(1, %s)\nSELECT payload FROM %s.cache_eval WHERE id = :id;\n' \
  "$keyspace" "$pg_schema" \
  | PGCONNECT_TIMEOUT=5 pgbench -n -M prepared -c 6 -j 4 -T 10 -r -f - "$postgres_url"
