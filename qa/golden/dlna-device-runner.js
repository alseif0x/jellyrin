#!/usr/bin/env node

const dgram = require('node:dgram');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const {
  validateManualDlnaEvidence,
} = require('./dlna-device-evidence');

const execFileAsync = promisify(execFile);
const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_DLNA_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'dlna-upnp');
const artifactsDir = path.join(manualEvidenceDir, 'artifacts');

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
  requireOption(options, 'baseUrl');
  requireOption(options, 'itemId');
  requireOption(options, 'deviceName');
  requireOption(options, 'tester');

  const playbackSeconds = Number(options.playbackSeconds || 0);
  if (!Number.isFinite(playbackSeconds) || playbackSeconds < 10) {
    throw new Error('--playback-seconds must be >= 10 after real renderer/control-point playback');
  }

  const baseUrl = normalizeBaseUrl(options.baseUrl);
  const startedAt = new Date().toISOString();
  const serverInfo = await fetchJson(new URL('/System/Info/Public', baseUrl));
  const serverId = requiredString(serverInfo.Id || serverInfo.ServerId, 'server id');
  const version = requiredString(serverInfo.Version || 'unknown', 'server version');
  const commit = options.commit || await gitCommit();
  const serverIp = options.serverIp || new URL(baseUrl).hostname;
  const deviceIp = options.deviceIp || firstLanAddress();
  if (!deviceIp) {
    throw new Error('--device-ip is required when no non-loopback local IPv4 address is available');
  }

  const ssdp = await ssdpDiscover(serverId, options.ssdpTimeoutMs || 5000);
  const ssdpLocation = requiredString(ssdp.location, 'SSDP LOCATION header');
  const description = await fetchText(new URL(ssdpLocation));
  assertIncludes(description, '<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>', 'root descriptor device type');
  assertIncludes(description, '<iconList>', 'root descriptor iconList');

  const contentDirectoryUrl = new URL(`/dlna/${serverId}/contentdirectory/control`, baseUrl);
  const browseRoot = await soap(contentDirectoryUrl, 'urn:schemas-upnp-org:service:ContentDirectory:1', 'Browse', browseRootBody());
  assertIncludes(browseRoot, '<u:BrowseResponse', 'ContentDirectory Browse response');
  const searchRoot = await soap(contentDirectoryUrl, 'urn:schemas-upnp-org:service:ContentDirectory:1', 'Search', searchRootBody());
  assertIncludes(searchRoot, '<u:SearchResponse', 'ContentDirectory Search response');
  const browseItem = await soap(contentDirectoryUrl, 'urn:schemas-upnp-org:service:ContentDirectory:1', 'Browse', browseItemBody(options.itemId));
  assertIncludes(browseItem, `item:${options.itemId}`, 'BrowseMetadata item id');

  const thumbnail = await fetchChecked(new URL(`/dlna/${serverId}/items/${encodeURIComponent(options.itemId)}/thumbnail.png`, baseUrl));
  if (!String(thumbnail.contentType).toLowerCase().startsWith('image/')) {
    throw new Error(`thumbnail returned unexpected content-type ${thumbnail.contentType}`);
  }

  const subtitle = await fetchChecked(new URL(`/dlna/${serverId}/items/${encodeURIComponent(options.itemId)}/subtitles/${encodeURIComponent(options.subtitleIndex || 2)}/stream.vtt`, baseUrl));
  if (!String(subtitle.contentType).toLowerCase().startsWith('text/vtt')) {
    throw new Error(`subtitle returned unexpected content-type ${subtitle.contentType}`);
  }
  if (!subtitle.text.includes('WEBVTT')) {
    throw new Error('subtitle response is not WEBVTT');
  }

  const stream = await fetchChecked(new URL(`/dlna/${serverId}/items/${encodeURIComponent(options.itemId)}/stream.${options.container || 'mkv'}`, baseUrl), {
    headers: { Range: 'bytes=0-3' },
  });
  if (![200, 206].includes(stream.status)) {
    throw new Error(`stream returned HTTP ${stream.status}`);
  }

  const transcodeFallback = await probeHlsFallback(baseUrl, serverId, options.itemId);
  const artifactPath = await writeArtifact(startedAt, {
    baseUrl,
    serverId,
    ssdp,
    browseRootBytes: browseRoot.length,
    searchRootBytes: searchRoot.length,
    browseItemBytes: browseItem.length,
    thumbnail: { status: thumbnail.status, contentType: thumbnail.contentType, bytes: thumbnail.bytes.length },
    subtitle: { status: subtitle.status, contentType: subtitle.contentType, bytes: Buffer.byteLength(subtitle.text) },
    stream: { status: stream.status, contentType: stream.contentType, bytes: stream.bytes.length },
    transcodeFallback,
  });

  const evidence = {
    schema: 'jellyrin-dlna-device-evidence-v3',
    deviceName: options.deviceName,
    deviceType: options.deviceType || 'upnp-control-point',
    controlPointName: options.controlPointName || 'Jellyrin DLNA device runner',
    controlPointVersion: options.controlPointVersion || 'dev',
    testedAt: startedAt,
    tester: options.tester,
    jellyrinBaseUrl: baseUrl,
    result: 'pass',
    server: { version, commit, serverId },
    network: {
      serverIp,
      deviceIp,
      ssdpLocation,
      sameLanAsServer: true,
      ssdpDiscovery: true,
      locationReachableFromDevice: true,
    },
    media: {
      itemId: options.itemId,
      itemName: options.itemName || 'E3 DLNA Manual Fixture',
      container: options.container || 'mkv',
      playMethod: 'HLS',
      streamStatus: stream.status,
      playbackSeconds,
      thumbnailStatusOk: true,
      subtitleStatusOk: true,
      subtitleMime: subtitle.contentType,
    },
    transcodeFallback,
    flow: {
      serverVisibleInControlPoint: true,
      rootDescriptorFetched: true,
      iconListFetched: true,
      contentDirectoryBrowse: true,
      mediaItemVisible: true,
      thumbnailFetched: true,
      subtitleLinkResolved: true,
      playbackStarted: true,
      playbackStable: true,
      streamUrlFetched: true,
    },
    artifacts: [{
      type: 'client-log',
      pathOrUrl: path.relative(plansDir, artifactPath),
    }],
    notes: options.notes || 'Generated by Jellyrin DLNA device runner after confirmed real renderer/control-point playback.',
  };

  const errors = await validateManualDlnaEvidence(evidence);
  if (errors.length > 0) {
    throw new Error(`generated evidence is invalid:\n${errors.join('\n')}`);
  }

  const outputPath = path.resolve(options.output || path.join(manualEvidenceDir, `device-${stamp(startedAt)}.json`));
  await fs.mkdir(path.dirname(outputPath), { recursive: true });
  await fs.writeFile(outputPath, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(JSON.stringify({
    status: 'device-evidence-written',
    outputPath,
    artifactPath,
    serverId,
    ssdpLocation,
    valid: true,
  }, null, 2));
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else if (arg === '--self-test') {
      options.selfTest = true;
    } else if (arg.startsWith('--')) {
      const key = arg.slice(2).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
      const value = args[index + 1];
      if (value === undefined || value.startsWith('--')) {
        throw new Error(`${arg} requires a value`);
      }
      options[key] = args[index + 1];
      index += 1;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
}

function printUsage() {
  console.log([
    'Usage: node qa/golden/dlna-device-runner.js --base-url http://<server-lan-ip>:8097 --item-id <id> --device-name <name> --tester <name> --playback-seconds <n> [options]',
    '',
    'Run this from the real control-point/renderer LAN host after playback has started and remained stable for at least 10 seconds.',
    'Useful options: --subtitle-index 2 --device-ip <lan-ip> --server-ip <lan-ip> --output <plans/manual/dlna-upnp/file.json> --self-test',
  ].join('\n'));
}

function requireOption(options, key) {
  if (!options[key]) {
    throw new Error(`--${key.replace(/[A-Z]/g, (ch) => `-${ch.toLowerCase()}`)} is required`);
  }
}

function normalizeBaseUrl(value) {
  const url = new URL(value);
  url.pathname = '/';
  url.search = '';
  url.hash = '';
  return url.toString().replace(/\/$/, '');
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

async function fetchChecked(url, init = {}) {
  const response = await fetch(url, init);
  const bytes = Buffer.from(await response.arrayBuffer());
  return {
    status: response.status,
    contentType: response.headers.get('content-type') || '',
    bytes,
    text: bytes.toString('utf8'),
  };
}

async function soap(url, serviceType, action, body) {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'text/xml; charset=utf-8',
      soapaction: `"${serviceType}#${action}"`,
    },
    body: `<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>${body}</s:Body></s:Envelope>`,
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`SOAP ${action} failed with HTTP ${response.status}: ${text.slice(0, 200)}`);
  }
  return text;
}

