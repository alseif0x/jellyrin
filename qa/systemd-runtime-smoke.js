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
const evidencePath = path.join(generatedDir, 'systemd-runtime-smoke.json');
const evidenceMarkdownPath = path.join(generatedDir, 'systemd-runtime-smoke.md');
const startupTimeoutMs = Number(process.env.JELLYRIN_SYSTEMD_RUNTIME_SMOKE_TIMEOUT_MS || 45000);

async function main() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'jellyrin-systemd-runtime-'));
  const layout = {
    root,
    unit: path.join(root, 'etc', 'systemd', 'system', 'jellyrin.service'),
    envFile: path.join(root, 'etc', 'jellyrin', 'jellyrin.env'),
    execStart: path.join(root, 'usr', 'local', 'bin', 'jellyrin-server'),
    data: path.join(root, 'var', 'lib', 'jellyrin'),
    config: path.join(root, 'etc', 'jellyrin'),
    cache: path.join(root, 'var', 'cache', 'jellyrin'),
    logs: path.join(root, 'var', 'log', 'jellyrin'),
    web: path.join(root, 'srv', 'jellyrin', 'web'),
  };
  const checks = [];
  const serviceText = await fs.readFile(path.join(repoRoot, 'ops', 'jellyrin.service'), 'utf8');
  const envText = await fs.readFile(path.join(repoRoot, 'ops', 'jellyrin.env.example'), 'utf8');
  checks.push(check('service-execstart-installed-path', serviceText.includes('ExecStart=/usr/local/bin/jellyrin-server')));
  checks.push(check('service-env-file-installed-path', serviceText.includes('EnvironmentFile=/etc/jellyrin/jellyrin.env')));

  await prepareInstallRoot(layout, serviceText);
  const sourceBinary = await ensureServerBinary();
  await fs.copyFile(sourceBinary, layout.execStart);
  await fs.chmod(layout.execStart, 0o755);
  const port = await availablePort();
  const env = rewriteEnv(envText, layout, port);
  await fs.writeFile(layout.envFile, renderEnvFile(env));

  const result = await runInstalledServer(layout.execStart, env, port);
  checks.push(check('installed-binary-copied', await exists(layout.execStart)));
  checks.push(check('env-data-dir-rewritten', env.JELLYRIN_DATA_DIR === layout.data));
  checks.push(check('env-database-url-rewritten', env.DATABASE_URL === `sqlite://${path.join(layout.data, 'jellyrin.db')}?mode=rwc`));
  checks.push(check('installed-runtime-healthz', result.health?.Status === 'Healthy'));
  checks.push(check('installed-runtime-readyz', result.ready?.Status === 'Ready'));
  checks.push(check('installed-runtime-public-info', publicInfoLooksCompatible(result.publicInfo)));
  checks.push(check('installed-runtime-sqlite-created', await exists(path.join(layout.data, 'jellyrin.db'))));
  checks.push(check('installed-runtime-server-stopped', result.stopped));

  const failed = checks.filter((item) => item.status !== 'passed');
  const evidence = {
    generatedAt: new Date().toISOString(),
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: {
      passed: checks.length - failed.length,
      failed: failed.length,
      total: checks.length,
    },
    layout: redactTempRoot(layout),
    sourceBinary: path.relative(repoRoot, sourceBinary),
    installedCommand: '/usr/local/bin/jellyrin-server with EnvironmentFile=/etc/jellyrin/jellyrin.env',
    checks,
    commandResult: {
      exit: result.exit,
      stdoutTail: tail(result.stdout),
      stderrTail: tail(result.stderr),
    },
  };

  await fs.mkdir(generatedDir, { recursive: true });
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence));
  await fs.rm(root, { recursive: true, force: true });
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

async function prepareInstallRoot(layout, serviceText) {
  for (const dir of [
    path.dirname(layout.unit),
    layout.config,
    path.dirname(layout.execStart),
    layout.data,
    layout.cache,
    layout.logs,
    layout.web,
  ]) {
    await fs.mkdir(dir, { recursive: true });
  }
  await fs.writeFile(layout.unit, serviceText);
}

async function ensureServerBinary() {
  const configured = process.env.JELLYRIN_SERVER_BIN;
  if (configured && await exists(configured)) {
    return configured;
  }
  const binary = path.join(repoRoot, 'target', 'debug', 'jellyrin-server');
  if (await exists(binary)) {
    return binary;
  }
  const build = await runCommand('cargo', ['build', '-p', 'jellyrin-server']);
  if (build.code !== 0) {
    throw new Error(`cargo build -p jellyrin-server failed: ${tail(build.stderr).join('\n')}`);
  }
  if (!await exists(binary)) {
    throw new Error(`missing built server binary: ${binary}`);
  }
  return binary;
}

function rewriteEnv(envText, layout, port) {
  const env = parseEnv(envText);
  env.JELLYRIN_HOST = '127.0.0.1';
  env.JELLYRIN_PORT = String(port);
  env.JELLYRIN_DATA_DIR = layout.data;
  env.JELLYRIN_CONFIG_DIR = layout.config;
  env.JELLYRIN_CACHE_DIR = layout.cache;
  env.JELLYRIN_LOG_DIR = layout.logs;
  env.JELLYRIN_WEB_DIR = layout.web;
  env.DATABASE_URL = `sqlite://${path.join(layout.data, 'jellyrin.db')}?mode=rwc`;
  env.RUST_LOG = process.env.RUST_LOG || 'jellyrin=warn,tower_http=warn';
  return env;
}

function parseEnv(envText) {
  const env = {};
  for (const line of envText.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) {
      continue;
    }
    const index = trimmed.indexOf('=');
    if (index === -1) {
      continue;
    }
    env[trimmed.slice(0, index)] = trimmed.slice(index + 1);
  }
  return env;
}

function renderEnvFile(env) {
  return `${Object.entries(env).map(([key, value]) => `${key}=${value}`).join('\n')}\n`;
}

async function runInstalledServer(binary, env, port) {
  const child = spawn(binary, [], {
    cwd: path.dirname(binary),
    env: {
      ...process.env,
      ...env,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  let exit = null;
  child.stdout.on('data', (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
  });
  child.on('exit', (code, signal) => {
    exit = { code, signal };
  });

  let health = null;
  let ready = null;
  let publicInfo = null;
  let stopped = false;
  try {
    const baseUrl = `http://127.0.0.1:${port}`;
    health = await waitForJson(`${baseUrl}/healthz`, startupTimeoutMs);
    ready = await fetchJson(`${baseUrl}/readyz`);
    publicInfo = await fetchJson(`${baseUrl}/System/Info/Public`);
  } finally {
    stopped = await stopChild(child);
  }

  return {
    health,
    ready,
    publicInfo,
    stopped: stopped && exit && (exit.code === 0 || exit.signal === 'SIGINT'),
    exit,
    stdout,
    stderr,
  };
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

function runCommand(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    child.on('error', (error) => resolve({ code: 1, stdout, stderr: error.message }));
    child.on('close', (code, signal) => resolve({ code: code || 0, signal, stdout, stderr }));
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

function publicInfoLooksCompatible(publicInfo) {
  return Boolean(publicInfo?.Id) && publicInfo.ProductName === 'Jellyfin Server';
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
  lines.push('# Systemd Runtime Smoke');
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
    summary: { passed: 0, failed: 1, total: 1 },
    checks: [check('systemd-runtime-smoke', false)],
    error: error.message,
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(result, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(result));
  console.error(error);
  process.exit(1);
});
