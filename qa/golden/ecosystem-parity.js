#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

const validStatuses = new Set([
  'not-started',
  'designed',
  'implemented',
  'upstream-validated',
  'device-validated',
  'release-ready',
  'stub-compatible',
  'synthetic-only',
  'best-effort',
  'unsupported-decided',
]);

const closingStatuses = new Set([
  'implemented',
  'upstream-validated',
  'device-validated',
  'release-ready',
]);

const blockedClosureStatuses = new Set([
  'stub-compatible',
  'synthetic-only',
  'best-effort',
  'unsupported-decided',
]);

const gateDefinitions = [
  {
    id: 'ecosystem-harness',
    evolutive: 'E0',
    plan: '0027-e0-ecosystem-harness-dashboard-plan.md',
    targetStatus: 'implemented',
    command: 'npm run evidence:ecosystem',
    evidencePath: 'plans/generated/ecosystem-parity.json',
    openRisks: [],
  },
  {
    id: 'syncplay-advanced',
    evolutive: 'E4',
    plan: '0028-e4-syncplay-advanced-plan.md',
    targetStatus: 'upstream-validated',
    command: 'npm run golden:syncplay',
    evidencePath: 'plans/generated/syncplay-advanced',
    openRisks: ['Timing, websocket ordering and drift can be flaky without a controlled latency harness.'],
  },
  {
    id: 'non-web-clients',
    evolutive: 'E6',
    plan: '0029-e6-non-web-clients-plan.md',
    targetStatus: 'device-validated',
    command: 'npm run golden:clients',
    evidencePath: 'plans/generated/non-web-clients',
    openRisks: ['Some clients may require manual-repeatable validation if automation is not practical.'],
  },
  {
    id: 'livetv-real',
    evolutive: 'E2',
    plan: '0030-e2-live-tv-real-plan.md',
    targetStatus: 'device-validated',
    command: 'npm run golden:livetv',
    evidencePath: 'plans/generated/livetv-real',
    openRisks: ['Real tuner validation may need HDHomeRun hardware; simulator evidence must stay explicit.'],
  },
  {
    id: 'dlna-upnp',
    evolutive: 'E3',
    plan: '0031-e3-dlna-upnp-plan.md',
    targetStatus: 'device-validated',
    command: 'npm run golden:dlna',
    evidencePath: 'plans/generated/dlna-upnp',
    openRisks: ['Multicast behavior depends on firewall, network interfaces and service sandboxing.'],
  },
  {
    id: 'plugin-dual-runtime',
    evolutive: 'E1',
    plan: '0025-plugin-dual-runtime-parity-plan.md',
    targetStatus: 'release-ready',
    command: 'npm run golden:plugins',
    evidencePath: 'plans/generated/plugin-dual-runtime',
    openRisks: ['DotNetJellyfin sidecar compatibility depends on Jellyfin internal service coverage.'],
  },
  {
    id: 'channels-providers',
    evolutive: 'E5',
    plan: '0032-e5-channels-providers-plan.md',
    targetStatus: 'upstream-validated',
    command: 'npm run golden:channels',
    evidencePath: 'plans/generated/channels-providers',
    openRisks: ['External provider parity depends on the plugin runtime and provider failure isolation.'],
  },
  {
    id: 'ecosystem-release',
    evolutive: 'E7',
    plan: '0033-e7-ecosystem-operations-release-plan.md',
    targetStatus: 'release-ready',
    command: 'npm run golden:ecosystem-release',
    evidencePath: 'plans/generated/ecosystem-release',
    openRisks: ['Release closure depends on all previous ecosystem gates and process hardening.'],
  },
];

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const gates = await Promise.all(gateDefinitions.map(buildGate));
  const invalidStatusGates = gates.filter((gate) => !validStatuses.has(gate.status));
  const invalidClosedGates = gates.filter((gate) => gate.rawClosed && blockedClosureStatuses.has(gate.status));
  const closedGates = gates.filter((gate) => gate.closed).length;
  const completionPercent = percent(closedGates, gates.length);

  const dashboard = {
    generatedAt: new Date().toISOString(),
    plansDir,
    statusModel: {
      validStatuses: [...validStatuses],
      closingStatuses: [...closingStatuses],
      blockedClosureStatuses: [...blockedClosureStatuses],
    },
    summary: {
      totalGates: gates.length,
      closedGates,
      completionPercent,
      invalidStatusGates: invalidStatusGates.length,
      invalidClosedGates: invalidClosedGates.length,
    },
    gates,
    nextActions: buildNextActions(gates),
  };

  await fs.writeFile(
    path.join(generatedDir, 'ecosystem-parity.json'),
    `${JSON.stringify(dashboard, null, 2)}\n`,
  );
  await fs.writeFile(
    path.join(generatedDir, 'ecosystem-parity.md'),
    renderMarkdown(dashboard),
  );
  console.log(`wrote ${path.join(generatedDir, 'ecosystem-parity.md')}`);

  if (invalidStatusGates.length > 0 || invalidClosedGates.length > 0) {
    process.exitCode = 1;
  }
}

