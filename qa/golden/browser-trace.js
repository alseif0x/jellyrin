#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { chromium } = require('playwright');
const execFileAsync = promisify(execFile);

const outputRoot = process.env.JELLYRIN_BROWSER_TRACE_OUT
  || path.resolve(__dirname, '../../../../plans/generated/e2e-traces');
const flow = process.env.JELLYRIN_BROWSER_FLOW || 'p0-direct-play';
const chromiumExecutable = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE
  || '/home/cdmonio/.cache/ms-playwright/chromium_headless_shell-1208/chrome-headless-shell-linux64/chrome-headless-shell';
const mediaFixtureDir = process.env.JELLYRIN_MEDIA_FIXTURE_DIR
  || path.resolve(__dirname, '../../var/fixtures/m2-movies');
const subtitleTrickplayFixtureName = 'Jellyrin Subtitle Trickplay Long Fixture';

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
  if (!['login-home', 'p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay'].includes(flow)) {
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
      transcodePlaybackInfo200: false,
      transcodingUrlPresent: false,
      hlsMaster200: false,
      hlsMedia200: false,
      hlsSegment200: false,
      hlsPlaylistShapes: [],
      hlsSegmentContentTypes: [],
      adminSystemInfo200: false,
      adminStorage200: false,
      adminScheduledTasks200: false,
      adminActivityLog200: false,
      adminDevices200: false,
      adminPlugins200: false,
      adminRepositories200: false,
      adminConfigPages200: false,
      libraryViews200: false,
      libraryGroupingOptions200: false,
      libraryVirtualFolders200: false,
      libraryItemsCounts200: false,
      libraryItems200: false,
      libraryLatest200: false,
      libraryViewMatched: false,
      libraryItemMatched: false,
      subtitlePlaybackInfo200: false,
      subtitleStreamMatched: false,
      subtitlePlaylist200: false,
      subtitlePlaylistShape: false,
      subtitleVtt200: false,
      subtitleVttCue: false,
      trickplayPlaylist200: false,
      trickplayImagesOnly: false,
      trickplayTile200: false,
      trickplayTileJpeg: false,
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
    } else if (flow === 'resume') {
      await runResumeFlow(page, summary, publicInfo, target);
    } else if (flow === 'transcode-hls') {
      await runTranscodeHlsFlow(page, summary, publicInfo, target);
    } else if (flow === 'libraries') {
      await runLibrariesFlow(page, summary, publicInfo, target);
    } else if (flow === 'subtitles-trickplay') {
      await runSubtitlesTrickplayFlow(page, summary, publicInfo, target);
    } else {
      await runAdminDashboardFlow(page, summary, publicInfo, target);
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

async function runAdminDashboardFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard');
  await page.waitForLoadState('networkidle');

  const endpoints = [
    ['System/Info', '/System/Info'],
    ['System/Info/Storage', '/System/Info/Storage'],
    ['ScheduledTasks', '/ScheduledTasks?IsEnabled=true'],
    ['System/ActivityLog/Entries', '/System/ActivityLog/Entries?StartIndex=0&Limit=20'],
    ['Devices', '/Devices'],
    ['Plugins', '/Plugins'],
    ['Repositories', '/Repositories'],
    ['ConfigurationPages', '/web/ConfigurationPages?EnableInMainMenu=true'],
  ];
  for (const [name, url] of endpoints) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`${name} returned HTTP ${result.status}`);
    }
    if (result.json === null) {
      throw new Error(`${name} did not return JSON`);
    }
  }

  await page.goto(`${summary.baseUrl}/web/#/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle');
}

async function runLibrariesFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const viewsResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserViews?UserId=${encodeURIComponent(auth.User.Id)}`,
    token: auth.AccessToken,
  });
  if (viewsResult.status !== 200) {
    throw new Error(`UserViews returned HTTP ${viewsResult.status}`);
  }
  const libraryView = (viewsResult.json?.Items || [])
    .find((item) => ['movies', 'boxsets'].includes(String(item.CollectionType || '').toLowerCase()))
    || viewsResult.json?.Items?.[0];
  if (!libraryView?.Id) {
    throw new Error('UserViews returned no library views');
  }
  summary.invariants.libraryViewMatched = true;

  const endpoints = [
    ['UserViews/GroupingOptions', `/UserViews/GroupingOptions?UserId=${encodeURIComponent(auth.User.Id)}`],
    ['Library/VirtualFolders', '/Library/VirtualFolders'],
    ['Items/Counts', `/Items/Counts?UserId=${encodeURIComponent(auth.User.Id)}&ParentId=${encodeURIComponent(libraryView.Id)}`],
  ];
  for (const [name, url] of endpoints) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`${name} returned HTTP ${result.status}`);
    }
    if (result.json === null) {
      throw new Error(`${name} did not return JSON`);
    }
  }

  const itemsResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&ParentId=${encodeURIComponent(libraryView.Id)}&Recursive=true&IncludeItemTypes=Movie&Fields=PrimaryImageAspectRatio,MediaSources,DateCreated&StartIndex=0&Limit=12`,
    token: auth.AccessToken,
  });
  if (itemsResult.status !== 200) {
    throw new Error(`Items returned HTTP ${itemsResult.status}`);
  }
  const libraryItem = itemsResult.json?.Items?.find((item) => item.Type === 'Movie') || itemsResult.json?.Items?.[0];
  if (!libraryItem?.Id) {
    throw new Error('library Items returned no media items');
  }
  summary.invariants.libraryItemMatched = true;

  const latestResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/Users/${encodeURIComponent(auth.User.Id)}/Items/Latest?ParentId=${encodeURIComponent(libraryView.Id)}&IncludeItemTypes=Movie&Limit=12&Fields=PrimaryImageAspectRatio,MediaSources,DateCreated`,
    token: auth.AccessToken,
  });
  if (latestResult.status !== 200) {
    throw new Error(`Users/Items/Latest returned HTTP ${latestResult.status}`);
  }
  if (!Array.isArray(latestResult.json)) {
    throw new Error('Users/Items/Latest did not return a JSON array');
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.library = {
    id: '<dynamic>',
    name: libraryView.Name,
    collectionType: libraryView.CollectionType,
    itemName: libraryItem.Name,
  };
}

