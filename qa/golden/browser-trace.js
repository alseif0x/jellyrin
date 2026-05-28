#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { chromium } = require('playwright');

const outputRoot = process.env.JELLYRIN_BROWSER_TRACE_OUT
  || path.resolve(__dirname, '../../../../plans/generated/e2e-traces');
const flow = process.env.JELLYRIN_BROWSER_FLOW || 'p0-direct-play';
const chromiumExecutable = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE
  || '/home/cdmonio/.cache/ms-playwright/chromium_headless_shell-1208/chrome-headless-shell-linux64/chrome-headless-shell';

const targetDefinitions = [
  {
    name: 'upstream',
    baseUrl: process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096',
    username: process.env.JELLYFIN_ADMIN_USER,
    password: process.env.JELLYFIN_ADMIN_PASSWORD,
    apiKey: process.env.JELLYFIN_API_KEY,
  },
  {
    name: 'jellyrin',
    baseUrl: process.env.JELLYRIN_URL || process.env.JELLYRIN_E2E_BASE_URL || 'http://127.0.0.1:8097',
    username: process.env.JELLYRIN_ADMIN_USER || process.env.JELLYRIN_E2E_ADMIN_USER,
    password: process.env.JELLYRIN_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_ADMIN_PASSWORD,
    apiKey: process.env.JELLYRIN_API_KEY,
  },
];

async function main() {
  if (!['login-home', 'p0-direct-play', 'resume'].includes(flow)) {
    throw new Error(`Unsupported browser flow: ${flow}`);
  }

  const requestedTargets = new Set(
    (process.env.JELLYRIN_BROWSER_TARGETS || 'upstream,jellyrin')
      .split(',')
      .map((target) => target.trim())
      .filter(Boolean),
  );
  const targets = targetDefinitions.filter((target) => requestedTargets.has(target.name));
  const flowDir = path.join(outputRoot, flow);
  await fs.mkdir(flowDir, { recursive: true });

  const browser = await chromium.launch({
    headless: true,
    executablePath: chromiumExecutable,
  });

  const summaries = [];
  try {
    for (const target of targets) {
      const summary = await captureTarget(browser, flowDir, target);
      summaries.push(summary);
    }
  } finally {
    await browser.close();
  }

  const comparison = compareSummaries(summaries);
  const report = {
    generatedAt: new Date().toISOString(),
    flow,
    summaries,
    comparison,
  };
  await fs.writeFile(path.join(flowDir, 'comparison.json'), `${JSON.stringify(report, null, 2)}\n`);

  const completed = summaries.filter((summary) => summary.status === 'completed').length;
  console.log(`${completed}/${summaries.length} browser trace targets completed`);
  console.log(`wrote ${flowDir}`);
  if (comparison.failed) {
    for (const reason of comparison.reasons) {
      console.error(reason);
    }
    process.exitCode = 1;
  }
}

