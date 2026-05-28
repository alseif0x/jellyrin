#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');
const {
  requiredProfiles,
  buildCompatibilityMatrix,
  loadManualDeviceEvidence,
} = require('./non-web-device-evidence');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const traceDir = path.join(generatedDir, 'e2e-traces', 'non-web-client');
const comparisonPath = path.join(traceDir, 'comparison.json');
const evidencePath = path.join(generatedDir, 'non-web-clients.json');
const evidenceMarkdownPath = path.join(generatedDir, 'non-web-clients.md');
const matrixPath = path.join(generatedDir, 'non-web-client-matrix.json');
const matrixMarkdownPath = path.join(generatedDir, 'non-web-client-matrix.md');

const baselineEvidence = {
  gate: 'non-web-clients',
  status: 'implemented',
  percent: 25,
  closed: false,
  sourcePhase: 'E6.1a',
  evidence: 'MPV Shim/Jellyfin Media Player contract flow is wired but has not completed against upstream and Jellyrin yet.',
  openRisks: [
    'Device validation still requires at least one real non-web client playback run.',
    'Kodi, Android TV, Android mobile, Swiftfin/iOS and Roku contracts still need dedicated client profiles.',
  ],
};

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const result = await runBrowserTrace();
  const comparison = await readJsonIfExists(comparisonPath);
  const manualEvidence = await loadManualDeviceEvidence();
  const completedProfiles = completedProfilesFromComparison(comparison);
  const compatibilityMatrix = buildCompatibilityMatrix(completedProfiles, manualEvidence);
  const evidence = buildEvidence(result, comparison, manualEvidence, compatibilityMatrix);

  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(evidenceMarkdownPath, renderMarkdown(evidence, comparison, manualEvidence));
  await fs.writeFile(matrixPath, `${JSON.stringify(compatibilityMatrix, null, 2)}\n`);
  await fs.writeFile(matrixMarkdownPath, renderMatrixMarkdown(compatibilityMatrix));
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);
  console.log(`wrote ${matrixPath}`);
  console.log(`wrote ${matrixMarkdownPath}`);

  if (evidence.status !== 'upstream-validated' && evidence.status !== 'device-validated') {
    process.exitCode = result.code || 1;
  }
}

function runBrowserTrace() {
  return new Promise((resolve) => {
    const child = spawn(
      process.execPath,
      [path.join(__dirname, 'browser-trace.js')],
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          JELLYRIN_BROWSER_FLOW: 'non-web-client',
        },
        stdio: 'inherit',
      },
    );
    child.on('close', (code, signal) => resolve({ code: code || 0, signal }));
  });
}