async function buildGate(definition) {
  const planPath = path.join(plansDir, definition.plan);
  const planExists = await exists(planPath);
  const generatedEvidencePath = path.join(generatedDir, `${definition.id}.json`);
  const externalEvidence = await readJsonIfExists(generatedEvidencePath);

  if (definition.evolutive === 'E0') {
    return {
      ...definition,
      status: planExists ? 'implemented' : 'not-started',
      closed: planExists,
      rawClosed: planExists,
      percent: planExists ? 100 : 0,
      evidence: planExists
        ? `Plan exists and this dashboard generated ${path.relative(plansDir, path.join(generatedDir, 'ecosystem-parity.json'))}`
        : 'missing E0 plan',
      planExists,
      generatedEvidence: path.relative(plansDir, generatedEvidencePath),
      evidenceSourcePhase: '0027',
      lastUpdatedAt: new Date().toISOString(),
      openRisks: definition.openRisks,
    };
  }

  if (externalEvidence) {
    const status = externalEvidence.status || 'not-started';
    const targetReached = status === definition.targetStatus;
    const rawClosed = Boolean(externalEvidence.closed);
    return {
      ...definition,
      status,
      closed: targetReached,
      rawClosed,
      percent: targetReached ? 100 : Number(externalEvidence.percent || 0),
      evidence: externalEvidence.evidence || 'external ecosystem evidence found',
      planExists,
      generatedEvidence: path.relative(plansDir, generatedEvidencePath),
      evidenceSourcePhase: externalEvidence.sourcePhase || externalEvidence.evidenceSourcePhase || 'external',
      lastUpdatedAt: externalEvidence.updatedAt || externalEvidence.lastUpdatedAt || null,
      openRisks: externalEvidence.openRisks || definition.openRisks,
    };
  }

  return {
    ...definition,
    status: 'not-started',
    closed: false,
    rawClosed: false,
    percent: 0,
    evidence: planExists
      ? 'plan exists; no new ecosystem evidence generated yet'
      : 'missing canonical plan',
    planExists,
    generatedEvidence: path.relative(plansDir, generatedEvidencePath),
    evidenceSourcePhase: '0027',
    lastUpdatedAt: null,
    openRisks: definition.openRisks,
  };
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') {
      return false;
    }
    throw error;
  }
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

function buildNextActions(gates) {
  const actions = [];
  const missingPlans = gates.filter((gate) => !gate.planExists);
  if (missingPlans.length > 0) {
    actions.push(`Create missing canonical plans: ${missingPlans.map((gate) => gate.evolutive).join(', ')}.`);
  }
  const open = gates.filter((gate) => gate.evolutive !== 'E0' && !gate.closed);
  if (open.length > 0) {
    const gate = open[0];
    const verb = gate.status === 'not-started' || Number(gate.percent || 0) === 0 ? 'Start' : 'Continue';
    actions.push(`${verb} next roadmap gate: ${gate.evolutive} ${gate.id} via ${gate.command}.`);
  }
  actions.push('Do not inherit 0023 unsupported-decided evidence as closure for ecosystem gates.');
  actions.push('Keep plans/generated artifacts outside git; commit only harness/generator changes.');
  return actions;
}

function percent(value, total) {
  if (!total) {
    return 0;
  }
  return Number(((value / total) * 100).toFixed(1));
}

function renderMarkdown(dashboard) {
  const lines = [];
  lines.push('# Ecosystem Parity Dashboard');
  lines.push('');
  lines.push(`Generated: ${dashboard.generatedAt}`);
  lines.push(`Plans dir: \`${dashboard.plansDir}\``);
  lines.push('');
  lines.push('## Summary');
  lines.push('');
  lines.push(`- Closed gates: ${dashboard.summary.closedGates}/${dashboard.summary.totalGates}`);
  lines.push(`- Completion: ${dashboard.summary.completionPercent}%`);
  lines.push(`- Invalid status gates: ${dashboard.summary.invalidStatusGates}`);
  lines.push(`- Invalid closed gates: ${dashboard.summary.invalidClosedGates}`);
  lines.push('');
  lines.push('## Gates');
  lines.push('');
  lines.push('| Evolutive | Gate | Status | Target | Progress | Plan | Evidence | Risks | Command |');
  lines.push('| --- | --- | --- | --- | ---: | --- | --- | --- | --- |');
  for (const gate of dashboard.gates) {
    lines.push(`| ${gate.evolutive} | ${gate.id} | \`${gate.status}\` | \`${gate.targetStatus}\` | ${gate.percent}% | \`${gate.plan}\` | ${gate.evidence} | ${gate.openRisks.join('; ') || 'none'} | \`${gate.command}\` |`);
  }
  lines.push('');
  lines.push('## Status Rules');
  lines.push('');
  lines.push(`- Valid statuses: ${dashboard.statusModel.validStatuses.map((status) => `\`${status}\``).join(', ')}`);
  lines.push(`- Closing statuses: ${dashboard.statusModel.closingStatuses.map((status) => `\`${status}\``).join(', ')}`);
  lines.push(`- Blocked closure statuses: ${dashboard.statusModel.blockedClosureStatuses.map((status) => `\`${status}\``).join(', ')}`);
  lines.push('- A gate closes only when its status exactly matches its target status.');
  lines.push('- Ecosystem gates cannot close with inherited 0023 decision-only evidence.');
  lines.push('');
  lines.push('## Next Actions');
  lines.push('');
  for (const action of dashboard.nextActions) {
    lines.push(`- ${action}`);
  }
  lines.push('');
  lines.push('## Secret Hygiene');
  lines.push('');
  lines.push('- This dashboard reads plan metadata and generated ecosystem summaries only.');
  lines.push('- It does not copy raw request logs, websocket payloads, tokens or passwords.');
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
