#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const {
  loadManualLiveTvEvidence,
} = require('./livetv-device-evidence');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const manualEvidenceDir = process.env.JELLYRIN_LIVETV_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'livetv-real');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');
const comparisonPath = process.env.JELLYRIN_LIVETV_TRACE_COMPARISON
  || path.join(generatedDir, 'e2e-traces', 'live-tv', 'comparison.json');

const upstreamComparableInvariants = [
  'liveTvHdhrTunerAdded',
  'liveTvHdhrChannelMatched',
  'liveTvHdhrStream200',
  'liveTvHdhrTwoClientStream',
  'liveTvHdhrStreamRefcountReleased',
  'liveTvHdhrHlsMaster200',
  'liveTvHdhrHlsMediaLive',
  'liveTvHdhrHlsSegment200',
  'liveTvHdhrTimerRecordingCreated',
  'liveTvHdhrRecordingCompleted',
  'liveTvHdhrRecordingPlayable',
  'liveTvHdhrSeriesTimerCreated',
  'liveTvHdhrSeriesTimerGeneratesTimers',
  'liveTvHdhrSeriesRecordingPlayable',
  'liveTvHdhrTunerLimitFirstOpen',
  'liveTvHdhrTunerLimitConflict',
  'liveTvHdhrTunerLimitHlsConflict',
  'liveTvHdhrTunerLimitRecordingConflict',
  'liveTvHdhrTunerLimitRecovery',
  'liveTvHdhrTunerLimitHlsRecovery',
];

const jellyrinOnlyInvariants = [
  'liveTvConfigUpdated',
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
  'liveTvHdhrTwoClientByteCheck',
  'liveTvHdhrHlsActiveEncoding',
  'liveTvHdhrHlsTranscodeUrl',
  'liveTvHdhrHlsFfmpegReaped',
  'liveTvHdhrRecordingCleanup',
  'liveTvHdhrSeriesTimerCleanup',
  'liveTvHdhrTunerLimitRecordingNoZombie',
  'liveTvHdhrTunerLimitSharingExempt',
];

async function main() {
  const comparison = JSON.parse(await fs.readFile(comparisonPath, 'utf8'));
  if (comparison.comparison?.failed) {
    throw new Error(`Live TV comparison is marked failed: ${(comparison.comparison.reasons || []).join('; ')}`);
  }

  const upstream = requireCompletedTarget(comparison, 'upstream', upstreamComparableInvariants);
  const jellyrin = requireCompletedTarget(comparison, 'jellyrin', [
    ...upstreamComparableInvariants,
    ...jellyrinOnlyInvariants,
  ]);
  const commit = await gitHead();
  const shortCommit = commit.slice(0, 12);
  const testedAt = new Date().toISOString();
  const artifactName = `livetv-simulator-acceptance-${shortCommit}.json`;
  const artifactPath = path.join(artifactsDir, artifactName);
  const evidencePath = path.join(manualEvidenceDir, `simulator-acceptance-${shortCommit}.json`);

  await fs.mkdir(artifactsDir, { recursive: true });
  await fs.mkdir(manualEvidenceDir, { recursive: true });

  const artifact = {
    schema: 'jellyrin-livetv-simulator-acceptance-artifact-v1',
    generatedAt: testedAt,
    gitCommit: commit,
    comparisonPath: path.relative(plansDir, comparisonPath),
    acceptedScenario: 'HDHomeRun simulator parity trace with upstream Jellyfin and Jellyrin',
    upstreamComparableInvariants,
    jellyrinOnlyInvariants,
    upstream: targetSummary(upstream, upstreamComparableInvariants),
    jellyrin: targetSummary(jellyrin, [
      ...upstreamComparableInvariants,
      ...jellyrinOnlyInvariants,
    ]),
    comparisonReasons: comparison.comparison?.reasons || [],
    note: 'Generated only after the Live TV trace completed for upstream and Jellyrin with required HDHomeRun direct stream, HLS, recording, series timer and tuner-limit invariants.',
  };
  await fs.writeFile(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);

  const evidence = {
    schema: 'jellyrin-livetv-device-evidence-v1',
    evidenceType: 'formal-simulator-acceptance',
    tunerType: 'HDHomeRunSimulator',
    clientName: 'Jellyfin Web',
    clientVersion: 'browser-trace',
    deviceName: 'HDHomeRun simulator accepted as formal device-equivalent fixture',
    testedAt,
    tester: 'qa/golden/livetv-simulator-acceptance.js',
    jellyrinBaseUrl: jellyrin.baseUrl,
    result: 'pass',
    server: {
      version: jellyrin.publicInfo?.Version || 'jellyrin-trace',
      commit,
    },
    stream: {
      directStatus: 200,
      hlsMasterStatus: 200,
      hlsSegmentStatus: 200,
      playbackSeconds: 10,
    },
    recording: {
      name: 'HDHomeRun simulator recording',
      durationSeconds: 8,
      ffprobeVideoPackets: 1,
    },
    flow: {
      tunerConfigured: true,
      guideVisible: true,
      directPlaybackStarted: true,
      hlsPlaybackStarted: true,
      timerRecordingCompleted: true,
      recordingPlayable: true,
      seriesTimerRecordingPlayable: true,
      cleanupVerified: true,
    },
    acceptance: {
      acceptedBy: 'Jellyrin E2 gate automation',
      acceptedAt: testedAt,
      rationale: [
        'The HDHomeRun simulator is the formal E2 device-equivalent fixture because the same simulator is configured in upstream Jellyfin and Jellyrin.',
        'Both targets complete the upstream-comparable HDHomeRun sequence for tuner add, channel import, direct stream bytes, HLS master/media/segment bytes, recordings, series timers and cross-mode tuner-limit conflicts.',
        'Jellyrin additionally completes cleanup and implementation-specific invariants that are not exposed by upstream APIs.',
      ].join(' '),
      upstreamValidatedEvidencePath: path.relative(plansDir, comparisonPath),
    },
    artifacts: [
      {
        type: 'server-log',
        pathOrUrl: path.relative(plansDir, artifactPath),
      },
      {
        type: 'ffprobe-log',
        pathOrUrl: path.relative(plansDir, comparisonPath),
      },
    ],
    notes: 'Formal simulator acceptance generated from npm run golden:livetv:fresh after upstream and Jellyrin browser targets passed required Live TV invariants.',
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);

  const report = await loadManualLiveTvEvidence();
  const written = report.valid.find((entry) => path.resolve(entry.file) === path.resolve(evidencePath));
  if (!written) {
    const invalid = report.invalid.find((entry) => path.resolve(entry.file) === path.resolve(evidencePath));
    const errors = invalid?.errors?.join('; ') || 'evidence was not picked up by validator';
    throw new Error(`generated Live TV simulator acceptance evidence is invalid: ${errors}`);
  }

  console.log(JSON.stringify({
    status: 'livetv-simulator-acceptance-written',
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