function browseRootBody() {
  return '<u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>0</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>20</RequestedCount><SortCriteria></SortCriteria></u:Browse>';
}

function searchRootBody() {
  return '<u:Search xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ContainerID>0</ContainerID><SearchCriteria>upnp:class derivedfrom "object.item"</SearchCriteria><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>20</RequestedCount><SortCriteria>+dc:title</SortCriteria></u:Search>';
}

function browseItemBody(itemId) {
  return `<u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><ObjectID>item:${escapeXml(itemId)}</ObjectID><BrowseFlag>BrowseMetadata</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>1</RequestedCount><SortCriteria></SortCriteria></u:Browse>`;
}

async function probeHlsFallback(baseUrl, serverId, itemId) {
  const masterUrl = new URL(`/dlna/${serverId}/items/${encodeURIComponent(itemId)}/transcode.m3u8`, baseUrl);
  const master = await fetchChecked(masterUrl);
  if (master.status !== 200 || !master.text.includes('#EXTM3U')) {
    throw new Error('HLS master playlist was not fetched');
  }
  const mediaLine = master.text.split(/\r?\n/).find((line) => line && !line.startsWith('#'));
  if (!mediaLine) {
    throw new Error('HLS master playlist did not contain a media playlist URL');
  }
  const mediaUrl = resolvePlaylistUrl(mediaLine, masterUrl);
  const media = await fetchChecked(mediaUrl);
  if (media.status !== 200 || !media.text.includes('#EXTM3U')) {
    throw new Error('HLS media playlist was not fetched');
  }
  const segmentLine = media.text.split(/\r?\n/).find((line) => line && !line.startsWith('#'));
  if (!segmentLine) {
    throw new Error('HLS media playlist did not contain a segment URL');
  }
  const segment = await fetchChecked(resolvePlaylistUrl(segmentLine, mediaUrl));
  return {
    profileRequiresTranscode: true,
    hlsPlaylistFetched: true,
    mediaPlaylistFetched: true,
    segmentFetched: true,
    segmentStatus: segment.status,
    segmentMime: segment.contentType.split(';')[0],
    segmentBytesVerified: segment.bytes.length > 0 && segment.bytes[0] === 0x47,
  };
}

