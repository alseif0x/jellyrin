#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'dlna-discovery');
const tracePath = path.join(traceDir, 'golden-run.log');
const traceJsonPath = path.join(traceDir, 'golden-run.json');
const evidencePath = path.join(generatedDir, 'dlna-discovery.json');
const evidenceMarkdownPath = path.join(generatedDir, 'dlna-discovery.md');

const goldenCommands = [
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'ssdp',
    '--',
    '--nocapture',
  ],
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'ssdp_udp_handler_responds_to_msearch',
    '--',
    '--ignored',
    '--nocapture',
  ],
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });
  await fs.mkdir(traceDir, { recursive: true });

  const result = await runGolden();
  await fs.writeFile(tracePath, result.output);
  await fs.writeFile(traceJsonPath, `${JSON.stringify(result, null, 2)}\n`);

  const evidence = buildEvidence(result);
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);
  console.log(`wrote ${tracePath}`);

  if (result.code !== 0) {
    process.exitCode = result.code || 1;
  }
}

async function runGolden() {
  const results = [];
  for (const command of goldenCommands) {
    results.push(await runCommand(command));
  }
  const failed = results.find((result) => result.code !== 0);
  return {
    commands: goldenCommands.map((command) => command.join(' ')),
    code: failed?.code || 0,
    signal: failed?.signal || null,
    output: results.map((result) => result.output).join('\n'),
    generatedAt: new Date().toISOString(),
    results,
  };
}

function runCommand(command) {
  return new Promise((resolve) => {
    const child = spawn(command[0], command.slice(1), {
      cwd: repoRoot,
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const chunks = [];
    child.stdout.on('data', (chunk) => {
      process.stdout.write(chunk);
      chunks.push(chunk);
    });
    child.stderr.on('data', (chunk) => {
      process.stderr.write(chunk);
      chunks.push(chunk);
    });
    child.on('close', (code, signal) => {
      resolve({
        command: command.join(' '),
        code: code || 0,
        signal,
        output: Buffer.concat(chunks).toString('utf8'),
      });
    });
  });
}

function buildEvidence(result) {
  const passed = result.code === 0;
  return {
    gate: 'dlna-discovery',
    status: passed ? 'implemented' : 'not-started',
    percent: passed ? 100 : 0,
    closed: passed,
    sourcePhase: 'E3.1c',
    evidence: passed
      ? [
          'Jellyrin SSDP discovery golden completed with local UDP send/receive.',
          'The trace validates advertised SSDP targets, M-SEARCH response headers, NOTIFY alive/byebye formatting, base URL replacement for unspecified bind addresses and a real UDP M-SEARCH round trip against respond_to_ssdp_search.',
          'This is stronger than packet-only unit coverage, but it is still local-loopback evidence rather than LAN renderer/device validation.',
        ].join(' ')
      : [
          'DLNA discovery golden did not complete.',
          'If the failure is PermissionDenied while binding UDP sockets, rerun outside the sandbox because this golden validates real UDP send/receive.',
        ].join(' '),
    updatedAt: result.generatedAt,
    completedTargets: passed ? ['ssdp-contract', 'local-udp'] : [],
    skippedTargets: ['multi-interface-lan', 'renderer-device'],
    failedTargets: passed ? [] : ['local-udp'],
    tracePath: path.relative(plansDir, tracePath),
    validatedCommands: goldenCommands.map((command) => command.join(' ')),
    openRisks: [
      'Closing E3 still requires discovery from a real control point/renderer on LAN.',
      'The local UDP golden does not validate multicast routing across multiple host interfaces.',
      'Firewall, systemd and packaging rules for UDP 1900 remain E7/device-environment concerns.',
    ],
  };
}

function renderMarkdown(evidence) {
  const lines = [];
  lines.push('# DLNA Discovery Gate Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push('');
  lines.push(`Status: \`${evidence.status}\``);
  lines.push('');
  lines.push(`Progress: ${evidence.percent}%`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(evidence.evidence);
  lines.push('');
  lines.push(`- Trace: \`${evidence.tracePath}\``);
  lines.push(`- Completed targets: ${evidence.completedTargets.join(', ') || 'none'}`);
  lines.push(`- Skipped targets: ${evidence.skippedTargets.join(', ') || 'none'}`);
  lines.push(`- Failed targets: ${evidence.failedTargets.join(', ') || 'none'}`);
  lines.push('');
  lines.push('## Validation');
  lines.push('');
  for (const command of evidence.validatedCommands) {
    lines.push(`- \`${command}\``);
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
