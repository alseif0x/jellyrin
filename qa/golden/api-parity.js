#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const upstreamBaseUrl = trimTrailingSlash(process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096');
const jellyrinBaseUrl = trimTrailingSlash(process.env.JELLYRIN_URL || 'http://127.0.0.1:8097');
const goldenMode = process.env.JELLYRIN_GOLDEN_MODE || 'smoke';
const outputPath = process.env.JELLYRIN_GOLDEN_OUT
  || path.resolve(__dirname, '../../../../plans/generated/golden-traces/api-parity-latest.json');
const fixtureOverview = 'Golden overview';
const fixturePngBytes = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMCAQAABQABDQottAAAAABJRU5ErkJggg==',
  'base64',
);

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
  { name: 'users-me', method: 'GET', path: '/Users/Me', requiresUserToken: true },
  { name: 'users', method: 'GET', path: '/Users' },
  { name: 'views', method: 'GET', dataDependentList: true, shapeMode: 'strict-only', path: ({ userId }) => `/UserViews?UserId=${encodeURIComponent(userId)}` },
  { name: 'items-movies-first-page', method: 'GET', path: ({ userId }) => `/Items?UserId=${encodeURIComponent(userId)}&IncludeItemTypes=Movie&StartIndex=0&Limit=5` },
  { name: 'item-detail-movie', method: 'GET', requiresMovie: true, shapeMode: 'strict-only', path: ({ movieItemId, userId }) => `/Items/${encodeURIComponent(movieItemId)}?UserId=${encodeURIComponent(userId)}&Fields=Overview` },
  { name: 'item-playback-info-movie', method: 'GET', requiresMovie: true, requiresUserToken: true, path: ({ movieItemId }) => `/Items/${encodeURIComponent(movieItemId)}/PlaybackInfo` },
  {
    name: 'item-playback-info-post-movie',
    method: 'POST',
    requiresMovie: true,
    shapeMode: 'strict-only',
    path: ({ movieItemId }) => `/Items/${encodeURIComponent(movieItemId)}/PlaybackInfo`,
    body: ({ movieItemId }) => ({
      MediaSourceId: movieItemId,
      AudioStreamIndex: 1,
      SubtitleStreamIndex: -1,
      EnableDirectPlay: false,
      EnableDirectStream: true,
      EnableTranscoding: true,
      StartPositionTicks: 50_000_000,
    }),
  },
  {
    name: 'item-playback-info-transcode-movie',
    method: 'POST',
    requiresMovie: true,
    shapeMode: 'strict-only',
    path: ({ movieItemId }) => `/Items/${encodeURIComponent(movieItemId)}/PlaybackInfo`,
    body: ({ movieItemId, userId }) => ({
      UserId: userId,
      MediaSourceId: movieItemId,
      AudioStreamIndex: 1,
      SubtitleStreamIndex: -1,
      EnableDirectPlay: false,
      EnableDirectStream: false,
      EnableTranscoding: true,
      StartPositionTicks: 50_000_000,
      DeviceProfile: transcodeDeviceProfile(),
    }),
  },
  {
    name: 'item-playback-info-invalid-media-source',
    method: 'POST',
    requiresMovie: true,
    shapeMode: 'strict-only',
    path: ({ movieItemId }) => `/Items/${encodeURIComponent(movieItemId)}/PlaybackInfo`,
    body: () => ({
      MediaSourceId: '00000000000000000000000000000000',
      AudioStreamIndex: 1,
    }),
  },
  { name: 'activity-log-entries', method: 'GET', path: '/System/ActivityLog/Entries?StartIndex=0&Limit=5', shapeMode: 'strict-only' },
  { name: 'item-images-movie', method: 'GET', requiresMovie: true, ensureMovieImage: true, path: ({ movieItemId }) => `/Items/${encodeURIComponent(movieItemId)}/Images`, shapeMode: 'strict-only' },
  { name: 'sessions', method: 'GET', dataDependentList: true, shapeMode: 'strict-only', path: '/Sessions' },
  { name: 'scheduled-tasks', method: 'GET', path: '/ScheduledTasks' },
  { name: 'repositories', method: 'GET', path: '/Repositories' },
];