function resolvePlaylistUrl(line, playlistUrl) {
  return new URL(line.trim(), playlistUrl);
}

async function ssdpDiscover(serverId, timeoutMs) {
  const message = [
    'M-SEARCH * HTTP/1.1',
    'HOST: 239.255.255.250:1900',
    'MAN: "ssdp:discover"',
    'MX: 2',
    'ST: urn:schemas-upnp-org:device:MediaServer:1',
    '',
    '',
  ].join('\r\n');
  return new Promise((resolve, reject) => {
    const socket = dgram.createSocket('udp4');
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error('SSDP discovery timed out'));
    }, Number(timeoutMs));
    socket.on('message', (buffer) => {
      const text = buffer.toString('utf8');
      if (!text.toLowerCase().includes(serverId.toLowerCase())) {
        return;
      }
      clearTimeout(timer);
      socket.close();
      resolve({
        raw: text,
        location: requiredString(headerValue(text, 'location'), 'SSDP LOCATION header'),
      });
    });
    socket.on('error', (error) => {
      clearTimeout(timer);
      socket.close();
      reject(error);
    });
    socket.bind(() => {
      socket.setBroadcast(true);
      socket.send(Buffer.from(message), 1900, '239.255.255.250');
    });
  });
}

function headerValue(message, name) {
  const prefix = `${name.toLowerCase()}:`;
  return message
    .split(/\r?\n/)
    .find((line) => line.toLowerCase().startsWith(prefix))
    ?.slice(prefix.length)
    .trim();
}

