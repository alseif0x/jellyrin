# Jellyrin QA

## Hardened Web slideshow and comics reader

`qa/e2e/web-readers.spec.js` validates the hardened Jellyfin Web build against a fresh,
isolated Jellyrin installation. It completes the startup wizard, scans temporary photo and book
libraries, loads a real photo through the slideshow, and opens a generated three-page CBZ through
the archive worker. The comic checks cover Swiper navigation, RTL direction changes, and
single/double-page mode. It also rejects failed reader requests and uncaught page errors.

Use a blank disposable PostgreSQL database, apply the embedded schema, and run one worker:

```bash
DATABASE_URL='postgresql://user:secret@127.0.0.1/jellyrin_web_reader_e2e' \
  target/debug/jellyrin-migrate schema

DATABASE_URL='postgresql://user:secret@127.0.0.1/jellyrin_web_reader_e2e' \
JELLYRIN_E2E_SERVER_COMMAND=target/debug/jellyrin-server \
JELLYRIN_E2E_WEB_DIR="$PWD/web" \
npm run test:e2e:web-readers
```

The suite does not target deployed staging and must not be pointed at a database with an existing
wizard/admin state. Override `PLAYWRIGHT_CHROMIUM_EXECUTABLE` only when the normal Playwright
browser discovery is unavailable.

## MAGSTV operator configuration

`qa/magstv-configure-jellyrin.js` performs the first authenticated MAGSTV tuner import and checks
that Jellyrin persisted an encrypted credential reference, not plaintext, in both the response and
the canonical Live TV configuration. It then verifies that at least one channel from that tuner is
visible in the indexed catalogue. The helper never prints the token, credentials, provider
response bodies, or provider-secret reference.

Put provider values in a private `0600` JSON file (the default is
`var/secrets/magstv.json`) and the Jellyrin admin token in a separate private file:

```json
{
  "username": "<account username>",
  "password": "<account password>"
}
```

Run local validation without network access, then an authenticated read-only preflight, and only
then the real import:

```bash
JELLYRIN_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_API_TOKEN_FILE=/secure/path/jellyrin-admin-token \
JELLYRIN_MAGSTV_CONFIG=/secure/path/magstv.json \
npm run qa:magstv-configure -- --validate-only

JELLYRIN_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_API_TOKEN_FILE=/secure/path/jellyrin-admin-token \
JELLYRIN_MAGSTV_CONFIG=/secure/path/magstv.json \
npm run qa:magstv-configure -- --dry-run

JELLYRIN_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_API_TOKEN_FILE=/secure/path/jellyrin-admin-token \
JELLYRIN_MAGSTV_CONFIG=/secure/path/magstv.json \
npm run qa:magstv-configure
```

`JELLYRIN_MAGSTV_USERNAME` and `JELLYRIN_MAGSTV_PASSWORD` override the secure file for short-lived
CI. Bootstrap and egress are resolved internally by the plugin and are not account inputs.
`JELLYRIN_API_TOKEN` is also accepted. The real run contacts the provider and can take up to 120
seconds; `--dry-run` authenticates the admin token but performs no mutation.
The MAGSTV plugin must already be installed, active, and granted exactly `Network` and
`ProviderSecrets`.

### Full deployed MAGSTV settings and playback QA

`qa/e2e/deployed-magstv-plugin.spec.js` drives the real plugin configuration page rather than
posting its payload directly. It enters only the account username and password, presses
`Guardar e indexar`, verifies that Jellyrin persisted only an encrypted provider-secret
reference, waits for `Mags Movies`, `Mags Series`, and `Mags Live TV` to reach full-catalogue
minimums, opens all three views in Jellyfin Web, and reads media bytes from one live channel, one
movie, and one episode. Playwright tracing is disabled for this suite so credentials cannot be
captured in an artifact.

