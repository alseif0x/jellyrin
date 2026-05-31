#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');

const execFileAsync = promisify(execFile);
const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_DLNA_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'dlna-upnp');
const defaultOutputDir = path.join(manualEvidenceDir, 'fixture');

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const outputDir = path.resolve(options.outputDir || process.env.JELLYRIN_DLNA_FIXTURE_DIR || defaultOutputDir);
  await fs.mkdir(outputDir, { recursive: true });

  const subtitlePath = path.join(outputDir, 'E3-DLNA-Manual-Fixture.srt');
  const videoPath = path.join(outputDir, 'E3-DLNA-Manual-Fixture.mkv');
  const nfoPath = path.join(outputDir, 'E3-DLNA-Manual-Fixture.nfo');
  const runbookPath = path.join(outputDir, 'RUNBOOK.md');

  await fs.writeFile(
    subtitlePath,
    [
      '1',
      '00:00:00,000 --> 00:00:02,000',
      'Jellyrin E3 DLNA subtitle probe',
      '',
      '2',
      '00:00:02,000 --> 00:00:04,000',
      'Browse, thumbnail, subtitle and playback validation',
      '',
    ].join('\n'),
  );

  await createVideoFixture(videoPath, subtitlePath);
  await fs.writeFile(nfoPath, nfoMetadata());
  await fs.writeFile(runbookPath, runbook(outputDir, videoPath));

  const probe = await ffprobe(videoPath);
  const subtitleStream = probe.streams.find((stream) => stream.codec_type === 'subtitle');
  if (!subtitleStream) {
    throw new Error('fixture must contain an embedded subtitle stream');
  }

  console.log(JSON.stringify({
    status: 'ready',
    outputDir,
    mediaPath: videoPath,
    subtitlePath,
    nfoPath,
    runbookPath,
    expectedSubtitleIndex: subtitleStream.index,
    streams: probe.streams.map((stream) => ({
      index: stream.index,
      type: stream.codec_type,
      codec: stream.codec_name,
    })),
    nextStep: 'Add outputDir as a movies library in Jellyrin, then run golden:dlna:device-preflight with --item-id and --subtitle-index.',
  }, null, 2));
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === '--out-dir') {
      options.outputDir = args[index + 1];
      index += 1;
    } else if (arg === '--help' || arg === '-h') {
      console.log('Usage: node qa/golden/dlna-device-fixture.js [--out-dir <directory>]');
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

async function createVideoFixture(videoPath, subtitlePath) {
  const args = [
    '-hide_banner',
    '-loglevel',
    'error',
    '-nostdin',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc=size=640x360:rate=24',
    '-f',
    'lavfi',
    '-i',
    'sine=frequency=880:sample_rate=48000',
    '-i',
    subtitlePath,
    '-t',
    '8',
    '-map',
    '0:v:0',
    '-map',
    '1:a:0',
    '-map',
    '2:s:0',
    '-metadata',
    'title=E3 DLNA Manual Fixture',
    '-c:v',
    'libx264',
    '-pix_fmt',
    'yuv420p',
    '-profile:v',
    'main',
    '-level',
    '3.1',
    '-c:a',
    'aac',
    '-b:a',
    '128k',
    '-c:s',
    'srt',
    videoPath,
  ];
  try {
    await execFileAsync('ffmpeg', args, { cwd: repoRoot, maxBuffer: 1024 * 1024 });
  } catch (error) {
    throw new Error(`ffmpeg failed to create DLNA fixture: ${error.stderr || error.message}`);
  }
}

async function ffprobe(videoPath) {
  const { stdout } = await execFileAsync(
    'ffprobe',
    [
      '-hide_banner',
      '-loglevel',
      'error',
      '-print_format',
      'json',
      '-show_streams',
      videoPath,
    ],
    { cwd: repoRoot, maxBuffer: 1024 * 1024 },
  );
  return JSON.parse(stdout);
}

function nfoMetadata() {
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    '<movie>',
    '  <title>E3 DLNA Manual Fixture</title>',
    '  <sorttitle>E3 DLNA Manual Fixture</sorttitle>',
    '  <plot>Manual DLNA renderer validation fixture for Jellyrin E3.</plot>',
    '  <year>2026</year>',
    '  <genre>Test</genre>',
    '  <studio>Jellyrin QA</studio>',
    '</movie>',
    '',
  ].join('\n');
}

function runbook(outputDir, videoPath) {
  return [
    '# E3 DLNA Manual Fixture',
    '',
    'Use this fixture for the final real renderer/control-point validation.',
    '',
    '1. Add this directory as a Jellyrin movies library:',
    '',
    '```bash',
    `curl -X POST 'http://<server-lan-ip>:8097/Library/VirtualFolders?name=E3DLNA&collectionType=movies&paths=${encodeURIComponent(outputDir)}' -H 'X-Emby-Token: <admin-token>'`,
    '```',
    '',
    '2. Find the item id and subtitle index:',
    '',
    '```bash',
    `curl 'http://<server-lan-ip>:8097/Items?Recursive=true&IncludeItemTypes=Movie&SearchTerm=E3%20DLNA%20Manual%20Fixture&Fields=MediaSources,MediaStreams' -H 'X-Emby-Token: <admin-token>'`,
    '```',
    '',
    'The generated media is expected to expose subtitle stream index `2`.',
    '',
    '3. Run server-side readiness before the device test:',
    '',
    '```bash',
    'npm run golden:dlna:device-preflight -- --base-url http://<server-lan-ip>:8097 --item-id <item-id> --subtitle-index 2',
    '```',
    '',
    '4. From VLC, TV, or a UPnP control point on the same LAN, discover Jellyrin, browse to `E3 DLNA Manual Fixture`, fetch/play it for at least 10 seconds, confirm thumbnail and subtitle availability, then fill a JSON evidence file under `plans/manual/dlna-upnp`.',
    '',
    `Generated media path: \`${videoPath}\``,
    '',
  ].join('\n');
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error.message || error);
    process.exit(1);
  });
}

module.exports = {
  createVideoFixture,
  ffprobe,
  nfoMetadata,
  parseArgs,
};