async function runSubtitlesTrickplayFlow(page, summary, publicInfo, target) {
  await ensureSubtitleTrickplayFixture();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await refreshLibrary(page, auth);
  const movie = await waitForMovieByName(page, summary, auth, subtitleTrickplayFixtureName);
  const mediaSource = movie.MediaSources?.[0] || {};
  const subtitleStreams = (mediaSource.MediaStreams || movie.MediaStreams || [])
    .filter((stream) => stream.Type === 'Subtitle' && Number.isInteger(Number(stream.Index)));
  const subtitleStream = subtitleStreams.find((stream) => stream.IsExternal === false) || subtitleStreams[0];
  if (!subtitleStream) {
    throw new Error('subtitle-trickplay fixture has no subtitle stream');
  }
  summary.invariants.subtitleStreamMatched = true;
  const subtitleIndex = Number(subtitleStream.Index);
  const mediaSourceId = mediaSource.Id || movie.Id;
  const trickplayWidth = preferredTrickplayWidth(movie);

  const playbackInfo = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(movie.Id)}/PlaybackInfo`,
    token: auth.AccessToken,
    body: withoutUndefined({
      UserId: auth.User.Id,
      MediaSourceId: mediaSourceId,
      AudioStreamIndex: defaultStreamIndex(movie, 'Audio'),
      SubtitleStreamIndex: subtitleIndex,
      EnableDirectPlay: true,
      EnableDirectStream: true,
      EnableTranscoding: true,
      StartPositionTicks: 0,
    }),
  });
  if (playbackInfo.status !== 200) {
    throw new Error(`subtitle PlaybackInfo returned HTTP ${playbackInfo.status}`);
  }
  summary.invariants.subtitlePlaybackInfo200 = true;

  const subtitlePlaylist = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/${encodeURIComponent(mediaSourceId)}/Subtitles/${subtitleIndex}/subtitles.m3u8?SegmentLength=2`,
    token: auth.AccessToken,
  });
  if (subtitlePlaylist.status !== 200) {
    throw new Error(`subtitle playlist returned HTTP ${subtitlePlaylist.status}`);
  }
  summary.invariants.subtitlePlaylist200 = true;
  if (subtitlePlaylist.text.includes('#EXTM3U') && subtitlePlaylist.text.includes('stream.vtt')) {
    summary.invariants.subtitlePlaylistShape = true;
  } else {
    throw new Error('subtitle playlist missing expected HLS/VTT shape');
  }

  const subtitleVtt = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/${encodeURIComponent(mediaSourceId)}/Subtitles/${subtitleIndex}/Stream.vtt?AddVttTimeMap=true`,
    token: auth.AccessToken,
  });
  if (subtitleVtt.status !== 200) {
    throw new Error(`subtitle VTT stream returned HTTP ${subtitleVtt.status}`);
  }
  summary.invariants.subtitleVtt200 = true;
  if (subtitleVtt.text.startsWith('WEBVTT') && subtitleVtt.text.includes('Hello from Jellyrin')) {
    summary.invariants.subtitleVttCue = true;
  } else {
    throw new Error('subtitle VTT stream missing expected cue');
  }

  if (target.name === 'upstream') {
    await ensureUpstreamTrickplayReady(page, auth, movie);
  }

  const trickplayPlaylist = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/Trickplay/${trickplayWidth}/tiles.m3u8`,
    token: auth.AccessToken,
  });
  if (trickplayPlaylist.status !== 200) {
    throw new Error(`trickplay playlist returned HTTP ${trickplayPlaylist.status}`);
  }
  summary.invariants.trickplayPlaylist200 = true;
  if (trickplayPlaylist.text.includes('#EXT-X-IMAGES-ONLY')) {
    summary.invariants.trickplayImagesOnly = true;
  } else {
    throw new Error('trickplay playlist missing EXT-X-IMAGES-ONLY');
  }
  const tilePath = firstPlaylistUri(trickplayPlaylist.text);
  if (!tilePath || !/\.jpg(?:\?|$)/i.test(tilePath)) {
    throw new Error('trickplay playlist did not contain a JPEG tile URI');
  }

  const trickplayTile = await browserFetchBinary(page, {
    method: 'GET',
    url: resolveRelativeUrl(`/Videos/${movie.Id}/Trickplay/${trickplayWidth}/tiles.m3u8`, tilePath),
    token: auth.AccessToken,
  });
  if (trickplayTile.status !== 200) {
    throw new Error(`trickplay tile returned HTTP ${trickplayTile.status}`);
  }
  summary.invariants.trickplayTile200 = true;
  if (trickplayTile.contentType.split(';')[0].trim().toLowerCase() === 'image/jpeg' && trickplayTile.startsWithJpeg) {
    summary.invariants.trickplayTileJpeg = true;
  } else {
    throw new Error(`trickplay tile was not JPEG: ${trickplayTile.contentType}`);
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
    subtitleIndex,
    trickplayWidth,
  };
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

