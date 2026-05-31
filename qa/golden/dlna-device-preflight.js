#!/usr/bin/env node

const fs = require('node:fs/promises');
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
  const baseUrl = normalizeBaseUrl(options.baseUrl || process.env.JELLYRIN_BASE_URL || 'http://127.0.0.1:8097');
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

  const iconUrl = firstRegexGroup(description, /<url>(\/dlna\/[^<]+\/icons\/logo(?:-\d+)?\.png)<\/url>/);
  if (!iconUrl) {
    throw new Error('root descriptor must expose a DLNA PNG icon URL');
  }
  const icon = await fetchBytes(new URL(iconUrl, baseUrl));
  if (!icon.subarray(0, 4).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47]))) {
    throw new Error(`DLNA icon is not PNG: ${iconUrl}`);
  }

  const scpd = await fetchText(new URL(`/dlna/${serverId}/contentdirectory/contentdirectory.xml`, baseUrl));
  assertIncludes(scpd, '<name>SystemUpdateID</name>', 'ContentDirectory SystemUpdateID state variable');
  assertIncludes(scpd, '<name>Browse</name>', 'ContentDirectory Browse action');
  assertIncludes(scpd, '<name>Search</name>', 'ContentDirectory Search action');

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
    draftPath: path.relative(plansDir, outputPath),
    nextStep: 'Replace placeholders in the draft after VLC/TV discovery, playback, thumbnail and subtitle checks.',
  }, null, 2));
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === '--base-url') {
      options.baseUrl = args[index + 1];
      index += 1;
    } else if (arg === '--help' || arg === '-h') {
      console.log('Usage: node qa/golden/dlna-device-preflight.js [--base-url http://host:8097]');
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return options;
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

if (require.main === module) {
  main().catch((error) => {
    console.error(error.message || error);
    process.exit(1);
  });
}

module.exports = {
  buildDraftEvidence,
  normalizeBaseUrl,
};