This is an opt-in, mutating real-provider test. Supply secrets only through the environment (or
an administrator token in a private mode-0600 file):

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_MAGSTV_QA=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_E2E_API_TOKEN_FILE=/secure/path/jellyrin-admin-token \
JELLYRIN_MAGSTV_USERNAME='<account username>' \
JELLYRIN_MAGSTV_PASSWORD='<account password>' \
npm run test:e2e:magstv-plugin
```

By default the gate requires at least 1,000 channels, 30,000 movies, 20,000 series, and 100,001
episodes, preventing the former 10k/100k VOD truncation limits from passing. Override the four
`JELLYRIN_E2E_MAGSTV_MIN_*` variables only when the provider's canonical `All` catalogue has
legitimately changed. `JELLYRIN_E2E_MAGSTV_SYNC_TIMEOUT_MS` defaults to four hours. Set
`JELLYRIN_E2E_MAGSTV_CLICK_REFRESH=1` only when intentionally testing the explicit
`Actualizar catálogo` action; the normal first-run path relies on `Guardar e indexar` so it does
not start two concurrent full imports.

If the Playwright process times out or is interrupted after `Guardar e indexar` has already
succeeded, do not rerun the configuration path while that import may still be active. Resume the
same gate without supplying provider credentials and without starting another import:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_MAGSTV_QA=1 \
JELLYRIN_E2E_MAGSTV_VERIFY_ONLY=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_E2E_API_TOKEN_FILE=/secure/path/jellyrin-admin-token \
npm run test:e2e:magstv-plugin
```

The resume path loads the settings page read-only, verifies its two-field contract and the
existing encrypted tuner reference, waits for the current staged catalogue to become complete,
then opens all three views and runs the same live/movie/episode playback probes. It rejects
`JELLYRIN_E2E_MAGSTV_CLICK_REFRESH=1` so a recovery run cannot accidentally enqueue another
provider import.

## PostgreSQL Release Smokes

Production packaging no longer starts Jellyrin with SQLite. The systemd runtime and release
install/rollback smokes require a disposable PostgreSQL database whose test role can create
schemas and the `pg_trgm` extension:

```bash
export JELLYRIN_TEST_POSTGRES_URL='postgresql://jellyrin_test:secret@127.0.0.1/jellyrin_test'
npm run qa:systemd-runtime-smoke
npm run qa:release-install-smoke
```

`JELLYRIN_QA_POSTGRES_URL` takes precedence when both variables are set. Each smoke creates a
random schema, applies the embedded migrations, and drops the schema on completion. The rollback
smoke also requires the PostgreSQL client tools `psql`, `pg_dump`, and `pg_restore`. Packaging QA
asserts that the normal `jellyrin-server` dependency tree excludes SQLite while the separate
`jellyrin-migrate` binary retains its read-only SQLite source support.

## PostgreSQL Catalog Benchmark

Run the isolated 10k/100k/500k catalog matrix against a disposable database:

```bash
JELLYRIN_TEST_POSTGRES_URL='postgresql://jellyrin_test:secret@127.0.0.1/jellyrin_test' \
JELLYRIN_BENCHMARK_ALLOW_WRITE=1 \
node qa/postgres-catalog-benchmark.js
```

The runner creates a uniquely named schema, records p50/p95/max for browse, collection,
playback-join and folder pages, captures `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`, compares the
measured `collection_type/lower(name)/id` index, and drops its schema in `finally`. It never puts
the database password in the `psql` argument list. Results are written to
`plans/generated/postgres-catalog-benchmark.json`. Override dataset sizes or repetitions with
`JELLYRIN_CATALOG_BENCHMARK_SIZES` and `JELLYRIN_CATALOG_BENCHMARK_REPETITIONS`.

## Supply-chain gate

`node qa/supply-chain.js` is a read-only gate for immutable image/action pins, the dated Debian
snapshot and FFmpeg version, vulnerability-exception governance, and the CI evidence contract. It
does not execute a vulnerability scanner. On a Docker-capable release host, build the locked image
and run both `ops/generate-sbom.sh jellyrin:release supply-chain-artifacts` and
`ops/scan-vulnerabilities.sh jellyrin:release vulnerability-artifacts`; see `ops/supply-chain.md`
for scanner semantics, exception rules, lock refresh, per-architecture evidence and digest
promotion commands.

