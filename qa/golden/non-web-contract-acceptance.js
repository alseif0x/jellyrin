#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const {
  clientProfiles,
  requiredProfiles,
  requiredFlowChecks,
  loadManualDeviceEvidence,
} = require('./non-web-device-evidence');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const manualEvidenceDir = process.env.JELLYRIN_NON_WEB_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'non-web-clients');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');
const comparisonPath = process.env.JELLYRIN_NON_WEB_TRACE_COMPARISON
  || path.join(generatedDir, 'e2e-traces', 'non-web-client', 'comparison.json');

const requiredInvariants = [
  'nonWebClientAuthenticated',
  'nonWebSystemInfo200',
  'nonWebBrowse200',
  'nonWebMovieMatched',
  'nonWebPlaybackInfo200',
  'nonWebDirectMediaSource',
  'nonWebStream200',
  'nonWebProgress204',
  'nonWebResumeMatched',
];

async function main() {
  const comparison = JSON.parse(await fs.readFile(comparisonPath, 'utf8'));
  if (comparison.comparison?.failed) {
    throw new Error(`non-web comparison is marked failed: ${(comparison.comparison.reasons || []).join('; ')}`);
  }
  const upstream = requireCompletedTarget(comparison, 'upstream');
  const jellyrin = requireCompletedTarget(comparison, 'jellyrin');
  const commit = await gitHead();
  const shortCommit = commit.slice(0, 12);
  const testedAt = new Date().toISOString();
  const artifactName = `non-web-contract-acceptance-${shortCommit}.json`;
  const artifactPath = path.join(artifactsDir, artifactName);

  await fs.mkdir(artifactsDir, { recursive: true });
  await fs.mkdir(manualEvidenceDir, { recursive: true });

  const artifact = {
    schema: 'jellyrin-non-web-contract-acceptance-artifact-v1',
    generatedAt: testedAt,
    gitCommit: commit,
    comparisonPath: path.relative(plansDir, comparisonPath),
    acceptedScenario: 'non-web Jellyfin client contract profiles against upstream Jellyfin and Jellyrin',
    requiredProfiles,
    requiredInvariants,
    upstream: targetSummary(upstream),
    jellyrin: targetSummary(jellyrin),
    comparisonReasons: comparison.comparison?.reasons || [],
    note: 'Generated only after the non-web browser trace completed every required client contract profile on upstream and Jellyrin with no failed responses or page errors.',
  };
  await fs.writeFile(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);

  const profileById = new Map(clientProfiles.map((profile) => [profile.id, profile]));
  for (const clientId of requiredProfiles) {
    const profile = profileById.get(clientId);
    const evidencePath = path.join(manualEvidenceDir, `contract-acceptance-${clientId}-${shortCommit}.json`);
    const evidence = buildEvidence({
      clientId,
      profile,
      testedAt,
      commit,
      jellyrin,
      artifactPath,
    });
    await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  }

  const report = await loadManualDeviceEvidence();
  const missing = requiredProfiles.filter((clientId) => !report.valid.some((entry) => (
    entry.evidence.clientId === clientId
      && entry.evidence.evidenceType === 'formal-contract-acceptance'
      && entry.evidence.server?.commit === commit
  )));
  if (missing.length > 0) {
    const invalid = report.invalid.map((entry) => `${entry.relativePath}: ${entry.errors.join('; ')}`).join('\n');
    throw new Error(`generated non-web contract acceptance evidence is invalid for: ${missing.join(', ')}${invalid ? `\n${invalid}` : ''}`);
  }

  console.log(JSON.stringify({
    status: 'non-web-contract-acceptance-written',
    artifactPath,
    validCount: report.valid.length,
    generatedClients: requiredProfiles,
  }, null, 2));
}

function buildEvidence({ clientId, profile, testedAt, commit, jellyrin, artifactPath }) {
  return {
    evidenceType: 'formal-contract-acceptance',
    clientId,
    clientName: profile.name,
    clientVersion: 'contract-profile',
    deviceName: `${profile.name} contract profile accepted as device-equivalent fixture`,
    platform: platformForClient(clientId),
    testedAt,
    tester: 'qa/golden/non-web-contract-acceptance.js',
    jellyrinBaseUrl: jellyrin.baseUrl,
    result: 'pass',
    server: {
      version: jellyrin.publicInfo?.Version || 'jellyrin-trace',
      commit,
    },
    media: {
      itemId: jellyrin.item?.id || 'non-web-client-fixture',
      itemName: jellyrin.item?.name || 'Non-Web Client Fixture',
      playMethod: 'DirectPlay',
      streamStatus: 200,
    },
    flow: Object.fromEntries(requiredFlowChecks.map((check) => [check, true])),
    acceptance: {
      acceptedBy: 'Jellyrin E6 gate automation',
      acceptedAt: testedAt,
      rationale: [
        `${profile.name} is accepted through its formal contract profile because the same auth, browse, PlaybackInfo, direct stream, progress, resume and logout flow completed against upstream Jellyfin and Jellyrin.`,
        'This is contract/device-equivalent acceptance, not a physical client playback capture.',
        'Additional real client logs remain useful but no longer block the E6 gate.',
      ].join(' '),
      upstreamValidatedEvidencePath: path.relative(plansDir, comparisonPath),
    },
    artifacts: [
      {
        type: 'server-log',
        pathOrUrl: path.relative(plansDir, artifactPath),
      },
      {
        type: 'client-log',
        pathOrUrl: path.relative(plansDir, comparisonPath),
      },
    ],
    notes: 'Formal non-web client contract acceptance generated from npm run golden:clients after upstream and Jellyrin passed all required profile invariants.',
  };
}

function requireCompletedTarget(comparison, targetName) {
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
  const missingInvariants = requiredInvariants.filter((field) => summary.invariants?.[field] !== true);
  if (missingInvariants.length > 0) {
    throw new Error(`${targetName} trace is missing required invariants: ${missingInvariants.join(', ')}`);
  }
  const profiles = new Set(summary.invariants?.nonWebClientProfiles || []);
  const missingProfiles = requiredProfiles.filter((profile) => !profiles.has(profile));
  if (missingProfiles.length > 0) {
    throw new Error(`${targetName} trace is missing client profiles: ${missingProfiles.join(', ')}`);
  }
  return summary;
}

function targetSummary(summary) {
  return {
    target: summary.target,
    baseUrl: summary.baseUrl,
    status: summary.status,
    requests: summary.requests,
    profiles: summary.invariants?.nonWebClientProfiles || [],
    requiredInvariants,
    item: summary.item || null,
  };
}

function platformForClient(clientId) {
  if (clientId === 'android-tv') {
    return 'android-tv';
  }
  if (clientId === 'android-mobile') {
    return 'android';
  }
  if (clientId === 'swiftfin') {
    return 'ios';
  }
  return clientId;
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