async function main() {
  if (!['smoke', 'strict'].includes(goldenMode)) {
    throw new Error(`Unsupported JELLYRIN_GOLDEN_MODE: ${goldenMode}`);
  }

  const upstreamAuth = await authenticateFromEnv('JELLYFIN', upstreamBaseUrl);
  const jellyrinAuth = await authenticateFromEnv('JELLYRIN', jellyrinBaseUrl);
  const runAuthenticated = Boolean(upstreamAuth && jellyrinAuth);
  const upstreamContext = runAuthenticated
    ? await buildAuthenticatedContext(upstreamBaseUrl, upstreamAuth)
    : {};
  const jellyrinContext = runAuthenticated
    ? await buildAuthenticatedContext(jellyrinBaseUrl, jellyrinAuth)
    : {};
  const cases = runAuthenticated
    ? [...publicCases, ...authenticatedCases.map((testCase) => ({ ...testCase, authenticated: true }))]
    : publicCases;

  const results = [];
  for (const testCase of cases) {
    if (
      testCase.requiresMovie
      && (!upstreamContext.movieItemId || !jellyrinContext.movieItemId)
    ) {
      results.push({
        name: testCase.name,
        method: testCase.method,
        path: '<requires movie item>',
        authenticated: Boolean(testCase.authenticated),
        skipped: true,
        comparison: { ok: true, reason: 'skipped because one side has no movie item' },
      });
      continue;
    }
    if (
      testCase.requiresUserToken
      && (upstreamAuth?.method === 'api_key' || jellyrinAuth?.method === 'api_key')
    ) {
      results.push({
        name: testCase.name,
        method: testCase.method,
        path: typeof testCase.path === 'function' ? '<dynamic>' : testCase.path,
        authenticated: Boolean(testCase.authenticated),
        skipped: true,
        comparison: { ok: true, reason: 'skipped because API-key auth has no current user token' },
      });
      continue;
    }
    const upstream = await requestCase(upstreamBaseUrl, testCase, upstreamAuth, upstreamContext);
    const jellyrin = await requestCase(jellyrinBaseUrl, testCase, jellyrinAuth, jellyrinContext);
    if (testCase.dataDependentList && hasOnlyOneEmptyList(upstream.body, jellyrin.body)) {
      results.push({
        name: testCase.name,
        method: testCase.method,
        path: typeof testCase.path === 'function' ? '<dynamic>' : testCase.path,
        authenticated: Boolean(testCase.authenticated),
        upstream,
        jellyrin,
        skipped: true,
        comparison: { ok: true, reason: 'skipped because data-dependent lists differ between environments' },
      });
      continue;
    }
    const comparison = compareResponses(testCase, upstream, jellyrin);
    results.push({
      name: testCase.name,
      method: testCase.method,
      path: typeof testCase.path === 'function' ? '<dynamic>' : testCase.path,
      authenticated: Boolean(testCase.authenticated),
      upstream,
      jellyrin,
      comparison,
    });
  }

  const summary = {
    total: results.length,
    passed: results.filter((result) => result.comparison.ok && !result.skipped).length,
    failed: results.filter((result) => !result.comparison.ok).length,
    skipped: results.filter((result) => result.skipped).length,
    mode: goldenMode,
    strictEvaluated: results.filter((result) => result.comparison.strict?.evaluated).length,
    authenticated: runAuthenticated,
    authMethods: runAuthenticated
      ? { upstream: upstreamAuth.method, jellyrin: jellyrinAuth.method }
      : null,
  };
  const report = {
    generatedAt: new Date().toISOString(),
    mode: goldenMode,
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
  const apiKey = process.env[`${prefix}_API_KEY`];
  if (apiKey) {
    return {
      accessToken: apiKey,
      userId: null,
      method: 'api_key',
    };
  }

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
    method: 'password',
  };
}