async function runTranscodeHlsFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for transcode trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const playbackInfo = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(movie.Id)}/PlaybackInfo`,
    token: auth.AccessToken,
    body: withoutUndefined({
      UserId: auth.User.Id,
      MediaSourceId: movie.Id,
      AudioStreamIndex: defaultStreamIndex(movie, 'Audio'),
      SubtitleStreamIndex: -1,
      EnableDirectPlay: false,
      EnableDirectStream: false,
      EnableTranscoding: true,
      StartPositionTicks: 0,
      DeviceProfile: hlsTranscodeDeviceProfile(),
    }),
  });
  if (playbackInfo.status !== 200) {
    throw new Error(`transcode PlaybackInfo returned HTTP ${playbackInfo.status}`);
  }
  const mediaSource = playbackInfo.json?.MediaSources?.[0];
  const transcodingUrl = mediaSource?.TranscodingUrl;
  if (!transcodingUrl) {
    throw new Error('transcode PlaybackInfo did not return TranscodingUrl');
  }

  const master = await browserFetchText(page, {
    method: 'GET',
    url: transcodingUrl,
    token: auth.AccessToken,
  });
  if (master.status !== 200) {
    throw new Error(`HLS master playlist returned HTTP ${master.status}`);
  }
  const mediaPlaylistPath = firstPlaylistUri(master.text);
  if (!mediaPlaylistPath) {
    throw new Error('HLS master playlist did not contain a media playlist URI');
  }

  const media = await browserFetchText(page, {
    method: 'GET',
    url: resolveRelativeUrl(transcodingUrl, mediaPlaylistPath),
    token: auth.AccessToken,
  });
  if (media.status !== 200) {
    throw new Error(`HLS media playlist returned HTTP ${media.status}`);
  }
  const segmentPath = firstPlaylistUri(media.text);
  if (!segmentPath) {
    throw new Error('HLS media playlist did not contain a segment URI');
  }

  const segment = await browserFetchText(page, {
    method: 'GET',
    url: resolveRelativeUrl(transcodingUrl, segmentPath),
    token: auth.AccessToken,
  });
  if (![200, 206].includes(segment.status)) {
    throw new Error(`HLS segment returned HTTP ${segment.status}`);
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

async function refreshLibrary(page, auth) {
  const result = await browserFetchJson(page, {
    method: 'POST',
    url: '/Library/Refresh',
    token: auth.AccessToken,
  });
  if (![200, 204].includes(result.status)) {
    throw new Error(`Library/Refresh returned HTTP ${result.status}`);
  }
}

async function waitForMovieByName(page, summary, auth, name) {
  const deadline = Date.now() + 30_000;
  let lastTotal = 0;
  while (Date.now() < deadline) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&SearchTerm=${encodeURIComponent(name)}&Fields=MediaSources,RunTimeTicks,Path&Limit=5`,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`fixture movie lookup returned HTTP ${result.status}`);
    }
    lastTotal = result.json?.TotalRecordCount || 0;
    const movie = result.json?.Items?.find((item) => item.Name === name);
    if (movie) {
      return movie;
    }
    await page.waitForTimeout(1_000);
  }
  throw new Error(`fixture movie ${name} not found after refresh; last count=${lastTotal}`);
}

