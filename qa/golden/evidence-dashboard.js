#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

const statusRank = {
  'upstream-validated': 5,
  'web-validated': 4,
  implemented: 3,
  'route-compatible': 3,
  partial: 2,
  'stub-compatible': 1,
  planned: 0,
  pending: 0,
};

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });

  const sources = await loadSources();
  const routeSummary = summarizeRoutes(sources.routes);
  const dtoSummary = summarizeDtos(sources.dtos, sources.dtoFieldParity);
  const apiGolden = summarizeApiGolden(sources.apiGolden);
  const browserTraces = summarizeBrowserTraces(sources.browserTraces);
  const functionalAreas = summarizeFunctionalAreas(sources.functionalParity);
  const gates = buildGates(routeSummary, dtoSummary, apiGolden, browserTraces, functionalAreas);

  const dashboard = {
    generatedAt: new Date().toISOString(),
    plansDir,
    sources: sourceStatus(sources),
    routeSummary,
    dtoSummary,
    apiGolden,
    browserTraces,
    functionalAreas,
    gates,
    nextActions: nextActions(gates, dtoSummary, browserTraces),
  };

  await fs.writeFile(
    path.join(generatedDir, 'evidence-dashboard.json'),
    `${JSON.stringify(dashboard, null, 2)}\n`,
  );
  await fs.writeFile(
    path.join(generatedDir, 'evidence-dashboard.md'),
    renderMarkdown(dashboard),
  );
  console.log(`wrote ${path.join(generatedDir, 'evidence-dashboard.md')}`);
}

async function loadSources() {
  const browserTracesDir = path.join(generatedDir, 'e2e-traces');
  return {
    routes: await readJson('api-routes.json', []),
    dtos: await readJson('dto-coverage.json', []),
    dtoFieldParity: await readJson('dto-field-parity.json', null),
    apiGolden: await readJson(path.join('golden-traces', 'api-parity-latest.json'), null),
    functionalParity: await readText('functional-parity.md', ''),
    browserTraces: await readTraceComparisons(browserTracesDir),
  };
}

async function readJson(relativePath, fallback) {
  try {
    return JSON.parse(await fs.readFile(path.join(generatedDir, relativePath), 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') {
      return fallback;
    }
    throw error;
  }
}

async function readText(relativePath, fallback) {
  try {
    return await fs.readFile(path.join(generatedDir, relativePath), 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') {
      return fallback;
    }
    throw error;
  }
}

async function readTraceComparisons(browserTracesDir) {
  let entries = [];
  try {
    entries = await fs.readdir(browserTracesDir, { withFileTypes: true });
  } catch (error) {
    if (error.code === 'ENOENT') {
      return [];
    }
    throw error;
  }

  const traces = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) {
      continue;
    }
    const flow = entry.name;
    const comparisonPath = path.join(browserTracesDir, flow, 'comparison.json');
    try {
      const comparison = JSON.parse(await fs.readFile(comparisonPath, 'utf8'));
      traces.push({
        flow,
        generatedAt: comparison.generatedAt,
        failed: Boolean(comparison.comparison?.failed),
        reasons: comparison.comparison?.reasons || [],
        summaries: (comparison.summaries || []).map((summary) => ({
          target: summary.target,
          status: summary.status,
          skipped: Boolean(summary.skipped),
          reason: summary.reason || null,
          requests: summary.requests || 0,
          failedResponses: summary.failedResponses || [],
          pageErrors: summary.pageErrors || [],
          websockets: summary.websockets || 0,
          finalUrl: summary.finalUrl || null,
          playMethods: summary.invariants?.playMethods || [],
          websocketMessageTypes: summary.invariants?.websocketMessageTypes || [],
        })),
      });
    } catch (error) {
      if (error.code !== 'ENOENT') {
        throw error;
      }
    }
  }
  return traces.sort((a, b) => a.flow.localeCompare(b.flow));
}

