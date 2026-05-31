#!/usr/bin/env node

const fs = require('node:fs/promises');
const net = require('node:net');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { manualEvidenceTemplate } = require('./dlna-device-evidence');

const execFileAsync = promisify(execFile);
const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_DLNA_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'dlna-upnp');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');
const draftsDir = path.join(manualEvidenceDir, 'drafts');

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return;
  }
  if (options.selfTest) {
    selfTest();
    return;
  }
  const baseUrl = normalizeBaseUrl(options.baseUrl || process.env.JELLYRIN_BASE_URL || 'http://127.0.0.1:8097');
  requirePrivateLanBaseUrl(baseUrl);
  const serverInfo = await fetchJson(new URL('/System/Info/Public', baseUrl));
  const serverId = requiredString(serverInfo.Id || serverInfo.ServerId, 'System/Info/Public Id');
  const version = requiredString(serverInfo.Version || serverInfo.LocalAddress || 'unknown', 'System/Info/Public Version');
  const commit = await gitCommit();
  const descriptionUrl = new URL(`/dlna/${serverId}/description.xml`, baseUrl);
  const description = await fetchText(descriptionUrl);

  assertIncludes(description, '<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>', 'root descriptor device type');
  assertIncludes(description, '<iconList>', 'root descriptor iconList');
  assertIncludes(description, '<serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>', 'ContentDirectory service');
  assertIncludes(description, '<serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>', 'ConnectionManager service');
  assertIncludes(description, '<serviceType>urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1</serviceType>', 'MediaReceiverRegistrar service');

  const iconUrl = firstRegexGroup(description, /<url>([^<]*\/dlna\/[^<]+\/icons\/logo(?:-\d+)?\.png)<\/url>/i);
  if (!iconUrl) {
    throw new Error('root descriptor must expose a DLNA PNG icon URL');
  }
  const icon = await fetchBytes(resolveServerUrl(iconUrl, baseUrl));
  if (!icon.subarray(0, 4).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47]))) {
    throw new Error(`DLNA icon is not PNG: ${iconUrl}`);
  }

  const scpd = await fetchText(new URL(`/dlna/${serverId}/contentdirectory/contentdirectory.xml`, baseUrl));
  assertIncludes(scpd, '<name>SystemUpdateID</name>', 'ContentDirectory SystemUpdateID state variable');
  assertIncludes(scpd, '<name>Browse</name>', 'ContentDirectory Browse action');
  assertIncludes(scpd, '<name>Search</name>', 'ContentDirectory Search action');

  const contentDirectoryUrl = new URL(`/dlna/${serverId}/contentdirectory/control`, baseUrl);
  const browseRoot = await postSoap(
    contentDirectoryUrl,
    'urn:schemas-upnp-org:service:ContentDirectory:1',
    'Browse',
    [
      '<u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">',
      '<ObjectID>0</ObjectID>',
      '<BrowseFlag>BrowseDirectChildren</BrowseFlag>',
      '<Filter>*</Filter>',
      '<StartingIndex>0</StartingIndex>',
      '<RequestedCount>10</RequestedCount>',
      '<SortCriteria></SortCriteria>',
      '</u:Browse>',
    ].join(''),
  );
  assertIncludes(browseRoot, '<u:BrowseResponse', 'ContentDirectory Browse response');
  assertIncludes(browseRoot, '<Result>', 'ContentDirectory Browse DIDL payload');
  assertIncludes(browseRoot, '<NumberReturned>', 'ContentDirectory Browse NumberReturned');
  assertIncludes(browseRoot, '<TotalMatches>', 'ContentDirectory Browse TotalMatches');
  assertIncludes(browseRoot, '<UpdateID>', 'ContentDirectory Browse UpdateID');

  const searchRoot = await postSoap(
    contentDirectoryUrl,
    'urn:schemas-upnp-org:service:ContentDirectory:1',
    'Search',
    [
      '<u:Search xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">',
      '<ContainerID>0</ContainerID>',
      '<SearchCriteria>upnp:class derivedfrom "object.item"</SearchCriteria>',
      '<Filter>*</Filter>',
      '<StartingIndex>0</StartingIndex>',
      '<RequestedCount>10</RequestedCount>',
      '<SortCriteria>+dc:title</SortCriteria>',
      '</u:Search>',
    ].join(''),
  );
  assertIncludes(searchRoot, '<u:SearchResponse', 'ContentDirectory Search response');
  assertIncludes(searchRoot, '<Result>', 'ContentDirectory Search DIDL payload');

  const connectionManager = await postSoap(
    new URL(`/dlna/${serverId}/connectionmanager/control`, baseUrl),
    'urn:schemas-upnp-org:service:ConnectionManager:1',
    'GetProtocolInfo',
    '<u:GetProtocolInfo xmlns:u="urn:schemas-upnp-org:service:ConnectionManager:1" />',
  );
  assertIncludes(connectionManager, '<u:GetProtocolInfoResponse', 'ConnectionManager GetProtocolInfo response');
  assertIncludes(connectionManager, '<Source>', 'ConnectionManager SourceProtocolInfo');
  assertIncludes(connectionManager, 'http-get:*:video/mp4:', 'ConnectionManager video protocol info');

  const registrarUrl = new URL(`/dlna/${serverId}/mediareceiverregistrar/control`, baseUrl);
  const isAuthorized = await postSoap(
    registrarUrl,
    'urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1',
    'IsAuthorized',
    '<u:IsAuthorized xmlns:u="urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1"><DeviceID>uuid:jellyrin-preflight</DeviceID></u:IsAuthorized>',
  );
  assertIncludes(isAuthorized, '<u:IsAuthorizedResponse', 'MediaReceiverRegistrar IsAuthorized response');
  assertIncludes(isAuthorized, '<Result>1</Result>', 'MediaReceiverRegistrar IsAuthorized result');

  const isValidated = await postSoap(
    registrarUrl,
    'urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1',
    'IsValidated',
    '<u:IsValidated xmlns:u="urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1"><DeviceID>uuid:jellyrin-preflight</DeviceID></u:IsValidated>',
  );
  assertIncludes(isValidated, '<u:IsValidatedResponse', 'MediaReceiverRegistrar IsValidated response');
  assertIncludes(isValidated, '<Result>1</Result>', 'MediaReceiverRegistrar IsValidated result');

  const registerDevice = await postSoap(
    registrarUrl,
    'urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1',
    'RegisterDevice',
    '<u:RegisterDevice xmlns:u="urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1"><RegistrationReqMsg></RegistrationReqMsg></u:RegisterDevice>',
  );
  assertIncludes(registerDevice, '<u:RegisterDeviceResponse', 'MediaReceiverRegistrar RegisterDevice response');
  assertIncludes(registerDevice, '<RegistrationRespMsg>', 'MediaReceiverRegistrar RegisterDevice payload');

  const mediaProbe = await probeMediaRoutes({ baseUrl, serverId, itemId: options.itemId, subtitleIndex: options.subtitleIndex });
  const draft = buildDraftEvidence({
    baseUrl,
    commit,
    descriptionUrl: descriptionUrl.toString(),
    serverId,
    version,
  });
  const outputPath = await writeDraft(draft);
  console.log(JSON.stringify({
    status: 'ready-for-device-test',
    baseUrl,
    serverId,
    version,
    commit,
    descriptor: descriptionUrl.toString(),
    iconBytes: icon.length,
    soapChecks: [
      'ContentDirectory.Browse',
      'ContentDirectory.Search',
      'ConnectionManager.GetProtocolInfo',
      'MediaReceiverRegistrar.IsAuthorized',
      'MediaReceiverRegistrar.IsValidated',
      'MediaReceiverRegistrar.RegisterDevice',
    ],
    mediaProbe,
    draftPath: path.relative(plansDir, outputPath),
    nextStep: mediaProbe.status === 'skipped'
      ? 'Re-run preflight with --item-id and optionally --subtitle-index after choosing a media item, then replace placeholders after VLC/TV discovery and playback.'
      : 'Replace placeholders in the draft after VLC/TV discovery, playback, thumbnail and subtitle checks.',
  }, null, 2));
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === '--base-url') {
      options.baseUrl = requireArgValue(args, index, arg);
      index += 1;
    } else if (arg === '--item-id') {
      options.itemId = requireArgValue(args, index, arg);
      index += 1;
    } else if (arg === '--subtitle-index') {
      options.subtitleIndex = requireArgValue(args, index, arg);
      index += 1;
    } else if (arg === '--self-test') {
      options.selfTest = true;
    } else if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

