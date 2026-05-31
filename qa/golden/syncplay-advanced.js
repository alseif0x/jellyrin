#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'syncplay');
const comparisonPath = path.join(traceDir, 'comparison.json');
const evidencePath = path.join(generatedDir, 'syncplay-advanced.json');
const evidenceMarkdownPath = path.join(generatedDir, 'syncplay-advanced.md');

const baselineEvidence = {
  gate: 'syncplay-advanced',
  status: 'implemented',
  percent: 20,
  closed: false,
  sourcePhase: 'E4.1a',
  evidence: 'SyncPlay state tracks participant readiness/buffering/last-seen, queue set/move/remove, ping activity, and SyncPlayGroupUpdate Command/Payload compatibility.',
  openRisks: [
    'Still needs upstream/browser golden execution against Jellyfin and Jellyrin.',
    'Still needs timeline, drift correction, reconnect and race handling.',
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

  if (evidence.failedTargets?.length > 0 || evidence.traceExitCode) {
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
          JELLYRIN_BROWSER_FLOW: 'syncplay',
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
      gate: 'syncplay-advanced',
      status: 'upstream-validated',
      percent: 100,
      closed: true,
      sourcePhase: 'E4.2a/E4.3a/E4.4a/E4.5a/E4.5b/E4.5c/E4.5d/E4.4b',
      evidence: 'SyncPlay browser golden completed against upstream and Jellyrin with no comparison failures for group creation, join/list/get, SetNewQueue/Pause/Seek/Unpause command handling, deterministic race handling, drift correction, same-device reconnect dedupe, logout cleanup, stale timeout cleanup and final group cleanup.',
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [],
    };
  }

  const jellyrinCompleted = completedTargets.includes('jellyrin');
  const jellyrinSummary = summaries.find((summary) => summary.target === 'jellyrin');
  const jellyrinPlayFanout =
    jellyrinSummary?.invariants?.syncplayPlay204 === true &&
    jellyrinSummary?.invariants?.syncplayPlayFanout === true;
  const jellyrinReconnect =
    jellyrinPlayFanout &&
    jellyrinSummary?.invariants?.syncplayGuestReconnectDeduped === true;
  const jellyrinLogoutCleanup =
    jellyrinReconnect &&
    jellyrinSummary?.invariants?.syncplayGuestLogoutRemoved === true;
  const jellyrinStaleCleanup =
    jellyrinLogoutCleanup &&
    jellyrinSummary?.invariants?.syncplayStaleCleanup === true;
  const jellyrinRaceSequenced =
    jellyrinStaleCleanup &&
    jellyrinSummary?.invariants?.syncplayRaceSequenced === true;
  const jellyrinDriftCorrection =
    jellyrinRaceSequenced &&
    jellyrinSummary?.invariants?.syncplayDriftCorrection === true;
  return {
    ...baselineEvidence,
    percent: jellyrinCompleted ? (jellyrinDriftCorrection ? 88 : jellyrinRaceSequenced ? 80 : jellyrinStaleCleanup ? 72 : jellyrinLogoutCleanup ? 65 : jellyrinReconnect ? 60 : jellyrinPlayFanout ? 50 : 35) : baselineEvidence.percent,
    sourcePhase: jellyrinDriftCorrection
      ? 'E4.2a/E4.3a/E4.4a/E4.5a/E4.5b/E4.5c/E4.5d/E4.4b'
      : jellyrinRaceSequenced
      ? 'E4.2a/E4.3a/E4.4a/E4.5a/E4.5b/E4.5c/E4.5d'
      : jellyrinStaleCleanup
      ? 'E4.2a/E4.3a/E4.4a/E4.5a/E4.5b/E4.5c'
      : jellyrinLogoutCleanup
      ? 'E4.2a/E4.3a/E4.4a/E4.5a/E4.5b'
      : jellyrinReconnect
      ? 'E4.2a/E4.3a/E4.4a/E4.5a'
      : jellyrinPlayFanout
        ? 'E4.2a/E4.3a/E4.4a'
        : baselineEvidence.sourcePhase,
    updatedAt,
    evidence: jellyrinCompleted && jellyrinDriftCorrection
      ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout, deterministic command sequencing under concurrent requests, drift correction, same-device reconnect dedupe, logout cleanup, stale timeout cleanup and final group cleanup. E4 still needs upstream comparable execution.'
      : jellyrinCompleted && jellyrinRaceSequenced
        ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout, deterministic command sequencing under concurrent requests, same-device reconnect dedupe, logout cleanup, stale timeout cleanup and final group cleanup. E4 still needs upstream comparable execution plus drift correction sub-gate.'
        : jellyrinCompleted && jellyrinStaleCleanup
          ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout, same-device reconnect dedupe, logout cleanup, stale timeout cleanup and final group cleanup. E4 still needs upstream comparable execution plus race sub-gate.'
          : jellyrinCompleted && jellyrinLogoutCleanup
            ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout, same-device reconnect dedupe, logout cleanup and final group cleanup. E4 still needs upstream comparable execution plus stale timeout cleanup and race sub-gates.'
            : jellyrinCompleted && jellyrinReconnect
              ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout, same-device reconnect dedupe and cleanup. E4 still needs upstream comparable execution plus stale cleanup and race sub-gates.'
              : jellyrinCompleted && jellyrinPlayFanout
                ? 'Jellyrin SyncPlay browser trace completed with Play/Pause/Seek/Unpause websocket fanout and cleanup. E4 still needs upstream comparable execution plus reconnect, stale cleanup and race sub-gates.'
      : jellyrinCompleted
        ? 'Jellyrin SyncPlay browser trace completed, but E4 is not upstream-validated yet.'
      : `${baselineEvidence.evidence} Browser trace did not complete enough targets for E4 closure.`,
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
  lines.push('# SyncPlay Advanced Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push(`Status: \`${evidence.status}\``);
  lines.push(`Progress: ${evidence.percent}%`);
  lines.push(`Closed: ${evidence.closed}`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(`- ${evidence.evidence}`);
  if (evidence.tracePath) {
    lines.push(`- Trace: \`${evidence.tracePath}\``);
  }
  if (Array.isArray(evidence.completedTargets)) {
    lines.push(`- Completed targets: ${evidence.completedTargets.join(', ') || 'none'}`);
  }
  if (Array.isArray(evidence.skippedTargets) && evidence.skippedTargets.length > 0) {
    lines.push(`- Skipped targets: ${evidence.skippedTargets.join(', ')}`);
  }
  if (Array.isArray(evidence.failedTargets) && evidence.failedTargets.length > 0) {
    lines.push(`- Failed targets: ${evidence.failedTargets.join(', ')}`);
  }
  if (Array.isArray(evidence.failedReasons) && evidence.failedReasons.length > 0) {
    lines.push('');
    lines.push('## Failed Reasons');
    lines.push('');
    for (const reason of evidence.failedReasons) {
      lines.push(`- ${reason}`);
    }
  }
  if (comparison?.summaries?.length) {
    lines.push('');
    lines.push('## Trace Targets');
    lines.push('');
    lines.push('| Target | Status | Skipped | Requests | Failed responses | Page errors |');
    lines.push('| --- | --- | --- | ---: | --- | --- |');
    for (const summary of comparison.summaries) {
      lines.push(`| ${summary.target} | \`${summary.status}\` | ${Boolean(summary.skipped)} | ${summary.requests || 0} | ${(summary.failedResponses || []).join(', ') || 'none'} | ${(summary.pageErrors || []).join(', ') || 'none'} |`);
    }
  }
  lines.push('');
  lines.push('## Open Risks');
  lines.push('');
  for (const risk of evidence.openRisks || []) {
    lines.push(`- ${risk}`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
