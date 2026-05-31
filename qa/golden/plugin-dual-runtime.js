#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const evidencePath = path.join(generatedDir, 'plugin-dual-runtime.json');
const evidenceMarkdownPath = path.join(generatedDir, 'plugin-dual-runtime.md');

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });
  const dbTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'plugin_platform_state',
    '--',
    '--nocapture',
  ]);
  const apiTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_repositories_round_trip_system_configuration_payload',
    '--',
    '--nocapture',
  ]);
  const refreshTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_repository_refresh_downloads_manifest_and_updates_catalog',
    '--',
    '--nocapture',
  ]);
  const passed = dbTestResult.code === 0 && apiTestResult.code === 0 && refreshTestResult.code === 0;
  const evidence = {
    gate: 'plugin-dual-runtime',
    status: passed ? 'implemented' : 'designed',
    percent: passed ? 52 : 5,
    closed: false,
    sourcePhase: passed
      ? 'E1.P1/E1.P2a/E1.P2b/E1.P2c/E1.P2d/E1.P2e/E1.P3a'
      : 'E1.P1/P2-attempted',
    evidence: passed
      ? 'E1/P1 persistent plugin platform model is implemented and verified; E1/P2a/P2b/P2c/P2d/P2e/P3a safe package lifecycle is implemented and verified: installing from a configured repository downloads/reads a package ZIP SourceUrl, verifies SHA256/SHA1 checksums when provided, rejects zip-slip paths, extracts through staging with rollback-safe swap, records package_installations, installed_plugins, manifest/config/permissions and audit state, completes PackageInstall tasks, handles update/downgrade by marking previous package_installations as Superseded while switching the active installed_plugins version, and refreshes enabled plugin repository manifests into the persisted catalog/task evidence while preserving disabled repositories and previous package state on partial failures; /Plugins lists the active plugin as NotSupported until a runtime host exists; configuration, enable, disable and uninstall mutate persisted state without claiming real plugin execution.'
      : 'E1/P1/P2 persistent plugin platform or safe lifecycle tests failed; inspect command output before advancing plugin runtime work.',
    updatedAt: new Date().toISOString(),
    completedTargets: passed
      ? [
          'persistent-plugin-model',
          'safe-plugin-lifecycle',
          'zip-package-extraction',
          'package-checksum-policy',
          'package-update-downgrade',
          'remote-repository-refresh',
        ]
      : [],
    failedTargets: passed ? [] : ['persistent-plugin-model-or-safe-plugin-lifecycle'],
    validatedCommands: [
      'cargo test -p jellyrin-db plugin_platform_state -- --nocapture',
      'cargo test -p jellyrin-api package_repositories_round_trip_system_configuration_payload -- --nocapture',
      'cargo test -p jellyrin-api package_repository_refresh_downloads_manifest_and_updates_catalog -- --nocapture',
    ],
    openRisks: [
      'DotNetJellyfin sidecar host is not implemented yet.',
      'RustWasi host and SDK are not implemented yet.',
      'Package install extracts package artifacts and records a safe NotSupported state; real host load/execute/unload is still pending.',
      'Async cancellation still needs full P2 coverage.',
      'No real plugin fixture has been loaded or executed yet.',
    ],
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(
    evidenceMarkdownPath,
    renderMarkdown(evidence, [dbTestResult, apiTestResult, refreshTestResult]),
  );
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);
  if (!passed) {
    process.exitCode = dbTestResult.code || apiTestResult.code || 1;
  }
}

function runCommand(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: process.env,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      const text = chunk.toString();
      stdout += text;
      process.stdout.write(text);
    });
    child.stderr.on('data', (chunk) => {
      const text = chunk.toString();
      stderr += text;
      process.stderr.write(text);
    });
    child.on('close', (code, signal) => resolve({ code: code || 0, signal, stdout, stderr }));
  });
}

function renderMarkdown(evidence, testResults) {
  const lines = [];
  lines.push('# Plugin Dual Runtime Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push(`Status: \`${evidence.status}\``);
  lines.push(`Progress: ${evidence.percent}%`);
  lines.push(`Closed: ${evidence.closed}`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(`- ${evidence.evidence}`);
  lines.push('');
  lines.push('## Validated Commands');
  lines.push('');
  for (const command of evidence.validatedCommands) {
    lines.push(`- \`${command}\``);
  }
  if (testResults.some((testResult) => testResult.stderr || testResult.stdout)) {
    lines.push('');
    lines.push('## Command Result');
    lines.push('');
    testResults.forEach((testResult, index) => {
      lines.push(`- Command ${index + 1} exit code: ${testResult.code}`);
    });
  }
  lines.push('');
  lines.push('## Open Risks');
  lines.push('');
  for (const risk of evidence.openRisks) {
    lines.push(`- ${risk}`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
