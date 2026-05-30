#!/usr/bin/env node

'use strict';

const http = require('node:http');
const { spawnSync } = require('node:child_process');
const os = require('node:os');
const path = require('node:path');
const fs = require('node:fs');

// MPEG-2 TS clip with monotonically-increasing PCR/PTS/DTS (no backward jumps).
// Generated at simulator startup using ffmpeg (option b: pre-generate a clip long enough
// that the test never reaches EOF; 600 s is well beyond the ~60 s window of the golden
// test run, even accounting for all sequential operations on both targets).
//
// Why option (b) over option (a) (in-place TS resampling):
//   Option (a) requires parsing 188-byte TS packets, locating PCR fields in adaptation
//   headers and PTS/DTS in PES headers, and adjusting every field by the per-loop offset.
//   This is correct but fragile if the embedded clip has unusual flags or PCR gaps.
//   Option (b) delegates correct muxing to ffmpeg and is trivially verifiable with ffprobe.
//
// Root cause of the previous DTS regression (fixed here):
//   The old TS_LOOP_BODY (a ~10 kB base64-encoded ~1 s clip) was streamed in a byte loop
//   without adjusting timestamps. Each full loop reset DTS from the clip's last value back
//   to its first value (e.g. 213087 -> 126000), causing ~245 backward DTS jumps per 5 MB.
//   ffmpeg with -copyts reads from a temp file (SharedHttpStream) that accumulated these
//   non-monotonic timestamps and therefore decoded 0 frames (packet corrupt, drop=62209).
//   Jellyrin was unaffected only because it reads directly over HTTP and lets ffmpeg
//   regenerate timestamps from scratch (-copyts absent).
//
// Rate throttling (native playback speed):
//   The stream is served at approximately native bitrate (~100 kbps = 12.5 kB/s) so that
//   upstream Jellyfin's SharedHttpStream temp file grows slowly — matching real live TV.
//   Without throttling, the sim sends the entire clip in ~7 s; the SharedHttpStream
//   pre-buffers everything, and by the time a new ffmpeg consumer attaches it reads from
//   a write-position near the end of the clip, producing only a ~1.6 s segment before EOF.
//   With throttling, ffmpeg always finds fresh data arriving at the head of the stream.
//
// Clip properties: 176x144 black frame, mpeg2video, 29.97 fps, 100 kbps, ~6.3 MB / 600 s.
// PAT+PMT start at byte 0 so ffprobe identifies the codec immediately on the first few TS
// packets — no long analyzeduration window required for codec detection.

const FFMPEG_BIN = process.env.FFMPEG_BIN || '/usr/bin/ffmpeg';
const CLIP_DURATION_S = 600;

// Approximate target throughput in bytes/second.
// Derived from: 100 kbps video = 12500 B/s, plus ~5% TS overhead = ~13125 B/s.
// We use 13000 B/s (slightly under) to avoid readers ever stalling on an empty buffer.
const TARGET_BYTES_PER_SEC = 13000;

// Chunk size for each write call. Larger chunks reduce syscall overhead but increase
// latency jitter. 1880 bytes (10 TS packets) is a reasonable balance.
const CHUNK_SIZE = 1880;

// Delay in ms between chunks to approximate TARGET_BYTES_PER_SEC.
const CHUNK_DELAY_MS = Math.round((CHUNK_SIZE / TARGET_BYTES_PER_SEC) * 1000);

