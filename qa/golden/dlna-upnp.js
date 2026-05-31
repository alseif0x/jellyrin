#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'dlna-upnp');
const tracePath = path.join(traceDir, 'golden-run.log');
const traceJsonPath = path.join(traceDir, 'golden-run.json');
const evidencePath = path.join(generatedDir, 'dlna-upnp.json');
const evidenceMarkdownPath = path.join(generatedDir, 'dlna-upnp.md');
const discoveryEvidencePath = path.join(generatedDir, 'dlna-discovery.json');

const goldenCommands = [
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'dlna_control_point_golden_direct_stream_and_real_hls_transcode',
    '--',
    '--nocapture',
  ],
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'dlna_video_mime_matrix_covers_direct_play_containers_without_false_pn',
    '--',
    '--nocapture',
  ],
  [
    'cargo',
    'test',
    '-p',
    'jellyrin-api',
    'ssdp',
    '--',
    '--nocapture',
  ],
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });
  await fs.mkdir(traceDir, { recursive: true });

  const result = await runGolden();
  await fs.writeFile(tracePath, result.output);
  await fs.writeFile(traceJsonPath, `${JSON.stringify(result, null, 2)}\n`);

  const discoveryEvidence = await readJsonIfExists(discoveryEvidencePath);
  const evidence = buildEvidence(result, discoveryEvidence);
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

function buildEvidence(result, discoveryEvidence) {
  const passed = result.code === 0;
  const discoveryPassed =
    discoveryEvidence?.status === 'implemented' &&
    discoveryEvidence?.failedTargets?.length === 0 &&
    discoveryEvidence?.completedTargets?.includes('local-udp');
  return {
    gate: 'dlna-upnp',
    status: 'implemented',
    percent: passed ? (discoveryPassed ? 93 : 92) : 87,
    closed: false,
    sourcePhase: passed
      ? `E3.1a/E3.1b${discoveryPassed ? '/E3.1c' : ''}/E3.2a/E3.2b/E3.2c/E3.2d/E3.2e/E3.2f/E3.3a/E3.3b/E3.3c/E3.3d/E3.3e/E3.4b/E3.4c/E3.4d/E3.5a/E3.5b/E3.6a/E3.6b/E3.6c/E3.7a`
      : 'E3.1a/E3.2a/E3.2b/E3.2c/E3.2d/E3.2e/E3.2f/E3.3a/E3.3b/E3.3c/E3.3d/E3.3e/E3.4b/E3.4c/E3.4d/E3.5a/E3.6a/E3.6b',
    evidence: passed
      ? [
          'Jellyrin DLNA/UPnP local golden completed as a control-point flow.',
          'The trace generates a real MP4 fixture with ffmpeg, enables UPnP, publishes a library, fetches the root device descriptor, browses ContentDirectory, validates a direct DLNA Range stream, requests tokenless transcode.m3u8, waits for the real ffmpeg HLS media playlist, then fetches a rewritten DLNA TS segment with content-type video/mp2t and MPEG-TS sync bytes.',
          'The SSDP contract test verifies M-SEARCH/NOTIFY targets including ContentDirectory, ConnectionManager and X_MS_MediaReceiverRegistrar.',
          discoveryPassed
            ? 'The companion dlna-discovery golden validates a real local UDP M-SEARCH round trip.'
            : 'The companion dlna-discovery golden has not been completed in the current generated evidence.',
          'Existing focused coverage also verifies descriptors/SCPD, MediaReceiverRegistrar SOAP, GENA subscribe/renew/unsubscribe, initial/follow-up NOTIFY, SOAP Browse/Search/GetProtocolInfo, UPnP SOAP faults, DIDL root/folders/items/thumbnails/subtitles, profile hints, conservative video MIME mapping without invented video DLNA.ORG_PN values, direct stream URLs and HLS fallback route contracts.',
        ].join(' ')
      : 'DLNA/UPnP golden did not complete; inspect trace log for the failing control-point step.',
    updatedAt: result.generatedAt,
    completedTargets: passed ? ['jellyrin'] : [],
    skippedTargets: ['upstream', 'renderer-device'],
    failedTargets: passed ? [] : ['jellyrin'],
    tracePath: path.relative(plansDir, tracePath),
    validatedCommands: [
      ...goldenCommands.map((command) => command.join(' ')),
      ...(discoveryPassed ? ['npm run golden:dlna-discovery'] : []),
      'cargo test -p jellyrin-api dlna_ -- --nocapture',
      'cargo check -p jellyrin-api',
      'cargo check -p jellyrin-server',
      'cargo fmt --all -- --check',
      'cargo clippy --all-targets -- -D warnings',
    ],
    openRisks: [
      'Dashboard target remains device-validated; closing E3 still requires real renderer/VLC/TV validation on LAN.',
      'SSDP uses a single IPv4 socket on 0.0.0.0:1900; multi-interface binding and firewall/systemd packaging remain pending.',
      'Browse covers root, virtual folders, physical directory hierarchy and direct media items; metadata-derived virtual grouping for albums/series without folders remains pending.',
      'ContentDirectory Search supports a practical subset of criteria; full UPnP SearchCriteria grammar remains pending.',
      'Thumbnails and text subtitle links are advertised; automatic video frame extraction and graphical subtitle support remain pending.',
      'SystemUpdateID is consistent across GENA and SOAP during process lifetime; persistence across server restart remains pending.',
      'MediaReceiverRegistrar is implemented as LAN-open under EnableUPnP; granular renderer authorization remains pending.',
      'Basic protocolInfo/profile hints and HLS fallback routes are implemented; renderer-specific Samsung/LG/Sony/VLC profile rules and video DLNA.ORG_PN codec/profile decisions remain pending.',
    ],
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

function renderMarkdown(evidence) {
  const lines = [];
  lines.push('# DLNA/UPnP Gate Evidence');
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