async function ensureUpstreamTrickplayReady(page, auth, movie) {
  const folders = await browserFetchJson(page, {
    method: 'GET',
    url: '/Library/VirtualFolders',
    token: auth.AccessToken,
  });
  if (folders.status !== 200) {
    throw new Error(`upstream virtual folders lookup returned HTTP ${folders.status}`);
  }
  const folder = folders.json?.find((candidate) => (
    candidate.LibraryOptions?.PathInfos || []
  ).some((pathInfo) => pathInfo.Path && movie.Path?.startsWith(pathInfo.Path)));
  if (!folder?.ItemId || !folder.LibraryOptions) {
    throw new Error('upstream trickplay fixture library folder not found');
  }

  if (!folder.LibraryOptions.EnableTrickplayImageExtraction) {
    const update = await browserFetchJson(page, {
      method: 'POST',
      url: '/Library/VirtualFolders/LibraryOptions',
      token: auth.AccessToken,
      body: {
        Id: folder.ItemId,
        LibraryOptions: {
          ...folder.LibraryOptions,
          EnableTrickplayImageExtraction: true,
          ExtractTrickplayImagesDuringLibraryScan: false,
        },
      },
    });
    if (![200, 204].includes(update.status)) {
      throw new Error(`upstream trickplay library option update returned HTTP ${update.status}`);
    }
  }

  const tasks = await browserFetchJson(page, {
    method: 'GET',
    url: '/ScheduledTasks',
    token: auth.AccessToken,
  });
  if (tasks.status !== 200) {
    throw new Error(`upstream scheduled tasks lookup returned HTTP ${tasks.status}`);
  }
  const trickplayTask = tasks.json?.find((task) => task.Key === 'RefreshTrickplayImages');
  if (!trickplayTask?.Id) {
    throw new Error('upstream trickplay scheduled task not found');
  }
  const start = await browserFetchJson(page, {
    method: 'POST',
    url: `/ScheduledTasks/Running/${encodeURIComponent(trickplayTask.Id)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(start.status)) {
    throw new Error(`upstream trickplay task start returned HTTP ${start.status}`);
  }

  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    const playlist = await browserFetchText(page, {
      method: 'GET',
      url: `/Videos/${encodeURIComponent(movie.Id)}/Trickplay/${preferredTrickplayWidth(movie)}/tiles.m3u8`,
      token: auth.AccessToken,
    });
    if (playlist.status === 200) {
      return;
    }
    await page.waitForTimeout(2_000);
  }
  throw new Error('upstream trickplay playlist was not generated before timeout');
}

function preferredTrickplayWidth(movie) {
  const mediaSource = movie.MediaSources?.[0] || {};
  const videoStream = (mediaSource.MediaStreams || movie.MediaStreams || [])
    .find((stream) => stream.Type === 'Video' && Number(stream.Width) > 0);
  return Math.min(320, Math.max(1, Number(videoStream?.Width || 320)));
}

async function ensureSubtitleTrickplayFixture() {
  await fs.mkdir(mediaFixtureDir, { recursive: true });
  const moviePath = path.join(mediaFixtureDir, `${subtitleTrickplayFixtureName}.mkv`);
  const subtitlePath = path.join(mediaFixtureDir, `${subtitleTrickplayFixtureName}.eng.srt`);
  try {
    await fs.access(moviePath);
    return;
  } catch (_) {
    // Create below.
  }
  await fs.writeFile(
    subtitlePath,
    '1\n00:00:00,000 --> 00:00:01,500\nHello from Jellyrin subtitles\n\n'
      + '2\n00:00:08,000 --> 00:00:10,000\nSecond cue for trickplay coverage\n\n',
  );
  await execFileAsync('ffmpeg', [
    '-hide_banner',
    '-nostdin',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc=size=160x90:rate=1',
    '-f',
    'lavfi',
    '-i',
    'anullsrc=channel_layout=stereo:sample_rate=44100',
    '-i',
    subtitlePath,
    '-t',
    '12',
    '-map',
    '0:v:0',
    '-map',
    '1:a:0',
    '-map',
    '2:s:0',
    '-c:v',
    'mpeg4',
    '-c:a',
    'aac',
    '-c:s',
    'srt',
    '-metadata:s:s:0',
    'language=eng',
    moviePath,
  ]);
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

function defaultStreamIndex(movie, streamType) {
  const streams = movie.MediaSources?.[0]?.MediaStreams || movie.MediaStreams || [];
  const stream = streams.find((candidate) => candidate.Type === streamType && candidate.IsDefault)
    || streams.find((candidate) => candidate.Type === streamType);
  return stream?.Index;
}

function withoutUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, child]) => child !== undefined));
}

function hlsTranscodeDeviceProfile() {
  return {
    DirectPlayProfiles: [],
    TranscodingProfiles: [
      {
        Container: 'ts',
        Type: 'Video',
        AudioCodec: 'aac,mp2,opus,flac',
        VideoCodec: 'h264',
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

async function browserFetchText(page, request) {
  return page.evaluate(async ({ method, url, token }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'X-Emby-Token': token,
      },
    });
    return {
      status: response.status,
      contentType: response.headers.get('content-type') || '',
      text: await response.text(),
    };
  }, request);
}

async function browserFetchBinary(page, request) {
  return page.evaluate(async ({ method, url, token }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'X-Emby-Token': token,
      },
    });
    const bytes = new Uint8Array(await response.arrayBuffer());
    return {
      status: response.status,
      contentType: response.headers.get('content-type') || '',
      startsWithJpeg: bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xd8,
    };
  }, request);
}

function firstPlaylistUri(text) {
  return String(text || '')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => line && !line.startsWith('#'));
}

function resolveRelativeUrl(base, next) {
  return new URL(next, new URL(base, 'http://placeholder.invalid')).pathname
    + new URL(next, new URL(base, 'http://placeholder.invalid')).search;
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
  if (!['p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay'].includes(flow)) {
    return;
  }
  const pathname = new URL(record.url).pathname;
  const key = criticalRequestKey(record);
  if (key) {
    summary.criticalRequests[key] = criticalRequestSummary(record, requestPostData);
  }
  if (pathname.endsWith('/PlaybackInfo') && record.status === 200) {
    summary.invariants.playbackInfo200 = true;
    if (flow === 'subtitles-trickplay') {
      summary.invariants.subtitlePlaybackInfo200 = true;
    }
    if (flow === 'transcode-hls') {
      summary.invariants.transcodePlaybackInfo200 = true;
      if (record.responseShape?.MediaSources?.[0]?.TranscodingUrl) {
        summary.invariants.transcodingUrlPresent = true;
      }
    }
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
  if (flow === 'transcode-hls') {
    if (record.method === 'GET' && /master\.m3u8$/i.test(pathname) && record.status === 200) {
      summary.invariants.hlsMaster200 = true;
      addUnique(summary.invariants.hlsPlaylistShapes, 'master');
    }
    if (record.method === 'GET' && /(?:main|live|stream)\.m3u8$/i.test(pathname) && record.status === 200) {
      summary.invariants.hlsMedia200 = true;
      addUnique(summary.invariants.hlsPlaylistShapes, 'media');
    }
    if (record.method === 'GET' && /\/(?:hls|hls1)\/.*\.(?:ts|mp4|aac|mp3)$/i.test(pathname) && [200, 206].includes(record.status)) {
      summary.invariants.hlsSegment200 = true;
      addUnique(summary.invariants.hlsSegmentContentTypes, mediaType(record.responseContentType));
    }
  }
  if (flow === 'admin-dashboard' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/System/Info') {
      summary.invariants.adminSystemInfo200 = true;
    }
    if (record.method === 'GET' && pathname === '/System/Info/Storage') {
      summary.invariants.adminStorage200 = true;
    }
    if (record.method === 'GET' && pathname === '/ScheduledTasks') {
      summary.invariants.adminScheduledTasks200 = true;
    }
    if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
      summary.invariants.adminActivityLog200 = true;
    }
    if (record.method === 'GET' && pathname === '/Devices') {
      summary.invariants.adminDevices200 = true;
    }
    if (record.method === 'GET' && pathname === '/Plugins') {
      summary.invariants.adminPlugins200 = true;
    }
    if (record.method === 'GET' && pathname === '/Repositories') {
      summary.invariants.adminRepositories200 = true;
    }
    if (record.method === 'GET' && pathname === '/web/ConfigurationPages') {
      summary.invariants.adminConfigPages200 = true;
    }
  }
  if (flow === 'libraries' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/UserViews') {
      summary.invariants.libraryViews200 = true;
    }
    if (record.method === 'GET' && pathname === '/UserViews/GroupingOptions') {
      summary.invariants.libraryGroupingOptions200 = true;
    }
    if (record.method === 'GET' && pathname === '/Library/VirtualFolders') {
      summary.invariants.libraryVirtualFolders200 = true;
    }
    if (record.method === 'GET' && pathname === '/Items/Counts') {
      summary.invariants.libraryItemsCounts200 = true;
    }
    if (record.method === 'GET' && pathname === '/Items') {
      summary.invariants.libraryItems200 = true;
    }
    if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/Latest$/i.test(pathname)) {
      summary.invariants.libraryLatest200 = true;
    }
  }
  if (flow === 'subtitles-trickplay' && record.status === 200) {
    if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/subtitles\.m3u8$/i.test(pathname)) {
      summary.invariants.subtitlePlaylist200 = true;
    }
    if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/Stream\.vtt$/i.test(pathname)) {
      summary.invariants.subtitleVtt200 = true;
    }
    if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/tiles\.m3u8$/i.test(pathname)) {
      summary.invariants.trickplayPlaylist200 = true;
    }
    if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/[^/]+\.jpg$/i.test(pathname)) {
      summary.invariants.trickplayTile200 = true;
    }
  }
}

function criticalRequestKey(record) {
  const pathname = new URL(record.url).pathname;
  if (record.method === 'POST' && pathname.toLowerCase() === '/users/authenticatebyname') {
    return 'auth';
  }
  if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/Latest$/i.test(pathname)) {
    return 'library-latest';
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
  if (record.method === 'GET' && /master\.m3u8$/i.test(pathname)) {
    return 'hls-master';
  }
  if (record.method === 'GET' && /(?:main|live|stream)\.m3u8$/i.test(pathname)) {
    return 'hls-media';
  }
  if (record.method === 'GET' && /\/(?:hls|hls1)\/.*\.(?:ts|mp4|aac|mp3)$/i.test(pathname)) {
    return 'hls-segment';
  }
  if (record.method === 'GET' && pathname === '/System/Info') {
    return 'admin-system-info';
  }
  if (record.method === 'GET' && pathname === '/System/Info/Storage') {
    return 'admin-storage';
  }
  if (record.method === 'GET' && pathname === '/ScheduledTasks') {
    return 'admin-scheduled-tasks';
  }
  if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
    return 'admin-activity-log';
  }
  if (record.method === 'GET' && pathname === '/Devices') {
    return 'admin-devices';
  }
  if (record.method === 'GET' && pathname === '/Plugins') {
    return 'admin-plugins';
  }
  if (record.method === 'GET' && pathname === '/Repositories') {
    return 'admin-repositories';
  }
  if (record.method === 'GET' && pathname === '/web/ConfigurationPages') {
    return 'admin-config-pages';
  }
  if (record.method === 'GET' && pathname === '/UserViews') {
    return 'library-user-views';
  }
  if (record.method === 'GET' && pathname === '/UserViews/GroupingOptions') {
    return 'library-grouping-options';
  }
  if (record.method === 'GET' && pathname === '/Library/VirtualFolders') {
    return 'library-virtual-folders';
  }
  if (record.method === 'GET' && pathname === '/Items/Counts') {
    return 'library-items-counts';
  }
  if (record.method === 'GET' && pathname === '/Items') {
    return 'library-items';
  }
  if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/subtitles\.m3u8$/i.test(pathname)) {
    return 'subtitle-playlist';
  }
  if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/Stream\.vtt$/i.test(pathname)) {
    return 'subtitle-vtt';
  }
  if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/tiles\.m3u8$/i.test(pathname)) {
    return 'trickplay-playlist';
  }
  if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/[^/]+\.jpg$/i.test(pathname)) {
    return 'trickplay-tile';
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
  if (!['p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay'].includes(flow)) {
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
    : flow === 'transcode-hls'
      ? ['playback-info', 'hls-master', 'hls-media', 'hls-segment']
      : flow === 'admin-dashboard'
        ? [
            'admin-system-info',
            'admin-storage',
            'admin-scheduled-tasks',
            'admin-activity-log',
            'admin-devices',
            'admin-plugins',
            'admin-repositories',
            'admin-config-pages',
          ]
        : flow === 'libraries'
          ? [
              'library-user-views',
              'library-grouping-options',
              'library-virtual-folders',
              'library-items-counts',
              'library-items',
              'library-latest',
            ]
          : flow === 'subtitles-trickplay'
            ? ['playback-info', 'subtitle-playlist', 'subtitle-vtt', 'trickplay-playlist', 'trickplay-tile']
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
  if (key === 'video-stream' || key === 'hls-segment') {
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
  if (key === 'trickplay-tile') {
    if (upstreamRequest.status !== jellyrinRequest.status) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != ${jellyrinRequest.status}`);
    }
    if (mediaType(upstreamRequest.contentType) !== mediaType(jellyrinRequest.contentType)) {
      reasons.push(`cross-target ${key}: media type ${upstreamRequest.contentType} != ${jellyrinRequest.contentType}`);
    }
    return reasons;
  }
  if (upstreamRequest.status !== jellyrinRequest.status) {
    reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != ${jellyrinRequest.status}`);
  }
  if ([
    'item-detail',
    'playback-info',
    'resume-list',
    'admin-system-info',
    'admin-storage',
    'admin-scheduled-tasks',
    'admin-activity-log',
    'admin-devices',
    'admin-plugins',
    'admin-repositories',
    'admin-config-pages',
    'library-user-views',
    'library-grouping-options',
    'library-virtual-folders',
    'library-items-counts',
    'library-items',
    'library-latest',
  ].includes(key)) {
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
    'admin-system-info': ['ProductName', 'Version', 'ServerName', 'StartupWizardCompleted'],
    'admin-storage': ['ProgramDataFolder', 'WebFolder', 'CacheFolder', 'LogFolder', 'TranscodingTempFolder'],
    'admin-scheduled-tasks': ['[].Id', '[].Name', '[].Key', '[].State'],
    'admin-activity-log': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Date'],
    'admin-devices': ['Items'],
    'admin-plugins': [],
    'admin-repositories': [],
    'admin-config-pages': [],
    'library-user-views': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'library-grouping-options': [],
    'library-virtual-folders': ['[].Name', '[].CollectionType', '[].Locations'],
    'library-items-counts': [],
    'library-items': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].UserData'],
    'library-latest': ['[].Id', '[].Name', '[].Type'],
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

function addUnique(values, value) {
  if (!values.includes(value)) {
    values.push(value);
    values.sort();
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
  if (!['p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay'].includes(flow) || summary.status !== 'completed') {
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
  if (flow === 'transcode-hls') {
    if (!summary.invariants.transcodePlaybackInfo200) {
      failures.push('missing transcode PlaybackInfo 200 invariant');
    }
    if (!summary.invariants.transcodingUrlPresent) {
      failures.push('missing TranscodingUrl invariant');
    }
    if (!summary.invariants.hlsMaster200) {
      failures.push('missing HLS master playlist 200 invariant');
    }
    if (!summary.invariants.hlsMedia200) {
      failures.push('missing HLS media playlist 200 invariant');
    }
    if (!summary.invariants.hlsSegment200) {
      failures.push('missing HLS segment 200/206 invariant');
    }
    return failures;
  }
  if (flow === 'admin-dashboard') {
    for (const [field, label] of [
      ['adminSystemInfo200', 'System/Info'],
      ['adminStorage200', 'System/Info/Storage'],
      ['adminScheduledTasks200', 'ScheduledTasks'],
      ['adminActivityLog200', 'System/ActivityLog/Entries'],
      ['adminDevices200', 'Devices'],
      ['adminPlugins200', 'Plugins'],
      ['adminRepositories200', 'Repositories'],
      ['adminConfigPages200', 'web/ConfigurationPages'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing admin ${label} 200 invariant`);
      }
    }
    return failures;
  }
  if (flow === 'libraries') {
    for (const [field, label] of [
      ['libraryViews200', 'UserViews'],
      ['libraryGroupingOptions200', 'UserViews/GroupingOptions'],
      ['libraryVirtualFolders200', 'Library/VirtualFolders'],
      ['libraryItemsCounts200', 'Items/Counts'],
      ['libraryItems200', 'Items'],
      ['libraryLatest200', 'Users/Items/Latest'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing library ${label} 200 invariant`);
      }
    }
    if (!summary.invariants.libraryViewMatched) {
      failures.push('missing library view match invariant');
    }
    if (!summary.invariants.libraryItemMatched) {
      failures.push('missing library item match invariant');
    }
    return failures;
  }
  if (flow === 'subtitles-trickplay') {
    for (const [field, label] of [
      ['subtitlePlaybackInfo200', 'subtitle PlaybackInfo'],
      ['subtitleStreamMatched', 'subtitle stream match'],
      ['subtitlePlaylist200', 'subtitle playlist'],
      ['subtitlePlaylistShape', 'subtitle playlist shape'],
      ['subtitleVtt200', 'subtitle VTT stream'],
      ['subtitleVttCue', 'subtitle VTT cue'],
      ['trickplayPlaylist200', 'trickplay playlist'],
      ['trickplayImagesOnly', 'trickplay images-only playlist'],
      ['trickplayTile200', 'trickplay tile'],
      ['trickplayTileJpeg', 'trickplay JPEG tile'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
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
    'You rendered descendant <Routes> (or called `useRoutes()`) at "/"',
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
