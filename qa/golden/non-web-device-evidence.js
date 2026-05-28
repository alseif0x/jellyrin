#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_NON_WEB_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'non-web-clients');
const templatePath = path.join(manualEvidenceDir, 'template.json');

const clientProfiles = [
  {
    id: 'mpv-shim',
    name: 'MPV Shim / Jellyfin Media Player',
    priority: 'P1',
    validationTarget: 'manual-repeatable',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mkv', 'webm', 'mov', 'avi', 'ts'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt'],
    knownQuirks: ['desktop clients vary DeviceProfile fields by mpv/libmpv build'],
  },
  {
    id: 'kodi',
    name: 'Kodi plugin',
    priority: 'P2',
    validationTarget: 'contract-simulated',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mkv', 'webm', 'mov', 'avi', 'ts'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt', 'ass', 'ssa', 'sub'],
    knownQuirks: ['Kodi-specific sync queue behavior needs later real plugin validation'],
  },
  {
    id: 'android-tv',
    name: 'Jellyfin Android TV',
    priority: 'P1',
    validationTarget: 'contract-simulated',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mkv', 'webm'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt'],
    knownQuirks: ['TV device codec support varies by hardware generation'],
  },
  {
    id: 'android-mobile',
    name: 'Jellyfin Android mobile',
    priority: 'P1',
    validationTarget: 'contract-simulated',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mkv', 'webm'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt'],
    knownQuirks: ['external player handoff may change playback progress cadence'],
  },
  {
    id: 'swiftfin',
    name: 'Swiftfin / iOS',
    priority: 'P1',
    validationTarget: 'contract-simulated',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mov'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt'],
    knownQuirks: ['Apple platform playback is sensitive to HLS/container combinations'],
  },
  {
    id: 'roku',
    name: 'Jellyfin Roku',
    priority: 'P2',
    validationTarget: 'contract-simulated',
    authMethod: 'AuthenticateByName with MediaBrowser authorization',
    discoveryMethod: '/System/Info',
    playbackModes: ['direct-play', 'direct-stream', 'hls-transcode'],
    directPlayFormats: ['mp4', 'm4v', 'mov'],
    transcodeRequirements: ['hls', 'h264', 'aac'],
    subtitleRequirements: ['srt', 'vtt'],
    knownQuirks: ['Roku firmware media capability differences require real-device evidence before closure'],
  },
];

const requiredProfiles = clientProfiles.map((profile) => profile.id);
const requiredFlowChecks = [
  'discovery',
  'login',
  'browse',
  'itemDetail',
  'playbackInfo',
  'playbackStarted',
  'progressReported',
  'resumeVisible',
  'logout',
];

async function loadManualDeviceEvidence() {
  await ensureTemplate();
  let entries = [];
  try {
    entries = await fs.readdir(manualEvidenceDir, { withFileTypes: true });
  } catch (error) {
    if (error.code === 'ENOENT') {
      return {
        directory: manualEvidenceDir,
        templatePath,
        valid: [],
        invalid: [],
      };
    }
    throw error;
  }

  const files = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith('.json') && entry.name !== 'template.json')
    .map((entry) => path.join(manualEvidenceDir, entry.name))
    .sort();

  const valid = [];
  const invalid = [];
  for (const file of files) {
    try {
      const evidence = JSON.parse(await fs.readFile(file, 'utf8'));
      const errors = validateManualEvidence(evidence);
      const result = {
        file,
        relativePath: path.relative(plansDir, file),
        evidence,
        errors,
      };
      if (errors.length > 0) {
        invalid.push(result);
      } else {
        valid.push(result);
      }
    } catch (error) {
      invalid.push({
        file,
        relativePath: path.relative(plansDir, file),
        evidence: null,
        errors: [`invalid JSON: ${error.message}`],
      });
    }
  }

  return {
    directory: manualEvidenceDir,
    templatePath,
    valid,
    invalid,
  };
}

async function ensureTemplate() {
  await fs.mkdir(manualEvidenceDir, { recursive: true });
  try {
    await fs.access(templatePath);
  } catch (error) {
    if (error.code !== 'ENOENT') {
      throw error;
    }
    await fs.writeFile(templatePath, `${JSON.stringify(manualEvidenceTemplate(), null, 2)}\n`);
  }
}

