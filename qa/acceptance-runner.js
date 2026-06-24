#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const projectRoot = path.resolve(__dirname, '..');
const outputDir = process.env.JELLYRIN_ACCEPTANCE_OUTPUT
  || path.join(projectRoot, 'output', 'acceptance');
const cargoTargetDir = process.env.CARGO_TARGET_DIR
  || process.env.JELLYRIN_ACCEPTANCE_TARGET_DIR
  || '/tmp/jellyrin-target-acceptance';

const config = {
  upstreamUrl: trimTrailingSlash(process.env.JELLYFIN_BASE_URL || 'http://127.0.0.1:8096'),
  jellyrinUrl: trimTrailingSlash(process.env.JELLYRIN_BASE_URL || 'http://127.0.0.1:8097'),
  user: process.env.JELLYRIN_E2E_USER || process.env.JELLYRIN_E2E_ADMIN_USER || 'joe',
  password: process.env.JELLYRIN_E2E_PASSWORD || process.env.JELLYRIN_E2E_ADMIN_PASSWORD,
  jellyrinOnly: truthy(process.env.JELLYRIN_ACCEPTANCE_JELLYRIN_ONLY),
  playbackItemId: process.env.JELLYRIN_E2E_ITEM_ID || '1bdad953-d342-d2d5-5760-75d1f172a4e4',
  playbackAudioStreamIndex: process.env.JELLYRIN_E2E_AUDIO_STREAM_INDEX || '1',
  playbackSubtitleStreamIndex: process.env.JELLYRIN_E2E_SUBTITLE_STREAM_INDEX || '4',
  playbackStartPositionTicks: process.env.JELLYRIN_E2E_START_POSITION_TICKS || '601757610',
};

