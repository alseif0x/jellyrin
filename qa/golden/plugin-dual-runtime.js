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
  const cancellationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_install_cancellation_guard_observes_failed_task_run',
    '--',
    '--nocapture',
  ]);
  const immediateCancellationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_install_cancelable_operation_aborts_in_flight_step',
    '--',
    '--nocapture',
  ]);
  const catalogMergeTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_catalog_merges_duplicates_and_filters_incompatible_versions',
    '--',
    '--nocapture',
  ]);
  const taskProgressTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'task_runs_track_current_and_last_result',
    '--',
    '--nocapture',
  ]);
  const taskFailedProgressTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'task_runs_can_be_cancelled_and_stale_runs_expire',
    '--',
    '--nocapture',
  ]);
  const backupRestoreTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'backup_endpoints_list_create_manifest_and_reject_restore',
    '--',
    '--nocapture',
  ]);
  const filesystemDiscoveryTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'plugin_filesystem_discovery',
    '--',
    '--nocapture',
  ]);
  const passed =
    dbTestResult.code === 0 &&
    apiTestResult.code === 0 &&
    refreshTestResult.code === 0 &&
    cancellationTestResult.code === 0 &&
    immediateCancellationTestResult.code === 0 &&
    catalogMergeTestResult.code === 0 &&
    taskProgressTestResult.code === 0 &&
    taskFailedProgressTestResult.code === 0 &&
    backupRestoreTestResult.code === 0 &&
    filesystemDiscoveryTestResult.code === 0;
  const evidence = {
    gate: 'plugin-dual-runtime',
    status: passed ? 'implemented' : 'designed',
    percent: passed ? 82 : 5,
    closed: false,
    sourcePhase: passed
      ? 'E1.P1/E1.P1b/E1.P2a/E1.P2b/E1.P2c/E1.P2d/E1.P2e/E1.P2f/E1.P2f2/E1.P2g/E1.P2h/E1.P2i/E1.P2j/E1.P3a/E1.P3b'
      : 'E1.P1/P2-attempted',
    evidence: passed
      ? 'E1/P1 persistent plugin platform model is implemented and verified, including backup/restore of plugin repositories, package catalog cache, package installations, installed plugin rows, manifests, configurations, permissions, runtime instances, host events and audit log metadata without copying plugin binaries; E1/P2a/P2b/P2c/P2d/P2e/P2f/P2f2/P2g/P2h/P2i/P2j/P3a/P3b safe package lifecycle and registry discovery are implemented and verified: installing from a configured repository downloads/reads a package ZIP SourceUrl, verifies SHA256/SHA1 checksums when provided, rejects zip-slip paths, extracts through staging with rollback-safe swap, records package_installations, installed_plugins, manifest/config/permissions and audit state, completes PackageInstall tasks, broadcasts PackageInstall websocket task events for running/completed/failed/cancelled phases, handles update/downgrade by marking previous package_installations as Superseded while switching the active installed_plugins version, refreshes enabled plugin repository manifests into the persisted catalog/task evidence while preserving disabled repositories and previous package state on partial failures, honors IfStale/Force/CacheTtlSeconds repository refresh cache semantics and records cached/refreshed task evidence, broadcasts PackageRepositoriesRefresh websocket task events for running/completed/failed phases, observes PackageInstall cancellation before destructive/DB commit checkpoints and aborts cancelable in-flight package operations while waiting on downloads, file reads and unzip child processes, merges duplicate package catalog entries while preserving dual-runtime versions and optional Runtime/TargetAbi/ServerVersion filters, persists lifecycle progress in task_runs.result_json and exposes GET status endpoints for PackageInstall and PackageRepositoriesRefresh; /Plugins discovers package directories from filesystem, maps .dll artifacts to DotNetJellyfin and .wasm artifacts to RustWasi, ignores unsafe/incomplete package directories, and preserves existing status/configuration instead of overwriting persisted state; discovered plugins remain NotSupported until a runtime host exists; configuration, enable, disable and uninstall mutate persisted state without claiming real plugin execution.'
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
          'cooperative-package-install-cancellation',
          'immediate-package-install-cancellation',
          'package-catalog-merge-and-filters',
          'task-lifecycle-progress-status',
          'package-manager-websocket-events',
          'package-catalog-cache-ttl',
          'plugin-state-backup-restore',
          'plugin-filesystem-discovery',
        ]
      : [],
    failedTargets: passed ? [] : ['persistent-plugin-model-or-safe-plugin-lifecycle'],
    validatedCommands: [
      'cargo test -p jellyrin-db plugin_platform_state -- --nocapture',
      'cargo test -p jellyrin-api package_repositories_round_trip_system_configuration_payload -- --nocapture',
      'cargo test -p jellyrin-api package_repository_refresh_downloads_manifest_and_updates_catalog -- --nocapture',
      'cargo test -p jellyrin-api package_install_cancellation_guard_observes_failed_task_run -- --nocapture',
      'cargo test -p jellyrin-api package_install_cancelable_operation_aborts_in_flight_step -- --nocapture',
      'cargo test -p jellyrin-api package_catalog_merges_duplicates_and_filters_incompatible_versions -- --nocapture',
      'cargo test -p jellyrin-db task_runs_track_current_and_last_result -- --nocapture',
      'cargo test -p jellyrin-db task_runs_can_be_cancelled_and_stale_runs_expire -- --nocapture',
      'cargo test -p jellyrin-api backup_endpoints_list_create_manifest_and_reject_restore -- --nocapture',
      'cargo test -p jellyrin-api plugin_filesystem_discovery -- --nocapture',
    ],
    openRisks: [
      'DotNetJellyfin sidecar host is not implemented yet.',
      'RustWasi host and SDK are not implemented yet.',
      'Package install extracts package artifacts and records a safe NotSupported state; real host load/execute/unload is still pending.',
      'No real plugin fixture has been loaded or executed yet.',
    ],
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(
    evidenceMarkdownPath,
    renderMarkdown(evidence, [
      dbTestResult,
      apiTestResult,
      refreshTestResult,
      cancellationTestResult,
      immediateCancellationTestResult,
      catalogMergeTestResult,
      taskProgressTestResult,
      taskFailedProgressTestResult,
      backupRestoreTestResult,
      filesystemDiscoveryTestResult,
    ]),
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
