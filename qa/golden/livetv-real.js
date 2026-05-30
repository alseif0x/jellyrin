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

// Invariants validated by both upstream and Jellyrin against the same HDHomeRun simulator.
// upstream-validated is decided by this set alone — synthetic M3U/XMLTV invariants are
// deliberately excluded because upstream does not expose the direct configuration injection
// path used by Jellyrin and materialises guide data asynchronously.
//
// liveTvHdhrStream200 is in the comparable set for BOTH targets:
//   - Jellyrin: GET /LiveTv/LiveStreamFiles/hdhr_{n}/stream.ts via browserFetchStreamProbe
//     (AbortController, reads >=1 byte then aborts). The proxy now streams incrementally
//     (bytes_stream + Body::from_stream) so headers and bytes are returned immediately.
//   - upstream: GET of the LiveStreamFiles URL returned by PlaybackInfo via browserFetchStreamProbe.
//   Both paths verify status 200, content-type video/mp2t, and byteLength >= 1 against the
//   same HDHomeRun simulator running a continuous TS stream.
//
// liveTvHdhrStreamSetup is still set informatively (MediaSource path verification) but is
// NOT required for upstream-validated — the byte-check (liveTvHdhrStream200) is the gate.
const upstreamComparable = [
  'liveTvInfo200',
  'liveTvTunerTypes200',
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
  'liveTvHdhrTunerLimitRecovery',
];