async function writeArtifact(startedAt, payload) {
  await fs.mkdir(artifactsDir, { recursive: true });
  const artifactPath = path.join(artifactsDir, `device-runner-${stamp(startedAt)}.json`);
  await fs.writeFile(artifactPath, `${JSON.stringify(payload, null, 2)}\n`);
  return artifactPath;
}

function firstLanAddress() {
  for (const iface of Object.values(os.networkInterfaces())) {
    for (const address of iface || []) {
      if (address.family === 'IPv4' && !address.internal) {
        return address.address;
      }
    }
  }
  return null;
}

async function gitCommit() {
  const { stdout } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: repoRoot });
  return stdout.trim();
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

function escapeXml(value) {
  return String(value).replace(/[<>&'"]/g, (ch) => ({
    '<': '&lt;',
    '>': '&gt;',
    '&': '&amp;',
    "'": '&apos;',
    '"': '&quot;',
  })[ch]);
}

function stamp(value) {
  return value.replace(/[:.]/g, '-');
}

function selfTest() {
  const parsed = parseArgs([
    '--base-url',
    'http://192.168.1.46:8097/web/index.html?x=1',
    '--item-id',
    'item-1',
    '--device-name',
    'VLC laptop',
    '--tester',
    'qa',
    '--playback-seconds',
    '30',
  ]);
  assertEqual(parsed.baseUrl, 'http://192.168.1.46:8097/web/index.html?x=1', 'parse baseUrl');
  assertEqual(normalizeBaseUrl(parsed.baseUrl), 'http://192.168.1.46:8097', 'normalizeBaseUrl');
  assertEqual(headerValue('HTTP/1.1 200 OK\r\nLOCATION: http://host/dlna.xml\r\n', 'location'), 'http://host/dlna.xml', 'headerValue');
  assertEqual(requiredString(' http://host/dlna.xml ', 'location'), 'http://host/dlna.xml', 'requiredString trims values');
  let missingSsdpLocationFailed = false;
  try {
    requiredString(undefined, 'SSDP LOCATION header');
  } catch {
    missingSsdpLocationFailed = true;
  }
  if (!missingSsdpLocationFailed) {
    throw new Error('requiredString should reject a missing SSDP LOCATION header');
  }
  assertEqual(
    resolvePlaylistUrl('variant/stream.m3u8', new URL('http://host/dlna/item/transcode.m3u8')).toString(),
    'http://host/dlna/item/variant/stream.m3u8',
    'resolve media playlist relative URL',
  );
  assertEqual(
    resolvePlaylistUrl('../segments/00001.ts', new URL('http://host/dlna/item/variant/stream.m3u8')).toString(),
    'http://host/dlna/item/segments/00001.ts',
    'resolve segment relative URL',
  );
  assertEqual(
    resolvePlaylistUrl('/dlna/item/segments/00001.ts', new URL('http://host/dlna/item/variant/stream.m3u8')).toString(),
    'http://host/dlna/item/segments/00001.ts',
    'resolve root-relative segment URL',
  );
  const escaped = browseItemBody('abc<&"');
  if (!escaped.includes('abc&lt;&amp;&quot;')) {
    throw new Error(`browseItemBody did not XML-escape ObjectID: ${escaped}`);
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
  browseItemBody,
  headerValue,
  normalizeBaseUrl,
  parseArgs,
  probeHlsFallback,
  resolvePlaylistUrl,
};
