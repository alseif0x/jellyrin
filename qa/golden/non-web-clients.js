#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'non-web-client');
const comparisonPath = path.join(traceDir, 'comparison.json');
const evidencePath = path.join(generatedDir, 'non-web-clients.json');
const evidenceMarkdownPath = path.join(generatedDir, 'non-web-clients.md');

const baselineEvidence = {
  gate: 'non-web-clients',
  status: 'implemented',
  percent: 25,
  closed: false,
  sourcePhase: 'E6.1a',
  evidence: 'MPV Shim/Jellyfin Media Player contract flow is wired but has not completed against upstream and Jellyrin yet.',
  openRisks: [
    'Device validation still requires at least one real non-web client playback run.',
    'Kodi, Android TV, Android mobile, Swiftfin/iOS and Roku contracts still need dedicated client profiles.',
  ],
};

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const result = await runBrowserTrace();
  const comparison = await readJsonIfExists(comparisonPath);
  const evidence = buildEvidence(result, comparison);

  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence, comparison));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (evidence.status !== 'upstream-validated' && evidence.status !== 'device-validated') {
    process.exitCode = result.code || 1;
  }
}

function runBrowserTrace() {
  return new Promise((resolve) => {
    const child = spawn(
      process.execPath,
      [path.join(__dirname, 'browser-trace.js')],
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          JELLYRIN_BROWSER_FLOW: 'non-web-client',
        },
        stdio: 'inherit',
      },
    );
    child.on('close', (code, signal) => resolve({ code: code || 0, signal }));
  });
}

function buildEvidence(result, comparison) {
  const updatedAt = new Date().toISOString();
  if (!comparison) {
    return {
      ...baselineEvidence,
      updatedAt,
      evidence: `${baselineEvidence.evidence} Browser trace did not produce comparison.json.`,
    };
  }

  const summaries = comparison.summaries || [];
  const completedTargets = summaries
    .filter((summary) => summary.status === 'completed')
    .map((summary) => summary.target)
    .sort();
  const skippedTargets = summaries
    .filter((summary) => summary.skipped)
    .map((summary) => summary.target)
    .sort();
  const failedTargets = summaries
    .filter((summary) => summary.status === 'failed')
    .map((summary) => summary.target)
    .sort();
  const failed = Boolean(comparison.comparison?.failed);

  if (!failed && completedTargets.includes('jellyrin') && completedTargets.includes('upstream')) {
    return {
      gate: 'non-web-clients',
      status: 'upstream-validated',
      percent: 55,
      closed: false,
      sourcePhase: 'E6.1b',
      evidence: 'MPV Shim/Jellyfin Media Player contract golden completed against upstream and Jellyrin with no comparison failures.',
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [
        'Dashboard target remains device-validated; run a real MPV Shim/Jellyfin Media Player playback session before closing E6.',
        'Kodi, Android TV, Android mobile, Swiftfin/iOS and Roku contracts still need dedicated client profiles.',
      ],
    };
  }

  const jellyrinCompleted = completedTargets.includes('jellyrin');
  return {
    ...baselineEvidence,
    percent: jellyrinCompleted ? 35 : baselineEvidence.percent,
    updatedAt,
    evidence: jellyrinCompleted
      ? 'Jellyrin non-web client contract trace completed, but E6 is not upstream-validated yet.'
      : `${baselineEvidence.evidence} Browser trace did not complete enough targets for E6 progress.`,
    completedTargets,
    skippedTargets,
    failedTargets,
    failedReasons: comparison.comparison?.reasons || [],
    traceExitCode: result.code,
    tracePath: path.relative(plansDir, comparisonPath),
  };
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

function renderMarkdown(evidence, comparison) {
  const lines = [];
  lines.push('# Non-Web Clients Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push(`Gate: ${evidence.gate}`);
  lines.push(`Status: ${evidence.status}`);
  lines.push(`Percent: ${evidence.percent}`);
  lines.push(`Closed: ${evidence.closed}`);
  lines.push(`Source phase: ${evidence.sourcePhase}`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(evidence.evidence);
  lines.push('');
  if (evidence.tracePath) {
    lines.push(`- Trace: \`${evidence.tracePath}\``);
  }
  if (Array.isArray(evidence.completedTargets)) {
    lines.push(`- Completed targets: ${evidence.completedTargets.join(', ') || 'none'}`);
  }
  if (Array.isArray(evidence.failedTargets)) {
    lines.push(`- Failed targets: ${evidence.failedTargets.join(', ') || 'none'}`);
  }
  lines.push('');
  if (Array.isArray(evidence.failedReasons) && evidence.failedReasons.length > 0) {
    lines.push('## Failed Reasons');
    lines.push('');
    for (const reason of evidence.failedReasons) {
      lines.push(`- ${reason}`);
    }
    lines.push('');
  }
  if (Array.isArray(evidence.openRisks) && evidence.openRisks.length > 0) {
    lines.push('## Open Risks');
    lines.push('');
    for (const risk of evidence.openRisks) {
      lines.push(`- ${risk}`);
    }
    lines.push('');
  }
  if (comparison?.comparison?.reasons?.length) {
    lines.push('## Comparison Reasons');
    lines.push('');
    for (const reason of comparison.comparison.reasons) {
      lines.push(`- ${reason}`);
    }
    lines.push('');
  }
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