// Generate the clip once at module load time and keep it in memory.
// spawnSync is used to block until ffmpeg finishes — acceptable at startup (~1.2 s).
// If ffmpeg is unavailable the simulator throws immediately (fail-fast).
function generateMonotonicClip() {
  // Write to a temp file then read into memory.
  const tmpFile = path.join(os.tmpdir(), `hdhomerun-sim-${process.pid}.ts`);
  try {
    const result = spawnSync(
      FFMPEG_BIN,
      [
        '-y',
        '-f', 'lavfi',
        '-i', `color=black:size=176x144:rate=29.97:duration=${CLIP_DURATION_S}`,
        '-c:v', 'mpeg2video',
        '-b:v', '100k',
        '-r', '29.97',
        '-f', 'mpegts',
        tmpFile,
      ],
      { timeout: 60000, stdio: 'pipe' },
    );
    if (result.status !== 0) {
      const stderr = result.stderr ? result.stderr.toString().slice(-500) : '(no stderr)';
      throw new Error(`ffmpeg exited ${result.status}: ${stderr}`);
    }
    const buf = fs.readFileSync(tmpFile);
    return buf;
  } finally {
    try { fs.unlinkSync(tmpFile); } catch (_) { /* ignore */ }
  }
}

const TS_CLIP = generateMonotonicClip();

const LINEUP_CHANNELS = [
  {
    GuideNumber: '4.1',
    GuideName: 'NBC HD',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 1,
    Favorite: 0,
    DRM: 0,
  },
  {
    GuideNumber: '5.1',
    GuideName: 'CBS HD',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 1,
    Favorite: 1,
    DRM: 0,
  },
  {
    GuideNumber: '6.1',
    GuideName: 'Pay-Per-View',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 0,
    Favorite: 0,
    DRM: 1,
  },
];

function buildLineupWithUrls(baseUrl) {
  return LINEUP_CHANNELS.map((channel) => ({
    ...channel,
    URL: `${baseUrl}/auto/v${channel.GuideNumber}`,
  }));
}

function buildDiscoverResponse(baseUrl) {
  return {
    FriendlyName: 'Jellyrin HDHomeRun Simulator',
    ModelNumber: 'HDHR5-4K',
    FirmwareName: 'hdhomerun4_atsc',
    FirmwareVersion: '20220818',
    DeviceID: 'JELLYRIN1',
    DeviceAuth: '',
    BaseURL: baseUrl,
    LineupURL: `${baseUrl}/lineup.json`,
    TunerCount: 4,
  };
}

