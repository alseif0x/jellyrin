#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_CHANNELS_PROVIDER_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'channels-providers');
const templatePath = path.join(manualEvidenceDir, 'template.json');
const schema = 'jellyrin-channels-provider-evidence-v1';
const allowedProviderTypes = ['plugin-dotnet', 'plugin-rust-wasi', 'built-in', 'remote-http', 'iptv', 'other'];
const allowedArtifactTypes = ['screenshot', 'client-log', 'server-log', 'provider-trace', 'network-capture', 'browser-trace'];

const requiredFlowChecks = [
  'providerListed',
  'providerFeaturesListed',
  'providerItemsListed',
  'searchMatched',
  'latestMatched',
  'imageResolved',
  'mediaSourceResolved',
  'streamBytesRead',
  'diagnosticsHealthy',
  'failureIsolationObserved',
];

async function loadManualChannelsProviderEvidence() {
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
      const errors = validateManualChannelsProviderEvidence(evidence);
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
    if (existing.schema !== schema) {
      await fs.writeFile(templatePath, `${JSON.stringify(manualEvidenceTemplate(), null, 2)}\n`);
    }
  } catch (error) {
    if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) {
      throw error;
    }
    await fs.writeFile(templatePath, `${JSON.stringify(manualEvidenceTemplate(), null, 2)}\n`);
  }
}

function validateManualChannelsProviderEvidence(evidence) {
  const errors = [];
  if (!evidence || typeof evidence !== 'object' || Array.isArray(evidence)) {
    return ['evidence must be a JSON object'];
  }

  if (evidence.schema !== schema) {
    errors.push(`schema must be ${schema}`);
  }
  for (const field of ['providerId', 'providerName', 'providerType', 'clientName', 'clientVersion', 'testedAt', 'tester', 'jellyrinBaseUrl', 'result']) {
    requireString(errors, evidence, field);
  }
  if (typeof evidence.providerType === 'string' && !allowedProviderTypes.includes(evidence.providerType)) {
    errors.push(`providerType must be one of: ${allowedProviderTypes.join(', ')}`);
  }
  if (evidence.result !== 'pass') {
    errors.push('result must be "pass"');
  }
  if (evidence.testedAt && Number.isNaN(Date.parse(evidence.testedAt))) {
    errors.push('testedAt must be an ISO-compatible date string');
  }
  requireUrl(errors, evidence.jellyrinBaseUrl, 'jellyrinBaseUrl');

  if (!evidence.server || typeof evidence.server !== 'object' || Array.isArray(evidence.server)) {
    errors.push('server must be an object');
  } else {
    for (const field of ['commit', 'version']) {
      requireString(errors, evidence.server, `server.${field}`, field);
    }
    if (typeof evidence.server.commit === 'string' && !/^[0-9a-f]{7,40}$/i.test(evidence.server.commit)) {
      errors.push('server.commit must be a git commit hash');
    }
  }

  if (!evidence.provider || typeof evidence.provider !== 'object' || Array.isArray(evidence.provider)) {
    errors.push('provider must be an object');
  } else {
    requireMinNumber(errors, evidence.provider.itemCount, 1, 'provider.itemCount');
    requireMinNumber(errors, evidence.provider.featureCount, 1, 'provider.featureCount');
    requireMinNumber(errors, evidence.provider.refreshHistoryCount, 1, 'provider.refreshHistoryCount');
  }

  if (!evidence.media || typeof evidence.media !== 'object' || Array.isArray(evidence.media)) {
    errors.push('media must be an object');
  } else {
    for (const field of ['itemId', 'itemName', 'mediaSourceId']) {
      requireString(errors, evidence.media, `media.${field}`, field);
    }
    requireStatus(errors, evidence.media.imageStatus, 'media.imageStatus');
    requireStatus(errors, evidence.media.streamStatus, 'media.streamStatus');
    requireMinNumber(errors, evidence.media.streamBytes, 1, 'media.streamBytes');
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
      if (typeof artifact.type === 'string' && !allowedArtifactTypes.includes(artifact.type)) {
        errors.push(`artifacts[${index}].type must be one of: ${allowedArtifactTypes.join(', ')}`);
      }
    });
  }
  return errors;
}

function requireString(errors, value, label, key = label) {
  if (typeof value?.[key] !== 'string' || value[key].trim() === '') {
    errors.push(`${label} is required`);
  }
}

function requireStatus(errors, value, label) {
  const status = Number(value);
  if (![200, 206].includes(status)) {
    errors.push(`${label} must be 200 or 206`);
  }
}

function requireMinNumber(errors, value, min, label) {
  const number = Number(value);
  if (!Number.isFinite(number) || number < min) {
    errors.push(`${label} must be at least ${min}`);
  }
}

function requireUrl(errors, value, label) {
  try {
    const url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol)) {
      errors.push(`${label} must be an HTTP(S) URL`);
    }
  } catch {
    errors.push(`${label} must be a valid URL`);
  }
}

function summarizeManualChannelsProviderEvidence(report) {
  return {
    directory: report.directory,
    templatePath: report.templatePath,
    validCount: report.valid.length,
    invalidCount: report.invalid.length,
    validRuns: report.valid.map((entry) => ({
      providerId: entry.evidence.providerId,
      providerName: entry.evidence.providerName,
      providerType: entry.evidence.providerType,
      clientName: entry.evidence.clientName,
      clientVersion: entry.evidence.clientVersion,
      testedAt: entry.evidence.testedAt,
      jellyrinBaseUrl: entry.evidence.jellyrinBaseUrl,
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
    schema,
    providerId: 'replace-with-provider-id',
    providerName: 'replace-with-provider-name',
    providerType: 'plugin-rust-wasi',
    clientName: 'Jellyfin Web',
    clientVersion: 'replace-with-client-version',
    testedAt: new Date().toISOString(),
    tester: 'replace-with-tester',
    jellyrinBaseUrl: 'http://replace-with-jellyrin-host:8097',
    result: 'pass',
    server: {
      version: 'replace-with-server-version',
      commit: 'replace-with-git-commit',
    },
    provider: {
      itemCount: 1,
      featureCount: 1,
      refreshHistoryCount: 1,
    },
    media: {
      itemId: 'replace-with-channel-item-id',
      itemName: 'replace-with-channel-item-name',
      mediaSourceId: 'replace-with-media-source-id',
      imageStatus: 200,
      streamStatus: 200,
      streamBytes: 1,
    },
    flow: Object.fromEntries(requiredFlowChecks.map((check) => [check, true])),
    artifacts: [
      {
        type: 'provider-trace',
        pathOrUrl: 'manual/channels-providers/artifacts/replace-with-trace.json',
      },
    ],
    notes: 'Use this for a real external provider trace or a formal provider-package validation run.',
  };
}

module.exports = {
  loadManualChannelsProviderEvidence,
  summarizeManualChannelsProviderEvidence,
  manualEvidenceTemplate,
};

if (require.main === module) {
  loadManualChannelsProviderEvidence()
    .then((report) => {
      console.log(JSON.stringify(summarizeManualChannelsProviderEvidence(report), null, 2));
    })
    .catch((error) => {
      console.error(error);
      process.exit(1);
    });
}