function buildEvidence(result, comparison, manualEvidence, compatibilityMatrix) {
  const updatedAt = new Date().toISOString();
  const deviceEvidence = summarizeManualEvidence(manualEvidence);
  const matrixEvidence = summarizeCompatibilityMatrix(compatibilityMatrix);
  if (!comparison) {
    return {
      ...baselineEvidence,
      updatedAt,
      deviceEvidence,
      matrixEvidence,
      evidence: `${baselineEvidence.evidence} Browser trace did not produce comparison.json.`,
    };
  }

  const summaries = comparison.summaries || [];
  const completedTargets = summaries
    .filter((summary) => summary.status === 'completed')
    .map((summary) => summary.target)
    .sort();
  const skippedTargets = summaries
    .filter((summary) => summary.skipped)
    .map((summary) => summary.target)
    .sort();
  const failedTargets = summaries
    .filter((summary) => summary.status === 'failed')
    .map((summary) => summary.target)
    .sort();
  const failed = Boolean(comparison.comparison?.failed);
  const completedProfiles = commonCompletedProfiles(summaries);

  const contractsComplete = !failed
    && completedTargets.includes('jellyrin')
    && completedTargets.includes('upstream')
    && completedProfiles.length === requiredProfiles.length;
  if (contractsComplete && deviceEvidence.validCount > 0) {
    return {
      gate: 'non-web-clients',
      status: 'device-validated',
      percent: 100,
      closed: true,
      sourcePhase: 'E6.3',
      evidence: `Non-web client contracts completed against upstream and Jellyrin, and ${deviceEvidence.validCount} real client playback evidence file(s) passed validation.`,
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      completedProfiles,
      deviceEvidence,
      matrixEvidence,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [],
    };
  }

  if (contractsComplete) {
    return {
      gate: 'non-web-clients',
      status: 'upstream-validated',
      percent: matrixEvidence.hasBestEffort ? 80 : 90,
      closed: false,
      sourcePhase: 'E6.3',
      evidence: matrixEvidence.hasBestEffort
        ? 'Non-web client contract golden completed against upstream and Jellyrin; manual device evidence intake is ready.'
        : 'Non-web client contract golden completed against upstream and Jellyrin, manual device evidence intake is ready, and the generated client matrix has no best-effort entries.',
      updatedAt,
      completedTargets,
      skippedTargets,
      failedTargets,
      completedProfiles,
      deviceEvidence,
      matrixEvidence,
      tracePath: path.relative(plansDir, comparisonPath),
      openRisks: [
        'Dashboard target remains device-validated; run real playback sessions from representative non-web clients before closing E6.',
        `Add at least one passing device evidence JSON under ${deviceEvidence.directory}.`,
        'Contract profiles validate Jellyfin-compatible API shape, but client-version-specific behavior still needs manual/device evidence.',
      ],
    };
  }

  const jellyrinCompleted = completedTargets.includes('jellyrin');
  return {
    ...baselineEvidence,
    percent: jellyrinCompleted ? 35 : baselineEvidence.percent,
    updatedAt,
    evidence: jellyrinCompleted
      ? 'Jellyrin non-web client contract trace completed, but E6 is not upstream-validated yet.'
      : `${baselineEvidence.evidence} Browser trace did not complete enough targets for E6 progress.`,
    completedTargets,
    skippedTargets,
    failedTargets,
    completedProfiles,
    deviceEvidence,
    matrixEvidence,
    failedReasons: comparison.comparison?.reasons || [],
    traceExitCode: result.code,
    tracePath: path.relative(plansDir, comparisonPath),
  };
}

function completedProfilesFromComparison(comparison) {
  const summaries = comparison?.summaries || [];
  return commonCompletedProfiles(summaries);
}

function summarizeManualEvidence(manualEvidence) {
  return {
    directory: manualEvidence.directory,
    templatePath: manualEvidence.templatePath,
    validCount: manualEvidence.valid.length,
    invalidCount: manualEvidence.invalid.length,
    validClients: manualEvidence.valid.map((entry) => ({
      clientId: entry.evidence.clientId,
      clientName: entry.evidence.clientName,
      clientVersion: entry.evidence.clientVersion,
      deviceName: entry.evidence.deviceName,
      platform: entry.evidence.platform,
      testedAt: entry.evidence.testedAt,
      file: entry.relativePath,
    })),
    invalidFiles: manualEvidence.invalid.map((entry) => ({
      file: entry.relativePath,
      errors: entry.errors,
    })),
  };
}

function summarizeCompatibilityMatrix(matrix) {
  const statusCounts = {};
  for (const client of matrix.clients) {
    statusCounts[client.currentStatus] = (statusCounts[client.currentStatus] || 0) + 1;
  }
  return {
    path: path.relative(plansDir, matrixPath),
    markdownPath: path.relative(plansDir, matrixMarkdownPath),
    clientCount: matrix.clients.length,
    hasBestEffort: matrix.hasBestEffort,
    statusCounts,
  };
}

