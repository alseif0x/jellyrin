#!/usr/bin/env node

const fs = require('node:fs/promises');
const net = require('node:net');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');

const execFileAsync = promisify(execFile);
const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_DLNA_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'dlna-upnp');
const templatePath = path.join(manualEvidenceDir, 'template.json');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');
const allowedDeviceTypes = ['vlc', 'tv', 'console', 'upnp-control-point', 'renderer'];
const allowedArtifactTypes = ['screenshot', 'client-log', 'server-log', 'packet-capture', 'screen-recording'];
const manualEvidenceMaxAgeMs = 30 * 24 * 60 * 60 * 1000;
const artifactExtensionsByType = {
  screenshot: ['.png', '.jpg', '.jpeg', '.webp'],
  'client-log': ['.json', '.log', '.txt', '.har'],
  'server-log': ['.json', '.log', '.txt'],
  'packet-capture': ['.pcap', '.pcapng'],
  'screen-recording': ['.mp4', '.mov', '.mkv', '.webm'],
};
let currentCommitCache = null;

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
  validateTestedAt(errors, evidence.testedAt);
  if (evidence.result !== 'pass') {
    errors.push('result must be "pass"');
  }
  if (typeof evidence.deviceType === 'string' && !allowedDeviceTypes.includes(evidence.deviceType)) {
    errors.push(`deviceType must be one of: ${allowedDeviceTypes.join(', ')}`);
  }
  requireUrl(errors, evidence, 'jellyrinBaseUrl');
  requireLanLocation(errors, evidence.jellyrinBaseUrl, 'jellyrinBaseUrl');

  if (!evidence.server || typeof evidence.server !== 'object' || Array.isArray(evidence.server)) {
    errors.push('server must be an object');
  } else {
    for (const field of ['commit', 'version', 'serverId']) {
      requireString(errors, evidence.server, `server.${field}`, field);
    }
    if (typeof evidence.server?.commit === 'string' && !/^[0-9a-f]{7,40}$/i.test(evidence.server.commit)) {
      errors.push('server.commit must be a git commit hash');
    }
    if (typeof evidence.server?.commit === 'string' && /^[0-9a-f]{7,40}$/i.test(evidence.server.commit)) {
      const currentCommit = await gitCommit();
      if (currentCommit !== evidence.server.commit) {
        errors.push(`server.commit must match current git HEAD ${currentCommit}`);
      }
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
    requireLanIpv4(errors, evidence.network.serverIp, 'network.serverIp');
    requireLanIpv4(errors, evidence.network.deviceIp, 'network.deviceIp');
    requireDistinctIpv4(errors, evidence.network.serverIp, evidence.network.deviceIp, 'network.serverIp', 'network.deviceIp');
    requireUrl(errors, evidence.network, 'network.ssdpLocation', 'ssdpLocation');
    requireLanLocation(errors, evidence.network.ssdpLocation, 'network.ssdpLocation');
    requireLocationHostMatchesIp(errors, evidence.network.ssdpLocation, evidence.network.serverIp, 'network.ssdpLocation', 'network.serverIp');
    requireLocationContainsServerId(errors, evidence.network.ssdpLocation, evidence.server?.serverId, 'network.ssdpLocation', 'server.serverId');
    requireLocationHostMatchesIp(errors, evidence.jellyrinBaseUrl, evidence.network.serverIp, 'jellyrinBaseUrl', 'network.serverIp');
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
      if (typeof artifact.type === 'string' && !allowedArtifactTypes.includes(artifact.type)) {
        errors.push(`artifacts[${index}].type must be one of: ${allowedArtifactTypes.join(', ')}`);
      }
      await requireExistingArtifact(errors, artifact, index);
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

function requireLanIpv4(errors, value, label) {
  if (typeof value !== 'string' || value.trim() === '' || looksLikeTemplatePlaceholder(value)) {
    return;
  }
  if (net.isIP(value) !== 4) {
    errors.push(`${label} must be an IPv4 address`);
    return;
  }
  const octets = value.split('.').map((part) => Number(part));
  const [first, second, third, fourth] = octets;
  if (
    first === 0 ||
    first === 127 ||
    first >= 224 ||
    value === '255.255.255.255' ||
    (first === 169 && second === 254) ||
    [first, second, third, fourth].some((octet) => !Number.isInteger(octet) || octet < 0 || octet > 255)
  ) {
    errors.push(`${label} must be a LAN-reachable IPv4 address, not loopback, link-local, multicast, broadcast or unspecified`);
    return;
  }
  if (!isPrivateLanIpv4(first, second)) {
    errors.push(`${label} must be a private LAN IPv4 address (10/8, 172.16/12, or 192.168/16)`);
  }
}

function isPrivateLanIpv4(first, second) {
  return first === 10 || (first === 172 && second >= 16 && second <= 31) || (first === 192 && second === 168);
}

function requireDistinctIpv4(errors, firstIp, secondIp, firstLabel, secondLabel) {
  if (
    typeof firstIp !== 'string' ||
    typeof secondIp !== 'string' ||
    looksLikeTemplatePlaceholder(firstIp) ||
    looksLikeTemplatePlaceholder(secondIp)
  ) {
    return;
  }
  if (net.isIP(firstIp) === 4 && net.isIP(secondIp) === 4 && firstIp === secondIp) {
    errors.push(`${secondLabel} must be a different LAN device than ${firstLabel}`);
  }
}

function requireLanLocation(errors, value, label) {
  if (typeof value !== 'string' || value.trim() === '' || looksLikeTemplatePlaceholder(value)) {
    return;
  }
  try {
    const parsed = new URL(value);
    const host = parsed.hostname;
    if (net.isIP(host) === 4) {
      requireLanIpv4(errors, host, `${label} host`);
    }
  } catch {
    // requireUrl reports the URL shape error.
  }
}

function requireLocationHostMatchesIp(errors, location, serverIp, locationLabel, serverIpLabel) {
  if (
    typeof location !== 'string' ||
    typeof serverIp !== 'string' ||
    looksLikeTemplatePlaceholder(location) ||
    looksLikeTemplatePlaceholder(serverIp)
  ) {
    return;
  }
  try {
    const host = new URL(location).hostname;
    if (net.isIP(host) === 4 && net.isIP(serverIp) === 4 && host !== serverIp) {
      errors.push(`${locationLabel} host must match ${serverIpLabel}`);
    }
  } catch {
    // requireUrl reports the URL shape error.
  }
}

function requireLocationContainsServerId(errors, location, serverId, locationLabel, serverIdLabel) {
  if (
    typeof location !== 'string' ||
    typeof serverId !== 'string' ||
    looksLikeTemplatePlaceholder(location) ||
    looksLikeTemplatePlaceholder(serverId)
  ) {
    return;
  }
  try {
    const pathname = new URL(location).pathname.toLowerCase();
    if (!pathname.includes(serverId.toLowerCase())) {
      errors.push(`${locationLabel} path must include ${serverIdLabel}`);
    }
  } catch {
    // requireUrl reports the URL shape error.
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

function validateTestedAt(errors, testedAt) {
  if (typeof testedAt !== 'string' || testedAt.trim() === '' || looksLikeTemplatePlaceholder(testedAt)) {
    return;
  }
  const timestamp = Date.parse(testedAt);
  if (Number.isNaN(timestamp)) {
    errors.push('testedAt must be an ISO-compatible date string');
    return;
  }
  const now = Date.now();
  if (timestamp > now + 5 * 60 * 1000) {
    errors.push('testedAt must not be in the future');
  }
  if (now - timestamp > manualEvidenceMaxAgeMs) {
    errors.push('testedAt must be within the last 30 days');
  }
}

async function requireExistingArtifact(errors, artifact, index) {
  const pathOrUrl = artifact.pathOrUrl;
  if (typeof pathOrUrl !== 'string' || pathOrUrl.trim() === '' || looksLikeTemplatePlaceholder(pathOrUrl)) {
    return;
  }
  if (/^https?:\/\//i.test(pathOrUrl)) {
    errors.push(`artifacts[${index}].pathOrUrl must be a local evidence file path, not an external URL`);
    return;
  }
  const artifactPath = path.isAbsolute(pathOrUrl) ? pathOrUrl : path.resolve(plansDir, pathOrUrl);
  const relativeToManualEvidence = path.relative(manualEvidenceDir, artifactPath);
  if (relativeToManualEvidence.startsWith('..') || path.isAbsolute(relativeToManualEvidence)) {
    errors.push(`artifacts[${index}].pathOrUrl must stay under ${manualEvidenceDir}`);
    return;
  }
  const relativeToArtifacts = path.relative(artifactsDir, artifactPath);
  if (relativeToArtifacts.startsWith('..') || path.isAbsolute(relativeToArtifacts)) {
    errors.push(`artifacts[${index}].pathOrUrl must stay under ${artifactsDir}`);
    return;
  }
  requireArtifactExtension(errors, artifact.type, artifactPath, index);
  try {
    const realArtifactsDir = await fs.realpath(artifactsDir);
    const realArtifactPath = await fs.realpath(artifactPath);
    const realRelativeToArtifacts = path.relative(realArtifactsDir, realArtifactPath);
    if (realRelativeToArtifacts.startsWith('..') || path.isAbsolute(realRelativeToArtifacts)) {
      errors.push(`artifacts[${index}].pathOrUrl real path must stay under ${artifactsDir}`);
      return;
    }
    const stat = await fs.stat(artifactPath);
    if (!stat.isFile()) {
      errors.push(`artifacts[${index}].pathOrUrl must point to a file`);
    } else if (stat.size === 0) {
      errors.push(`artifacts[${index}].pathOrUrl file must not be empty`);
    }
  } catch {
    errors.push(`artifacts[${index}].pathOrUrl file does not exist: ${artifactPath}`);
  }
}

function requireArtifactExtension(errors, type, artifactPath, index) {
  if (typeof type !== 'string' || !artifactExtensionsByType[type]) {
    return;
  }
  const extension = path.extname(artifactPath).toLowerCase();
  if (!artifactExtensionsByType[type].includes(extension)) {
    errors.push(`artifacts[${index}].pathOrUrl extension must match ${type}: ${artifactExtensionsByType[type].join(', ')}`);
  }
}

function looksLikeTemplatePlaceholder(value) {
  return /replace-with|\{serverId\}|\|/i.test(value);
}

async function gitCommit() {
  if (!currentCommitCache) {
    const { stdout } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: repoRoot });
    currentCommitCache = stdout.trim();
  }
  return currentCommitCache;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return;
  }
  if (options.selfTest) {
    await selfTest();
    return;
  }
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

function parseArgs(args) {
  const options = {};
  for (const arg of args) {
    if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else if (arg === '--self-test') {
      options.selfTest = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

function printUsage() {
  console.log('Usage: node qa/golden/dlna-device-evidence.js [--self-test]');
}

async function selfTest() {
  const artifactPath = path.join(manualEvidenceDir, 'artifacts', 'self-test-artifact.txt');
  const emptyArtifactPath = path.join(manualEvidenceDir, 'artifacts', 'empty-self-test-artifact.txt');
  await fs.mkdir(path.dirname(artifactPath), { recursive: true });
  await fs.writeFile(artifactPath, 'self-test\n');
  try {
    const valid = manualEvidenceTemplate();
    valid.deviceName = 'VLC laptop';
    valid.deviceType = 'vlc';
    valid.controlPointName = 'VLC';
    valid.controlPointVersion = '3.0.21';
    valid.tester = 'qa';
    valid.jellyrinBaseUrl = 'http://192.168.1.46:8097';
    valid.server.version = 'dev';
    valid.server.commit = await gitCommit();
    valid.server.serverId = '58deb718-f9ee-4ac5-a1d4-05286d64cf42';
    valid.network.serverIp = '192.168.1.46';
    valid.network.deviceIp = '192.168.1.50';
    valid.network.ssdpLocation = 'http://192.168.1.46:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml';
    valid.media.itemId = 'fixture-item-id';
    valid.media.itemName = 'E3 DLNA Manual Fixture';
    valid.media.container = 'mkv';
    valid.artifacts = [{
      type: 'client-log',
      pathOrUrl: path.relative(plansDir, artifactPath),
    }];

    const validErrors = await validateManualDlnaEvidence(valid);
    if (validErrors.length > 0) {
      throw new Error(`valid fixture evidence failed self-test:\n${validErrors.join('\n')}`);
    }

    const invalidDeviceType = { ...valid, deviceType: 'replace-with-device' };
    await assertInvalid(invalidDeviceType, 'deviceType still contains template placeholder text');

    const invalidArtifactType = {
      ...valid,
      artifacts: [{ type: 'other', pathOrUrl: path.relative(plansDir, artifactPath) }],
    };
    await assertInvalid(invalidArtifactType, 'artifacts[0].type must be one of');

    const mismatchedArtifactExtension = {
      ...valid,
      artifacts: [{ type: 'screenshot', pathOrUrl: path.relative(plansDir, artifactPath) }],
    };
    await assertInvalid(mismatchedArtifactExtension, 'artifacts[0].pathOrUrl extension must match screenshot');

    const externalArtifact = {
      ...valid,
      artifacts: [{ type: 'client-log', pathOrUrl: 'https://example.invalid/capture.log' }],
    };
    await assertInvalid(externalArtifact, 'artifacts[0].pathOrUrl must be a local evidence file path');

    const staleCommit = {
      ...valid,
      server: { ...valid.server, commit: '0123456789abcdef0123456789abcdef01234567' },
    };
    await assertInvalid(staleCommit, 'server.commit must match current git HEAD');

    const futureTestedAt = {
      ...valid,
      testedAt: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
    };
    await assertInvalid(futureTestedAt, 'testedAt must not be in the future');

    const oldTestedAt = {
      ...valid,
      testedAt: '2020-01-01T00:00:00.000Z',
    };
    await assertInvalid(oldTestedAt, 'testedAt must be within the last 30 days');

    const loopbackBaseUrl = {
      ...valid,
      jellyrinBaseUrl: 'http://127.0.0.1:8097',
    };
    await assertInvalid(loopbackBaseUrl, 'jellyrinBaseUrl host must be a LAN-reachable IPv4 address');

    const mismatchedBaseUrl = {
      ...valid,
      jellyrinBaseUrl: 'http://192.168.1.99:8097',
    };
    await assertInvalid(mismatchedBaseUrl, 'jellyrinBaseUrl host must match network.serverIp');

    const loopbackServerIp = {
      ...valid,
      network: { ...valid.network, serverIp: '127.0.0.1' },
    };
    await assertInvalid(loopbackServerIp, 'network.serverIp must be a LAN-reachable IPv4 address');

    const invalidDeviceIp = {
      ...valid,
      network: { ...valid.network, deviceIp: 'not-an-ip' },
    };
    await assertInvalid(invalidDeviceIp, 'network.deviceIp must be an IPv4 address');

    const publicServerIp = {
      ...valid,
      jellyrinBaseUrl: 'http://8.8.8.8:8097',
      network: {
        ...valid.network,
        serverIp: '8.8.8.8',
        ssdpLocation: 'http://8.8.8.8:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml',
      },
    };
    await assertInvalid(publicServerIp, 'network.serverIp must be a private LAN IPv4 address');

    const publicDeviceIp = {
      ...valid,
      network: { ...valid.network, deviceIp: '8.8.8.8' },
    };
    await assertInvalid(publicDeviceIp, 'network.deviceIp must be a private LAN IPv4 address');

    const sameDeviceAndServerIp = {
      ...valid,
      network: { ...valid.network, deviceIp: valid.network.serverIp },
    };
    await assertInvalid(sameDeviceAndServerIp, 'network.deviceIp must be a different LAN device than network.serverIp');

    const loopbackSsdpLocation = {
      ...valid,
      network: {
        ...valid.network,
        ssdpLocation: 'http://127.0.0.1:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml',
      },
    };
    await assertInvalid(loopbackSsdpLocation, 'network.ssdpLocation host must be a LAN-reachable IPv4 address');

    const mismatchedSsdpLocation = {
      ...valid,
      network: {
        ...valid.network,
        ssdpLocation: 'http://192.168.1.99:8097/dlna/58deb718-f9ee-4ac5-a1d4-05286d64cf42/description.xml',
      },
    };
    await assertInvalid(mismatchedSsdpLocation, 'network.ssdpLocation host must match network.serverIp');

    const mismatchedSsdpServerId = {
      ...valid,
      network: {
        ...valid.network,
        ssdpLocation: 'http://192.168.1.46:8097/dlna/11111111-2222-3333-8444-555555555555/description.xml',
      },
    };
    await assertInvalid(mismatchedSsdpServerId, 'network.ssdpLocation path must include server.serverId');

    const outsideArtifact = {
      ...valid,
      artifacts: [{ type: 'client-log', pathOrUrl: '/tmp/outside-dlna-evidence.txt' }],
    };
    await assertInvalid(outsideArtifact, 'artifacts[0].pathOrUrl must stay under');

    const nonArtifactsDirArtifactPath = path.join(manualEvidenceDir, 'not-an-artifact.txt');
    await fs.writeFile(nonArtifactsDirArtifactPath, 'not an artifact\n');
    const outsideArtifactsDir = {
      ...valid,
      artifacts: [{ type: 'client-log', pathOrUrl: path.relative(plansDir, nonArtifactsDirArtifactPath) }],
    };
    await assertInvalid(outsideArtifactsDir, 'artifacts[0].pathOrUrl must stay under');
    await fs.rm(nonArtifactsDirArtifactPath, { force: true });

    const symlinkOutsideArtifactPath = path.join(manualEvidenceDir, 'artifacts', 'symlink-outside.log');
    const symlinkOutsideTargetPath = path.join(manualEvidenceDir, 'symlink-target.log');
    await fs.writeFile(symlinkOutsideTargetPath, 'symlink target\n');
    await fs.symlink(symlinkOutsideTargetPath, symlinkOutsideArtifactPath);
    const symlinkOutsideArtifact = {
      ...valid,
      artifacts: [{ type: 'client-log', pathOrUrl: path.relative(plansDir, symlinkOutsideArtifactPath) }],
    };
    await assertInvalid(symlinkOutsideArtifact, 'artifacts[0].pathOrUrl real path must stay under');
    await fs.rm(symlinkOutsideArtifactPath, { force: true });
    await fs.rm(symlinkOutsideTargetPath, { force: true });

    await fs.writeFile(emptyArtifactPath, '');
    const emptyArtifact = {
      ...valid,
      artifacts: [{ type: 'client-log', pathOrUrl: path.relative(plansDir, emptyArtifactPath) }],
    };
    await assertInvalid(emptyArtifact, 'artifacts[0].pathOrUrl file must not be empty');
    await fs.rm(emptyArtifactPath, { force: true });

    console.log(JSON.stringify({ status: 'self-test-ok' }, null, 2));
  } finally {
    await fs.rm(artifactPath, { force: true });
    await fs.rm(emptyArtifactPath, { force: true });
    await fs.rm(path.join(manualEvidenceDir, 'not-an-artifact.txt'), { force: true });
    await fs.rm(path.join(manualEvidenceDir, 'artifacts', 'symlink-outside.log'), { force: true });
    await fs.rm(path.join(manualEvidenceDir, 'symlink-target.log'), { force: true });
  }
}

async function assertInvalid(evidence, expectedErrorSubstring) {
  const errors = await validateManualDlnaEvidence(evidence);
  if (!errors.some((error) => error.includes(expectedErrorSubstring))) {
    throw new Error(`expected validation error containing "${expectedErrorSubstring}", got:\n${errors.join('\n')}`);
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
  parseArgs,
  requiredFlowChecks,
  requiredNetworkChecks,
  summarizeManualDlnaEvidence,
  validateManualDlnaEvidence,
};
