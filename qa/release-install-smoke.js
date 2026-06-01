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
  const databasePath = path.join(layout.data, 'jellyrin.db');
  const databaseUrl = `sqlite://${databasePath}?mode=rwc`;
  const checks = [];
  const commandResults = [];

  const first = await runServerPhase(layout, databaseUrl, 'fresh-install');
  commandResults.push(first.commandResult);
  checks.push(check('server-healthz', first.health.Status === 'Healthy'));
  checks.push(check('server-readyz', first.ready.Status === 'Ready'));
  checks.push(check('public-info', publicInfoLooksCompatible(first.publicInfo)));
  checks.push(check('fresh-install-server-stopped', first.stopped));
  checks.push(check('data-dir-created', await exists(layout.data)));
  checks.push(check('config-dir-created', await exists(layout.config)));
  checks.push(check('cache-dir-created', await exists(layout.cache)));
  checks.push(check('log-dir-created', await exists(layout.logs)));
  checks.push(check('sqlite-created', await exists(databasePath)));

  const configMarker = path.join(layout.config, 'release-smoke-marker.json');
  await fs.writeFile(configMarker, `${JSON.stringify({ phase: 'baseline' }, null, 2)}\n`);
  const rollbackDir = path.join(root, 'rollback');
  const rollbackDatabasePath = path.join(rollbackDir, 'jellyrin.db');
  const rollbackConfigDir = path.join(rollbackDir, 'config');
  await fs.mkdir(rollbackDir, { recursive: true });
  await backupSqliteDatabase(databasePath, rollbackDatabasePath);
  await fs.cp(layout.config, rollbackConfigDir, { recursive: true });

  const second = await runServerPhase(layout, databaseUrl, 'upgrade-restart', async (baseUrl) => {
    const startupConfig = await fetchJson(`${baseUrl}/Startup/Configuration`);
    startupConfig.ServerName = 'Release Smoke Mutated';
    await postJson(`${baseUrl}/Startup/Configuration`, startupConfig);
    return fetchJson(`${baseUrl}/System/Info/Public`);
  });
  commandResults.push(second.commandResult);
  checks.push(check('upgrade-restart-healthz', second.health.Status === 'Healthy'));
  checks.push(check('upgrade-restart-readyz', second.ready.Status === 'Ready'));
  checks.push(check('upgrade-restart-preserves-server-id', second.publicInfo.Id === first.publicInfo.Id));
  checks.push(check('upgrade-restart-mutates-state', second.extra?.ServerName === 'Release Smoke Mutated'));
  checks.push(check('upgrade-restart-server-stopped', second.stopped));

  await restoreSqliteDatabase(rollbackDatabasePath, databasePath);
  await fs.rm(layout.config, { recursive: true, force: true });
  await fs.cp(rollbackConfigDir, layout.config, { recursive: true });
  const restoredMarker = JSON.parse(await fs.readFile(configMarker, 'utf8'));
  checks.push(check('rollback-config-restored', restoredMarker.phase === 'baseline'));

  const third = await runServerPhase(layout, databaseUrl, 'rollback-restore');
  commandResults.push(third.commandResult);
  checks.push(check('rollback-healthz', third.health.Status === 'Healthy'));
  checks.push(check('rollback-readyz', third.ready.Status === 'Ready'));
  checks.push(check('rollback-preserves-server-id', third.publicInfo.Id === first.publicInfo.Id));
  checks.push(check('rollback-restores-server-name', third.publicInfo.ServerName === first.publicInfo.ServerName));
  checks.push(check('rollback-server-stopped', third.stopped));

  const failed = checks.filter((item) => item.status !== 'passed');
  const result = {
    generatedAt: new Date().toISOString(),
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: {
      passed: checks.length - failed.length,
      failed: failed.length,
      total: checks.length,
    },
    layout: redactTempRoot(layout),
    checks,
    command: 'cargo run -p jellyrin-server --quiet -- --host 127.0.0.1 --port <port> --data-dir <tmp> --config-dir <tmp> --cache-dir <tmp> --log-dir <tmp> --web-dir <tmp> --database-url sqlite://<tmp>/jellyrin.db?mode=rwc',
    commandResults,
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

async function runServerPhase(layout, databaseUrl, phase, duringRun) {
  const port = await availablePort();
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
  let exitResult = null;
  child.stdout.on('data', (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
  });
  child.on('exit', (code, signal) => {
    exitResult = { code, signal };
  });

  let health;
  let ready;
  let publicInfo;
  let extra = null;
  let stopped = false;
  try {
    health = await waitForJson(`${baseUrl}/healthz`, startupTimeoutMs);
    ready = await fetchJson(`${baseUrl}/readyz`);
    publicInfo = await fetchJson(`${baseUrl}/System/Info/Public`);
    if (duringRun) {
      extra = await duringRun(baseUrl);
    }
  } finally {
    stopped = await stopChild(child);
  }

  return {
    phase,
    health,
    ready,
    publicInfo,
    extra,
    stopped: stopped && exitResult && (exitResult.code === 0 || exitResult.signal === 'SIGINT'),
    commandResult: {
      phase,
      baseUrl,
      exit: exitResult,
      stdoutTail: tail(stdout),
      stderrTail: tail(stderr),
    },
  };
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

async function postJson(url, payload) {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
    },
    body: JSON.stringify(payload),
  });
  if (!response.ok && response.status !== 204) {
    throw new Error(`${url} returned HTTP ${response.status}`);
  }
}

function publicInfoLooksCompatible(publicInfo) {
  return Boolean(publicInfo.Id) && publicInfo.ProductName === 'Jellyfin Server';
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

async function backupSqliteDatabase(sourcePath, backupPath) {
  for (const suffix of ['', '-wal', '-shm']) {
    const source = `${sourcePath}${suffix}`;
    if (await exists(source)) {
      await fs.copyFile(source, `${backupPath}${suffix}`);
    }
  }
}

async function restoreSqliteDatabase(backupPath, targetPath) {
  for (const suffix of ['', '-wal', '-shm']) {
    await fs.rm(`${targetPath}${suffix}`, { force: true });
  }
  for (const suffix of ['', '-wal', '-shm']) {
    const source = `${backupPath}${suffix}`;
    if (await exists(source)) {
      await fs.copyFile(source, `${targetPath}${suffix}`);
    }
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