async function captureTarget(browser, flowDir, target) {
  const summary = {
    target: target.name,
    baseUrl: trimTrailingSlash(target.baseUrl),
    flow,
    status: 'pending',
    skipped: false,
    requests: 0,
    failedResponses: [],
    consoleErrors: [],
    pageErrors: [],
    websockets: 0,
    screenshot: `${target.name}.screenshot.png`,
    criticalRequests: {},
    invariants: {
      playbackInfo200: false,
      streamOk: false,
      sessionPlaying204: false,
      websocketSessions: false,
      websocketKeepAlive: false,
      websocketMessageTypes: [],
      unexpectedTranscodePath: false,
      playMethods: [],
      playbackProgress204: false,
      resumeList200: false,
      resumeItemMatched: false,
      resumePositionTicks: null,
    },
  };

  const requestLog = await jsonlWriter(path.join(flowDir, `${target.name}.requests.jsonl`));
  const consoleLog = await jsonlWriter(path.join(flowDir, `${target.name}.console.jsonl`));
  const websocketLog = await jsonlWriter(path.join(flowDir, `${target.name}.websocket.jsonl`));

  if ((!target.username || !target.password) && !target.apiKey) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'missing username/password or API key environment variables';
    await requestLog.close();
    await consoleLog.close();
    await websocketLog.close();
    return summary;
  }

  const context = await browser.newContext({
    baseURL: summary.baseUrl,
    ignoreHTTPSErrors: true,
  });
  const page = await context.newPage();
  wirePageCapture(page, summary, requestLog, consoleLog, websocketLog);

  try {
    const publicInfoResponse = await page.request.get(`${summary.baseUrl}/System/Info/Public`);
    if (!publicInfoResponse.ok()) {
      throw new Error(`System public info returned HTTP ${publicInfoResponse.status()}`);
    }
    const publicInfo = await publicInfoResponse.json();
    if (!publicInfo.StartupWizardCompleted) {
      throw new Error('Startup wizard is not completed for target');
    }

    if (flow === 'login-home') {
      await runLoginHomeFlow(page, summary, publicInfo, target);
    } else if (flow === 'p0-direct-play') {
      await runDirectPlayFlow(page, summary, publicInfo, target);
    } else {
      await runResumeFlow(page, summary, publicInfo, target);
    }
    if (summary.skipped) {
      return summary;
    }
    await page.screenshot({ path: path.join(flowDir, summary.screenshot), fullPage: true });
    summary.finalUrl = sanitizeUrl(page.url());
    summary.status = 'completed';
  } catch (error) {
    summary.status = 'failed';
    summary.error = error.message;
    await page.screenshot({ path: path.join(flowDir, summary.screenshot), fullPage: true }).catch(() => {});
  } finally {
    await context.close();
    await requestLog.close();
    await consoleLog.close();
    await websocketLog.close();
  }

  return summary;
}

async function runLoginHomeFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');
}

