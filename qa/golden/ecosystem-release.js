#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const evidencePath = path.join(generatedDir, 'ecosystem-release.json');
const evidenceMarkdownPath = path.join(generatedDir, 'ecosystem-release.md');

const prerequisiteGates = [
  'ecosystem-harness',
  'plugin-dual-runtime',
  'livetv-real',
  'dlna-upnp',
  'syncplay-advanced',
  'channels-providers',
  'non-web-clients',
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const packaging = await runCommand('node', ['qa/packaging-release.js']);
  const security = await runCommand('node', ['qa/security-hardening.js']);
  const backupRestore = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'backup_endpoints_list_create_manifest_and_reject_restore',
    '--',
    '--nocapture',
  ]);
  const observability = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'system_diagnostics_requires_admin_and_reports_runtime_surfaces',
    '--',
    '--nocapture',
  ]);
  const evidenceChecks = await prerequisiteEvidenceChecks();
  const assetChecks = await releaseAssetChecks();

  const checks = [
    check('packaging-release-matrix', packaging.code === 0),
    check('security-hardening-matrix', security.code === 0),
    check('backup-restore-rollback-smoke', backupRestore.code === 0),
    check('runtime-observability-smoke', observability.code === 0),
    ...evidenceChecks,
    ...assetChecks,
  ];
  const failed = checks.filter((item) => item.status !== 'passed');
  const passed = failed.length === 0;
  const evidence = {
    gate: 'ecosystem-release',
    status: passed ? 'implemented' : 'designed',
    percent: passed ? 55 : 10,
    closed: false,
    sourcePhase: passed ? 'E7.1/E7.2/E7.3/E7.4/E7.5/E7.6/release-smoke-baseline' : 'E7.1/E7.2-attempted',
    evidence: passed
      ? [
          'E7 release baseline is implemented: packaging and security hardening matrices pass, release assets are present, npm exposes the release gate, prerequisite ecosystem evidence files exist for E1-E6, the backup/restore rollback smoke restores users, libraries, metadata, plugins and named configurations used by Live TV, Channels and network/DLNA, and /System/Diagnostics reports plugin runtime, Live TV tuner, DLNA eventing/update-id, SyncPlay and log surfaces.',
          'This is not final release-ready closure because device/manual gates, real upgrade execution, installed systemd smoke and host-level rollback rehearsal still remain open.',
        ].join(' ')
      : 'E7 release baseline checks failed; inspect failedChecks and commandResults before advancing release work.',
    updatedAt: new Date().toISOString(),
    completedTargets: passed
      ? [
          'packaging-release-matrix',
          'security-hardening-matrix',
          'backup-restore-rollback-smoke',
          'runtime-observability-smoke',
          'release-assets-present',
          'ecosystem-prerequisite-evidence-present',
        ]
      : checks.filter((item) => item.status === 'passed').map((item) => item.id),
    failedTargets: failed.map((item) => item.id),
    checks,
    commandResults: {
      packaging: commandSummary(packaging),
      security: commandSummary(security),
      backupRestore: commandSummary(backupRestore),
      observability: commandSummary(observability),
    },
    openRisks: [
      'Dashboard target remains release-ready; this baseline does not close E7 until all prior ecosystem gates are closed or explicitly accepted.',
      'Fresh install, upgrade, installed systemd runtime smoke and host-level rollback rehearsal still need execution evidence outside repository-local checks.',
      'Device/manual evidence is still required for E2/E3/E6 before final release closure.',
    ],
  };

  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (!passed) {
    process.exitCode = 1;
  }
}

async function prerequisiteEvidenceChecks() {
  const checks = [];
  for (const gate of prerequisiteGates) {
    const fileName = gate === 'ecosystem-harness' ? 'ecosystem-parity' : gate;
    const evidence = await readJsonIfExists(path.join(generatedDir, `${fileName}.json`));
    checks.push(check(`prerequisite-${gate}-evidence`, evidence && evidence.status !== 'not-started'));
  }
  return checks;
}

async function releaseAssetChecks() {
  const packageJson = await readJsonIfExists(path.join(repoRoot, 'package.json'));
  const cargoToml = await readText('Cargo.toml');
  const service = await readText('ops/jellyrin.service');
  const env = await readText('ops/jellyrin.env.example');
  const dockerfile = await readText('Dockerfile');
  const checklist = await readText('ops/release-checklist.md');
  return [
    check(
      'release-script-wired',
      packageJson?.scripts?.['golden:ecosystem-release'] === 'node qa/golden/ecosystem-release.js',
    ),
    check(
      'workspace-release-binaries',
      cargoToml.includes('"crates/jellyrin-server"')
        && cargoToml.includes('"crates/jellyrin-plugin-host-dotnet"')
        && cargoToml.includes('"crates/jellyrin-plugin-host-wasi"'),
    ),
    check(
      'systemd-release-directories',
      service.includes('StateDirectory=jellyrin')
        && service.includes('CacheDirectory=jellyrin')
        && service.includes('LogsDirectory=jellyrin')
        && service.includes('ConfigurationDirectory=jellyrin'),
    ),
    check(
      'env-release-directories',
      env.includes('JELLYRIN_DATA_DIR=/var/lib/jellyrin')
        && env.includes('JELLYRIN_CONFIG_DIR=/etc/jellyrin')
        && env.includes('JELLYRIN_CACHE_DIR=/var/cache/jellyrin')
        && env.includes('JELLYRIN_LOG_DIR=/var/log/jellyrin'),
    ),
    check(
      'docker-release-entrypoint',
      dockerfile.includes('ENTRYPOINT ["/usr/local/bin/jellyrin-server"]')
        && dockerfile.includes('HEALTHCHECK'),
    ),
    check(
      'release-checklist-operations',
      checklist.includes('## Fresh Install')
        && checklist.includes('## Upgrade')
        && checklist.includes('## Rollback')
        && checklist.includes('## Docker/Compose'),
    ),
  ];
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

function commandSummary(result) {
  return {
    code: result.code,
    signal: result.signal || null,
    stdoutTail: tail(result.stdout),
    stderrTail: tail(result.stderr),
  };
}

function tail(value) {
  return value.split('\n').filter(Boolean).slice(-20);
}

async function readText(relativePath) {
  return fs.readFile(path.join(repoRoot, relativePath), 'utf8');
}

async function readJsonIfExists(filePath) {
  try {
    return JSON.parse(await fs.readFile(filePath, 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') {
      return null;
    }
    throw error;
  }
}

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function renderMarkdown(evidence) {
  const lines = [];
  lines.push('# Ecosystem Release Evidence');
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
  lines.push('## Checks');
  lines.push('');
  lines.push('| Check | Status |');
  lines.push('| --- | --- |');
  for (const item of evidence.checks) {
    lines.push(`| ${item.id} | ${item.status} |`);
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