function requireArgValue(args, index, flag) {
  const value = args[index + 1];
  if (value === undefined || value.startsWith('--')) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function printUsage() {
  console.log('Usage: node qa/golden/dlna-device-preflight.js [--base-url http://host:8097] [--item-id <uuid>] [--subtitle-index <n>] [--self-test]');
}

function normalizeBaseUrl(value) {
  const url = new URL(value);
  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error('base URL must use http or https');
  }
  url.pathname = '/';
  url.search = '';
  url.hash = '';
  return url.toString().replace(/\/$/, '');
}

function requirePrivateLanBaseUrl(baseUrl) {
  const host = new URL(baseUrl).hostname;
  if (net.isIP(host) !== 4) {
    throw new Error('base URL host must be the Jellyrin private LAN IPv4 address used by the renderer, for example http://192.168.1.46:8097');
  }
  const [first, second, third, fourth] = host.split('.').map((part) => Number(part));
  if (
    first === 0 ||
    first === 127 ||
    first >= 224 ||
    host === '255.255.255.255' ||
    (first === 169 && second === 254) ||
    [first, second, third, fourth].some((octet) => !Number.isInteger(octet) || octet < 0 || octet > 255)
  ) {
    throw new Error('base URL host must be LAN-reachable, not loopback, link-local, multicast, broadcast or unspecified');
  }
  if (!(first === 10 || (first === 172 && second >= 16 && second <= 31) || (first === 192 && second === 168))) {
    throw new Error('base URL host must be a private LAN IPv4 address (10/8, 172.16/12, or 192.168/16)');
  }
}

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`GET ${url} failed with HTTP ${response.status}`);
  }
  return response.json();
}

