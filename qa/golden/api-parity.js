#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const upstreamBaseUrl = trimTrailingSlash(process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096');
const jellyrinBaseUrl = trimTrailingSlash(process.env.JELLYRIN_URL || 'http://127.0.0.1:8097');
const outputPath = process.env.JELLYRIN_GOLDEN_OUT
  || path.resolve(__dirname, '../../../../plans/generated/golden-traces/api-parity-latest.json');

const publicCases = [
  { name: 'public-info', method: 'GET', path: '/System/Info/Public' },
  { name: 'public-users', method: 'GET', path: '/Users/Public' },
  { name: 'branding-configuration', method: 'GET', path: '/Branding/Configuration' },
  { name: 'branding-css', method: 'GET', path: '/Branding/Css.css', text: true },
  { name: 'localization-options', method: 'GET', path: '/Localization/Options' },
  { name: 'cultures', method: 'GET', path: '/Localization/Cultures' },
  { name: 'countries', method: 'GET', path: '/Localization/Countries' },
];

const authenticatedCases = [
  { name: 'system-info', method: 'GET', path: '/System/Info' },
  { name: 'users-me', method: 'GET', path: '/Users/Me' },
  { name: 'users', method: 'GET', path: '/Users' },
  { name: 'views', method: 'GET', path: '/UserViews' },
  { name: 'items-movies-first-page', method: 'GET', path: '/Items?IncludeItemTypes=Movie&StartIndex=0&Limit=5' },
  { name: 'sessions', method: 'GET', path: '/Sessions' },
  { name: 'scheduled-tasks', method: 'GET', path: '/ScheduledTasks' },
  { name: 'repositories', method: 'GET', path: '/Repositories' },
];

async function main() {
  const upstreamAuth = await authenticateFromEnv('JELLYFIN', upstreamBaseUrl);
  const jellyrinAuth = await authenticateFromEnv('JELLYRIN', jellyrinBaseUrl);
  const runAuthenticated = upstreamAuth && jellyrinAuth;
  const cases = runAuthenticated
    ? [...publicCases, ...authenticatedCases.map((testCase) => ({ ...testCase, authenticated: true }))]
    : publicCases;

  const results = [];
  for (const testCase of cases) {
    const upstream = await requestCase(upstreamBaseUrl, testCase, upstreamAuth);
    const jellyrin = await requestCase(jellyrinBaseUrl, testCase, jellyrinAuth);
    const comparison = compareResponses(upstream, jellyrin);
    results.push({
      name: testCase.name,
      method: testCase.method,
      path: testCase.path,
      authenticated: Boolean(testCase.authenticated),
      upstream,
      jellyrin,
      comparison,
    });
  }

  const summary = {
    total: results.length,
    passed: results.filter((result) => result.comparison.ok).length,
    failed: results.filter((result) => !result.comparison.ok).length,
    authenticated: runAuthenticated,
  };
  const report = {
    generatedAt: new Date().toISOString(),
    upstreamBaseUrl,
    jellyrinBaseUrl,
    summary,
    results,
  };

  await fs.mkdir(path.dirname(outputPath), { recursive: true });
  await fs.writeFile(outputPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`${summary.passed}/${summary.total} golden API cases matched`);
  console.log(`wrote ${outputPath}`);

  if (summary.failed > 0) {
    for (const result of results.filter((entry) => !entry.comparison.ok)) {
      console.error(`${result.name}: ${result.comparison.reason}`);
    }
    process.exitCode = 1;
  }
}

async function authenticateFromEnv(prefix, baseUrl) {
  const username = process.env[`${prefix}_ADMIN_USER`];
  const password = process.env[`${prefix}_ADMIN_PASSWORD`];
  if (!username || !password) {
    return null;
  }
  const response = await fetch(`${baseUrl}/Users/AuthenticateByName`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: 'MediaBrowser Client="Jellyrin Golden", Device="Harness", DeviceId="jellyrin-golden", Version="dev"',
    },
    body: JSON.stringify({ Username: username, Pw: password }),
  });
  if (!response.ok) {
    throw new Error(`${prefix} authentication failed with HTTP ${response.status}`);
  }
  const body = await response.json();
  return {
    accessToken: body.AccessToken,
    userId: body.User?.Id,
  };
}

async function requestCase(baseUrl, testCase, auth) {
  const headers = {};
  if (testCase.authenticated) {
    headers['X-Emby-Token'] = auth.accessToken;
  }
  const url = `${baseUrl}${testCase.path}`;
  const response = await fetch(url, { method: testCase.method, headers });
  const contentType = response.headers.get('content-type') || '';
  const body = testCase.text || !contentType.includes('application/json')
    ? await response.text()
    : await response.json();
  return {
    status: response.status,
    contentType,
    body: normalizeBody(body),
  };
}

function compareResponses(upstream, jellyrin) {
  if (upstream.status !== jellyrin.status) {
    return { ok: false, reason: `status ${upstream.status} != ${jellyrin.status}` };
  }
  if (upstream.status >= 400) {
    return { ok: true, reason: 'matched error status' };
  }
  const upstreamShape = shapeOf(upstream.body);
  const jellyrinShape = shapeOf(jellyrin.body);
  if (JSON.stringify(upstreamShape) !== JSON.stringify(jellyrinShape)) {
    return {
      ok: false,
      reason: `shape mismatch upstream=${JSON.stringify(upstreamShape)} jellyrin=${JSON.stringify(jellyrinShape)}`,
    };
  }
  return { ok: true, reason: 'matched status and normalized shape' };
}

function shapeOf(value) {
  if (Array.isArray(value)) {
    return value.length === 0 ? [] : [shapeOf(value[0])];
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, shapeOf(value[key])]),
    );
  }
  if (value === null) {
    return 'null';
  }
  return typeof value;
}

function normalizeBody(body) {
  if (Array.isArray(body)) {
    return body.map(normalizeBody);
  }
  if (!body || typeof body !== 'object') {
    return body;
  }
  const normalized = {};
  for (const [key, value] of Object.entries(body)) {
    if (['Id', 'ServerId', 'LocalAddress', 'ServerName'].includes(key)) {
      normalized[key] = '<dynamic>';
    } else if (key === 'Items' && Array.isArray(value)) {
      normalized[key] = value.map(normalizeBody);
    } else {
      normalized[key] = normalizeBody(value);
    }
  }
  return normalized;
}

function trimTrailingSlash(value) {
  return value.replace(/\/+$/, '');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