async function buildAuthenticatedContext(baseUrl, auth) {
  const context = {
    userId: auth.userId,
    movieItemId: null,
  };
  if (!context.userId) {
    const usersResponse = await fetch(`${baseUrl}/Users`, {
      headers: { 'X-Emby-Token': auth.accessToken },
    });
    if (usersResponse.ok && usersResponse.headers.get('content-type')?.includes('application/json')) {
      const users = await usersResponse.json();
      context.userId = users?.[0]?.Id || null;
    }
  }
  if (!context.userId) {
    return context;
  }
  const response = await fetch(
    `${baseUrl}/Items?UserId=${encodeURIComponent(context.userId)}&Recursive=true&IncludeItemTypes=Movie&StartIndex=0&Limit=1`,
    { headers: { 'X-Emby-Token': auth.accessToken } },
  );
  if (response.ok && response.headers.get('content-type')?.includes('application/json')) {
    const body = await response.json();
    context.movieItemId = body.Items?.[0]?.Id || null;
  }
  if (context.movieItemId) {
    await ensureFixtureImages(baseUrl, auth, context.movieItemId);
    await ensureFixtureOverview(baseUrl, auth, context.userId, context.movieItemId);
  }
  return context;
}

function transcodeDeviceProfile() {
  return {
    DirectPlayProfiles: [],
    TranscodingProfiles: [
      {
        Container: 'mp4',
        Type: 'Video',
        AudioCodec: 'aac,mp2,opus,flac',
        VideoCodec: 'av1,h264,vp9',
        Context: 'Streaming',
        Protocol: 'hls',
        MaxAudioChannels: '2',
        MinSegments: '1',
        BreakOnNonKeyFrames: false,
      },
    ],
    ContainerProfiles: [],
    CodecProfiles: [],
  };
}