async function fetchText(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`GET ${url} failed with HTTP ${response.status}`);
  }
  return response.text();
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`GET ${url} failed with HTTP ${response.status}`);
  }
  return Buffer.from(await response.arrayBuffer());
}

async function probeMediaRoutes({ baseUrl, serverId, itemId, subtitleIndex }) {
  if (!itemId) {
    return {
      status: 'skipped',
      reason: 'pass --item-id to validate DLNA thumbnail and optional subtitle route readiness before the device run',
    };
  }

  const thumbnailUrl = new URL(`/dlna/${serverId}/items/${encodeURIComponent(itemId)}/thumbnail.png`, baseUrl);
  const thumbnail = await fetchWithHeaders(thumbnailUrl);
  if (!thumbnail.ok) {
    throw new Error(`GET ${thumbnailUrl} failed with HTTP ${thumbnail.status}`);
  }
  if (!String(thumbnail.headers.get('content-type') || '').toLowerCase().startsWith('image/')) {
    throw new Error(`DLNA thumbnail route returned unexpected content-type: ${thumbnail.headers.get('content-type')}`);
  }
  const thumbnailBytes = Buffer.from(await thumbnail.arrayBuffer());
  if (thumbnailBytes.length < 8) {
    throw new Error('DLNA thumbnail route returned an empty body');
  }

  const probe = {
    status: 'passed',
    itemId,
    thumbnail: {
      status: thumbnail.status,
      contentType: thumbnail.headers.get('content-type'),
      bytes: thumbnailBytes.length,
    },
  };

  if (subtitleIndex !== undefined) {
    const subtitleUrl = new URL(
      `/dlna/${serverId}/items/${encodeURIComponent(itemId)}/subtitles/${encodeURIComponent(subtitleIndex)}/stream.vtt`,
      baseUrl,
    );
    const subtitle = await fetchWithHeaders(subtitleUrl);
    if (!subtitle.ok) {
      throw new Error(`GET ${subtitleUrl} failed with HTTP ${subtitle.status}`);
    }
    const subtitleContentType = String(subtitle.headers.get('content-type') || '').toLowerCase();
    if (!subtitleContentType.startsWith('text/vtt')) {
      throw new Error(`DLNA subtitle route returned unexpected content-type: ${subtitle.headers.get('content-type')}`);
    }
    const text = await subtitle.text();
    if (!text.includes('WEBVTT')) {
      throw new Error('DLNA subtitle route did not return a WEBVTT payload');
    }
    probe.subtitle = {
      index: subtitleIndex,
      status: subtitle.status,
      contentType: subtitle.headers.get('content-type'),
      bytes: Buffer.byteLength(text),
    };
  }

  return probe;
}

