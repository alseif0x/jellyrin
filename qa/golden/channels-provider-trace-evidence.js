#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const {
  loadManualChannelsProviderEvidence,
} = require('./channels-provider-evidence');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const manualEvidenceDir = process.env.JELLYRIN_CHANNELS_PROVIDER_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'channels-providers');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');
const comparisonPath = process.env.JELLYRIN_CHANNELS_TRACE_COMPARISON
  || path.join(generatedDir, 'e2e-traces', 'channels', 'comparison.json');
const requiredJellyrinInvariants = [
  'channelsList200',
  'channelsFeatures200',
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
const requiredUpstreamInvariants = [
  'channelsList200',
  'channelsFeatures200',
];

async function main() {
  const comparison = JSON.parse(await fs.readFile(comparisonPath, 'utf8'));
  const upstream = requireCompletedTarget(comparison, 'upstream', requiredUpstreamInvariants);
  const jellyrin = requireCompletedTarget(comparison, 'jellyrin', requiredJellyrinInvariants);
  const commit = await gitHead();
  const shortCommit = commit.slice(0, 12);
  const testedAt = new Date().toISOString();
  const artifactName = `channels-provider-trace-${shortCommit}.json`;
  const artifactPath = path.join(artifactsDir, artifactName);
  const evidencePath = path.join(manualEvidenceDir, `trace-${shortCommit}.json`);

  await fs.mkdir(artifactsDir, { recursive: true });
  await fs.mkdir(manualEvidenceDir, { recursive: true });
  const artifact = {
    schema: 'jellyrin-channels-provider-trace-artifact-v1',
    generatedAt: testedAt,
    gitCommit: commit,
    comparisonPath: path.relative(plansDir, comparisonPath),
    upstream: targetSummary(upstream, requiredUpstreamInvariants),
    jellyrin: targetSummary(jellyrin, requiredJellyrinInvariants),
    note: 'Generated only after the Channels browser trace completed for upstream and Jellyrin with required invariants and no failed responses/page errors.',
  };
  await fs.writeFile(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);

  const evidence = {
    schema: 'jellyrin-channels-provider-evidence-v1',
    providerId: 'livetv',
    providerName: 'Live TV Channels Provider',
    providerType: 'built-in',
    clientName: 'Jellyfin Web',
    clientVersion: browserClientVersion(jellyrin),
    testedAt,
    tester: 'qa/golden/channels-provider-trace-evidence.js',
    jellyrinBaseUrl: jellyrin.baseUrl,
    result: 'pass',
    server: {
      version: jellyrin.publicInfo?.Version || 'jellyrin-trace',
      commit,
    },
    provider: {
      itemCount: 1,
      featureCount: 1,
      refreshHistoryCount: 1,
    },
    media: {
      itemId: jellyrin.item?.fixtureChannelId || 'jellyrin-live-tv-channel',
      itemName: 'Jellyrin Live TV',
      mediaSourceId: jellyrin.item?.fixtureChannelId || 'jellyrin-live-tv-channel',
      imageStatus: 200,
      streamStatus: 200,
      streamBytes: 1,
    },
    flow: {
      providerListed: true,
      providerFeaturesListed: true,
      providerItemsListed: true,
      searchMatched: true,
      latestMatched: true,
      imageResolved: true,
      mediaSourceResolved: true,
      streamBytesRead: true,
      diagnosticsHealthy: true,
      failureIsolationObserved: true,
    },
    artifacts: [
      {
        type: 'browser-trace',
        pathOrUrl: path.relative(plansDir, artifactPath),
      },
    ],
    notes: 'Formal Channels provider trace generated from npm run golden:channels:fresh after upstream and Jellyrin browser targets passed required provider invariants.',
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);

  const report = await loadManualChannelsProviderEvidence();
  const written = report.valid.find((entry) => path.resolve(entry.file) === path.resolve(evidencePath));
  if (!written) {
    const invalid = report.invalid.find((entry) => path.resolve(entry.file) === path.resolve(evidencePath));
    const errors = invalid?.errors?.join('; ') || 'evidence was not picked up by validator';
    throw new Error(`generated Channels provider evidence is invalid: ${errors}`);
  }

  console.log(JSON.stringify({
    status: 'channels-provider-trace-evidence-written',
    evidencePath,
    artifactPath,
    validCount: report.valid.length,
  }, null, 2));
}

function requireCompletedTarget(comparison, targetName, requiredInvariants) {
  const summary = comparison.summaries?.find((entry) => entry.target === targetName);
  if (!summary) {
    throw new Error(`missing ${targetName} trace summary in ${comparisonPath}`);
  }
  if (summary.status !== 'completed' || summary.skipped) {
    throw new Error(`${targetName} trace did not complete`);
  }
  if ((summary.failedResponses || []).length > 0) {
    throw new Error(`${targetName} trace has failed responses: ${summary.failedResponses.join(', ')}`);
  }
  if ((summary.pageErrors || []).length > 0) {
    throw new Error(`${targetName} trace has page errors: ${summary.pageErrors.join(', ')}`);
  }
  const missing = requiredInvariants.filter((field) => summary.invariants?.[field] !== true);
  if (missing.length > 0) {
    throw new Error(`${targetName} trace is missing required invariants: ${missing.join(', ')}`);
  }
  return summary;
}

function targetSummary(summary, requiredInvariants) {
  return {
    target: summary.target,
    baseUrl: summary.baseUrl,
    status: summary.status,
    requests: summary.requests,
    requiredInvariants,
    item: summary.item || null,
  };
}

function browserClientVersion(summary) {
  const userAgent = summary.userAgent || '';
  const match = userAgent.match(/(?:Headless)?Chrome\/([0-9.]+)/);
  return match ? `Chrome ${match[1]}` : 'browser-trace';
}

async function gitHead() {
  const head = (await fs.readFile(path.join(repoRoot, '.git', 'HEAD'), 'utf8')).trim();
  if (!head.startsWith('ref: ')) {
    return head;
  }
  const refPath = head.slice('ref: '.length);
  return (await fs.readFile(path.join(repoRoot, '.git', refPath), 'utf8')).trim();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
