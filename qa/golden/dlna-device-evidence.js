#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_DLNA_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'dlna-upnp');
const templatePath = path.join(manualEvidenceDir, 'template.json');

const requiredFlowChecks = [
  'serverVisibleInControlPoint',
  'rootDescriptorFetched',
  'iconListFetched',
  'contentDirectoryBrowse',
  'mediaItemVisible',
  'thumbnailFetched',
  'subtitleLinkResolved',
  'playbackStarted',
  'playbackStable',
  'streamUrlFetched',
];

const requiredNetworkChecks = [
  'sameLanAsServer',
  'ssdpDiscovery',
  'locationReachableFromDevice',
];

async function loadManualDlnaEvidence() {
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
      const errors = await validateManualDlnaEvidence(evidence);
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
    const existing = JSON.parse(await fs.readFile(templatePath, 'utf8'));
    if (existing.schema !== manualEvidenceTemplate().schema) {
      await fs.writeFile(templatePath, `${JSON.stringify(manualEvidenceTemplate(), null, 2)}\n`);
    }
  } catch (error) {
    if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) {
      throw error;
    }
    await fs.writeFile(templatePath, `${JSON.stringify(manualEvidenceTemplate(), null, 2)}\n`);
  }
}

async function validateManualDlnaEvidence(evidence) {
  const errors = [];
  if (!evidence || typeof evidence !== 'object' || Array.isArray(evidence)) {
    return ['evidence must be a JSON object'];
  }

  if (evidence.schema !== manualEvidenceTemplate().schema) {
    errors.push(`schema must be ${manualEvidenceTemplate().schema}`);
  }
  for (const field of ['deviceName', 'deviceType', 'controlPointName', 'controlPointVersion', 'testedAt', 'tester', 'jellyrinBaseUrl', 'result']) {
    requireString(errors, evidence, field);
  }
  if (evidence.testedAt && Number.isNaN(Date.parse(evidence.testedAt))) {
    errors.push('testedAt must be an ISO-compatible date string');
  }
  if (evidence.result !== 'pass') {
    errors.push('result must be "pass"');
  }
  requireUrl(errors, evidence, 'jellyrinBaseUrl');

  if (!evidence.server || typeof evidence.server !== 'object' || Array.isArray(evidence.server)) {
    errors.push('server must be an object');
  } else {
    for (const field of ['commit', 'version', 'serverId']) {
      requireString(errors, evidence.server, `server.${field}`, field);
    }
    if (typeof evidence.server?.commit === 'string' && !/^[0-9a-f]{7,40}$/i.test(evidence.server.commit)) {
      errors.push('server.commit must be a git commit hash');
    }
    if (
      typeof evidence.server?.serverId === 'string' &&
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(evidence.server.serverId)
    ) {
      errors.push('server.serverId must be a UUID');
    }
  }

  if (!evidence.network || typeof evidence.network !== 'object' || Array.isArray(evidence.network)) {
    errors.push('network must be an object');
  } else {
    for (const field of ['serverIp', 'deviceIp', 'ssdpLocation']) {
      requireString(errors, evidence.network, `network.${field}`, field);
    }
    requireUrl(errors, evidence.network, 'network.ssdpLocation', 'ssdpLocation');
    for (const check of requiredNetworkChecks) {
      if (evidence.network[check] !== true) {
        errors.push(`network.${check} must be true`);
      }
    }
  }

  if (!evidence.media || typeof evidence.media !== 'object' || Array.isArray(evidence.media)) {
    errors.push('media must be an object');
  } else {
    for (const field of ['itemId', 'itemName', 'container', 'playMethod']) {
      requireString(errors, evidence.media, `media.${field}`, field);
    }
    if (!['DirectPlay', 'DirectStream', 'HLS'].includes(evidence.media.playMethod)) {
      errors.push('media.playMethod must be DirectPlay, DirectStream, or HLS');
    }
    const streamStatus = Number(evidence.media.streamStatus);
    if (![200, 206].includes(streamStatus)) {
      errors.push('media.streamStatus must be 200 or 206');
    }
    const playbackSeconds = Number(evidence.media.playbackSeconds);
    if (!Number.isFinite(playbackSeconds) || playbackSeconds < 10) {
      errors.push('media.playbackSeconds must be at least 10');
    }
    for (const check of ['thumbnailStatusOk', 'subtitleStatusOk']) {
      if (evidence.media[check] !== true) {
        errors.push(`media.${check} must be true`);
      }
    }
    if (
      evidence.media.subtitleMime !== 'text/vtt' &&
      evidence.media.subtitleMime !== 'text/vtt; charset=utf-8'
    ) {
      errors.push('media.subtitleMime must be text/vtt or text/vtt; charset=utf-8');
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

  validateTranscodeFallback(errors, evidence.transcodeFallback);

  if (!Array.isArray(evidence.artifacts) || evidence.artifacts.length === 0) {
    errors.push('artifacts must contain at least one capture or log reference');
  } else {
    for (const [index, artifact] of evidence.artifacts.entries()) {
      if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) {
        errors.push(`artifacts[${index}] must be an object`);
        continue;
      }
      requireString(errors, artifact, `artifacts[${index}].type`, 'type');
      requireString(errors, artifact, `artifacts[${index}].pathOrUrl`, 'pathOrUrl');
      await requireExistingArtifact(errors, artifact.pathOrUrl, index);
    }
  }

  return errors;
}

function summarizeManualDlnaEvidence(report) {
  return {
    directory: report.directory,
    templatePath: report.templatePath,
    validCount: report.valid.length,
    invalidCount: report.invalid.length,
    validDevices: report.valid.map((entry) => ({
      deviceName: entry.evidence.deviceName,
      deviceType: entry.evidence.deviceType,
      controlPointName: entry.evidence.controlPointName,
      controlPointVersion: entry.evidence.controlPointVersion,
      testedAt: entry.evidence.testedAt,
      jellyrinBaseUrl: entry.evidence.jellyrinBaseUrl,
      playMethod: entry.evidence.media?.playMethod,
      transcodeFallback: entry.evidence.transcodeFallback?.profileRequiresTranscode === true,
      file: entry.relativePath,
    })),
    invalidFiles: report.invalid.map((entry) => ({
      file: entry.relativePath,
      errors: entry.errors,
    })),
  };
}

function manualEvidenceTemplate() {
  return {
    schema: 'jellyrin-dlna-device-evidence-v3',
    deviceName: 'replace-with-tv-vlc-or-renderer-name',
    deviceType: 'vlc|tv|console|upnp-control-point|renderer',
    controlPointName: 'VLC / TV media browser / BubbleUPnP / other',
    controlPointVersion: 'replace-with-version',
    testedAt: new Date().toISOString(),
    tester: 'replace-with-tester',
    jellyrinBaseUrl: 'http://replace-with-jellyrin-host:8097',
    result: 'pass',
    server: {
      version: 'replace-with-server-version',
      commit: 'replace-with-git-commit',
      serverId: 'replace-with-server-id',
    },
    network: {
      serverIp: 'replace-with-server-lan-ip',
      deviceIp: 'replace-with-device-lan-ip',
      ssdpLocation: 'http://replace-with-server-lan-ip:8097/dlna/{serverId}/description.xml',
      sameLanAsServer: true,
      ssdpDiscovery: true,
      locationReachableFromDevice: true,
    },
    media: {
      itemId: 'replace-with-item-id',
      itemName: 'replace-with-item-name',
      container: 'mp4',
      playMethod: 'HLS',
      streamStatus: 200,
      playbackSeconds: 30,
      thumbnailStatusOk: true,
      subtitleStatusOk: true,
      subtitleMime: 'text/vtt; charset=utf-8',
    },
    transcodeFallback: {
      profileRequiresTranscode: true,
      hlsPlaylistFetched: true,
      mediaPlaylistFetched: true,
      segmentFetched: true,
      segmentStatus: 200,
      segmentMime: 'video/mp2t',
      segmentBytesVerified: true,
    },
    flow: Object.fromEntries(requiredFlowChecks.map((check) => [check, true])),
    artifacts: [
      {
        type: 'screenshot|client-log|server-log|packet-capture|screen-recording',
        pathOrUrl: 'manual/dlna-upnp/artifacts/replace-with-file',
      },
    ],
    notes: 'Optional notes about renderer quirks, subtitles, image artwork, or transcoding behavior.',
  };
}

function requireString(errors, value, label, key = label) {
  if (typeof value[key] !== 'string' || value[key].trim() === '') {
    errors.push(`${label} is required`);
    return;
  }
  if (looksLikeTemplatePlaceholder(value[key])) {
    errors.push(`${label} still contains template placeholder text`);
  }
}

function requireUrl(errors, value, label, key = label) {
  if (typeof value[key] !== 'string' || value[key].trim() === '') {
    return;
  }
  try {
    const parsed = new URL(value[key]);
    if (!['http:', 'https:'].includes(parsed.protocol)) {
      errors.push(`${label} must be an HTTP URL`);
    }
  } catch {
    errors.push(`${label} must be a valid URL`);
  }
}

function validateTranscodeFallback(errors, transcodeFallback) {
  if (!transcodeFallback || typeof transcodeFallback !== 'object' || Array.isArray(transcodeFallback)) {
    errors.push('transcodeFallback must be an object');
    return;
  }
  for (const check of ['profileRequiresTranscode', 'hlsPlaylistFetched', 'mediaPlaylistFetched', 'segmentFetched', 'segmentBytesVerified']) {
    if (transcodeFallback[check] !== true) {
      errors.push(`transcodeFallback.${check} must be true`);
    }
  }
  const segmentStatus = Number(transcodeFallback.segmentStatus);
  if (![200, 206].includes(segmentStatus)) {
    errors.push('transcodeFallback.segmentStatus must be 200 or 206');
  }
  if (transcodeFallback.segmentMime !== 'video/mp2t') {
    errors.push('transcodeFallback.segmentMime must be video/mp2t');
  }
}

async function requireExistingArtifact(errors, pathOrUrl, index) {
  if (typeof pathOrUrl !== 'string' || pathOrUrl.trim() === '' || looksLikeTemplatePlaceholder(pathOrUrl)) {
    return;
  }
  if (/^https?:\/\//i.test(pathOrUrl)) {
    errors.push(`artifacts[${index}].pathOrUrl must be a local evidence file path, not an external URL`);
    return;
  }
  const artifactPath = path.isAbsolute(pathOrUrl) ? pathOrUrl : path.resolve(plansDir, pathOrUrl);
  try {
    const stat = await fs.stat(artifactPath);
    if (!stat.isFile()) {
      errors.push(`artifacts[${index}].pathOrUrl must point to a file`);
    }
  } catch {
    errors.push(`artifacts[${index}].pathOrUrl file does not exist: ${artifactPath}`);
  }
}

function looksLikeTemplatePlaceholder(value) {
  return /replace-with|\{serverId\}|\|/i.test(value);
}

async function main() {
  const report = await loadManualDlnaEvidence();
  console.log(JSON.stringify({
    directory: report.directory,
    templatePath: report.templatePath,
    validCount: report.valid.length,
    invalidCount: report.invalid.length,
    valid: report.valid.map((entry) => ({
      deviceName: entry.evidence.deviceName,
      controlPointName: entry.evidence.controlPointName,
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
  loadManualDlnaEvidence,
  manualEvidenceTemplate,
  requiredFlowChecks,
  requiredNetworkChecks,
  summarizeManualDlnaEvidence,
  validateManualDlnaEvidence,
};
