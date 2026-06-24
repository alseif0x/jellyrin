# Jellyrin QA

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