function summarizeRoutes(routes) {
  const byStatus = countBy(routes, 'status');
  const total = routes.length;
  const implemented = routes.filter((route) => route.status === 'implemented' || route.status === 'web-validated').length;
  const upstreamValidated = routes.filter((route) => route.upstream_trace === 'upstream-validated').length;
  const webValidated = routes.filter((route) => route.web_trace === 'web-validated').length;
  const pendingUpstream = routes.filter((route) => route.upstream_trace === 'pending').length;
  const pendingWeb = routes.filter((route) => route.web_trace === 'pending').length;
  return {
    total,
    byStatus,
    implemented,
    implementedPercent: percent(implemented, total),
    webValidated,
    webValidatedPercent: percent(webValidated, total),
    upstreamValidated,
    upstreamValidatedPercent: percent(upstreamValidated, total),
    pendingWeb,
    pendingUpstream,
  };
}

function summarizeDtos(dtos, dtoFieldParity) {
  const byStatus = countBy(dtos, 'status');
  const highDangerPending = dtos.filter((dto) => dto.danger_if_wrong === 'high' && dto.status !== 'implemented');
  const pendingGolden = dtos.filter((dto) => String(dto.snapshot || '').includes('pending'));
  const implemented = dtos.filter((dto) => dto.status === 'implemented').length;
  const golden = dtoFieldParity?.summary || {};
  return {
    total: dtos.length,
    byStatus,
    implemented,
    implementedPercent: percent(implemented, dtos.length),
    highDangerPending: highDangerPending.length,
    pendingGolden: pendingGolden.length,
    goldenStatus: dtoFieldParity ? 'available' : 'missing',
    goldenValidated: golden.upstreamValidated || 0,
    goldenValidatedPercent: golden.upstreamValidatedPercent || 0,
    partialGolden: golden.partialGolden || 0,
    missingGoldenEvidence: golden.missingEvidence ?? pendingGolden.length,
    topHighDangerPending: highDangerPending.slice(0, 10).map((dto) => `${dto.dto_family}.${dto.field}`),
  };
}

function summarizeApiGolden(apiGolden) {
  if (!apiGolden) {
    return { status: 'missing' };
  }
  const summary = apiGolden.summary || {};
  return {
    status: summary.failed === 0 ? 'upstream-validated' : 'partial',
    mode: summary.mode || apiGolden.mode || 'unknown',
    authenticated: Boolean(summary.authenticated),
    total: summary.total || 0,
    passed: summary.passed || 0,
    failed: summary.failed || 0,
    skipped: summary.skipped || 0,
    strictEvaluated: summary.strictEvaluated || 0,
    authMethods: summary.authMethods || {},
  };
}

function summarizeBrowserTraces(traces) {
  return traces.map((trace) => {
    const completedTargets = trace.summaries.filter((summary) => summary.status === 'completed');
    const targetNames = completedTargets.map((summary) => summary.target).sort();
    const status = trace.failed
      ? 'partial'
      : targetNames.includes('upstream') && targetNames.includes('jellyrin')
        ? 'upstream-validated'
        : completedTargets.length > 0
          ? 'web-validated'
          : 'pending';
    return {
      flow: trace.flow,
      status,
      generatedAt: trace.generatedAt,
      completedTargets: targetNames,
      failed: trace.failed,
      reasons: trace.reasons,
      summaries: trace.summaries,
    };
  });
}

function summarizeFunctionalAreas(markdown) {
  const rows = [];
  for (const line of markdown.split('\n')) {
    if (!line.startsWith('|') || line.includes('---') || line.includes('Area | Status')) {
      continue;
    }
    const cells = line.split('|').slice(1, -1).map((cell) => cell.trim());
    if (cells.length < 4) {
      continue;
    }
    rows.push({
      area: stripTicks(cells[0]),
      status: stripTicks(cells[1]),
      evidence: stripTicks(cells[2]),
      mainGap: stripTicks(cells[3]),
    });
  }
  return rows;
}

