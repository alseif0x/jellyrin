#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'live-tv');
const comparisonPath = path.join(traceDir, 'comparison.json');
const evidencePath = path.join(generatedDir, 'livetv-real.json');
const evidenceMarkdownPath = path.join(generatedDir, 'livetv-real.md');

const requiredInvariants = [
  'liveTvConfigUpdated',
  'liveTvInfo200',
  'liveTvTunerTypes200',
  'liveTvChannels200',
  'liveTvChannelMatched',
  'liveTvGuidePrograms200',
  'liveTvProgramMatched',
  'liveTvStream200',
  'liveTvRecordings200',
  'liveTvRecordingStream200',
  'liveTvTimerCreated',
  'liveTvTimerDeleted',
  'liveTvSeriesTimerCreated',
  'liveTvSeriesTimerDeleted',
];

const baselineEvidence = {
  gate: 'livetv-real',
  status: 'not-started',
  percent: 0,
  closed: false,
  sourcePhase: 'E2.1',
  evidence: 'Live TV browser trace exists but has not completed against upstream and Jellyrin yet.',
  openRisks: [
    'M3U/XMLTV simulator evidence is not HDHomeRun or real tuner validation.',
    'Live HLS/transcode, stream refcount, restart recovery and recording scheduler still need deeper E2 sub-gates.',
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

  const jellyrinCompleted = Array.isArray(evidence.completedTargets) && evidence.completedTargets.includes('jellyrin');
  if (!jellyrinCompleted && evidence.status !== 'upstream-validated' && evidence.status !== 'device-validated') {
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
          JELLYRIN_BROWSER_FLOW: 'live-tv',
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
  const invariantCoverage = liveTvInvariantCoverage(summaries);

  if (!failed && completedTargets.includes('jellyrin') && completedTargets.includes('upstream') && invariantCoverage.complete) {
    return {
      gate: 'livetv-real',
      status: 'upstream-validated',
      percent: 45,
      closed: false,
      sourcePhase: 'E2.1',
      evidence: 'Live TV M3U/XMLTV fixture golden completed against upstream and Jellyrin with channels, guide, direct TS stream, recordings, timers and series timers validated.',
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      invariantCoverage,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [
        'Dashboard target remains device-validated; HDHomeRun or real tuner/simulator evidence is still required before closing E2.',
        'Live HLS/transcode, two-client stream refcount, restart recovery and real recording-file creation still need implementation evidence.',
      ],
    };
  }

  const jellyrinCompleted = completedTargets.includes('jellyrin');
  return {
    ...baselineEvidence,
    status: jellyrinCompleted ? 'implemented' : baselineEvidence.status,
    percent: jellyrinCompleted ? 35 : baselineEvidence.percent,
    updatedAt,
    evidence: jellyrinCompleted
      ? 'Jellyrin Live TV trace completed with channels, guide, direct TS stream, recordings, timers and series timers validated. Upstream direct livetv configuration injection is not comparable yet.'
      : `${baselineEvidence.evidence} Browser trace did not complete enough targets for E2 progress.`,
    completedTargets,
    skippedTargets,
    failedTargets,
    invariantCoverage,
    failedReasons: comparison.comparison?.reasons || [],
    traceExitCode: result.code,
    tracePath: path.relative(plansDir, comparisonPath),
    openRisks: jellyrinCompleted
      ? [
          'Upstream Jellyfin does not expose the synthetic M3U/XMLTV fixture through the direct System/Configuration/livetv path used by this harness; a real HDHomeRun/M3U setup path or upstream fixture hook is still needed.',
          'Dashboard target remains device-validated; HDHomeRun or real tuner/simulator evidence is still required before closing E2.',
          'Live HLS/transcode, two-client stream refcount, restart recovery and real recording-file creation still need deeper E2 sub-gates.',
        ]
      : baselineEvidence.openRisks,
  };
}

function liveTvInvariantCoverage(summaries) {
  const completedSummaries = summaries.filter((summary) => summary.status === 'completed');
  const missingByTarget = {};
  for (const summary of completedSummaries) {
    const missing = requiredInvariants.filter((field) => summary.invariants?.[field] !== true);
    if (missing.length > 0) {
      missingByTarget[summary.target] = missing;
    }
  }
  return {
    required: requiredInvariants,
    complete: completedSummaries.length > 0 && Object.keys(missingByTarget).length === 0,
    missingByTarget,
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
  lines.push('# Live TV Real Evidence');
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
  if (evidence.invariantCoverage) {
    lines.push(`- Required invariants: ${evidence.invariantCoverage.required.length}`);
    lines.push(`- Invariant coverage complete: ${evidence.invariantCoverage.complete}`);
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
  if (Array.isArray(evidence.openRisks) && evidence.openRisks.length > 0) {
    lines.push('');
    lines.push('## Open Risks');
    lines.push('');
    for (const risk of evidence.openRisks) {
      lines.push(`- ${risk}`);
    }
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
