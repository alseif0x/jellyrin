# Live TV source invariant and rollout gate

Jellyrin stores a Live TV source in exactly one of two forms:

| Channel kind | `stream_url` | `ProviderReference` |
| --- | --- | --- |
| Legacy channel | Non-empty | Absent or empty |
| Opaque provider channel | Empty | Non-empty string |

Persisting both fields risks retaining a credential-bearing provider URL after
the channel has moved to just-in-time source resolution. Persisting neither
leaves the channel unplayable. Migration `202608080106` therefore enforces the
exclusive-or invariant without deleting or rewriting any catalogue row.

## Required preflight

Run the query for the active driver before deploying migration `202608080106`.
Both result columns must be zero. The queries return counts only and must not be
changed to print `stream_url` or provider metadata into deployment logs.

PostgreSQL:

```sql
SELECT
    count(*) FILTER (
        WHERE NULLIF(btrim(stream_url), '') IS NOT NULL
          AND metadata ? 'ProviderReference'
          AND NULLIF(btrim(metadata ->> 'ProviderReference'), '') IS NOT NULL
    ) AS mixed_rows,
    count(*) FILTER (
        WHERE NULLIF(btrim(stream_url), '') IS NULL
          AND NOT (
              metadata ? 'ProviderReference'
              AND NULLIF(btrim(metadata ->> 'ProviderReference'), '') IS NOT NULL
          )
    ) AS missing_source_rows
FROM live_tv_channels;
```

SQLite:

```sql
WITH source_state AS (
    SELECT
        NULLIF(trim(stream_url), '') IS NOT NULL AS has_stream_url,
        CASE
            WHEN json_valid(metadata_json) THEN
                COALESCE(
                    json_type(metadata_json, '$.ProviderReference') = 'text'
                    AND NULLIF(
                        trim(json_extract(metadata_json, '$.ProviderReference')),
                        ''
                    ) IS NOT NULL,
                    0
                )
            ELSE 0
        END AS has_provider_reference
    FROM live_tv_channels
)
SELECT
    sum(has_stream_url AND has_provider_reference) AS mixed_rows,
    sum(NOT has_stream_url AND NOT has_provider_reference) AS missing_source_rows
FROM source_state;
```

If either count is non-zero, do not validate the migration and do not delete or
redact rows in place. Back up the database, re-import the affected provider
catalogues with the current plugin/provider version, run the preflight again,
and only continue when both counts are zero. Also scan the backup under the
project's secret-retention procedure before deciding whether it can be kept.

The migrations repeat the preflight so a missed gate fails closed. Their errors
contain counts and remediation instructions, never source URLs or metadata.

## Post-reindex retention audit

After re-importing provider catalogues and before opening ingress, run the
counts-only audit with the PostgreSQL runtime/read-only credential:

```bash
DATABASE_URL='postgresql://...' jellyrin-migrate audit-source-hygiene \
  --report /root/jellyrin-source-hygiene.json
```

During a SQLite cutover, add `--source /path/to/read-only-snapshot.db`. The
command checks every media row including tombstones for `RemoteSourceUrl`,
`RemoteMediaProbe.SourceUrl` and malformed probe objects. It also rejects a
non-empty `stream_url` for Xtream or `plugin:*` tuners while allowing a genuine
legacy M3U tuner. It uses a bounded PostgreSQL `REPEATABLE READ`, `READ ONLY`
snapshot and emits counts only—never IDs, metadata or URLs. Exit status is `0`
when clean, `2` for findings and `3` if the scan cannot complete. Re-import on
findings; do not redact rows in place.

The database report is necessary but does not cover transient process arguments
or logs. With ingress closed at the recorded rollout boundary and a controlled
FFmpeg/ffprobe playback still active, run as root:

```bash
ops/audit-runtime-hygiene.sh \
  --since 2026-08-09T00:00:00Z \
  --relay-port 8096 \
  --report /root/jellyrin-runtime-hygiene.json
```

The wrapper snapshots the unit journal and every current cgroup `cmdline`, then
scans those plus regular Jellyrin and path-only Nginx logs. Symlinks, unreadable
or changing sources, an incomplete NUL-delimited argv, oversized input, a failed
journal/cgroup snapshot, or no process make the audit incomplete. HTTP(S) in a
FFmpeg/ffprobe argument is rejected except for the exact `http` loopback relay,
configured port, fixed internal path and 43-character opaque token. Userinfo,
sensitive query keys and Xtream credential paths are rejected everywhere. The
report contains only counts and timestamps; exit codes are again `0` clean, `2`
findings and `3` incomplete. A point-in-time cgroup scan does not prove the past,
so it must be paired with the complete journal/log window.

## Driver implementations

PostgreSQL already has `live_tv_channels_source_or_provider_reference`, which
rejects the “neither” state. Migration `202608080106` adds the complementary
`live_tv_channels_opaque_reference_excludes_stream_url` constraint as `NOT
VALID`, then validates it after the preflight. Together they enforce exact XOR.

SQLite cannot add a `CHECK` constraint to an existing table without rebuilding
the table. To avoid a risky table copy, migration `202608080106` preflights all
existing rows and installs equivalent `BEFORE INSERT` and `BEFORE UPDATE`
triggers. This is an implementation difference, not a difference in accepted
states; SQLite remains a supported database driver.

The historical migration numbering is intentionally asymmetric: both trees
contain `202608080103` and `202608080105`; SQLite's retention migration is
`202608080104`, while PostgreSQL's equivalent retention work predates it as
`202608070002`. Do not add a backdated PostgreSQL `202608080104` migration to an
already deployed migration sequence.

## Catalogue image URLs

Xtream catalogue artwork is durable only when it is an absolute `http` or
`https` URL with no userinfo, query string, or fragment. URLs that require a
token in any of those components are omitted. The policy applies to live
channel logos, VOD images, series images, and episode images so provider tokens
cannot migrate into database rows or backups through artwork metadata.