function buildGates(routeSummary, dtoSummary, apiGolden, browserTraces, functionalAreas) {
  const startupWizard = browserTraces.find((trace) => trace.flow === 'startup-wizard');
  const p0Direct = browserTraces.find((trace) => trace.flow === 'p0-direct-play');
  const loginHome = browserTraces.find((trace) => trace.flow === 'login-home');
  const resume = browserTraces.find((trace) => trace.flow === 'resume');
  const transcodeHls = browserTraces.find((trace) => trace.flow === 'transcode-hls');
  const adminDashboard = browserTraces.find((trace) => trace.flow === 'admin-dashboard');
  const libraries = browserTraces.find((trace) => trace.flow === 'libraries');
  const subtitlesTrickplay = browserTraces.find((trace) => trace.flow === 'subtitles-trickplay');
  const audioHlsLegacy = browserTraces.find((trace) => trace.flow === 'audio-hls-legacy');
  const music = browserTraces.find((trace) => trace.flow === 'music');
  const series = browserTraces.find((trace) => trace.flow === 'series');
  const playlistsCollections = browserTraces.find((trace) => trace.flow === 'playlists-collections');
  const images = browserTraces.find((trace) => trace.flow === 'images');
  const metadataSearch = browserTraces.find((trace) => trace.flow === 'metadata-search');
  const authUsers = browserTraces.find((trace) => trace.flow === 'auth-users');
  const sessionsWebsocket = browserTraces.find((trace) => trace.flow === 'sessions-websocket');
  const syncplay = browserTraces.find((trace) => trace.flow === 'syncplay');
  const pluginsPackages = browserTraces.find((trace) => trace.flow === 'plugins-packages');
  const liveTv = browserTraces.find((trace) => trace.flow === 'live-tv');
  return [
    {
      id: 'routes',
      status: routeSummary.implemented === routeSummary.total ? 'route-compatible' : 'partial',
      evidence: `${routeSummary.implemented}/${routeSummary.total} implemented or web-validated`,
    },
    {
      id: 'api-golden-strict',
      status: apiGolden.status || 'missing',
      evidence: `${apiGolden.passed || 0}/${apiGolden.total || 0} passed, failed=${apiGolden.failed || 0}, strict=${apiGolden.strictEvaluated || 0}`,
    },
    {
      id: 'browser-startup-wizard',
      status: startupWizard?.status || 'pending',
      evidence: startupWizard
        ? `${startupWizard.completedTargets.join(',') || 'none'} completed, failed=${startupWizard.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-p0-direct-play',
      status: p0Direct?.status || 'pending',
      evidence: p0Direct
        ? `${p0Direct.completedTargets.join(',') || 'none'} completed, failed=${p0Direct.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-login-home',
      status: loginHome?.status || 'pending',
      evidence: loginHome
        ? `${loginHome.completedTargets.join(',') || 'none'} completed, failed=${loginHome.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-resume',
      status: resume?.status || 'pending',
      evidence: resume
        ? `${resume.completedTargets.join(',') || 'none'} completed, failed=${resume.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-transcode-hls',
      status: transcodeHls?.status || 'pending',
      evidence: transcodeHls
        ? `${transcodeHls.completedTargets.join(',') || 'none'} completed, failed=${transcodeHls.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-admin-dashboard',
      status: adminDashboard?.status || 'pending',
      evidence: adminDashboard
        ? `${adminDashboard.completedTargets.join(',') || 'none'} completed, failed=${adminDashboard.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-libraries',
      status: libraries?.status || 'pending',
      evidence: libraries
        ? `${libraries.completedTargets.join(',') || 'none'} completed, failed=${libraries.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-subtitles-trickplay',
      status: subtitlesTrickplay?.status || 'pending',
      evidence: subtitlesTrickplay
        ? `${subtitlesTrickplay.completedTargets.join(',') || 'none'} completed, failed=${subtitlesTrickplay.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-audio-hls-legacy',
      status: audioHlsLegacy?.status || 'pending',
      evidence: audioHlsLegacy
        ? `${audioHlsLegacy.completedTargets.join(',') || 'none'} completed, failed=${audioHlsLegacy.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-music',
      status: music?.status || 'pending',
      evidence: music
        ? `${music.completedTargets.join(',') || 'none'} completed, failed=${music.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-series',
      status: series?.status || 'pending',
      evidence: series
        ? `${series.completedTargets.join(',') || 'none'} completed, failed=${series.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-playlists-collections',
      status: playlistsCollections?.status || 'pending',
      evidence: playlistsCollections
        ? `${playlistsCollections.completedTargets.join(',') || 'none'} completed, failed=${playlistsCollections.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-images',
      status: images?.status || 'pending',
      evidence: images
        ? `${images.completedTargets.join(',') || 'none'} completed, failed=${images.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-metadata-search',
      status: metadataSearch?.status || 'pending',
      evidence: metadataSearch
        ? `${metadataSearch.completedTargets.join(',') || 'none'} completed, failed=${metadataSearch.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-auth-users',
      status: authUsers?.status || 'pending',
      evidence: authUsers
        ? `${authUsers.completedTargets.join(',') || 'none'} completed, failed=${authUsers.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-sessions-websocket',
      status: sessionsWebsocket?.status || 'pending',
      evidence: sessionsWebsocket
        ? `${sessionsWebsocket.completedTargets.join(',') || 'none'} completed, failed=${sessionsWebsocket.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-syncplay',
      status: syncplay?.status || 'pending',
      evidence: syncplay
        ? `${syncplay.completedTargets.join(',') || 'none'} completed, failed=${syncplay.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-plugins-packages',
      status: pluginsPackages?.status || 'pending',
      evidence: pluginsPackages
        ? `${pluginsPackages.completedTargets.join(',') || 'none'} completed, failed=${pluginsPackages.failed}`
        : 'missing trace',
    },
    {
      id: 'browser-live-tv',
      status: liveTv?.status || 'pending',
      evidence: liveTv
        ? `${liveTv.completedTargets.join(',') || 'none'} completed, failed=${liveTv.failed}`
        : 'missing trace',
    },
    {
      id: 'dto-field-parity',
      status: dtoSummary.missingGoldenEvidence === 0 && dtoSummary.partialGolden === 0 ? 'upstream-validated' : 'partial',
      evidence: `${dtoSummary.goldenValidated}/${dtoSummary.total} DTO fields upstream-validated, partial=${dtoSummary.partialGolden}, missing=${dtoSummary.missingGoldenEvidence}`,
    },
    {
      id: 'functional-areas',
      status: aggregateFunctionalStatus(functionalAreas),
      evidence: `${functionalAreas.length} tracked areas`,
    },
  ];
}

function aggregateFunctionalStatus(areas) {
  if (areas.length === 0) {
    return 'pending';
  }
  const statuses = areas.map((area) => area.status);
  if (statuses.every((status) => ['implemented', 'web-validated', 'upstream-validated', 'unsupported-decided'].includes(status))) {
    return 'upstream-validated';
  }
  if (statuses.some((status) => status === 'partial')) {
    return 'partial';
  }
  return statuses.sort((a, b) => (statusRank[a] || 0) - (statusRank[b] || 0))[0];
}

function nextActions(gates, dtoSummary, browserTraces) {
  const actions = [];
  if (gates.find((gate) => gate.id === 'browser-login-home')?.status !== 'upstream-validated') {
    actions.push('Run login-home against both upstream and Jellyrin with the API-key preauth path.');
  }
  if (dtoSummary.missingGoldenEvidence > 0 || dtoSummary.partialGolden > 0) {
    actions.push('Close remaining G4 DTO field parity gaps with targeted traces for transcode, activity log and image info.');
  }
  const missingFlows = ['startup-wizard', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay', 'audio-hls-legacy', 'music', 'series', 'playlists-collections', 'images', 'metadata-search', 'auth-users', 'sessions-websocket', 'syncplay', 'plugins-packages', 'live-tv']
    .filter((flow) => !browserTraces.some((trace) => trace.flow === flow));
  if (missingFlows.length > 0) {
    actions.push(`Add browser traces for: ${missingFlows.join(', ')}.`);
  }
  actions.push('Keep plans/generated artifacts outside git; commit only harness/generator changes.');
  return actions;
}

function sourceStatus(sources) {
  return {
    routes: sources.routes.length > 0,
    dtos: sources.dtos.length > 0,
    dtoFieldParity: Boolean(sources.dtoFieldParity),
    apiGolden: Boolean(sources.apiGolden),
    functionalParity: sources.functionalParity.trim().length > 0,
    browserTraceComparisons: sources.browserTraces.length,
  };
}

function countBy(rows, key) {
  return rows.reduce((counts, row) => {
    const value = row[key] || 'missing';
    counts[value] = (counts[value] || 0) + 1;
    return counts;
  }, {});
}

function percent(value, total) {
  if (!total) {
    return 0;
  }
  return Number(((value / total) * 100).toFixed(1));
}

function stripTicks(value) {
  return value.replace(/^`|`$/g, '');
}

function renderMarkdown(dashboard) {
  const lines = [];
  lines.push('# Evidence Dashboard');
  lines.push('');
  lines.push(`Generated: ${dashboard.generatedAt}`);
  lines.push(`Plans dir: \`${dashboard.plansDir}\``);
  lines.push('');
  lines.push('## Gates');
  lines.push('');
  lines.push('| Gate | Status | Evidence |');
  lines.push('| --- | --- | --- |');
  for (const gate of dashboard.gates) {
    lines.push(`| ${gate.id} | \`${gate.status}\` | ${gate.evidence} |`);
  }
  lines.push('');
  lines.push('## Route Matrix');
  lines.push('');
  lines.push(`- Total routes: ${dashboard.routeSummary.total}`);
  lines.push(`- Implemented or web-validated: ${dashboard.routeSummary.implemented} (${dashboard.routeSummary.implementedPercent}%)`);
  lines.push(`- Web-validated route rows: ${dashboard.routeSummary.webValidated} (${dashboard.routeSummary.webValidatedPercent}%)`);
  lines.push(`- Upstream-validated route rows: ${dashboard.routeSummary.upstreamValidated} (${dashboard.routeSummary.upstreamValidatedPercent}%)`);
  lines.push(`- Pending web traces: ${dashboard.routeSummary.pendingWeb}`);
  lines.push(`- Pending upstream traces: ${dashboard.routeSummary.pendingUpstream}`);
  lines.push('');
  lines.push('## Golden API');
  lines.push('');
  lines.push(`- Status: \`${dashboard.apiGolden.status}\``);
  lines.push(`- Mode: \`${dashboard.apiGolden.mode}\``);
  lines.push(`- Results: ${dashboard.apiGolden.passed}/${dashboard.apiGolden.total} passed, ${dashboard.apiGolden.failed} failed, ${dashboard.apiGolden.skipped} skipped`);
  lines.push(`- Strict evaluated: ${dashboard.apiGolden.strictEvaluated}`);
  lines.push(`- Authenticated: ${dashboard.apiGolden.authenticated}`);
  lines.push('');
  lines.push('## Browser Traces');
  lines.push('');
  lines.push('| Flow | Status | Targets | Failed | Notes |');
  lines.push('| --- | --- | --- | --- | --- |');
  for (const trace of dashboard.browserTraces) {
    lines.push(`| ${trace.flow} | \`${trace.status}\` | ${trace.completedTargets.join(', ') || 'none'} | ${trace.failed} | ${trace.reasons.join('; ') || 'ok'} |`);
  }
  lines.push('');
  lines.push('## DTO Parity');
  lines.push('');
  lines.push(`- Implemented fields: ${dashboard.dtoSummary.implemented}/${dashboard.dtoSummary.total} (${dashboard.dtoSummary.implementedPercent}%)`);
  lines.push(`- Upstream/Jellyrin golden validated: ${dashboard.dtoSummary.goldenValidated}/${dashboard.dtoSummary.total} (${dashboard.dtoSummary.goldenValidatedPercent}%)`);
  lines.push(`- Partial golden evidence: ${dashboard.dtoSummary.partialGolden}`);
  lines.push(`- Missing golden evidence: ${dashboard.dtoSummary.missingGoldenEvidence}`);
  lines.push(`- High-danger pending fields: ${dashboard.dtoSummary.highDangerPending}`);
  if (dashboard.dtoSummary.topHighDangerPending.length > 0) {
    lines.push(`- Top high-danger pending: ${dashboard.dtoSummary.topHighDangerPending.join(', ')}`);
  }
  lines.push('');
  lines.push('## Functional Areas');
  lines.push('');
  lines.push('| Area | Status | Main gap |');
  lines.push('| --- | --- | --- |');
  for (const area of dashboard.functionalAreas) {
    lines.push(`| ${area.area} | \`${area.status}\` | ${area.mainGap} |`);
  }
  lines.push('');
  lines.push('## Next Actions');
  lines.push('');
  for (const action of dashboard.nextActions) {
    lines.push(`- ${action}`);
  }
  lines.push('');
  lines.push('## Secret Hygiene');
  lines.push('');
  lines.push('- This dashboard reads only comparison summaries and route/DTO metadata.');
  lines.push('- It does not copy raw request logs, websocket payloads, console logs, tokens or passwords.');
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