## Optional Redis Evaluation

Redis is not an application dependency and is disabled by default. To reevaluate it with a
specific cache candidate, run the isolated microbenchmark with local `redis-server`, `redis-cli`
and `redis-benchmark` binaries. The optional PostgreSQL role must be allowed to create and drop a
temporary schema:

```bash
JELLYRIN_REDIS_EVAL_POSTGRES_URL='postgresql://user:secret@127.0.0.1/test_database' \
  ./qa/redis-cache-benchmark.sh
```

Set `JELLYRIN_REDIS_EVAL_SATURATE=1` to exercise the configured `maxmemory` and eviction policy.
This runner is intentionally not a CI gate: it measures raw local lookup cost and memory, while
the activation decision requires an end-to-end A/B test of the affected endpoint. See
`docs/redis-decision.md` for the current decision and thresholds.

## Acceptance Runner

Run the local acceptance gate against Jellyfin `8096` and Jellyrin `8097`:

```bash
npm run qa:acceptance
```

The runner executes the deployed playback gate, strict golden API parity, focused Rust
playback/HLS tests, focused Xtream/SQLite Live TV tests, deployed Jellyrin Live TV HLS checks,
syntax checks for the QA harness and dashboard regeneration. It writes:

- `output/acceptance/acceptance.json`
- `output/acceptance/acceptance.md`
- one log file per command case

Override defaults with:

- `JELLYFIN_BASE_URL`
- `JELLYRIN_BASE_URL`
- `JELLYRIN_E2E_USER`
- `JELLYRIN_E2E_PASSWORD`
- `JELLYRIN_E2E_LIVE_TV_ITEM_IDS`
- `JELLYRIN_E2E_LIVE_TV_START_INDEX`
- `JELLYRIN_E2E_LIVE_TV_LIMIT`
- `JELLYRIN_ACCEPTANCE_TARGET_DIR`
- `JELLYRIN_ACCEPTANCE_KEEP_GOING=1`
- `JELLYRIN_ACCEPTANCE_JELLYRIN_ONLY=1`: skip upstream Jellyfin auth-dependent gates and run only Jellyrin checks.

Use Jellyrin-only mode when the Jellyfin reference on `8096` is running but does not expose valid
test credentials:

```bash
JELLYRIN_ACCEPTANCE_JELLYRIN_ONLY=1 npm run qa:acceptance
```

## Playback Compatibility Runner

Run the full deployed playback compatibility gate against Jellyfin `8096` and Jellyrin `8097`:

```bash
npm run qa:playback-compat
```

The runner executes:

- HLS contract probe against Jellyfin
- HLS contract probe against Jellyrin
- Jellyfin Web playback/seek probe against Jellyfin
- Jellyfin Web playback/seek probe against Jellyrin

It writes:

- `output/playback-compat/playback-compat.json`
- `output/playback-compat/playback-compat.md`
- one log file per case

Override defaults with:

- `JELLYFIN_BASE_URL`
- `JELLYRIN_BASE_URL`
- `JELLYRIN_E2E_USER`
- `JELLYRIN_E2E_PASSWORD`
- `JELLYRIN_E2E_ITEM_ID`
- `JELLYRIN_E2E_AUDIO_STREAM_INDEX`
- `JELLYRIN_E2E_SUBTITLE_STREAM_INDEX`
- `JELLYRIN_E2E_START_POSITION_TICKS`

## Deployed HLS Playback Compatibility

Run this suite against an already-running Jellyfin or Jellyrin instance. It validates the HLS
playback contract used by Jellyfin Web:

- authenticates through `Users/AuthenticateByName`
- requests `PlaybackInfo` with a Jellyfin-compatible HLS `DeviceProfile`
- validates the HLS master and media playlists
- checks VOD shape, segment count and absence of unexpected discontinuities
- downloads initial buffer segments
- downloads a far seek segment
- repeats the same probe from a browser context with Playwright
- stops the test transcode sessions through `DELETE /Videos/ActiveEncodings`

