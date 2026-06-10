#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const projectRoot = path.resolve(__dirname, '..');
const outputDir = process.env.JELLYRIN_PLAYBACK_COMPAT_OUTPUT
  || path.join(projectRoot, 'output', 'playback-compat');
const retries = Number(process.env.JELLYRIN_PLAYBACK_COMPAT_RETRIES || 1);

const defaults = {
  upstreamUrl: process.env.JELLYFIN_BASE_URL || 'http://127.0.0.1:8096',
  jellyrinUrl: process.env.JELLYRIN_BASE_URL || 'http://127.0.0.1:8097',
  user: process.env.JELLYRIN_E2E_USER || process.env.JELLYRIN_E2E_ADMIN_USER || 'joe',
  password: requiredPassword(
    process.env.JELLYRIN_E2E_PASSWORD || process.env.JELLYRIN_E2E_ADMIN_PASSWORD,
    'JELLYRIN_E2E_PASSWORD or JELLYRIN_E2E_ADMIN_PASSWORD',
  ),
  itemId: process.env.JELLYRIN_E2E_ITEM_ID || '1bdad953-d342-d2d5-5760-75d1f172a4e4',
  audioStreamIndex: process.env.JELLYRIN_E2E_AUDIO_STREAM_INDEX || '1',
  subtitleStreamIndex: process.env.JELLYRIN_E2E_SUBTITLE_STREAM_INDEX || '4',
  startPositionTicks: process.env.JELLYRIN_E2E_START_POSITION_TICKS || '601757610',
};

async function main() {
  await fs.mkdir(outputDir, { recursive: true });
  const startedAt = new Date().toISOString();
  const cases = [
    playbackCase('jellyfin-hls', defaults.upstreamUrl, 'qa/e2e/deployed-playback-hls.spec.js'),
    playbackCase('jellyrin-hls', defaults.jellyrinUrl, 'qa/e2e/deployed-playback-hls.spec.js'),
    playbackCase('jellyfin-web', defaults.upstreamUrl, 'qa/e2e/deployed-playback-web.spec.js'),
    playbackCase('jellyrin-web', defaults.jellyrinUrl, 'qa/e2e/deployed-playback-web.spec.js'),
  ];

  const results = [];
  for (const testCase of cases) {
    process.stderr.write(`\n== ${testCase.name} (${testCase.baseUrl}) ==\n`);
    const result = await runCase(testCase);
    results.push(result);
    process.stderr.write(`${result.ok ? 'PASS' : 'FAIL'} ${testCase.name} in ${result.durationMs}ms\n`);
  }

  const report = {
    status: results.every(result => result.ok) ? 'passed' : 'failed',
    startedAt,
    finishedAt: new Date().toISOString(),
    config: redactConfig(defaults),
    results,
  };
  const jsonPath = path.join(outputDir, 'playback-compat.json');
  const mdPath = path.join(outputDir, 'playback-compat.md');
  await fs.writeFile(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  await fs.writeFile(mdPath, renderMarkdown(report));
  process.stderr.write(`\nWrote ${jsonPath}\nWrote ${mdPath}\n`);

  if (report.status !== 'passed') {
    process.exitCode = 1;
  }
}

function playbackCase(name, baseUrl, spec) {
  return {
    name,
    baseUrl,
    spec,
    env: {
      JELLYRIN_E2E_DEPLOYED: '1',
      JELLYRIN_E2E_NO_WEBSERVER: '1',
      JELLYRIN_E2E_BASE_URL: baseUrl,
      JELLYRIN_E2E_USER: defaults.user,
      JELLYRIN_E2E_PASSWORD: defaults.password,
      JELLYRIN_E2E_ITEM_ID: defaults.itemId,
      JELLYRIN_E2E_AUDIO_STREAM_INDEX: defaults.audioStreamIndex,
      JELLYRIN_E2E_SUBTITLE_STREAM_INDEX: defaults.subtitleStreamIndex,
      JELLYRIN_E2E_START_POSITION_TICKS: defaults.startPositionTicks,
    },
  };
}

async function runCase(testCase) {
  const started = Date.now();
  const args = ['playwright', 'test', testCase.spec, '--project=chromium'];
  const attempts = [];
  let output;
  for (let attempt = 0; attempt <= retries; attempt += 1) {
    if (attempt > 0) {
      process.stderr.write(`retry ${attempt}/${retries} ${testCase.name}\n`);
    }
    output = await spawnCapture('npx', args, {
      cwd: projectRoot,
      env: {
        ...process.env,
        ...testCase.env,
        FORCE_COLOR: '0',
        NO_COLOR: '1',
      },
    });
    attempts.push({
      code: output.code,
      signal: output.signal,
      ok: output.code === 0,
    });
    if (output.code === 0 || !playwrightRunIsRetryable(output)) {
      break;
    }
  }
  const durationMs = Date.now() - started;
  const artifactBase = path.join(outputDir, `${testCase.name}.log`);
  await fs.writeFile(artifactBase, output.stdout + output.stderr);
  return {
    name: testCase.name,
    baseUrl: testCase.baseUrl,
    spec: testCase.spec,
    ok: output.code === 0,
    code: output.code,
    signal: output.signal,
    durationMs,
    log: path.relative(projectRoot, artifactBase),
    attempts,
  };
}

function playwrightRunIsRetryable(output) {
  const text = `${output.stdout}\n${output.stderr}`;
  return text.includes('Target page, context or browser has been closed')
    || text.includes('TimeoutError');
}

function requiredPassword(value, label) {
  if (!value) {
    throw new Error(`${label} must be set`);
  }
  return value;
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

function redactConfig(config) {
  return {
    upstreamUrl: config.upstreamUrl,
    jellyrinUrl: config.jellyrinUrl,
    user: config.user,
    password: '<redacted>',
    itemId: config.itemId,
    audioStreamIndex: config.audioStreamIndex,
    subtitleStreamIndex: config.subtitleStreamIndex,
    startPositionTicks: config.startPositionTicks,
  };
}

function renderMarkdown(report) {
  const rows = report.results.map(result =>
    `| ${result.name} | ${result.baseUrl} | ${result.ok ? 'PASS' : 'FAIL'} | ${result.durationMs} | ${result.log} |`,
  );
  return `# Playback Compatibility

Status: **${report.status.toUpperCase()}**

Started: ${report.startedAt}
Finished: ${report.finishedAt}

## Config

- Upstream: ${report.config.upstreamUrl}
- Jellyrin: ${report.config.jellyrinUrl}
- User: ${report.config.user}
- Item: ${report.config.itemId}
- Audio stream: ${report.config.audioStreamIndex}
- Subtitle stream: ${report.config.subtitleStreamIndex}
- Start position ticks: ${report.config.startPositionTicks}

## Results

| Case | Base URL | Status | Duration ms | Log |
| --- | --- | --- | ---: | --- |
${rows.join('\n')}
`;
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