async function fetchWithHeaders(url) {
  return fetch(url, {
    headers: {
      'user-agent': 'Jellyrin DLNA device preflight',
    },
  });
}

async function postSoap(url, serviceType, action, body) {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'text/xml; charset=utf-8',
      soapaction: `"${serviceType}#${action}"`,
    },
    body: soapEnvelope(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`POST ${url} ${action} failed with HTTP ${response.status}: ${text.slice(0, 200)}`);
  }
  return text;
}

function soapEnvelope(body) {
  return `<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>${body}</s:Body>
</s:Envelope>`;
}

function requiredString(value, label) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${label} is missing`);
  }
  return value.trim();
}

function assertIncludes(value, needle, label) {
  if (!value.includes(needle)) {
    throw new Error(`${label} missing`);
  }
}

function firstRegexGroup(value, regex) {
  const match = value.match(regex);
  return match?.[1] || null;
}

function resolveServerUrl(value, baseUrl) {
  return new URL(value, baseUrl);
}

async function gitCommit() {
  const { stdout } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: repoRoot });
  return stdout.trim();
}

function buildDraftEvidence({ baseUrl, commit, descriptionUrl, serverId, version }) {
  const draft = manualEvidenceTemplate();
  draft.jellyrinBaseUrl = baseUrl;
  draft.server.version = version;
  draft.server.commit = commit;
  draft.server.serverId = serverId;
  draft.network.ssdpLocation = descriptionUrl;
  draft.artifacts = [
    {
      type: 'preflight',
      pathOrUrl: 'manual/dlna-upnp/artifacts/replace-with-device-capture-after-test',
    },
  ];
  draft.notes = 'Preflight passed on the Jellyrin server host. Replace device/control-point/media/network placeholders after the real renderer test.';
  return draft;
}

async function writeDraft(draft) {
  await fs.mkdir(artifactsDir, { recursive: true });
  await fs.mkdir(draftsDir, { recursive: true });
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const outputPath = path.join(draftsDir, `draft-${stamp}.json`);
  await fs.writeFile(outputPath, `${JSON.stringify(draft, null, 2)}\n`);
  return outputPath;
}

function selfTest() {
  const parsed = parseArgs([
    '--base-url',
    'http://192.168.1.46:8097/web/index.html?x=1',
    '--item-id',
    'item-1',
    '--subtitle-index',
    '2',
  ]);
  assertEqual(parsed.baseUrl, 'http://192.168.1.46:8097/web/index.html?x=1', 'parse baseUrl');
  assertEqual(parsed.itemId, 'item-1', 'parse itemId');
  assertEqual(parsed.subtitleIndex, '2', 'parse subtitleIndex');
  assertEqual(normalizeBaseUrl(parsed.baseUrl), 'http://192.168.1.46:8097', 'normalizeBaseUrl');
  requirePrivateLanBaseUrl(normalizeBaseUrl(parsed.baseUrl));
  assertEqual(resolveServerUrl('/dlna/server/icons/logo.png', parsed.baseUrl).toString(), 'http://192.168.1.46:8097/dlna/server/icons/logo.png', 'resolveServerUrl');
  assertIncludes(soapEnvelope('<x />'), '<s:Body><x /></s:Body>', 'soapEnvelope body');
  for (const invalidBaseUrl of ['http://127.0.0.1:8097', 'http://8.8.8.8:8097', 'http://jellyrin.local:8097']) {
    let failed = false;
    try {
      requirePrivateLanBaseUrl(invalidBaseUrl);
    } catch {
      failed = true;
    }
    if (!failed) {
      throw new Error(`requirePrivateLanBaseUrl should reject ${invalidBaseUrl}`);
    }
  }
  let missingValueFailed = false;
  try {
    parseArgs(['--base-url', '--item-id', 'x']);
  } catch {
    missingValueFailed = true;
  }
  if (!missingValueFailed) {
    throw new Error('parseArgs should reject missing option values');
  }
  console.log(JSON.stringify({ status: 'self-test-ok' }, null, 2));
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label}: expected ${expected}, got ${actual}`);
  }
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error.message || error);
    process.exit(1);
  });
}

module.exports = {
  buildDraftEvidence,
  normalizeBaseUrl,
  parseArgs,
  requirePrivateLanBaseUrl,
  resolveServerUrl,
  soapEnvelope,
};