async function runResumeFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for resume trace';
    return;
  }
  if (!resumeTraceEligible(movie)) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no resume-eligible movie item for resume trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const positionTicks = resumeTracePositionTicks(movie);
  const progressResult = await browserFetchJson(page, {
    method: 'POST',
    url: '/Sessions/Playing/Progress',
    token: auth.AccessToken,
    body: {
      ItemId: movie.Id,
      MediaSourceId: movie.Id,
      PositionTicks: positionTicks,
      IsPaused: false,
    },
  });
  if (progressResult.status !== 204) {
    throw new Error(`Sessions/Playing/Progress returned HTTP ${progressResult.status}`);
  }

  const resumeResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserItems/Resume?UserId=${encodeURIComponent(auth.User.Id)}&Limit=12&MediaTypes=Video&Fields=PrimaryImageAspectRatio`,
    token: auth.AccessToken,
  });
  if (resumeResult.status < 200 || resumeResult.status >= 300) {
    throw new Error(`UserItems/Resume returned HTTP ${resumeResult.status}`);
  }
  const resume = resumeResult.json;
  const resumeItem = resume.Items?.find((item) => item.Id === movie.Id);
  if (!resumeItem) {
    throw new Error('resume list does not contain traced movie item');
  }
  if (resumeItem.UserData?.PlaybackPositionTicks !== positionTicks) {
    throw new Error(`resume position ${resumeItem.UserData?.PlaybackPositionTicks} != ${positionTicks}`);
  }
  if (resumeItem.UserData?.Played !== false) {
    throw new Error(`resume item Played state ${resumeItem.UserData?.Played} != false`);
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle');
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function runDirectPlayFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for direct-play trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.goto(`${summary.baseUrl}/web/#/details?id=${movie.Id}`, {
    waitUntil: 'domcontentloaded',
  });
  await page.getByText(movie.Name, { exact: true }).first().waitFor({ state: 'visible', timeout: 20_000 });
  await page.waitForLoadState('networkidle');

  const playbackInfo = page.waitForResponse((response) =>
    response.url().includes(`/Items/${movie.Id}/PlaybackInfo`) && response.status() === 200,
  );
  const stream = page.waitForResponse((response) =>
    response.url().includes(`/Videos/${movie.Id}/stream`) && [200, 206].includes(response.status()),
  );
  const playbackReport = page.waitForResponse((response) =>
    response.url().includes('/Sessions/Playing') && response.request().method() === 'POST' && response.status() === 204,
  );
  const playButton = page.locator('.btnPlay:not(.hide), .btnReplay:not(.hide)').first();
  await playButton.waitFor({ state: 'visible', timeout: 20_000 });
  await playButton.click();
  await playbackInfo;
  await stream;
  await playbackReport;
  await page.waitForLoadState('networkidle').catch(() => {});

  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function firstMovieItem(page, summary, auth) {
  const itemsResponse = await page.request.get(
    `${summary.baseUrl}/Items?UserId=${encodeURIComponent(auth.User.Id)}&IncludeItemTypes=Movie&Recursive=true&Fields=RunTimeTicks&StartIndex=0&Limit=10`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  if (!itemsResponse.ok()) {
    throw new Error(`Movie lookup returned HTTP ${itemsResponse.status()}`);
  }
  const items = await itemsResponse.json();
  const movies = items.Items?.filter((item) => item.Type === 'Movie' && item.MediaType === 'Video') || [];
  return movies.find(resumeTraceEligible) || movies[0];
}

function resumeTracePositionTicks(movie) {
  const runtimeTicks = Number(movie.RunTimeTicks || 0);
  if (Number.isFinite(runtimeTicks) && runtimeTicks > 0) {
    return Math.max(1, Math.floor(runtimeTicks / 2));
  }
  return 50_000_000;
}

function resumeTraceEligible(movie) {
  const runtimeTicks = Number(movie.RunTimeTicks || 0);
  return Number.isFinite(runtimeTicks) && runtimeTicks >= 300 * 10_000_000;
}

async function browserFetchJson(page, request) {
  return page.evaluate(async ({ method, url, token, body }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'Content-Type': 'application/json',
        'X-Emby-Token': token,
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    let json = null;
    if (text) {
      try {
        json = JSON.parse(text);
      } catch (_) {
        json = null;
      }
    }
    return {
      status: response.status,
      json,
    };
  }, request);
}

async function authenticateTarget(page, summary, target) {
  if (target.apiKey) {
    const usersResponse = await page.request.get(`${summary.baseUrl}/Users`, {
      headers: { 'X-Emby-Token': target.apiKey },
    });
    if (!usersResponse.ok()) {
      throw new Error(`API-key user lookup returned HTTP ${usersResponse.status()}`);
    }
    const users = await usersResponse.json();
    const user = users?.[0];
    if (!user?.Id) {
      throw new Error('API-key user lookup returned no users');
    }
    return {
      AccessToken: target.apiKey,
      User: user,
      authMethod: 'api_key',
    };
  }

  const apiAuthResponse = await page.request.post(`${summary.baseUrl}/Users/AuthenticateByName`, {
    headers: {
      Authorization: 'MediaBrowser Client="Jellyrin Browser Trace", Device="Harness", DeviceId="browser-trace", Version="dev"',
    },
    data: { Username: target.username, Pw: target.password },
  });
  if (!apiAuthResponse.ok()) {
    throw new Error(`API authentication returned HTTP ${apiAuthResponse.status()}`);
  }
  return {
    ...(await apiAuthResponse.json()),
    authMethod: 'password',
  };
}

async function establishWebSession(page, summary, publicInfo, target, auth, targetRoute) {
  if (auth.authMethod === 'api_key') {
    await preauthenticateWebWithApiKey(page, summary.baseUrl, publicInfo, auth);
    await page.goto(`${summary.baseUrl}/web/#${targetRoute}`, {
      waitUntil: 'domcontentloaded',
    });
    await page.waitForFunction((route) => window.location.hash === `#${route}`, targetRoute, { timeout: 20_000 });
    return;
  }
  await loginThroughWeb(page, summary.baseUrl, publicInfo.Id, target);
}

async function preauthenticateWebWithApiKey(page, baseUrl, publicInfo, auth) {
  await page.addInitScript(({ baseUrl, publicInfo, auth }) => {
    const now = Date.now();
    localStorage.setItem('jellyfin_credentials', JSON.stringify({
      Servers: [{
        Id: publicInfo.Id,
        Name: publicInfo.ServerName,
        LocalAddress: baseUrl,
        ManualAddress: baseUrl,
        LastConnectionMode: 2,
        DateLastAccessed: now,
        AccessToken: auth.AccessToken,
        UserId: auth.User.Id,
      }],
    }));
  }, { baseUrl, publicInfo, auth });
}

async function loginThroughWeb(page, baseUrl, serverId, target) {
  await page.goto(`${baseUrl}/web/#/login?serverid=${serverId}&url=%2Fhome`, {
    waitUntil: 'domcontentloaded',
  });
  const manualName = page.locator('#txtManualName');
  await manualName.waitFor({ state: 'visible', timeout: 5_000 }).catch(() => {});
  if (!(await manualName.isVisible().catch(() => false))) {
    await page.locator('.btnManual:visible').click({ timeout: 20_000 });
    await manualName.waitFor({ state: 'visible', timeout: 20_000 });
  }
  await manualName.fill(target.username);
  await page.locator('#txtManualPassword').fill(target.password);

  const authResponse = page.waitForResponse((response) =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200,
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authResponse;
  await page.waitForURL(/\/web\/#\/home/, { timeout: 20_000 });
}

function wirePageCapture(page, summary, requestLog, consoleLog, websocketLog) {
  page.on('response', async (response) => {
    const request = response.request();
    const requestPostData = sanitizePostData(request.postData());
    const record = {
      ts: new Date().toISOString(),
      method: request.method(),
      url: sanitizeUrl(response.url()),
      path: pathWithQuery(response.url()),
      status: response.status(),
      resourceType: request.resourceType(),
      requestHeaders: redactHeaders(request.headers()),
      requestPostData,
      responseHeaders: selectedResponseHeaders(response.headers()),
      responseContentType: response.headers()['content-type'] || '',
      queryKeysPreservingCase: Array.from(new URL(response.url()).searchParams.keys()),
    };
    if (record.responseContentType.includes('application/json')) {
      record.responseShape = await responseShape(response);
    }
    captureFlowInvariants(summary, record, requestPostData);
    summary.requests += 1;
    if (response.status() >= 400 && !allowedFailedResponse(response)) {
      summary.failedResponses.push(`${response.status()} ${sanitizeUrl(response.url())}`);
    }
    await requestLog.write(record);
  });

  page.on('console', async (message) => {
    const text = redactText(message.text());
    const record = {
      ts: new Date().toISOString(),
      type: message.type(),
      text,
      location: message.location(),
    };
    if (['error', 'warning'].includes(message.type())) {
      summary.consoleErrors.push(text);
    }
    await consoleLog.write(record);
  });

  page.on('pageerror', async (error) => {
    const record = {
      ts: new Date().toISOString(),
      message: redactText(error.message),
      stack: error.stack ? redactText(error.stack) : undefined,
    };
    summary.pageErrors.push(record.message);
    await consoleLog.write({ ...record, type: 'pageerror' });
  });

  page.on('websocket', (websocket) => {
    summary.websockets += 1;
    const url = sanitizeUrl(websocket.url());
    websocketLog.write({ ts: new Date().toISOString(), event: 'open', url });
    websocket.on('framesent', (frame) => {
      const parsed = parseJsonPayload(frame.payload);
      addWebsocketMessageType(summary, parsed);
      if (parsed && parsed.MessageType === 'KeepAlive') {
        summary.invariants.websocketKeepAlive = true;
      }
      websocketLog.write(websocketFrameRecord('sent', url, frame.payload));
    });
    websocket.on('framereceived', (frame) => {
      const parsed = parseJsonPayload(frame.payload);
      addWebsocketMessageType(summary, parsed);
      if (parsed && parsed.MessageType === 'Sessions') {
        summary.invariants.websocketSessions = true;
      }
      if (parsed && parsed.MessageType === 'ForceKeepAlive') {
        summary.invariants.websocketKeepAlive = true;
      }
      websocketLog.write(websocketFrameRecord('received', url, frame.payload));
    });
    websocket.on('close', () => {
      websocketLog.write({ ts: new Date().toISOString(), event: 'close', url });
    });
  });
}

function compareSummaries(summaries) {
  const reasons = [];
  for (const summary of summaries) {
    if (summary.status === 'failed') {
      reasons.push(`${summary.target}: ${summary.error}`);
    }
    if (summary.skipped) {
      reasons.push(`${summary.target}: skipped: ${summary.reason}`);
    }
    if (summary.failedResponses.length > 0) {
      reasons.push(`${summary.target}: unexpected failed responses: ${summary.failedResponses.join(', ')}`);
    }
    if (summary.pageErrors.length > 0) {
      reasons.push(`${summary.target}: page errors: ${summary.pageErrors.join(', ')}`);
    }
    const unexpectedConsoleErrors = summary.consoleErrors.filter((text) => !ignoredConsoleError(text));
    if (unexpectedConsoleErrors.length > 0) {
      reasons.push(`${summary.target}: unexpected console errors: ${unexpectedConsoleErrors.join(', ')}`);
    }
    for (const failure of invariantFailures(summary)) {
      reasons.push(`${summary.target}: ${failure}`);
    }
  }
  reasons.push(...compareCompletedTargets(summaries));
  return {
    failed: reasons.length > 0,
    reasons,
  };
}

function captureFlowInvariants(summary, record, requestPostData) {
  if (!['p0-direct-play', 'resume'].includes(flow)) {
    return;
  }
  const pathname = new URL(record.url).pathname;
  const key = criticalRequestKey(record);
  if (key) {
    summary.criticalRequests[key] = criticalRequestSummary(record, requestPostData);
  }
  if (pathname.endsWith('/PlaybackInfo') && record.status === 200) {
    summary.invariants.playbackInfo200 = true;
  }
  if (/\/Videos\/[^/]+\/stream/i.test(pathname) && [200, 206].includes(record.status)) {
    summary.invariants.streamOk = true;
  }
  if (pathname === '/Sessions/Playing' && record.method === 'POST' && record.status === 204) {
    summary.invariants.sessionPlaying204 = true;
    if (requestPostData && typeof requestPostData === 'object' && requestPostData.PlayMethod) {
      summary.invariants.playMethods.push(requestPostData.PlayMethod);
    }
  }
  if (pathname === '/Sessions/Playing/Progress' && record.method === 'POST' && record.status === 204) {
    summary.invariants.playbackProgress204 = true;
  }
  if (pathname === '/UserItems/Resume' && record.method === 'GET' && record.status === 200) {
    summary.invariants.resumeList200 = true;
    const items = record.responseShape?.Items;
    if (Array.isArray(items) && items.length > 0) {
      summary.invariants.resumeItemMatched = true;
      summary.invariants.resumePositionTicks = 'number';
    }
  }
  if (/\/transcoding\/|\/hls\/|\/hls1\/|\.m3u8$/i.test(pathname)) {
    summary.invariants.unexpectedTranscodePath = true;
  }
}

function criticalRequestKey(record) {
  const pathname = new URL(record.url).pathname;
  if (record.method === 'POST' && pathname.toLowerCase() === '/users/authenticatebyname') {
    return 'auth';
  }
  if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/[^/]+$/i.test(pathname)) {
    return 'item-detail';
  }
  if (record.method === 'POST' && /\/Items\/[^/]+\/PlaybackInfo$/i.test(pathname)) {
    return 'playback-info';
  }
  if (record.method === 'GET' && /\/Videos\/[^/]+\/stream/i.test(pathname)) {
    return 'video-stream';
  }
  if (record.method === 'POST' && pathname === '/Sessions/Playing') {
    return 'sessions-playing';
  }
  if (record.method === 'POST' && pathname === '/Sessions/Playing/Progress') {
    return 'sessions-playing-progress';
  }
  if (record.method === 'GET' && pathname === '/UserItems/Resume') {
    return 'resume-list';
  }
  return null;
}

function criticalRequestSummary(record, requestPostData) {
  const summary = {
    method: record.method,
    status: record.status,
    contentType: record.responseContentType,
    queryKeys: record.queryKeysPreservingCase,
    responseShape: record.responseShape,
  };
  if (record.responseHeaders['accept-ranges']) {
    summary.acceptRanges = record.responseHeaders['accept-ranges'];
  }
  if (record.responseHeaders['content-range']) {
    summary.hasContentRange = true;
  }
  if (requestPostData && typeof requestPostData === 'object' && requestPostData.PlayMethod) {
    summary.playMethod = requestPostData.PlayMethod;
  }
  return summary;
}

function compareCompletedTargets(summaries) {
  if (!['p0-direct-play', 'resume'].includes(flow)) {
    return [];
  }
  const upstream = summaries.find((summary) => summary.target === 'upstream' && summary.status === 'completed');
  const jellyrin = summaries.find((summary) => summary.target === 'jellyrin' && summary.status === 'completed');
  if (!upstream || !jellyrin) {
    return [];
  }

  const reasons = [];
  const keys = flow === 'resume'
    ? ['resume-list', 'sessions-playing-progress']
    : ['auth', 'item-detail', 'playback-info', 'video-stream', 'sessions-playing'];
  for (const key of keys) {
    const upstreamRequest = upstream.criticalRequests[key];
    const jellyrinRequest = jellyrin.criticalRequests[key];
    if (!upstreamRequest && !jellyrinRequest) {
      continue;
    }
    if (!upstreamRequest || !jellyrinRequest) {
      reasons.push(`cross-target: missing critical request ${key}`);
      continue;
    }
    reasons.push(...compareCriticalRequest(key, upstreamRequest, jellyrinRequest));
  }
  reasons.push(...compareTargetInvariants(upstream, jellyrin));
  return reasons;
}

function compareCriticalRequest(key, upstreamRequest, jellyrinRequest) {
  const reasons = [];
  if (upstreamRequest.method !== jellyrinRequest.method) {
    reasons.push(`cross-target ${key}: method ${upstreamRequest.method} != ${jellyrinRequest.method}`);
  }
  if (key === 'video-stream') {
    if (![200, 206].includes(upstreamRequest.status) || ![200, 206].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: stream status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    if (mediaType(upstreamRequest.contentType) !== mediaType(jellyrinRequest.contentType)) {
      reasons.push(`cross-target ${key}: media type ${upstreamRequest.contentType} != ${jellyrinRequest.contentType}`);
    }
    if (Boolean(upstreamRequest.hasContentRange) !== Boolean(jellyrinRequest.hasContentRange)) {
      reasons.push(`cross-target ${key}: content-range presence differs`);
    }
    return reasons;
  }
  if (upstreamRequest.status !== jellyrinRequest.status) {
    reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != ${jellyrinRequest.status}`);
  }
  if (['item-detail', 'playback-info', 'resume-list'].includes(key)) {
    reasons.push(...compareRequiredShape(key, upstreamRequest.responseShape, jellyrinRequest.responseShape));
  } else if (JSON.stringify(upstreamRequest.responseShape) !== JSON.stringify(jellyrinRequest.responseShape)) {
    reasons.push(`cross-target ${key}: response shape differs`);
  }
  if (key === 'sessions-playing' && !compatiblePlayMethod(upstreamRequest.playMethod, jellyrinRequest.playMethod)) {
    reasons.push(`cross-target ${key}: play method ${upstreamRequest.playMethod} != compatible ${jellyrinRequest.playMethod}`);
  }
  return reasons;
}

function compareRequiredShape(key, upstreamShape, jellyrinShape) {
  const required = {
    'item-detail': [
      'Id',
      'Name',
      'Type',
      'MediaType',
      'MediaSources',
      'MediaSources.[].Id',
      'MediaSources.[].MediaStreams',
      'MediaSources.[].MediaStreams.[].Type',
      'UserData',
      'UserData.PlaybackPositionTicks',
    ],
    'playback-info': [
      'MediaSources',
      'MediaSources.[].Id',
      'MediaSources.[].SupportsDirectPlay',
      'MediaSources.[].SupportsDirectStream',
      'MediaSources.[].MediaStreams',
      'MediaSources.[].MediaStreams.[].Type',
      'PlaySessionId',
    ],
    'resume-list': [
      'Items',
      'Items.[].Id',
      'Items.[].Name',
      'Items.[].Type',
      'Items.[].UserData',
      'Items.[].UserData.PlaybackPositionTicks',
      'Items.[].UserData.Played',
      'TotalRecordCount',
    ],
  }[key] || [];
  const reasons = [];
  const upstreamKeys = shapeKeys(upstreamShape);
  const jellyrinKeys = shapeKeys(jellyrinShape);
  for (const shapeKey of required) {
    if (!upstreamKeys.has(shapeKey)) {
      reasons.push(`cross-target ${key}: upstream missing required shape ${shapeKey}`);
    }
    if (!jellyrinKeys.has(shapeKey)) {
      reasons.push(`cross-target ${key}: jellyrin missing required shape ${shapeKey}`);
    }
  }
  return reasons;
}

function shapeKeys(value, prefix = '') {
  const keys = new Set();
  if (Array.isArray(value)) {
    if (value.length > 0) {
      for (const key of shapeKeys(value[0], `${prefix}[].`)) {
        keys.add(key);
      }
    }
    return keys;
  }
  if (value && typeof value === 'object') {
    for (const [key, child] of Object.entries(value)) {
      const fullKey = `${prefix}${key}`;
      keys.add(fullKey);
      for (const childKey of shapeKeys(child, `${fullKey}.`)) {
        keys.add(childKey);
      }
    }
  }
  return keys;
}

function compareTargetInvariants(upstream, jellyrin) {
  const reasons = [];
  if (upstream.invariants.websocketKeepAlive !== jellyrin.invariants.websocketKeepAlive) {
    reasons.push('cross-target: websocket KeepAlive invariant differs');
  }
  if (upstream.invariants.unexpectedTranscodePath !== jellyrin.invariants.unexpectedTranscodePath) {
    reasons.push('cross-target: unexpected transcode/HLS invariant differs');
  }
  return reasons;
}

function addWebsocketMessageType(summary, parsed) {
  if (!parsed || !parsed.MessageType) {
    return;
  }
  if (!summary.invariants.websocketMessageTypes.includes(parsed.MessageType)) {
    summary.invariants.websocketMessageTypes.push(parsed.MessageType);
    summary.invariants.websocketMessageTypes.sort();
  }
}

function compatiblePlayMethod(upstreamMethod, jellyrinMethod) {
  if (!upstreamMethod || !jellyrinMethod) {
    return true;
  }
  return ['DirectPlay', 'DirectStream'].includes(upstreamMethod)
    && ['DirectPlay', 'DirectStream'].includes(jellyrinMethod);
}

function mediaType(contentType) {
  return String(contentType || '').split(';')[0].trim().toLowerCase();
}

function invariantFailures(summary) {
  if (!['p0-direct-play', 'resume'].includes(flow) || summary.status !== 'completed') {
    return [];
  }
  const failures = [];
  if (flow === 'resume') {
    if (!summary.invariants.playbackProgress204) {
      failures.push('missing Sessions/Playing/Progress 204 invariant');
    }
    if (!summary.invariants.resumeList200) {
      failures.push('missing UserItems/Resume 200 invariant');
    }
    if (!summary.invariants.resumeItemMatched) {
      failures.push('missing resume item invariant');
    }
    return failures;
  }
  if (!summary.invariants.playbackInfo200) {
    failures.push('missing PlaybackInfo 200 invariant');
  }
  if (!summary.invariants.streamOk) {
    failures.push('missing video stream 200/206 invariant');
  }
  if (!summary.invariants.sessionPlaying204) {
    failures.push('missing Sessions/Playing 204 invariant');
  }
  if (!summary.invariants.websocketKeepAlive) {
    failures.push('missing websocket keepalive invariant');
  }
  if (summary.invariants.unexpectedTranscodePath) {
    failures.push('direct-play trace unexpectedly used transcode/HLS path');
  }
  if (
    summary.invariants.playMethods.length > 0
    && !summary.invariants.playMethods.every((method) => ['DirectPlay', 'DirectStream'].includes(method))
  ) {
    failures.push(`unexpected play methods: ${summary.invariants.playMethods.join(', ')}`);
  }
  return failures;
}

function ignoredConsoleError(text) {
  return [
    'A bad HTTP response code (404) was received when fetching the script.',
    'Failed to load resource: the server responded with a status of 404 (Not Found)',
    'Failed to load resource: the server responded with a status of 400 (Bad Request)',
    'React Router Future Flag Warning',
    'Not initializing chromecast: chrome object is missing',
    'MEDIA_NOT_SUPPORTED',
  ].some((allowed) => text.includes(allowed));
}

function allowedFailedResponse(response) {
  const url = response.url();
  if (url.includes('/Branding/Splashscreen')) {
    return true;
  }
  if (response.status() === 404 && new URL(url).pathname === '/web/undefined') {
    return true;
  }
  return response.status() === 400 && new URL(url).pathname === '/SyncPlay/List';
}

async function responseShape(response) {
  try {
    return shapeOf(await response.json());
  } catch (_) {
    return '<unreadable-json>';
  }
}

function websocketFrameRecord(direction, url, payload) {
  const parsed = parseJsonPayload(payload);
  const data = parsed && typeof parsed === 'object' ? parsed.Data : undefined;
  return {
    ts: new Date().toISOString(),
    event: 'frame',
    direction,
    url,
    messageType: parsed && typeof parsed === 'object' ? parsed.MessageType : undefined,
    dataShape: data === undefined ? undefined : shapeOf(data),
  };
}

function parseJsonPayload(payload) {
  if (typeof payload !== 'string') {
    return null;
  }
  try {
    return JSON.parse(payload);
  } catch (_) {
    return null;
  }
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

function sanitizePostData(postData) {
  if (!postData) {
    return null;
  }
  try {
    return redactValue(JSON.parse(postData));
  } catch (_) {
    return '<non-json-post-data>';
  }
}

function redactHeaders(headers) {
  return Object.fromEntries(
    Object.entries(headers)
      .filter(([key]) => safeRequestHeader(key))
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, value]) => [
        key,
        secretKey(key) ? '<redacted>' : value,
      ]),
  );
}

function safeRequestHeader(key) {
  return [
    'accept',
    'content-type',
    'origin',
    'range',
    'referer',
    'user-agent',
  ].includes(key.toLowerCase()) || secretKey(key);
}

function selectedResponseHeaders(headers) {
  const selected = {};
  for (const key of [
    'accept-ranges',
    'cache-control',
    'content-length',
    'content-range',
    'content-type',
    'etag',
    'last-modified',
    'location',
  ]) {
    if (headers[key] !== undefined) {
      selected[key] = secretKey(key) ? '<redacted>' : headers[key];
    }
  }
  return selected;
}

function redactValue(value) {
  if (Array.isArray(value)) {
    return value.map(redactValue);
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [
        key,
        secretKey(key) ? '<redacted>' : redactValue(child),
      ]),
    );
  }
  return value;
}

function secretKey(key) {
  return /authorization|cookie|token|api[_-]?key|password|passwd|pw|access[_-]?token|secret/i.test(key);
}

function sanitizeUrl(url) {
  const parsed = new URL(url);
  for (const key of Array.from(parsed.searchParams.keys())) {
    if (secretKey(key)) {
      parsed.searchParams.set(key, 'REDACTED');
    }
  }
  return parsed.toString();
}

function redactText(text) {
  return text
    .replace(/([?&](?:api[_-]?key|ApiKey|access[_-]?token|X-Emby-Token|token|password|Pw)=)[^&\s"']+/gi, '$1REDACTED')
    .replace(/("(?:access[_-]?token|AccessToken|api[_-]?key|ApiKey|X-Emby-Token|token|password|Pw)"\s*:\s*")[^"]+/gi, '$1REDACTED')
    .replace(/(Authorization["':= ]+)(Bearer\s+)?[A-Za-z0-9._~+/=-]{12,}/gi, '$1$2REDACTED');
}

function pathWithQuery(url) {
  const parsed = new URL(sanitizeUrl(url));
  return `${parsed.pathname}${parsed.search}`;
}

function trimTrailingSlash(value) {
  return value.replace(/\/+$/, '');
}

async function jsonlWriter(filePath) {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  const handle = await fs.open(filePath, 'w');
  return {
    async write(record) {
      await handle.write(`${JSON.stringify(record)}\n`);
    },
    async close() {
      await handle.close();
    },
  };
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
