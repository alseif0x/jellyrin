#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'dlna-events');
const tracePath = path.join(traceDir, 'golden-run.log');
const traceJsonPath = path.join(traceDir, 'golden-run.json');
const evidencePath = path.join(generatedDir, 'dlna-events.json');
const evidenceMarkdownPath = path.join(generatedDir, 'dlna-events.md');

const goldenCommands = [
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'dlna_initial_event_subscribe_sends_notify_seq_zero',
    '--',
    '--ignored',
    '--nocapture',
  ],
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'dlna_content_directory_change_sends_followup_notify',
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
    gate: 'dlna-events',
    status: passed ? 'implemented' : 'not-started',
    percent: passed ? 100 : 0,
    closed: passed,
    sourcePhase: 'E3.2g',
    evidence: passed
      ? [
          'Jellyrin GENA eventing golden completed with local TCP callback validation.',
          'The trace validates SUBSCRIBE response headers, initial NOTIFY SEQ 0 payloads for ContentDirectory and ConnectionManager, and follow-up ContentDirectory NOTIFY with SEQ 1 after SystemUpdateID changes.',
          'This proves the callback path locally, but it is still loopback evidence rather than a third-party control point validation.',
        ].join(' ')
      : [
          'DLNA eventing golden did not complete.',
          'If the failure is PermissionDenied while binding local TCP sockets, rerun outside the sandbox because this golden validates real callback delivery.',
        ].join(' '),
    updatedAt: result.generatedAt,
    completedTargets: passed ? ['gena-subscribe', 'initial-notify', 'followup-notify'] : [],
    skippedTargets: ['external-control-point'],
    failedTargets: passed ? [] : ['local-callback'],
    tracePath: path.relative(plansDir, tracePath),
    validatedCommands: goldenCommands.map((command) => command.join(' ')),
    openRisks: [
      'Closing E3 still requires eventing validation from a real control point on LAN.',
      'The local TCP golden does not validate firewall, NAT, renderer callback quirks or service sandboxing.',
    ],
  };
}

function renderMarkdown(evidence) {
  const lines = [];
  lines.push('# DLNA Eventing Gate Evidence');
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
