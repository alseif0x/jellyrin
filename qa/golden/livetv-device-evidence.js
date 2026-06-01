#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const manualEvidenceDir = process.env.JELLYRIN_LIVETV_DEVICE_EVIDENCE_DIR
  || path.join(plansDir, 'manual', 'livetv-real');
const templatePath = path.join(manualEvidenceDir, 'template.json');
const schema = 'jellyrin-livetv-device-evidence-v1';
const allowedEvidenceTypes = ['real-tuner-device', 'formal-simulator-acceptance'];
const allowedTunerTypes = ['HDHomeRun', 'HDHomeRunLegacy', 'HDHomeRunSimulator', 'M3U', 'IPTV'];
const allowedArtifactTypes = ['screenshot', 'client-log', 'server-log', 'packet-capture', 'screen-recording', 'ffprobe-log'];

const requiredFlowChecks = [
  'tunerConfigured',
  'guideVisible',
  'directPlaybackStarted',
  'hlsPlaybackStarted',
  'timerRecordingCompleted',
  'recordingPlayable',
  'seriesTimerRecordingPlayable',
  'cleanupVerified',
];

async function loadManualLiveTvEvidence() {
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
      const errors = validateManualLiveTvEvidence(evidence);
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

function validateManualLiveTvEvidence(evidence) {
  const errors = [];
  if (!evidence || typeof evidence !== 'object' || Array.isArray(evidence)) {
    return ['evidence must be a JSON object'];
  }

  if (evidence.schema !== schema) {
    errors.push(`schema must be ${schema}`);
  }
  for (const field of ['evidenceType', 'tunerType', 'clientName', 'clientVersion', 'deviceName', 'testedAt', 'tester', 'jellyrinBaseUrl', 'result']) {
    requireString(errors, evidence, field);
  }
  if (typeof evidence.evidenceType === 'string' && !allowedEvidenceTypes.includes(evidence.evidenceType)) {
    errors.push(`evidenceType must be one of: ${allowedEvidenceTypes.join(', ')}`);
  }
  if (typeof evidence.tunerType === 'string' && !allowedTunerTypes.includes(evidence.tunerType)) {
    errors.push(`tunerType must be one of: ${allowedTunerTypes.join(', ')}`);
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

  if (!evidence.stream || typeof evidence.stream !== 'object' || Array.isArray(evidence.stream)) {
    errors.push('stream must be an object');
  } else {
    requireStatus(errors, evidence.stream.directStatus, 'stream.directStatus');
    requireStatus(errors, evidence.stream.hlsMasterStatus, 'stream.hlsMasterStatus');
    requireStatus(errors, evidence.stream.hlsSegmentStatus, 'stream.hlsSegmentStatus');
    requireMinNumber(errors, evidence.stream.playbackSeconds, 10, 'stream.playbackSeconds');
  }

  if (!evidence.recording || typeof evidence.recording !== 'object' || Array.isArray(evidence.recording)) {
    errors.push('recording must be an object');
  } else {
    requireString(errors, evidence.recording, 'recording.name', 'name');
    requireMinNumber(errors, evidence.recording.durationSeconds, 3, 'recording.durationSeconds');
    requireMinNumber(errors, evidence.recording.ffprobeVideoPackets, 1, 'recording.ffprobeVideoPackets');
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

  if (evidence.evidenceType === 'formal-simulator-acceptance') {
    validateSimulatorAcceptance(errors, evidence.acceptance);
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

function validateSimulatorAcceptance(errors, acceptance) {
  if (!acceptance || typeof acceptance !== 'object' || Array.isArray(acceptance)) {
    errors.push('acceptance must be an object for formal-simulator-acceptance evidence');
    return;
  }
  for (const field of ['acceptedBy', 'acceptedAt', 'rationale', 'upstreamValidatedEvidencePath']) {
    requireString(errors, acceptance, `acceptance.${field}`, field);
  }
  if (acceptance.acceptedAt && Number.isNaN(Date.parse(acceptance.acceptedAt))) {
    errors.push('acceptance.acceptedAt must be an ISO-compatible date string');
  }
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

function summarizeManualLiveTvEvidence(report) {
  return {
    directory: report.directory,
    templatePath: report.templatePath,
    validCount: report.valid.length,
    invalidCount: report.invalid.length,
    validRuns: report.valid.map((entry) => ({
      evidenceType: entry.evidence.evidenceType,
      tunerType: entry.evidence.tunerType,
      clientName: entry.evidence.clientName,
      clientVersion: entry.evidence.clientVersion,
      deviceName: entry.evidence.deviceName,
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
    evidenceType: 'real-tuner-device',
    tunerType: 'HDHomeRun',
    clientName: 'Jellyfin Web',
    clientVersion: 'replace-with-client-version',
    deviceName: 'replace-with-browser-or-device-name',
    testedAt: new Date().toISOString(),
    tester: 'replace-with-tester',
    jellyrinBaseUrl: 'http://replace-with-jellyrin-host:8097',
    result: 'pass',
    server: {
      version: 'replace-with-server-version',
      commit: 'replace-with-git-commit',
    },
    stream: {
      directStatus: 200,
      hlsMasterStatus: 200,
      hlsSegmentStatus: 200,
      playbackSeconds: 10,
    },
    recording: {
      name: 'replace-with-recording-name',
      durationSeconds: 3,
      ffprobeVideoPackets: 1,
    },
    flow: Object.fromEntries(requiredFlowChecks.map((check) => [check, true])),
    acceptance: {
      acceptedBy: '',
      acceptedAt: '',
      rationale: '',
      upstreamValidatedEvidencePath: '',
    },
    artifacts: [
      {
        type: 'screenshot',
        pathOrUrl: 'manual/livetv-real/artifacts/replace-with-capture.png',
      },
    ],
    notes: 'For formal simulator acceptance, set evidenceType=formal-simulator-acceptance and fill acceptance.',
  };
}

module.exports = {
  loadManualLiveTvEvidence,
  summarizeManualLiveTvEvidence,
  manualEvidenceTemplate,
};

if (require.main === module) {
  loadManualLiveTvEvidence()
    .then((report) => {
      console.log(JSON.stringify(summarizeManualLiveTvEvidence(report), null, 2));
    })
    .catch((error) => {
      console.error(error);
      process.exit(1);
    });
}
