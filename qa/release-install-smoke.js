#!/usr/bin/env node

const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const evidencePath = path.join(generatedDir, 'release-install-smoke.json');
const evidenceMarkdownPath = path.join(generatedDir, 'release-install-smoke.md');
const startupTimeoutMs = Number(process.env.JELLYRIN_RELEASE_SMOKE_TIMEOUT_MS || 45000);

async function main() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'jellyrin-release-smoke-'));
  const layout = {
    root,
    data: path.join(root, 'var', 'lib', 'jellyrin'),
    config: path.join(root, 'etc', 'jellyrin'),
    cache: path.join(root, 'var', 'cache', 'jellyrin'),
    logs: path.join(root, 'var', 'log', 'jellyrin'),
    web: path.join(root, 'srv', 'jellyrin', 'web'),
  };
  for (const dir of [layout.data, layout.config, layout.cache, layout.logs, layout.web]) {
    await fs.mkdir(dir, { recursive: true });
  }
  const port = await availablePort();
  const databasePath = path.join(layout.data, 'jellyrin.db');
  const databaseUrl = `sqlite://${databasePath}?mode=rwc`;
  const baseUrl = `http://127.0.0.1:${port}`;
  const child = spawn(
    'cargo',
    [
      'run',
      '-p',
      'jellyrin-server',
      '--quiet',
      '--',
      '--host',
      '127.0.0.1',
      '--port',
      String(port),
      '--data-dir',
      layout.data,
      '--config-dir',
      layout.config,
      '--cache-dir',
      layout.cache,
      '--log-dir',
      layout.logs,
      '--web-dir',
      layout.web,
      '--database-url',
      databaseUrl,
    ],
    {
      cwd: repoRoot,
      env: {
        ...process.env,
        RUST_LOG: process.env.RUST_LOG || 'jellyrin=warn,tower_http=warn',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
  });

  const checks = [];
  let exitResult = null;
  child.on('exit', (code, signal) => {
    exitResult = { code, signal };
  });

  try {
    const health = await waitForJson(`${baseUrl}/healthz`, startupTimeoutMs);
    checks.push(check('server-healthz', health.Status === 'Healthy'));
    const ready = await fetchJson(`${baseUrl}/readyz`);
    checks.push(check('server-readyz', ready.Status === 'Ready'));
    const publicInfo = await fetchJson(`${baseUrl}/System/Info/Public`);
    checks.push(
      check('public-info', Boolean(publicInfo.Id) && publicInfo.ProductName === 'Jellyfin Server'),
    );
    checks.push(check('data-dir-created', await exists(layout.data)));
    checks.push(check('config-dir-created', await exists(layout.config)));
    checks.push(check('cache-dir-created', await exists(layout.cache)));
    checks.push(check('log-dir-created', await exists(layout.logs)));
    checks.push(check('sqlite-created', await exists(databasePath)));
  } finally {
    var stopped = await stopChild(child);
  }

  checks.push(
    check('server-stopped', stopped && (exitResult.code === 0 || exitResult.signal === 'SIGINT')),
  );
  const failed = checks.filter((item) => item.status !== 'passed');
  const result = {
    generatedAt: new Date().toISOString(),
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: {
      passed: checks.length - failed.length,
      failed: failed.length,
      total: checks.length,
    },
    baseUrl,
    layout: redactTempRoot(layout),
    checks,
    command: 'cargo run -p jellyrin-server --quiet -- --host 127.0.0.1 --port <port> --data-dir <tmp> --config-dir <tmp> --cache-dir <tmp> --log-dir <tmp> --web-dir <tmp> --database-url sqlite://<tmp>/jellyrin.db?mode=rwc',
    stdoutTail: tail(stdout),
    stderrTail: tail(stderr),
  };

  await fs.mkdir(generatedDir, { recursive: true });
  await fs.writeFile(evidencePath, `${JSON.stringify(result, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(result));
  await fs.rm(root, { recursive: true, force: true });
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

function availablePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = address && typeof address === 'object' ? address.port : null;
      server.close(() => {
        if (port) {
          resolve(port);
        } else {
          reject(new Error('failed to allocate a local port'));
        }
      });
    });
  });
}

async function waitForJson(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      return await fetchJson(url);
    } catch (error) {
      lastError = error;
      await sleep(250);
    }
  }
  throw new Error(`timed out waiting for ${url}: ${lastError ? lastError.message : 'no response'}`);
}

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url} returned HTTP ${response.status}`);
  }
  return response.json();
}

async function stopChild(child) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return true;
  }
  child.kill('SIGINT');
  const stoppedAfterSigint = await waitForExit(child, 5000);
  if (stoppedAfterSigint) {
    return true;
  }
  child.kill('SIGTERM');
  return waitForExit(child, 5000);
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }
  return new Promise((resolve) => {
    const timeout = setTimeout(() => resolve(false), timeoutMs);
    child.once('exit', () => {
      clearTimeout(timeout);
      resolve(true);
    });
  });
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function tail(value) {
  return value.split('\n').filter(Boolean).slice(-20);
}

function redactTempRoot(layout) {
  return Object.fromEntries(
    Object.entries(layout).map(([key, value]) => [key, value.replace(layout.root, '<tmp>')]),
  );
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Release Install Smoke');
  lines.push('');
  lines.push(`- Generated: ${result.generatedAt}`);
  lines.push(`- Status: ${result.status}`);
  lines.push(`- Passed: ${result.summary.passed}/${result.summary.total}`);
  lines.push('');
  lines.push('| Check | Status |');
  lines.push('| --- | --- |');
  for (const item of result.checks) {
    lines.push(`| ${item.id} | ${item.status} |`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch(async (error) => {
  await fs.mkdir(generatedDir, { recursive: true });
  const result = {
    generatedAt: new Date().toISOString(),
    status: 'failed',
    error: error.message,
    checks: [check('release-install-smoke', false)],
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(result, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown({
    ...result,
    summary: { passed: 0, failed: 1, total: 1 },
  }));
  console.error(error);
  process.exit(1);
});