async function main() {
  if (process.argv.includes('--help') || process.argv.includes('-h')) {
    printHelp();
    return;
  }
  config.password = requiredPassword(
    config.password,
    'JELLYRIN_E2E_PASSWORD or JELLYRIN_E2E_ADMIN_PASSWORD',
  );

  await fs.mkdir(outputDir, { recursive: true });
  const startedAt = new Date().toISOString();
  const cases = buildCases();
  const results = [];

  for (const testCase of cases) {
    process.stderr.write(`\n== ${testCase.name} ==\n`);
    const result = await runCase(testCase);
    results.push(result);
    process.stderr.write(`${result.ok ? 'PASS' : 'FAIL'} ${testCase.name} in ${result.durationMs}ms\n`);
    if (!result.ok && !isKeepGoing()) {
      break;
    }
  }

  const report = {
    status: results.every(result => result.ok) && results.length === cases.length ? 'passed' : 'failed',
    startedAt,
    finishedAt: new Date().toISOString(),
    config: redactConfig(config),
    cargoTargetDir,
    results,
  };

  const jsonPath = path.join(outputDir, 'acceptance.json');
  const mdPath = path.join(outputDir, 'acceptance.md');
  await fs.writeFile(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  await fs.writeFile(mdPath, renderMarkdown(report));
  process.stderr.write(`\nWrote ${jsonPath}\nWrote ${mdPath}\n`);

  if (report.status !== 'passed') {
    process.exitCode = 1;
  }
}

function buildCases() {
  const cases = [
    commandCase('node-check-playback-runner', 'node', ['--check', 'qa/playback-compat-runner.js']),
    commandCase('node-check-dashboard', 'node', ['--check', 'qa/golden/evidence-dashboard.js']),
    commandCase('node-check-playback-hls-spec', 'node', ['--check', 'qa/e2e/deployed-playback-hls.spec.js']),
    commandCase('node-check-playback-web-spec', 'node', ['--check', 'qa/e2e/deployed-playback-web.spec.js']),
    commandCase('node-check-live-tv-hls-spec', 'node', ['--check', 'qa/e2e/deployed-live-tv-hls.spec.js']),
    commandCase('cargo-fmt-check', 'cargo', ['fmt', '--check']),
    commandCase('cargo-check-api', 'cargo', ['check', '-p', 'jellyrin-api'], cargoEnv()),
    commandCase('cargo-test-core-hls', 'cargo', ['test', '-p', 'jellyrin-core', 'hls_ffmpeg_command', '--', '--nocapture'], cargoEnv()),
    commandCase(
      'cargo-test-api-playback-continuity',
      'cargo',
      ['test', '-p', 'jellyrin-api', 'playback_info_preserves_active_position_when_switching_streams', '--', '--nocapture'],
      cargoEnv(),
    ),
    commandCase(
      'cargo-test-api-hls-routes',
      'cargo',
      ['test', '-p', 'jellyrin-api', 'hls_routes_serve_active_transcode_files', '--', '--nocapture'],
      cargoEnv(),
    ),
    commandCase(
      'cargo-test-api-live-tv-xtream',
      'cargo',
      ['test', '-p', 'jellyrin-api', 'live_tv_persisted_xtream_channels_are_paged_from_sqlite', '--', '--nocapture'],
      cargoEnv(),
    ),
    probeCase('jellyrin-health', () => probeJson(`${config.jellyrinUrl}/health`)),
  ];

  if (config.jellyrinOnly) {
    cases.push(
      commandCase('jellyrin-hls', 'npx', ['playwright', 'test', 'qa/e2e/deployed-playback-hls.spec.js', '--project=chromium'], {
        JELLYRIN_E2E_DEPLOYED: '1',
        JELLYRIN_E2E_NO_WEBSERVER: '1',
        JELLYRIN_E2E_BASE_URL: config.jellyrinUrl,
        JELLYRIN_E2E_USER: config.user,
        JELLYRIN_E2E_PASSWORD: config.password,
        JELLYRIN_E2E_ITEM_ID: config.playbackItemId,
        JELLYRIN_E2E_AUDIO_STREAM_INDEX: config.playbackAudioStreamIndex,
        JELLYRIN_E2E_SUBTITLE_STREAM_INDEX: config.playbackSubtitleStreamIndex,
        JELLYRIN_E2E_START_POSITION_TICKS: config.playbackStartPositionTicks,
      }),
      commandCase('jellyrin-web', 'npx', ['playwright', 'test', 'qa/e2e/deployed-playback-web.spec.js', '--project=chromium'], {
        JELLYRIN_E2E_DEPLOYED: '1',
        JELLYRIN_E2E_NO_WEBSERVER: '1',
        JELLYRIN_E2E_BASE_URL: config.jellyrinUrl,
        JELLYRIN_E2E_USER: config.user,
        JELLYRIN_E2E_PASSWORD: config.password,
        JELLYRIN_E2E_ITEM_ID: config.playbackItemId,
        JELLYRIN_E2E_AUDIO_STREAM_INDEX: config.playbackAudioStreamIndex,
        JELLYRIN_E2E_SUBTITLE_STREAM_INDEX: config.playbackSubtitleStreamIndex,
        JELLYRIN_E2E_START_POSITION_TICKS: config.playbackStartPositionTicks,
      }),
    );
  } else {
    cases.push(
      probeCase('upstream-public-info', () => probeJson(`${config.upstreamUrl}/System/Info/Public`)),
      commandCase('golden-api-strict', 'npm', ['run', 'golden:api'], {
        JELLYRIN_GOLDEN_MODE: 'strict',
        JELLYFIN_UPSTREAM_URL: config.upstreamUrl,
        JELLYRIN_URL: config.jellyrinUrl,
        JELLYFIN_ADMIN_USER: process.env.JELLYFIN_ADMIN_USER || config.user,
        JELLYFIN_ADMIN_PASSWORD: process.env.JELLYFIN_ADMIN_PASSWORD || config.password,
        JELLYRIN_ADMIN_USER: process.env.JELLYRIN_ADMIN_USER || config.user,
        JELLYRIN_ADMIN_PASSWORD: process.env.JELLYRIN_ADMIN_PASSWORD || config.password,
      }),
      commandCase('playback-compat', 'npm', ['run', 'qa:playback-compat'], {
        JELLYFIN_BASE_URL: config.upstreamUrl,
        JELLYRIN_BASE_URL: config.jellyrinUrl,
        JELLYRIN_E2E_USER: config.user,
        JELLYRIN_E2E_PASSWORD: config.password,
      }),
    );
  }

  cases.push(
    commandCase('jellyrin-live-tv-hls', 'npx', ['playwright', 'test', 'qa/e2e/deployed-live-tv-hls.spec.js', '--project=chromium'], {
      JELLYRIN_E2E_DEPLOYED: '1',
      JELLYRIN_E2E_NO_WEBSERVER: '1',
      JELLYRIN_E2E_BASE_URL: config.jellyrinUrl,
      JELLYRIN_E2E_USER: config.user,
      JELLYRIN_E2E_PASSWORD: config.password,
      JELLYRIN_E2E_LIVE_TV_ITEM_IDS: process.env.JELLYRIN_E2E_LIVE_TV_ITEM_IDS || '',
      JELLYRIN_E2E_LIVE_TV_START_INDEX: process.env.JELLYRIN_E2E_LIVE_TV_START_INDEX || '4',
      JELLYRIN_E2E_LIVE_TV_LIMIT: process.env.JELLYRIN_E2E_LIVE_TV_LIMIT || '3',
    }),
    commandCase('evidence-dashboard', 'npm', ['run', 'evidence:dashboard']),
  );

  return cases;
}

function commandCase(name, command, args, env = {}) {
  return { type: 'command', name, command, args, env };
}

function probeCase(name, run) {
  return { type: 'probe', name, run };
}

function cargoEnv() {
  return { CARGO_TARGET_DIR: cargoTargetDir };
}

async function runCase(testCase) {
  const started = Date.now();
  if (testCase.type === 'probe') {
    try {
      const probe = await testCase.run();
      return {
        name: testCase.name,
        type: testCase.type,
        ok: true,
        durationMs: Date.now() - started,
        probe,
      };
    } catch (error) {
      return {
        name: testCase.name,
        type: testCase.type,
        ok: false,
        durationMs: Date.now() - started,
        error: String(error && error.stack ? error.stack : error),
      };
    }
  }

  const output = await spawnCapture(testCase.command, testCase.args, {
    cwd: projectRoot,
    env: {
      ...process.env,
      ...testCase.env,
      FORCE_COLOR: '0',
      NO_COLOR: '1',
    },
  });
  const logPath = path.join(outputDir, `${testCase.name}.log`);
  await fs.writeFile(logPath, output.stdout + output.stderr);
  return {
    name: testCase.name,
    type: testCase.type,
    command: [testCase.command, ...testCase.args].join(' '),
    ok: output.code === 0,
    code: output.code,
    signal: output.signal,
    durationMs: Date.now() - started,
    log: path.relative(projectRoot, logPath),
  };
}

async function probeJson(url) {
  const response = await fetch(url);
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${url} returned HTTP ${response.status}: ${text.slice(0, 500)}`);
  }
  return {
    url,
    status: response.status,
    body: parseJsonOrText(text),
  };
}

function parseJsonOrText(text) {
  try {
    return JSON.parse(text);
  } catch {
    return text.slice(0, 500);
  }
}

function spawnCapture(command, args, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      ...options,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => {
      const text = chunk.toString();
      stdout += text;
      process.stdout.write(text);
    });
    child.stderr.on('data', chunk => {
      const text = chunk.toString();
      stderr += text;
      process.stderr.write(text);
    });
    child.on('error', reject);
    child.on('exit', (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
}

function renderMarkdown(report) {
  const rows = report.results.map(result =>
    `| ${result.name} | ${result.type} | ${result.ok ? 'PASS' : 'FAIL'} | ${result.durationMs} | ${result.log || ''} |`,
  );
  return `# Jellyrin Acceptance

Status: **${report.status.toUpperCase()}**

Started: ${report.startedAt}
Finished: ${report.finishedAt}

## Config

- Upstream: ${report.config.upstreamUrl}
- Jellyrin: ${report.config.jellyrinUrl}
- User: ${report.config.user}
- Cargo target dir: ${report.cargoTargetDir}

## Results

| Case | Type | Status | Duration ms | Log |
| --- | --- | --- | ---: | --- |
${rows.join('\n')}
`;
}

function redactConfig(value) {
  return {
    upstreamUrl: value.upstreamUrl,
    jellyrinUrl: value.jellyrinUrl,
    user: value.user,
    password: '<redacted>',
    jellyrinOnly: value.jellyrinOnly,
    playbackItemId: value.playbackItemId,
    playbackAudioStreamIndex: value.playbackAudioStreamIndex,
    playbackSubtitleStreamIndex: value.playbackSubtitleStreamIndex,
    playbackStartPositionTicks: value.playbackStartPositionTicks,
  };
}

function trimTrailingSlash(value) {
  return value.replace(/\/+$/, '');
}

function requiredPassword(value, label) {
  if (!value) {
    throw new Error(`${label} must be set`);
  }
  return value;
}

function isKeepGoing() {
  return ['1', 'true', 'yes'].includes(String(process.env.JELLYRIN_ACCEPTANCE_KEEP_GOING || '').toLowerCase());
}

function truthy(value) {
  return ['1', 'true', 'yes'].includes(String(value || '').toLowerCase());
}

function printHelp() {
  console.log(`Jellyrin acceptance runner

Usage:
  npm run qa:acceptance

Environment:
  JELLYFIN_BASE_URL=http://127.0.0.1:8096
  JELLYRIN_BASE_URL=http://127.0.0.1:8097
  JELLYRIN_E2E_USER=joe
  JELLYRIN_E2E_PASSWORD=...
  JELLYRIN_ACCEPTANCE_TARGET_DIR=/tmp/jellyrin-target-acceptance
  JELLYRIN_ACCEPTANCE_KEEP_GOING=1
`);
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