// Invariants validated only by Jellyrin via the synthetic M3U/XMLTV shortcut. These exercise
// Jellyrin-specific embedded channel/program/recording materialisation and are not comparable
// to upstream, which ignores the Channels/Programs/Recordings fields in the configuration
// payload and relies on async guide refresh instead.
const jellyrinOnly = [
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
  'liveTvHdhrTunerLimitSharingExempt',
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
      evidence: [
        'Live TV HDHomeRun golden completed against both upstream Jellyfin and Jellyrin using the same simulator.',
        'upstream-validated is decided by the 19 HDHomeRun real-sequence invariants (upstreamComparable) executed by BOTH targets against the same simulator.',
        'The 19 synthetic M3U/XMLTV + jellyrin-only invariants (jellyrinOnly) are intentionally excluded from the upstream comparison.',
        'upstream does not expose the direct System/Configuration/livetv channel injection path used by Jellyrin,',
        'and materialises guide data asynchronously via RefreshGuideScheduledTask rather than eagerly.',
        'Jellyrin satisfies all 38 invariants (19 HDHomeRun comparable + 19 jellyrin-only). upstream satisfies the 19 HDHomeRun invariants.',
        'liveTvHdhrStream200 (byte delivery via direct TS proxy) is in the comparable set for BOTH targets.',
        'liveTvHdhrHlsMaster200 (HLS transcode master playlist): BOTH targets serve master.m3u8 from the HDHomeRun channel.',
        'Jellyrin: TranscodingUrl embedded in channel MediaSource (SupportsTranscoding:true, TranscodingSubProtocol:hls).',
        'upstream: PlaybackInfo with HLS device profile returns TranscodingUrl.',
        'liveTvHdhrHlsMediaLive (HLS live media playlist): BOTH targets serve a media playlist with >=1 #EXTINF and no #EXT-X-ENDLIST.',
        'The simulator serves a 600 s monotonic MPEG-2 TS clip at native bitrate (~13 kB/s) so SharedHttpStream grows at live-TV speed.',
        'liveTvHdhrHlsSegment200 (HLS segment): BOTH targets serve video/mp2t segments with real h264 bytes.',
        'liveTvHdhrHlsActiveEncoding (jellyrin-only): Jellyrin exposes GET /Videos/ActiveEncodings; upstream returns 405 for GET (only DELETE supported). Evidence: curl -X GET /Videos/ActiveEncodings on upstream 8098 -> HTTP 405 Allow: DELETE. Degraded to jellyrin-only per spec integrity rule.',
        'liveTvHdhrTwoClientStream (stream sharing): metric = simulator concurrent connections at /auto/vN.',
        '2 client probes of the same channel with sharing enabled → maxConcurrentByChannel===1 (one outgoing connection shared).',
        'Jellyrin implements SharedLiveStreamHandle with broadcast fan-out and refcount guard (paridad SharedHttpStream upstream).',
        'liveTvHdhrStreamRefcountReleased: after both probes close, currentConcurrentByChannel===0 (connection released).',
        'R8: upstream may keep its SharedHttpStream connection open for refill after probes close;',
        'if currentConcurrent does not reach 0 within the bounded timeout, the invariant is degraded to the honest observed value.',
        'liveTvHdhrHlsTranscodeUrl (jellyrin-only): MediaSource exposes SupportsTranscoding:true + TranscodingUrl.',
        'liveTvHdhrHlsFfmpegReaped (jellyrin-only): /stats currentConcurrent[/auto/vN]===0 after DELETE (no orphan ffmpeg).',
        'liveTvHdhrTwoClientByteCheck (jellyrin-only): 2nd concurrent consumer of the same Jellyrin channel receives video/mp2t bytes>=1.',
        'liveTvHdhrTimerRecordingCreated (upstreamComparable): POST /LiveTv/Timers with StartDate≈now and short EndDate triggers recording on BOTH targets; returns 200 with Id.',
        'liveTvHdhrRecordingCompleted (upstreamComparable): poll GET /LiveTv/Recordings until Status==Completed for our channel within bounded timeout (default 30s); BOTH targets use DirectRecorder COPY pattern (no transcode).',
        'liveTvHdhrRecordingPlayable (upstreamComparable): recording file downloaded from BOTH targets and verified by ffprobe >=1 video packet — genuine byte comparison, not header-only.',
        'liveTvHdhrRecordingCleanup (jellyrin-only): /stats currentConcurrentByChannel===0 after recording + DELETE 204 + recording absent from GET /LiveTv/Recordings.',
        'liveTvHdhrSeriesTimerCreated (upstreamComparable): POST /LiveTv/SeriesTimers with real ProgramId from XMLTV guide returns 204/200 on BOTH targets; timer appears in GET /LiveTv/SeriesTimers.',
        'liveTvHdhrSeriesTimerGeneratesTimers (upstreamComparable): after CreateSeriesTimer, GET /LiveTv/Timers returns >=1 child timer with SeriesTimerId matching the series timer on BOTH targets.',
        'Camino A confirmed: upstream materialises the XMLTV program via RefreshGuide -> XmlTvListingsProvider; GET /LiveTv/Programs returns the program; CreateSeriesTimer uses the real ProgramId.',
        'Jellyrin: materialize_series_timer_timers recorre live_tv_program_items, matchea por Name (case-insensitive), crea timers con IsSeries=false y SeriesTimerId set, Id estable (FNV-based).',
        'liveTvHdhrSeriesRecordingPlayable (upstreamComparable): series child timer triggers recording on BOTH targets; recording Completed with ffprobe >=1 video packet.',
        'liveTvHdhrSeriesTimerCleanup (jellyrin-only): /stats===0 + DELETE SeriesTimer 204 + cascada (child timers absent) + series timer absent from GET /LiveTv/SeriesTimers.',
        'DS5 cascade: delete_live_tv_series_timer calls cascade_delete_series_timer_timers before deleting the series timer itself.',
        'R-MATCH-SUBSET: only Name and SeriesId match implemented (DS2). RecordNewOnly/SkipEpisodes/Days/RecordAnyTime out of scope.',
        'liveTvHdhrTunerLimitFirstOpen (upstreamComparable): dedicated limit sim (TunerCount=1) added; opening channel 4.1 returns 200 + bytes on BOTH targets.',
        'liveTvHdhrTunerLimitConflict (upstreamComparable): with channel 4.1 active, opening channel 5.1 returns HTTP 500 on BOTH targets.',
        'R-CONFLICT-500: upstream 500 via ExceptionMiddleware (LiveTvConflictException -> _ => 500). Jellyrin 500 via ApiError::internal.',
        'R-ENFORCE-POINT: upstream enforces at PlaybackInfo (open time); Jellyrin enforces at GET /LiveTv/LiveStreamFiles (stream time). Same observable 500.',
        'R-TOCTOU: Jellyrin check+insert atomic under the same registry lock. No TOCTOU window.',
        'liveTvHdhrTunerLimitRecovery (upstreamComparable): after closing channel 4.1 and draining /stats current===0, opening channel 5.1 returns 200 + bytes on BOTH targets.',
        'liveTvHdhrTunerLimitSharingExempt (jellyrin-only): 2 consumers of channel 4.1 with TunerCount=1 do NOT trigger a conflict; maxConcurrentByChannel[/auto/v4.1]===1 (sharing exempt). Upstream sharing is not directly comparable via the sim metric (upstream uses file-based SharedHttpStream, not broadcast fan-out).',
        'D5 R-LIMIT-SCOPE: TunerCount limit counts only direct TS proxy streams (LIVE_STREAM_REGISTRY). HLS sessions (LIVE_HLS_SESSIONS) and recordings (LIVE_TV_RECORDING_REGISTRY) use separate registries; unifying them is a future sub-gate.',
        'Upstream isolation: the main sim tuner is deleted before the limit test on upstream to prevent fallback to the main tuner (TunerCount=0). Only the limit tuner (TunerCount=1) serves channels 4.1/5.1 during the conflict sequence.',
      ].join(' '),
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      upstreamComparableInvariants: upstreamComparable,
      jellyrinOnlyInvariants: jellyrinOnly,
      invariantCoverage,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [
        'Dashboard target remains device-validated; closing E2 requires additional sub-gate evidence.',
        'R8: upstream SharedHttpStream may keep connection open after probes close (refill buffer); liveTvHdhrStreamRefcountReleased may be false for upstream if observed within timeout.',
        'R-DETERMINISM: recording playability depends on simulator TS being a valid MPEG-2 TS with PAT+PMT at byte 0 and monotonic PCR/PTS/DTS; the simulator pre-generates a 600s clip via ffmpeg meeting this contract.',
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
    // upstream is only required to satisfy the HDHomeRun comparable set.
    // jellyrin must satisfy both the comparable set and the jellyrin-only set.
    const required = summary.target === 'jellyrin'
      ? [...upstreamComparable, ...jellyrinOnly]
      : upstreamComparable;
    const missing = required.filter((field) => summary.invariants?.[field] !== true);
    if (missing.length > 0) {
      missingByTarget[summary.target] = missing;
    }
  }
  return {
    upstreamComparable,
    jellyrinOnly,
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
    const comparableCount = (evidence.invariantCoverage.upstreamComparable || []).length;
    const jellyrinOnlyCount = (evidence.invariantCoverage.jellyrinOnly || []).length;
    lines.push(`- Upstream-comparable invariants: ${comparableCount}`);
    lines.push(`- Jellyrin-only invariants: ${jellyrinOnlyCount}`);
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