function commonCompletedProfiles(summaries) {
  const profileSets = summaries
    .filter((summary) => summary.status === 'completed')
    .map((summary) => new Set(summary.invariants?.nonWebClientProfiles || []));
  if (profileSets.length === 0) {
    return [];
  }
  return requiredProfiles.filter((profile) => profileSets.every((profiles) => profiles.has(profile)));
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

function renderMarkdown(evidence, comparison, manualEvidence) {
  const lines = [];
  lines.push('# Non-Web Clients Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push(`Gate: ${evidence.gate}`);
  lines.push(`Status: ${evidence.status}`);
  lines.push(`Percent: ${evidence.percent}`);
  lines.push(`Closed: ${evidence.closed}`);
  lines.push(`Source phase: ${evidence.sourcePhase}`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(evidence.evidence);
  lines.push('');
  if (evidence.tracePath) {
    lines.push(`- Trace: \`${evidence.tracePath}\``);
  }
  if (Array.isArray(evidence.completedTargets)) {
    lines.push(`- Completed targets: ${evidence.completedTargets.join(', ') || 'none'}`);
  }
  if (Array.isArray(evidence.failedTargets)) {
    lines.push(`- Failed targets: ${evidence.failedTargets.join(', ') || 'none'}`);
  }
  if (Array.isArray(evidence.completedProfiles)) {
    lines.push(`- Completed client profiles: ${evidence.completedProfiles.join(', ') || 'none'}`);
  }
  if (evidence.deviceEvidence) {
    lines.push(`- Manual evidence directory: \`${evidence.deviceEvidence.directory}\``);
    lines.push(`- Manual evidence template: \`${evidence.deviceEvidence.templatePath}\``);
    lines.push(`- Valid manual evidence files: ${evidence.deviceEvidence.validCount}`);
    lines.push(`- Invalid manual evidence files: ${evidence.deviceEvidence.invalidCount}`);
  }
  if (evidence.matrixEvidence) {
    lines.push(`- Compatibility matrix: \`${evidence.matrixEvidence.path}\``);
    lines.push(`- Compatibility matrix markdown: \`${evidence.matrixEvidence.markdownPath}\``);
    lines.push(`- Matrix clients: ${evidence.matrixEvidence.clientCount}`);
    lines.push(`- Matrix has best-effort: ${evidence.matrixEvidence.hasBestEffort}`);
  }
  lines.push('');
  if (manualEvidence?.valid?.length) {
    lines.push('## Manual Device Evidence');
    lines.push('');
    for (const entry of manualEvidence.valid) {
      lines.push(`- ${entry.evidence.clientName} ${entry.evidence.clientVersion} on ${entry.evidence.platform}: \`${entry.relativePath}\``);
    }
    lines.push('');
  }
  if (manualEvidence?.invalid?.length) {
    lines.push('## Invalid Manual Evidence');
    lines.push('');
    for (const entry of manualEvidence.invalid) {
      lines.push(`- \`${entry.relativePath}\`: ${entry.errors.join('; ')}`);
    }
    lines.push('');
  }
  if (Array.isArray(evidence.failedReasons) && evidence.failedReasons.length > 0) {
    lines.push('## Failed Reasons');
    lines.push('');
    for (const reason of evidence.failedReasons) {
      lines.push(`- ${reason}`);
    }
    lines.push('');
  }
  if (Array.isArray(evidence.openRisks) && evidence.openRisks.length > 0) {
    lines.push('## Open Risks');
    lines.push('');
    for (const risk of evidence.openRisks) {
      lines.push(`- ${risk}`);
    }
    lines.push('');
  }
  if (comparison?.comparison?.reasons?.length) {
    lines.push('## Comparison Reasons');
    lines.push('');
    for (const reason of comparison.comparison.reasons) {
      lines.push(`- ${reason}`);
    }
    lines.push('');
  }
  return `${lines.join('\n')}\n`;
}

function renderMatrixMarkdown(matrix) {
  const lines = [];
  lines.push('# Non-Web Client Compatibility Matrix');
  lines.push('');
  lines.push(`Generated: ${matrix.generatedAt}`);
  lines.push(`Has best-effort: ${matrix.hasBestEffort}`);
  lines.push('');
  lines.push('| Client | Priority | Status | Version | Evidence |');
  lines.push('| --- | --- | --- | --- | --- |');
  for (const client of matrix.clients) {
    lines.push(`| ${client.clientName} | ${client.priority} | ${client.currentStatus} | ${client.version} | ${client.manualEvidence || client.validationCommand} |`);
  }
  lines.push('');
  lines.push('## Details');
  lines.push('');
  for (const client of matrix.clients) {
    lines.push(`### ${client.clientName}`);
    lines.push('');
    lines.push(`- Client ID: \`${client.clientId}\``);
    lines.push(`- Auth: ${client.authMethod}`);
    lines.push(`- Discovery: ${client.discoveryMethod}`);
    lines.push(`- Playback modes: ${client.requiredPlaybackModes.join(', ')}`);
    lines.push(`- Direct-play formats: ${client.directPlayFormats.join(', ')}`);
    lines.push(`- Transcode: ${client.transcodeRequirements.join(', ')}`);
    lines.push(`- Subtitles: ${client.subtitleRequirements.join(', ')}`);
    lines.push(`- Known quirks: ${client.knownQuirks.join('; ')}`);
    lines.push('');
  }
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