async function ensureFixtureOverview(baseUrl, auth, userId, movieItemId) {
  const headers = { 'X-Emby-Token': auth.accessToken };
  const detailPath = `/Items/${encodeURIComponent(movieItemId)}?UserId=${encodeURIComponent(userId)}&Fields=Overview`;
  const detailResponse = await fetch(`${baseUrl}${detailPath}`, { headers });
  if (!detailResponse.ok || !detailResponse.headers.get('content-type')?.includes('application/json')) {
    return;
  }
  const detail = await detailResponse.json();
  if (detail.Overview === fixtureOverview) {
    return;
  }
  detail.Overview = fixtureOverview;
  const updatePath = baseUrl === upstreamBaseUrl
    ? `/Items/${encodeURIComponent(movieItemId)}`
    : `/ItemUpdate/Items/${encodeURIComponent(movieItemId)}`;
  const update = await fetch(`${baseUrl}${updatePath}`, {
    method: 'POST',
    headers: {
      ...headers,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(detail),
  });
  if (!update.ok) {
    throw new Error(`fixture overview setup failed for ${baseUrl} with HTTP ${update.status}`);
  }
}

async function ensureFixtureImages(baseUrl, auth, movieItemId) {
  const headers = { 'X-Emby-Token': auth.accessToken };
  const imagesResponse = await fetch(`${baseUrl}/Items/${encodeURIComponent(movieItemId)}/Images`, { headers });
  let images = [];
  if (imagesResponse.ok && imagesResponse.headers.get('content-type')?.includes('application/json')) {
    images = await imagesResponse.json();
  }
  if (!Array.isArray(images)) {
    images = [];
  }
  if (!images.some((image) => image?.ImageType === 'Primary')) {
    await uploadFixtureImage(baseUrl, auth, movieItemId, 'Primary');
  }
  if (!images.some((image) => image?.ImageType === 'Backdrop' && image?.ImageIndex === 0)) {
    await uploadFixtureImage(baseUrl, auth, movieItemId, 'Backdrop', 0);
  }
}

async function uploadFixtureImage(baseUrl, auth, movieItemId, imageType, imageIndex = null) {
  const headers = { 'X-Emby-Token': auth.accessToken };
  const body = baseUrl === upstreamBaseUrl
    ? fixturePngBytes.toString('base64')
    : fixturePngBytes;
  const indexSegment = imageIndex === null ? '' : `/${imageIndex}`;
  const uploadPath = baseUrl === upstreamBaseUrl
    ? `/Items/${encodeURIComponent(movieItemId)}/Images/${encodeURIComponent(imageType)}${indexSegment}`
    : `/Image/Items/${encodeURIComponent(movieItemId)}/Images/${encodeURIComponent(imageType)}${indexSegment}`;
  const upload = await fetch(`${baseUrl}${uploadPath}`, {
    method: 'POST',
    headers: {
      ...headers,
      'Content-Type': 'image/png',
    },
    body,
  });
  if (!upload.ok) {
    throw new Error(`${imageType} image setup failed for ${baseUrl} with HTTP ${upload.status}`);
  }
}

async function requestCase(baseUrl, testCase, auth, context = {}) {
  const headers = {};
  if (testCase.authenticated) {
    headers['X-Emby-Token'] = auth.accessToken;
  }
  const requestPath = typeof testCase.path === 'function'
    ? testCase.path(context)
    : testCase.path;
  const requestBody = typeof testCase.body === 'function'
    ? testCase.body(context)
    : testCase.body;
  if (requestBody !== undefined) {
    headers['Content-Type'] = 'application/json';
  }
  const url = `${baseUrl}${requestPath}`;
  const response = await fetch(url, {
    method: testCase.method,
    headers,
    body: requestBody === undefined ? undefined : JSON.stringify(requestBody),
  });
  const contentType = response.headers.get('content-type') || '';
  const body = testCase.text || !contentType.includes('application/json')
    ? await response.text()
    : await response.json();
  return {
    path: requestPath,
    status: response.status,
    contentType,
    requestBody: requestBody === undefined ? undefined : normalizeBody(requestBody),
    body: normalizeBody(body),
  };
}

function compareResponses(testCase, upstream, jellyrin) {
  if (upstream.status !== jellyrin.status) {
    return { ok: false, reason: `status ${upstream.status} != ${jellyrin.status}` };
  }
  if (upstream.status >= 400) {
    return { ok: true, reason: 'matched error status', strict: { evaluated: false } };
  }
  if (testCase.shapeMode !== 'strict-only') {
    const upstreamShape = shapeOf(upstream.body);
    const jellyrinShape = shapeOf(jellyrin.body);
    if (JSON.stringify(upstreamShape) !== JSON.stringify(jellyrinShape)) {
      return {
        ok: false,
        reason: `shape mismatch upstream=${JSON.stringify(upstreamShape)} jellyrin=${JSON.stringify(jellyrinShape)}`,
      };
    }
  }
  const strict = goldenMode === 'strict'
    ? strictCompare(testCase.name, upstream.body, jellyrin.body, upstream.requestBody, jellyrin.requestBody)
    : { evaluated: false };
  if (strict.evaluated && !strict.ok) {
    return { ok: false, reason: strict.reason, strict };
  }
  return {
    ok: true,
    reason: strict.evaluated
      ? 'matched status, normalized shape and strict critical fields'
      : 'matched status and normalized shape',
    strict,
  };
}

function strictCompare(caseName, upstreamBody, jellyrinBody, upstreamRequestBody, jellyrinRequestBody) {
  const assertions = strictAssertions(caseName);
  if (assertions.length === 0) {
    return { evaluated: false, ok: true };
  }
  const failures = assertions
    .map((assertion) => assertion(upstreamBody, jellyrinBody, upstreamRequestBody, jellyrinRequestBody))
    .filter(Boolean);
  return {
    evaluated: true,
    ok: failures.length === 0,
    reason: failures.join('; '),
  };
}

function strictAssertions(caseName) {
  switch (caseName) {
    case 'public-info':
      return [
        sameField('ProductName'),
        sameField('Version'),
        sameField('StartupWizardCompleted'),
      ];
    case 'branding-configuration':
      return [
        sameType('SplashscreenEnabled'),
      ];
    case 'branding-css':
      return [
        sameValue((body) => body, 'css text'),
      ];
    case 'system-info':
      return [
        sameField('ProductName'),
        sameField('Version'),
        sameField('StartupWizardCompleted'),
        sameType('HasPendingRestart'),
        sameType('IsShuttingDown'),
        sameType('SupportsLibraryMonitor'),
        sameType('CompletedInstallations'),
        sameType('CastReceiverApplications'),
      ];
    case 'users':
      return [
        arrayItemsHaveFields(['Name', 'Id', 'ServerId', 'HasPassword', 'Policy', 'Configuration']),
        firstArrayItemHasFields(['Policy.IsAdministrator', 'Policy.EnableMediaPlayback', 'Policy.SyncPlayAccess']),
      ];
    case 'items-movies-first-page':
      return [
        queryResultHasFields(['Items', 'TotalRecordCount', 'StartIndex']),
        firstItemHasFields(['Id', 'Name', 'Type', 'ImageTags', 'UserData']),
      ];
    case 'item-playback-info-post-movie':
      return [
        requestBodyHasFields([
          'MediaSourceId',
          'AudioStreamIndex',
          'SubtitleStreamIndex',
          'EnableDirectPlay',
          'EnableDirectStream',
          'EnableTranscoding',
          'StartPositionTicks',
        ]),
        queryResultHasFields(['MediaSources', 'PlaySessionId']),
        firstMediaSourceHasFields([
          'SupportsDirectPlay',
          'SupportsDirectStream',
          'SupportsTranscoding',
          'MediaStreams',
          'MediaStreams.[].Channels',
          'MediaStreams.[].SampleRate',
        ]),
      ];
    case 'item-playback-info-transcode-movie':
      return [
        requestBodyHasFields([
          'MediaSourceId',
          'AudioStreamIndex',
          'SubtitleStreamIndex',
          'EnableDirectPlay',
          'EnableDirectStream',
          'EnableTranscoding',
          'StartPositionTicks',
          'DeviceProfile.TranscodingProfiles',
        ]),
        queryResultHasFields(['MediaSources', 'PlaySessionId']),
        firstMediaSourceHasFields([
          'SupportsDirectPlay',
          'SupportsDirectStream',
          'SupportsTranscoding',
          'TranscodingUrl',
          'MediaStreams',
        ]),
      ];
    case 'item-playback-info-invalid-media-source':
      return [
        requestBodyHasFields(['MediaSourceId', 'AudioStreamIndex']),
        queryResultHasFields(['MediaSources', 'PlaySessionId', 'ErrorCode']),
      ];
    case 'activity-log-entries':
      return [
        queryResultHasFields(['Items', 'TotalRecordCount', 'StartIndex']),
        firstItemHasFields(['Id', 'Name', 'Severity', 'Date', 'UserId', 'ShortOverview']),
      ];
    case 'item-images-movie':
      return [
        firstArrayItemHasFields(['ImageType', 'ImageIndex', 'ImageTag', 'Path']),
      ];
    case 'scheduled-tasks':
      return [
        arrayContainsFieldValue('Key', 'RefreshLibrary'),
        arrayItemsHaveFields(['Name', 'State', 'Id', 'Key', 'Category', 'IsHidden']),
      ];
    case 'repositories':
      return [
        arrayContainsFieldValue('Name', 'Jellyfin Stable'),
        arrayItemsHaveFields(['Name', 'Url', 'Enabled']),
      ];
    default:
      return [];
  }
}

function sameField(pathExpression) {
  return (upstreamBody, jellyrinBody) => {
    const upstreamValue = getPath(upstreamBody, pathExpression);
    const jellyrinValue = getPath(jellyrinBody, pathExpression);
    return JSON.stringify(upstreamValue) === JSON.stringify(jellyrinValue)
      ? null
      : `${pathExpression} strict mismatch upstream=${JSON.stringify(upstreamValue)} jellyrin=${JSON.stringify(jellyrinValue)}`;
  };
}

function sameType(pathExpression) {
  return (upstreamBody, jellyrinBody) => {
    const upstreamValue = getPath(upstreamBody, pathExpression);
    const jellyrinValue = getPath(jellyrinBody, pathExpression);
    return JSON.stringify(shapeOf(upstreamValue)) === JSON.stringify(shapeOf(jellyrinValue))
      ? null
      : `${pathExpression} strict type mismatch upstream=${JSON.stringify(shapeOf(upstreamValue))} jellyrin=${JSON.stringify(shapeOf(jellyrinValue))}`;
  };
}

function sameValue(projector, label) {
  return (upstreamBody, jellyrinBody) => {
    const upstreamValue = projector(upstreamBody);
    const jellyrinValue = projector(jellyrinBody);
    return upstreamValue === jellyrinValue
      ? null
      : `${label} strict mismatch`;
  };
}

function arrayItemsHaveFields(fieldPaths) {
  return (_upstreamBody, jellyrinBody) => {
    if (!Array.isArray(jellyrinBody)) {
      return 'expected Jellyrin body to be an array';
    }
    const missing = [];
    for (const [index, item] of jellyrinBody.entries()) {
      for (const fieldPath of fieldPaths) {
        if (getPath(item, fieldPath) === undefined) {
          missing.push(`[${index}].${fieldPath}`);
        }
      }
    }
    return missing.length === 0 ? null : `missing strict fields: ${missing.join(', ')}`;
  };
}

function firstArrayItemHasFields(fieldPaths) {
  return (_upstreamBody, jellyrinBody) => {
    if (!Array.isArray(jellyrinBody) || jellyrinBody.length === 0) {
      return 'expected Jellyrin body to contain at least one item';
    }
    const missing = fieldPaths.filter((fieldPath) => getPath(jellyrinBody[0], fieldPath) === undefined);
    return missing.length === 0 ? null : `first item missing strict fields: ${missing.join(', ')}`;
  };
}

function queryResultHasFields(fieldPaths) {
  return (_upstreamBody, jellyrinBody) => {
    const missing = fieldPaths.filter((fieldPath) => getPath(jellyrinBody, fieldPath) === undefined);
    return missing.length === 0 ? null : `query result missing strict fields: ${missing.join(', ')}`;
  };
}

function firstItemHasFields(fieldPaths) {
  return (_upstreamBody, jellyrinBody) => {
    const first = jellyrinBody?.Items?.[0];
    if (!first) {
      return null;
    }
    const missing = fieldPaths.filter((fieldPath) => getPath(first, fieldPath) === undefined);
    return missing.length === 0 ? null : `first query item missing strict fields: ${missing.join(', ')}`;
  };
}

function requestBodyHasFields(fieldPaths) {
  return (_upstreamBody, _jellyrinBody, upstreamRequestBody, jellyrinRequestBody) => {
    const missing = [];
    for (const [label, body] of [
      ['upstream', upstreamRequestBody],
      ['jellyrin', jellyrinRequestBody],
    ]) {
      for (const fieldPath of fieldPaths) {
        if (getPath(body, fieldPath) === undefined) {
          missing.push(`${label}.${fieldPath}`);
        }
      }
    }
    return missing.length === 0 ? null : `request body missing strict fields: ${missing.join(', ')}`;
  };
}

function firstMediaSourceHasFields(fieldPaths) {
  return (_upstreamBody, jellyrinBody) => {
    const first = jellyrinBody?.MediaSources?.[0];
    if (!first) {
      return 'expected Jellyrin response to contain at least one media source';
    }
    const missing = fieldPaths.filter((fieldPath) => !hasPath(first, fieldPath.split('.')));
    return missing.length === 0 ? null : `first media source missing strict fields: ${missing.join(', ')}`;
  };
}

function arrayContainsFieldValue(fieldPath, expectedValue) {
  return (_upstreamBody, jellyrinBody) => {
    if (!Array.isArray(jellyrinBody)) {
      return 'expected Jellyrin body to be an array';
    }
    return jellyrinBody.some((item) => getPath(item, fieldPath) === expectedValue)
      ? null
      : `missing array item where ${fieldPath}=${JSON.stringify(expectedValue)}`;
  };
}

function getPath(value, pathExpression) {
  return pathExpression
    .split('.')
    .reduce((current, part) => (current == null ? undefined : current[part]), value);
}

function hasPath(value, parts) {
  if (parts.length === 0) {
    return value !== undefined;
  }
  const [part, ...rest] = parts;
  if (part === '[]') {
    return Array.isArray(value) && value.some((child) => hasPath(child, rest));
  }
  if (Array.isArray(value)) {
    return value.some((child) => hasPath(child, parts));
  }
  if (!value || typeof value !== 'object' || !(part in value)) {
    return false;
  }
  return hasPath(value[part], rest);
}

function hasOnlyOneEmptyList(upstreamBody, jellyrinBody) {
  const upstreamLength = listLength(upstreamBody);
  const jellyrinLength = listLength(jellyrinBody);
  return (
    upstreamLength !== null
    && jellyrinLength !== null
    && ((upstreamLength === 0 && jellyrinLength > 0) || (upstreamLength > 0 && jellyrinLength === 0))
  );
}

function listLength(body) {
  if (Array.isArray(body)) {
    return body.length;
  }
  if (body && typeof body === 'object' && Array.isArray(body.Items)) {
    return body.Items.length;
  }
  return null;
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
    if (['Id', 'ServerId', 'LocalAddress', 'ServerName', 'MediaSourceId'].includes(key)) {
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