Example against Jellyrin on `8097`:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=http://127.0.0.1:8097 \
JELLYRIN_E2E_USER=joe \
JELLYRIN_E2E_PASSWORD='<password>' \
JELLYRIN_E2E_ITEM_ID=1bdad953-d342-d2d5-5760-75d1f172a4e4 \
JELLYRIN_E2E_AUDIO_STREAM_INDEX=1 \
JELLYRIN_E2E_SUBTITLE_STREAM_INDEX=4 \
JELLYRIN_E2E_START_POSITION_TICKS=601757610 \
npx playwright test qa/e2e/deployed-playback-hls.spec.js --project=chromium
```

Example against Jellyfin on `8096`:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=http://127.0.0.1:8096 \
JELLYRIN_E2E_USER=joe \
JELLYRIN_E2E_PASSWORD='<password>' \
JELLYRIN_E2E_ITEM_ID=1bdad953-d342-d2d5-5760-75d1f172a4e4 \
JELLYRIN_E2E_AUDIO_STREAM_INDEX=1 \
JELLYRIN_E2E_SUBTITLE_STREAM_INDEX=4 \
JELLYRIN_E2E_START_POSITION_TICKS=601757610 \
npx playwright test qa/e2e/deployed-playback-hls.spec.js --project=chromium
```

Useful optional variables:

- `JELLYRIN_E2E_SEEK_SEGMENT_INDEX`: force a specific far segment index.
- `JELLYRIN_E2E_SUBTITLE_STREAM_INDEX=-1`: run a lighter no-subtitle variant.
- `JELLYRIN_E2E_ITEM_ID`: pin a known video instead of discovering the first one.

## Deployed Live TV HLS Compatibility

Run this suite against an already-running Jellyrin instance with Live TV channels configured. It
validates the backend contract used by Jellyfin Web for live streams:

- authenticates through `Users/AuthenticateByName`
- discovers Live TV channels or uses pinned channel IDs
- requests `PlaybackInfo`
- validates the HLS master and media playlists
- downloads a real `.ts` segment
- reports `/Sessions/Playing/Stopped`
- verifies `System/Diagnostics` reports zero active Live TV tuner leases after stopping

Example against Jellyrin on `8097`:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=http://127.0.0.1:8097 \
JELLYRIN_E2E_USER=joe \
JELLYRIN_E2E_PASSWORD='<password>' \
JELLYRIN_E2E_LIVE_TV_ITEM_IDS=xtream_31039,xtream_31037,xtream_31040 \
npx playwright test qa/e2e/deployed-live-tv-hls.spec.js --project=chromium
```

Useful optional variables:

- `JELLYRIN_E2E_LIVE_TV_ITEM_IDS`: comma-separated channel IDs to pin stable channels.
- `JELLYRIN_E2E_LIVE_TV_START_INDEX`: channel discovery offset when IDs are not pinned.
- `JELLYRIN_E2E_LIVE_TV_LIMIT`: number of discovered channels to test.

## Deployed Jellyfin Web Playback

Run this suite when the actual Jellyfin Web player needs to be covered, not just the HLS HTTP
contract. It logs in through the web UI, opens the item detail page, clicks Play, waits for HLS
segments, inspects the `<video>` element, seeks through the player and fails on HLS/frontend request
errors.

Example against Jellyrin on `8097`:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=http://127.0.0.1:8097 \
JELLYRIN_E2E_USER=joe \
JELLYRIN_E2E_PASSWORD='<password>' \
JELLYRIN_E2E_ITEM_ID=1bdad953-d342-d2d5-5760-75d1f172a4e4 \
npx playwright test qa/e2e/deployed-playback-web.spec.js --project=chromium
```

Example against Jellyfin on `8096`:

```bash
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=http://127.0.0.1:8096 \
JELLYRIN_E2E_USER=joe \
JELLYRIN_E2E_PASSWORD='<password>' \
JELLYRIN_E2E_ITEM_ID=1bdad953-d342-d2d5-5760-75d1f172a4e4 \
npx playwright test qa/e2e/deployed-playback-web.spec.js --project=chromium
```
