#!/usr/bin/env node

const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const evidencePath = path.join(generatedDir, 'systemd-unit-smoke.json');
const evidenceMarkdownPath = path.join(generatedDir, 'systemd-unit-smoke.md');

async function main() {
  const serviceText = await fs.readFile(path.join(repoRoot, 'ops/jellyrin.service'), 'utf8');
  const envText = await fs.readFile(path.join(repoRoot, 'ops/jellyrin.env.example'), 'utf8');
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'jellyrin-systemd-smoke-'));
  const checks = [
    check('systemd-analyze-present', await commandExists('systemd-analyze')),
    check('service-execstart-release-binary', serviceText.includes('ExecStart=/usr/local/bin/jellyrin-server')),
    check('service-env-file', serviceText.includes('EnvironmentFile=/etc/jellyrin/jellyrin.env')),
    check('service-state-dirs', ['StateDirectory=jellyrin', 'CacheDirectory=jellyrin', 'LogsDirectory=jellyrin', 'ConfigurationDirectory=jellyrin'].every((needle) => serviceText.includes(needle))),
    check('service-hardening', ['NoNewPrivileges=true', 'PrivateTmp=true', 'ProtectHome=true', 'ProtectSystem=strict'].every((needle) => serviceText.includes(needle))),
    check('service-restart-policy', serviceText.includes('Restart=on-failure') && serviceText.includes('RestartSec=5')),
    check('service-network-online', serviceText.includes('After=network-online.target') && serviceText.includes('Wants=network-online.target')),
    check('env-release-paths', [
      'JELLYRIN_DATA_DIR=/var/lib/jellyrin',
      'JELLYRIN_CONFIG_DIR=/etc/jellyrin',
      'JELLYRIN_CACHE_DIR=/var/cache/jellyrin',
      'JELLYRIN_LOG_DIR=/var/log/jellyrin',
      'JELLYRIN_WEB_DIR=/srv/jellyrin/web',
    ].every((needle) => envText.includes(needle))),
    check('env-sqlite-release-db', envText.includes('DATABASE_URL=sqlite:///var/lib/jellyrin/jellyrin.db?mode=rwc')),
  ];

  let verify = { code: 1, stdout: '', stderr: 'systemd-analyze not available' };
  if (checks[0].status === 'passed') {
    await createSystemdRoot(root, serviceText, envText);
    verify = await runCommand('systemd-analyze', [
      'verify',
      `--root=${root}`,
      '/etc/systemd/system/jellyrin.service',
    ]);
  }
  checks.push(check('systemd-analyze-verify', verify.code === 0));

  const failed = checks.filter((item) => item.status !== 'passed');
  const result = {
    generatedAt: new Date().toISOString(),
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: {
      passed: checks.length - failed.length,
      failed: failed.length,
      total: checks.length,
    },
    checks,
    verify: {
      code: verify.code,
      stdoutTail: tail(verify.stdout),
      stderrTail: tail(verify.stderr),
    },
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

async function createSystemdRoot(root, serviceText, envText) {
  await fs.mkdir(path.join(root, 'etc/systemd/system'), { recursive: true });
  await fs.mkdir(path.join(root, 'etc/jellyrin'), { recursive: true });
  await fs.mkdir(path.join(root, 'usr/local/bin'), { recursive: true });
  await fs.writeFile(path.join(root, 'etc/systemd/system/jellyrin.service'), serviceText);
  await fs.writeFile(path.join(root, 'etc/jellyrin/jellyrin.env'), envText);
  await fs.writeFile(path.join(root, 'usr/local/bin/jellyrin-server'), '#!/bin/sh\nexit 0\n', {
    mode: 0o755,
  });
  for (const unit of ['sysinit.target', 'basic.target', 'multi-user.target', 'network-online.target']) {
    await fs.writeFile(
      path.join(root, `etc/systemd/system/${unit}`),
      `[Unit]\nDescription=${unit}\n`,
    );
  }
}

function commandExists(command) {
  return new Promise((resolve) => {
    const child = spawn(command, ['--version'], { stdio: ['ignore', 'ignore', 'ignore'] });
    child.on('error', () => resolve(false));
    child.on('exit', (code) => resolve(code === 0));
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

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function tail(value) {
  return value.split('\n').filter(Boolean).slice(-20);
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Systemd Unit Smoke');
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
    checks: [check('systemd-unit-smoke', false)],
    error: error.message,
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(result, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(result));
  console.error(error);
  process.exit(1);
});