function validateManualEvidence(evidence) {
  const errors = [];
  if (!evidence || typeof evidence !== 'object' || Array.isArray(evidence)) {
    return ['evidence must be a JSON object'];
  }
  requireString(errors, evidence, 'clientId');
  if (evidence.clientId && !requiredProfiles.includes(evidence.clientId)) {
    errors.push(`clientId must be one of ${requiredProfiles.join(', ')}`);
  }
  for (const field of ['clientName', 'clientVersion', 'deviceName', 'platform', 'testedAt', 'tester', 'jellyrinBaseUrl']) {
    requireString(errors, evidence, field);
  }
  if (evidence.testedAt && Number.isNaN(Date.parse(evidence.testedAt))) {
    errors.push('testedAt must be an ISO-compatible date string');
  }
  if (evidence.result !== 'pass') {
    errors.push('result must be "pass"');
  }
  if (!evidence.server || typeof evidence.server !== 'object' || Array.isArray(evidence.server)) {
    errors.push('server must be an object');
  } else {
    for (const field of ['commit', 'version']) {
      requireString(errors, evidence.server, `server.${field}`, field);
    }
  }
  if (!evidence.media || typeof evidence.media !== 'object' || Array.isArray(evidence.media)) {
    errors.push('media must be an object');
  } else {
    for (const field of ['itemId', 'itemName', 'playMethod']) {
      requireString(errors, evidence.media, `media.${field}`, field);
    }
    const streamStatus = Number(evidence.media.streamStatus);
    if (![200, 206].includes(streamStatus)) {
      errors.push('media.streamStatus must be 200 or 206');
    }
  }
  if (!evidence.flow || typeof evidence.flow !== 'object' || Array.isArray(evidence.flow)) {
    errors.push('flow must be an object');
  } else {
    for (const check of requiredFlowChecks) {
      if (evidence.flow[check] !== true) {
        errors.push(`flow.${check} must be true`);
      }
    }
  }
  if (!Array.isArray(evidence.artifacts) || evidence.artifacts.length === 0) {
    errors.push('artifacts must contain at least one capture or log reference');
  } else {
    evidence.artifacts.forEach((artifact, index) => {
      if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) {
        errors.push(`artifacts[${index}] must be an object`);
        return;
      }
      requireString(errors, artifact, `artifacts[${index}].type`, 'type');
      requireString(errors, artifact, `artifacts[${index}].pathOrUrl`, 'pathOrUrl');
    });
  }
  return errors;
}

function requireString(errors, value, label, key = label) {
  if (typeof value[key] !== 'string' || value[key].trim() === '') {
    errors.push(`${label} is required`);
  }
}

function manualEvidenceTemplate() {
  return {
    clientId: 'mpv-shim',
    clientName: 'Jellyfin MPV Shim',
    clientVersion: 'replace-with-client-version',
    deviceName: 'replace-with-device-name',
    platform: 'linux|windows|android|android-tv|ios|tvos|roku|kodi',
    testedAt: new Date().toISOString(),
    tester: 'replace-with-tester',
    jellyrinBaseUrl: 'http://replace-with-jellyrin-host:8097',
    result: 'pass',
    server: {
      version: 'replace-with-server-version',
      commit: 'replace-with-git-commit',
    },
    media: {
      itemId: 'replace-with-item-id',
      itemName: 'replace-with-item-name',
      playMethod: 'DirectPlay|DirectStream|Transcode',
      streamStatus: 200,
    },
    flow: Object.fromEntries(requiredFlowChecks.map((check) => [check, true])),
    artifacts: [
      {
        type: 'screenshot|client-log|server-log|screen-recording',
        pathOrUrl: 'manual/non-web-clients/artifacts/replace-with-file',
      },
    ],
    notes: 'Optional notes about playback, codecs, subtitles, or client-specific behavior.',
  };
}

function buildCompatibilityMatrix(completedProfiles, manualEvidence) {
  const completed = new Set(completedProfiles || []);
  const validEvidenceByClient = new Map();
  for (const entry of manualEvidence?.valid || []) {
    validEvidenceByClient.set(entry.evidence.clientId, entry);
  }
  const clients = clientProfiles.map((profile) => {
    const deviceEvidence = validEvidenceByClient.get(profile.id);
    const contractValidated = completed.has(profile.id);
    const validationLevel = deviceEvidence ? 'manual-repeatable' : contractValidated ? 'contract-simulated' : 'not-started';
    return {
      clientId: profile.id,
      clientName: profile.name,
      priority: profile.priority,
      version: deviceEvidence?.evidence.clientVersion || 'contract-profile',
      authMethod: profile.authMethod,
      discoveryMethod: profile.discoveryMethod,
      requiredEndpoints: [
        '/System/Info',
        '/Users/AuthenticateByName',
        '/Users/{UserId}/Views',
        '/Items/{ItemId}/PlaybackInfo',
        '/Videos/{ItemId}/stream.mp4',
        '/Sessions/Playing/Progress',
        '/UserItems/Resume',
        '/Sessions/Logout',
      ],
      requiredWebsocketMessages: [],
      requiredPlaybackModes: profile.playbackModes,
      directPlayFormats: profile.directPlayFormats,
      transcodeRequirements: profile.transcodeRequirements,
      subtitleRequirements: profile.subtitleRequirements,
      imageRequirements: ['primary image endpoints inherited from web/client base matrix'],
      knownQuirks: profile.knownQuirks,
      currentStatus: validationLevel,
      validationCommand: 'npm run golden:clients',
      manualEvidence: deviceEvidence?.relativePath || null,
    };
  });
  return {
    generatedAt: new Date().toISOString(),
    statusModel: ['not-started', 'contract-simulated', 'manual-repeatable'],
    hasBestEffort: clients.some((client) => client.currentStatus === 'best-effort'),
    clients,
  };
}

async function main() {
  const report = await loadManualDeviceEvidence();
  console.log(JSON.stringify({
    directory: report.directory,
    templatePath: report.templatePath,
    validCount: report.valid.length,
    invalidCount: report.invalid.length,
    valid: report.valid.map((entry) => ({
      clientId: entry.evidence.clientId,
      clientName: entry.evidence.clientName,
      clientVersion: entry.evidence.clientVersion,
      file: entry.relativePath,
    })),
    invalid: report.invalid.map((entry) => ({
      file: entry.relativePath,
      errors: entry.errors,
    })),
  }, null, 2));
  if (report.invalid.length > 0) {
    process.exitCode = 1;
  }
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}

module.exports = {
  clientProfiles,
  requiredProfiles,
  requiredFlowChecks,
  buildCompatibilityMatrix,
  loadManualDeviceEvidence,
  validateManualEvidence,
};