function start(port) {
  return new Promise((resolve, reject) => {
    const requestedPort = port || 0;
    const tempServer = http.createServer();
    tempServer.listen(requestedPort, '127.0.0.1', () => {
      const assignedPort = tempServer.address().port;
      tempServer.close(() => {
        const baseUrl = `http://127.0.0.1:${assignedPort}`;
        const discover = buildDiscoverResponse(baseUrl);

        // Concurrent connection counters keyed by channel path (e.g. '/auto/v4.1').
        // currentConcurrentByChannel: number of live connections right now.
        // maxConcurrentByChannel: peak concurrent seen since last reset.
        const currentConcurrentByChannel = Object.create(null);
        const maxConcurrentByChannel = Object.create(null);

        function channelConnOpen(channelPath) {
          currentConcurrentByChannel[channelPath] = (currentConcurrentByChannel[channelPath] || 0) + 1;
          if ((maxConcurrentByChannel[channelPath] || 0) < currentConcurrentByChannel[channelPath]) {
            maxConcurrentByChannel[channelPath] = currentConcurrentByChannel[channelPath];
          }
        }

        function channelConnClose(channelPath) {
          currentConcurrentByChannel[channelPath] = Math.max(0, (currentConcurrentByChannel[channelPath] || 1) - 1);
        }

        const server = http.createServer((req, res) => {
          const url = req.url.split('?')[0];

          if (url === '/discover.json') {
            const body = JSON.stringify(discover);
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/lineup.json') {
            const body = JSON.stringify(buildLineupWithUrls(baseUrl));
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/stats') {
            // Return current and max concurrent connections by channel path.
            const body = JSON.stringify({
              maxConcurrentByChannel: Object.assign(Object.create(null), maxConcurrentByChannel),
              currentConcurrentByChannel: Object.assign(Object.create(null), currentConcurrentByChannel),
            });
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/stats/reset' && req.method === 'POST') {
            // Clear both peak and current counters to establish a clean baseline for the next
            // measurement interval. currentConcurrentByChannel is also reset because upstream
            // Jellyfin's SharedHttpStream may keep the simulator connection open for stream
            // refill (R8) even after clients close — resetting current prevents this from
            // contaminating the next target's sharing metric.
            Object.keys(maxConcurrentByChannel).forEach((k) => { delete maxConcurrentByChannel[k]; });
            Object.keys(currentConcurrentByChannel).forEach((k) => { delete currentConcurrentByChannel[k]; });
            const body = JSON.stringify({ ok: true });
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url.startsWith('/auto/v')) {
            // Track this incoming connection for sharing metrics.
            channelConnOpen(url);

            // Stream the pre-generated monotonic TS clip at approximately native playback
            // rate (TARGET_BYTES_PER_SEC ≈ 13 kB/s ≈ 100 kbps + TS overhead).
            //
            // Rate throttling is critical for upstream Jellyfin's SharedHttpStream: without
            // it the sim sends the entire 600 s clip in ~48 s, the SharedHttpStream temp
            // file fills up quickly, and new ffmpeg consumers attach at the end of the
            // pre-buffered window rather than the head — producing only a ~1.6 s segment.
            // With throttling the file grows at live-TV speed, ensuring each consumer always
            // finds fresh data arriving at the write head.
            //
            // After the clip ends the connection is held open without sending more data.
            // This should not happen during normal test execution because the clip is 600 s
            // and the full golden test run (both targets) takes well under 60 s per target.
            res.writeHead(200, { 'Content-Type': 'video/mp2t' });
            let streaming = true;
            let offset = 0;
            function sendChunk() {
              if (!streaming) return;
              if (offset >= TS_CLIP.length) {
                // Clip exhausted — hold connection open. Should not occur during testing.
                setTimeout(sendChunk, 1000);
                return;
              }
              const chunk = TS_CLIP.subarray(offset, offset + CHUNK_SIZE);
              offset += chunk.length;
              const flushed = res.write(chunk);
              if (flushed) {
                setTimeout(sendChunk, CHUNK_DELAY_MS);
              } else {
                res.once('drain', sendChunk);
              }
            }
            req.on('close', () => { streaming = false; channelConnClose(url); });
            req.on('error', () => { streaming = false; channelConnClose(url); });
            sendChunk();
            return;
          }

          res.writeHead(404, { 'Content-Type': 'text/plain' });
          res.end('not found');
        });

        server.listen(assignedPort, '127.0.0.1', () => {
          resolve({
            url: baseUrl,
            close() {
              return new Promise((res) => {
                // closeAllConnections() drops all active keep-alive and streaming connections
                // immediately so server.close() resolves instead of waiting indefinitely for
                // ongoing TS stream clients (e.g. ffmpeg ingest) to disconnect on their own.
                if (typeof server.closeAllConnections === 'function') {
                  server.closeAllConnections();
                }
                server.close(res);
              });
            },
          });
        });
        server.on('error', reject);
      });
    });
    tempServer.on('error', reject);
  });
}

if (require.main === module) {
  const port = Number.parseInt(process.argv[2] || process.env.HDHOMERUN_SIM_PORT || '0', 10) || 0;
  start(port).then(({ url, close }) => {
    console.log(`HDHomeRun simulator listening at ${url}`);
    console.log(`  GET ${url}/discover.json`);
    console.log(`  GET ${url}/lineup.json`);
    console.log(`  GET ${url}/auto/v4.1`);
    process.on('SIGTERM', () => close().then(() => process.exit(0)));
    process.on('SIGINT', () => close().then(() => process.exit(0)));
  }).catch((error) => {
    console.error('Failed to start HDHomeRun simulator:', error);
    process.exit(1);
  });
}

module.exports = { start };
