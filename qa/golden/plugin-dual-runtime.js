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
  const testResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'plugin_platform_state',
    '--',
    '--nocapture',
  ]);
  const passed = testResult.code === 0;
  const evidence = {
    gate: 'plugin-dual-runtime',
    status: passed ? 'implemented' : 'designed',
    percent: passed ? 12 : 5,
    closed: false,
    sourcePhase: passed ? 'E1.P1' : 'E1.P1-attempted',
    evidence: passed
      ? 'E1/P1 persistent plugin platform model is implemented and verified: plugin_repositories, package_catalog_cache, package_installations, installed_plugins, plugin_manifests, plugin_configurations, plugin_permissions, plugin_runtime_instances, plugin_host_events and plugin_audit_log exist; system_configuration_payloads.PluginRepositories migrates into the persistent repository/catalog model; repository and catalog state survive SQLite reopen.'
      : 'E1/P1 persistent plugin platform model test failed; inspect command output before advancing plugin runtime work.',
    updatedAt: new Date().toISOString(),
    completedTargets: passed ? ['persistent-plugin-model'] : [],
    failedTargets: passed ? [] : ['persistent-plugin-model'],
    validatedCommands: [
      'cargo test -p jellyrin-db plugin_platform_state -- --nocapture',
    ],
    openRisks: [
      'DotNetJellyfin sidecar host is not implemented yet.',
      'RustWasi host and SDK are not implemented yet.',
      'Package installation still rejects lifecycle operations until P2/P3 are implemented.',
      'No real plugin fixture has been loaded or executed yet.',
    ],
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence, testResult));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);
  if (!passed) {
    process.exitCode = testResult.code || 1;
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

function renderMarkdown(evidence, testResult) {
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
  if (testResult.stderr || testResult.stdout) {
    lines.push('');
    lines.push('## Command Result');
    lines.push('');
    lines.push(`- Exit code: ${testResult.code}`);
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
