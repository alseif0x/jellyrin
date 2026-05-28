#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const dockerfile = await read('Dockerfile');
  const compose = await read('docker-compose.yml');
  const systemd = await read('ops/jellyrin.service');
  const env = await read('ops/jellyrin.env.example');
  const checklist = await read('ops/release-checklist.md');
  const readme = await read('README.md');
  const api = await read('crates/jellyrin-api/src/lib.rs');
  const server = await read('crates/jellyrin-server/src/main.rs');
  const db = await read('crates/jellyrin-db/src/lib.rs');

  const checks = [
    check('docker-release-build', dockerfile.includes('cargo build --release -p jellyrin-server') && dockerfile.includes('debian:bookworm-slim')),
    check('docker-runtime-ffmpeg', dockerfile.includes('ffmpeg') && dockerfile.includes('USER jellyrin')),
    check('docker-healthcheck', dockerfile.includes('/healthz') && dockerfile.includes('HEALTHCHECK')),
    check('compose-service', compose.includes('jellyrin:') && compose.includes('8096:8096')),
    check('compose-persistent-volumes', compose.includes('jellyrin-data') && compose.includes('jellyrin-config') && compose.includes('jellyrin-cache')),
    check('systemd-production-unit', systemd.includes('EnvironmentFile=/etc/jellyrin/jellyrin.env') && systemd.includes('ExecStart=/usr/local/bin/jellyrin-server')),
    check('systemd-hardening', systemd.includes('NoNewPrivileges=true') && systemd.includes('ProtectSystem=strict')),
    check('config-dirs-env', env.includes('JELLYRIN_DATA_DIR=/var/lib/jellyrin') && env.includes('DATABASE_URL=sqlite:///var/lib/jellyrin/jellyrin.db?mode=rwc')),
    check('server-health-routes', api.includes('route("/healthz", get(health))') && api.includes('route("/readyz", get(ready))')),
    check('startup-migrations', db.includes('MIGRATOR') && db.includes('.run(&pool)')),
    check('release-checklist-fresh-upgrade-rollback', checklist.includes('## Fresh Install') && checklist.includes('## Upgrade') && checklist.includes('## Rollback')),
    check('readme-release-entrypoint', readme.includes('## Release Packaging') && readme.includes('npm run qa:packaging-release')),
  ];

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
  };

  await fs.mkdir(generatedDir, { recursive: true });
  await fs.writeFile(
    path.join(generatedDir, 'packaging-release.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  await fs.writeFile(path.join(generatedDir, 'packaging-release.md'), renderMarkdown(result));
  console.log(`wrote ${path.join(generatedDir, 'packaging-release.md')}`);

  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

async function read(relativePath) {
  return fs.readFile(path.join(repoRoot, relativePath), 'utf8');
}

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Packaging Release Matrix');
  lines.push('');
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

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
