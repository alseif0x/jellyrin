# PostgreSQL operations

The Compose deployment creates three deliberately separate login roles:

| Role | Purpose | Application access |
| --- | --- | --- |
| `postgres` | Break-glass cluster administration | Never |
| `jellyrin_migrator` | Owns the database/schema and applies DDL | One-shot migration job only |
| `jellyrin_runtime` | Normal API and worker DML | Jellyrin runtime only |

The runtime role gets `CONNECT`, temporary-table access, schema `USAGE`, and
`SELECT`/`INSERT`/`UPDATE`/`DELETE` plus identity-sequence access on application objects made by
the migrator. `_sqlx_migrations` is the deliberate exception: runtime receives `SELECT` only so
readiness can verify the schema without being able to rewrite migration history. It cannot create,
alter, or drop persistent schema objects. PostgreSQL and Redis have no host ports in the normal
Compose deployment.

## First start

From the repository root:

```bash
cp ops/compose.env.example .env
cp ops/jellyrin.env.example ops/jellyrin.env
chmod 600 .env ops/jellyrin.env
docker compose up -d --build
```

Generate URL-safe secrets (hex is suitable for the connection URLs) and fill every required
value in `.env`. The `jellyrin-migrate` service waits for authenticated PostgreSQL,
applies the embedded schema with `jellyrin_migrator`, exits successfully, and only then allows
the runtime container to start.

The initialization script runs only when the PostgreSQL volume is empty. Changing `.env` does
not rotate roles in an existing cluster. For an existing volume, back it up and rotate each role
explicitly as an administrator; recreate the volume only when its contents are known to be
disposable.

Redis is an optional cache scaffold and is not part of durable storage. The current single-node
decision is to leave it stopped; `docs/redis-decision.md` records the benchmark and activation
thresholds. Start it only after the application has a measured use for it:

```bash
docker compose --profile distributed-cache up -d
```

The profile fails closed unless `REDIS_PASSWORD` is set.

## External or systemd deployment

Use TLS verification for any database connection outside the private local network. Install
`ops/jellyrin-migrate.service` with its DDL URL in `/etc/jellyrin-migrate.env`; keep the runtime
URL in `/etc/jellyrin/jellyrin.env`. The migration unit uses a transient OS identity so the
long-running runtime user cannot inspect its environment. Never put the migrator or administrator
URL in the Jellyrin runtime environment.

## Encrypted backups and restore drills

`backup.sh` writes one transactionally consistent custom-format `pg_dump`. It validates the dump
catalog before encryption, encrypts it with `age`, records metadata, checksums every resulting
file, and publishes the snapshot directory with one atomic rename. Sunday and first-of-month
snapshots are hard-linked into independent weekly and monthly retention tiers. Defaults are 14
daily, 63 weekly and 400 monthly days; override them in `/etc/jellyrin/postgres-backup.env`.

Backups must live outside both the PostgreSQL data volume and Jellyrin's application volumes.
Replicate that directory off-host. The provided systemd unit writes only
`/var/backups/jellyrin-postgres`; if another path is selected, update both the environment and
`ReadWritePaths`. Install the script as `/usr/local/libexec/jellyrin/postgres-backup`, install the
unit/timer under `/etc/systemd/system`, create the locked `jellyrin-backup` OS user, and provision:

- `/etc/jellyrin-backup/age-recipients.txt`: one or more public `age` recipients;
- `/etc/jellyrin-backup/pg_service.conf`: a `[jellyrin-backup]` libpq service using TLS;
- `/etc/jellyrin-backup/pgpass`: its password, mode `0600`.

For a PostgreSQL instance on the same host, the example service definitions in
`backup-pg-service.conf.example` and `restore-pg-service.conf.example` use the
local Unix socket. Peer authentication then avoids a long-lived database
password: the backup unit runs as the matching `jellyrin-backup` OS identity,
and a restore drill is launched deliberately as the `postgres` OS identity.
Install `jellyrin-postgres-backup-local-peer.conf.example` as a systemd drop-in
in that topology; it removes the unused `pgpass` credential. For a remote
database, keep the generic unit, replace the socket with a TLS hostname,
require full certificate verification, and provision a dedicated password.

Do not reuse the runtime role. Create a non-superuser login dedicated to backup, grant it
`CONNECT` on the database and membership in PostgreSQL's `pg_read_all_data` predefined role.
The service files keep credentials out of command arguments, logs and repository env files.

After installing, verify scheduling and one real snapshot:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now jellyrin-postgres-backup.timer
sudo systemctl start jellyrin-postgres-backup.service
sudo systemctl status jellyrin-postgres-backup.service
```

At least monthly, run `restore-drill.sh /absolute/snapshot/directory` on a recovery host. It first
verifies checksums and decryptability, creates a uniquely named database through a separate
`jellyrin-restore-admin` libpq service, restores with `--exit-on-error`, checks migration history,
validated constraints and application-table presence, then force-drops the isolated database on
every exit path. Because the supported dump contains `pg_stat_statements`, this offline recovery
service also needs authority to create that extension (normally a tightly held cluster
administrator); it must not be available to Jellyrin. Set
`JELLYRIN_POSTGRES_RESTORE_AGE_IDENTITY`, `PGSERVICEFILE` and `PGPASSFILE` through root-owned
credential files, never inline in shell history. Keep the `age` identity in an independently
recoverable secret store. A successful `pg_dump` without a recent successful restore drill does
not count as a verified backup.

For an RPO shorter than the backup interval, use managed physical backups and WAL archiving/PITR;
these scripts do not claim point-in-time recovery.

## Query telemetry

The Compose PostgreSQL command preloads `pg_stat_statements`, and fresh clusters create the
extension during initialization. Existing Compose volumes need an administrator to run
`CREATE EXTENSION IF NOT EXISTS pg_stat_statements` after the PostgreSQL restart. External
deployments must add `pg_stat_statements` to `shared_preload_libraries` and restart PostgreSQL
before creating the extension.

Take snapshots rather than resetting statistics during normal incident response. The following
query deliberately omits SQL parameters because `pg_stat_statements` normalizes literals:

```sql
SELECT queryid, calls, round(total_exec_time::numeric, 1) AS total_ms,
       round(mean_exec_time::numeric, 1) AS mean_ms, rows,
       left(query, 240) AS normalized_query
FROM pg_stat_statements
WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
ORDER BY total_exec_time DESC
LIMIT 25;
```

Correlate this with pool wait/timeout metrics and host CPU before changing an index or PostgreSQL
memory setting. Restrict telemetry access to operations roles; normalized query text can still
contain identifiers or application constants. Only call `pg_stat_statements_reset()` at the start
of an explicitly recorded measurement window.
