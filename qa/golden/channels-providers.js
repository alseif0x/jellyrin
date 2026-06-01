#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'channels');
const comparisonPath = path.join(traceDir, 'comparison.json');
const evidencePath = path.join(generatedDir, 'channels-providers.json');
const evidenceMarkdownPath = path.join(generatedDir, 'channels-providers.md');

const upstreamRequired = [
  'channelsList200',
  'channelsFeatures200',
];

const jellyrinRequired = [
  ...upstreamRequired,
  'channelsProviderMatched',
  'channelsFilterMatched',
  'channelsDeletionFilterMatched',
  'channelsItems200',
  'channelsItemMatched',
  'channelsLatest200',
  'channelsFeatureMatched',
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const result = await runBrowserTrace();
  const comparison = await readJsonIfExists(comparisonPath);
  const evidence = buildEvidence(result, comparison);

  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence, comparison));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (evidence.status === 'not-started' || evidence.status === 'designed') {
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
          JELLYRIN_BROWSER_FLOW: 'channels',
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
      gate: 'channels-providers',
      status: 'not-started',
      percent: 0,
      closed: false,
      sourcePhase: 'E5.1',
      evidence: 'Channels provider browser trace did not produce comparison.json.',
      updatedAt,
      traceExitCode: result.code,
      openRisks: ['No real E5 channels-provider evidence has been generated yet.'],
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
  const invariantCoverage = channelsInvariantCoverage(summaries);
  const targetsHealthy = summaries.every((summary) => summary.status === 'completed'
    && !summary.skipped
    && (summary.failedResponses || []).length === 0
    && (summary.pageErrors || []).length === 0);

  if (targetsHealthy && invariantCoverage.complete) {
    return {
      gate: 'channels-providers',
      status: 'implemented',
      percent: 45,
      closed: false,
      sourcePhase: 'E5.1/E5.4/E5.5/browser-basic',
      evidence: [
        'Channels browser golden completed against upstream Jellyfin and Jellyrin.',
        'Both targets satisfy the base Channels contract for GET /Channels and GET /Channels/Features.',
        'Jellyrin additionally exposes a real Live TV-backed channel provider fixture through /Channels, /Channels/livetv/Items, /Channels/Items/Latest and /Channels/livetv/Features.',
        'The fixture validates provider filtering, media-deletion filtering, latest item listing and feature capability shape.',
        'This is an implemented E5 baseline, not full upstream-validated external provider parity: plugin-backed providers, refresh/cache persistence, search, images and failure isolation remain open.',
      ].join(' '),
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      upstreamRequired,
      jellyrinRequired,
      invariantCoverage,
      tracePath: path.relative(plansDir, comparisonPath),
      comparisonNotes: comparison.comparison?.reasons || [],
      openRisks: [
        'E5 target remains upstream-validated; current evidence covers base Channels API plus a Jellyrin Live TV-backed provider fixture, not multiple external providers.',
        'Provider registry/cache, refresh task state, search, image resolution and media-source playback for external channel items are still pending.',
        'Rust/WASI and DotNet plugin channel-provider fixtures are still pending on fuller provider ABI execution.',
        'Provider failure/timeout isolation still needs direct tests and browser evidence.',
      ],
    };
  }

  return {
    gate: 'channels-providers',
    status: completedTargets.includes('jellyrin') ? 'designed' : 'not-started',
    percent: completedTargets.includes('jellyrin') ? 20 : 0,
    closed: false,
    sourcePhase: 'E5.1/browser-attempted',
    evidence: 'Channels provider browser trace ran but did not satisfy the complete baseline invariants.',
    updatedAt,
    completedTargets,
    skippedTargets,
    failedTargets,
    failedReasons: comparison.comparison?.reasons || [],
    invariantCoverage,
    traceExitCode: result.code,
    tracePath: path.relative(plansDir, comparisonPath),
    openRisks: [
      'The E5 baseline browser invariants are not all passing; inspect failedReasons and comparison trace.',
      'Full Channels provider parity remains pending.',
    ],
  };
}

function channelsInvariantCoverage(summaries) {
  const missingByTarget = {};
  for (const summary of summaries.filter((summary) => summary.status === 'completed')) {
    const required = summary.target === 'jellyrin' ? jellyrinRequired : upstreamRequired;
    const missing = required.filter((field) => summary.invariants?.[field] !== true);
    if (missing.length > 0) {
      missingByTarget[summary.target] = missing;
    }
  }
  const completedTargetNames = new Set(
    summaries.filter((summary) => summary.status === 'completed').map((summary) => summary.target),
  );
  return {
    upstreamRequired,
    jellyrinRequired,
    complete: completedTargetNames.has('upstream')
      && completedTargetNames.has('jellyrin')
      && Object.keys(missingByTarget).length === 0,
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
  lines.push('# Channels Providers Evidence');
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
  if (Array.isArray(evidence.failedReasons) && evidence.failedReasons.length > 0) {
    lines.push('');
    lines.push('## Failed Reasons');
    lines.push('');
    for (const reason of evidence.failedReasons) {
      lines.push(`- ${reason}`);
    }
  }
  if (Array.isArray(evidence.comparisonNotes) && evidence.comparisonNotes.length > 0) {
    lines.push('');
    lines.push('## Comparison Notes');
    lines.push('');
    for (const note of evidence.comparisonNotes) {
      lines.push(`- ${note}`);
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
