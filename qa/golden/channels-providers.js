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
  'channelsDiagnosticsMatched',
  'channelsFailureIsolated',
  'channelsFilterMatched',
  'channelsDeletionFilterMatched',
  'channelsItems200',
  'channelsItemMatched',
  'channelsSearchMatched',
  'channelsMediaSourceResolved',
  'channelsStreamBytes',
  'channelsLatest200',
  'channelsLatestSearchMatched',
  'channelsFeatureMatched',
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const localSubgates = await runLocalSubgates();
  const result = await runBrowserTrace();
  const comparison = await readJsonIfExists(comparisonPath);
  const evidence = buildEvidence(result, comparison, localSubgates);

  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence, comparison));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);

  if (localSubgates.some((subgate) => subgate.code !== 0)) {
    process.exitCode = localSubgates.find((subgate) => subgate.code !== 0)?.code || 1;
    return;
  }
  if (evidence.status === 'not-started' || evidence.status === 'designed') {
    process.exitCode = result.code || 1;
  }
}

async function runLocalSubgates() {
  const subgates = [
    {
      target: 'plugin-channel-provider-media-source',
      command: ['cargo', 'test', '-p', 'jellyrin-api', 'rust_wasi_channel_provider_feeds_channels_api', '--', '--nocapture'],
      evidence: 'Rust/WASI ChannelProvider fixture feeds /Channels, resolves provider item playback through MediaInfo/LiveStreams/Open and Close, and serves provider item Primary images through Image/Items',
    },
    {
      target: 'provider-refresh-cache-history',
      command: ['cargo', 'test', '-p', 'jellyrin-api', 'channels_refresh_task_persists_provider_cache_and_history', '--', '--nocapture'],
      evidence: 'RefreshChannels scheduled task persists provider cache, item counts, item ids, last refresh timestamp and bounded refresh history',
    },
    {
      target: 'runtime-plugin-provider-failure-isolation',
      command: ['cargo', 'test', '-p', 'jellyrin-api', 'rust_wasi_channel_provider_failure_isolated_from_channels_api', '--', '--nocapture'],
      evidence: 'Runtime ChannelProvider failures are isolated from /Channels and provider item browse while /Channels/Diagnostics reports Malfunctioned with failure detail',
    },
    {
      target: 'dotnet-plugin-channel-provider-media-source',
      command: ['cargo', 'test', '-p', 'jellyrin-api', 'dotnet_channel_provider_feeds_channels_api', '--', '--nocapture'],
      evidence: 'DotNetJellyfin ChannelProvider fixture activates through stdio host, feeds /Channels and /Channels/{id}/Items, resolves provider item playback through MediaInfo/LiveStreams/Open and reports healthy diagnostics',
    },
  ];
  const results = [];
  for (const subgate of subgates) {
    const result = await runCommand(subgate.command[0], subgate.command.slice(1));
    results.push({ ...subgate, ...result, command: subgate.command.join(' ') });
  }
  return results;
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

function runCommand(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env: process.env,
      stdio: 'inherit',
    });
    child.on('close', (code, signal) => resolve({ code: code || 0, signal }));
  });
}

function buildEvidence(result, comparison, localSubgates) {
  const updatedAt = new Date().toISOString();
  const localPassed = localSubgates.length > 0 && localSubgates.every((subgate) => subgate.code === 0);
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
      localSubgates,
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
  const localCompletedTargets = localPassed ? ['local-channel-provider-subgates'] : [];
  const allCompletedTargets = [...new Set([...completedTargets, ...localCompletedTargets])].sort();

  if (targetsHealthy && invariantCoverage.complete && localPassed) {
    return {
      gate: 'channels-providers',
      status: 'implemented',
      percent: 90,
      closed: false,
      sourcePhase: 'E5.1/E5.2/E5.3/E5.4/E5.5/browser-basic/plugin-provider-media-source/provider-images/refresh-cache-history/runtime-plugin-failure-isolation/dotnet-provider',
      evidence: [
        'Channels browser golden completed against upstream Jellyfin and Jellyrin.',
        'Both targets satisfy the base Channels contract for GET /Channels and GET /Channels/Features.',
        'Jellyrin additionally exposes a real Live TV-backed channel provider fixture through /Channels, /Channels/livetv/Items, /Channels/Items/Latest, /Channels/livetv/Features, /Channels/Diagnostics and MediaInfo live-stream resolution.',
        'The fixture validates provider filtering, media-deletion filtering, item SearchTerm filtering, latest item listing/search, feature capability shape, media-source resolution, direct stream byte delivery and failure isolation for a configured malfunctioning provider.',
        'The local Rust/WASI ChannelProvider fixture validates plugin-backed provider browse, non-Live-TV provider item MediaInfo/LiveStreams/Open and Close resolution, ImageTags and provider item Primary image serving through /Image/Items.',
        'The local DotNetJellyfin ChannelProvider fixture validates stdio-host activation, plugin-backed provider browse, non-Live-TV provider item MediaInfo/LiveStreams/Open resolution and healthy diagnostics.',
        'RefreshChannels now persists provider cache, item ids, refresh timestamps and refresh history in the channels named configuration.',
        'Runtime plugin ChannelProvider failures are isolated from /Channels and /Channels/{id}/Items while diagnostics reports Malfunctioned with failure detail.',
        'This is an implemented E5 baseline, not full upstream-validated external provider parity: remote HTTP image cache hydration, real external provider traces and broader non-Live-TV provider playback remain open.',
      ].join(' '),
      updatedAt,
      completedTargets: allCompletedTargets,
      skippedTargets,
      failedTargets,
      upstreamRequired,
      jellyrinRequired,
      invariantCoverage,
      localSubgates,
      tracePath: path.relative(plansDir, comparisonPath),
      comparisonNotes: comparison.comparison?.reasons || [],
      openRisks: [
        'E5 target remains upstream-validated; current evidence covers base Channels API plus a Jellyrin Live TV-backed provider fixture, not multiple external providers.',
        'Provider item image resolution is covered for embedded data/plugin payloads; remote HTTP image fetching and cache hydration still need broader provider traces.',
        'Rust/WASI and DotNetJellyfin plugin channel-provider browse and MediaInfo resolution are covered by local subgates; real Jellyfin extension-point adapters still need external provider traces.',
        'Provider failure/timeout isolation is covered for declarative and Rust/WASI runtime malfunctioned providers; broader browser evidence with real external providers is still pending.',
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
    localSubgates,
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
  if (Array.isArray(evidence.localSubgates) && evidence.localSubgates.length > 0) {
    lines.push('');
    lines.push('## Local Subgates');
    lines.push('');
    lines.push('| Subgate | Exit | Evidence | Command |');
    lines.push('| --- | ---: | --- | --- |');
    for (const subgate of evidence.localSubgates) {
      lines.push(`| ${subgate.target} | ${subgate.code} | ${subgate.evidence} | \`${subgate.command}\` |`);
    }
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
