const fs = require('node:fs/promises');
const fsConstants = require('node:fs').constants;
const { test, expect } = require('@playwright/test');

const PLUGIN_ID = '7a7a8541-29f8-4c35-99b1-66df55f8399e';
const TUNER_ID = 'magstv';
const DEVICE_ID = 'deployed-magstv-plugin-qa';
const REQUIRED_VIEWS = Object.freeze({
  movies: 'Mags Movies',
  series: 'Mags Series',
  liveTv: 'Mags Live TV',
});

const syncTimeoutMs = positiveIntegerEnvironment(
  'JELLYRIN_E2E_MAGSTV_SYNC_TIMEOUT_MS',
  2 * 60 * 60 * 1000,
);

// Unlike ordinary deployed tests, every failure artifact is disabled here: a failure between
// filling and submitting the form must not preserve even the account name in a screenshot.
test.use({ trace: 'off', video: 'off', screenshot: 'off' });

test.describe('deployed MAGSTV plugin settings, catalogue, and playback', () => {
  test.skip(
    process.env.JELLYRIN_E2E_DEPLOYED !== '1'
      || process.env.JELLYRIN_E2E_MAGSTV_QA !== '1',
    'This opt-in suite changes the deployed MAGSTV tuner and contacts the real provider',
  );

  test('configures only username/password, synchronizes all three views, and plays one item of each kind', async ({ page, request, baseURL }) => {
    test.setTimeout(syncTimeoutMs + 20 * 60 * 1000);
    const providerCredentials = loadProviderCredentials();
    const auth = await loadAdministratorAuthentication(request);
    const publicInfo = await getJson(request, '/System/Info/Public');
    const configurationPage = await discoverConfigurationPage(request, auth.AccessToken);
    const browserFailures = monitorBrowserFailures(page);

    await installBrowserSession(page, baseURL, publicInfo, auth);
    await page.goto(
      `/web/#/configurationpage?name=${encodeURIComponent(configurationPage.Name)}`,
      { waitUntil: 'domcontentloaded' },
    );

    const form = page.locator('#magstvForm');
    await expect(form).toBeVisible({ timeout: 60_000 });
    await expect(form.locator('input')).toHaveCount(2);
    await expect(form.locator('#username')).toBeVisible();
    await expect(form.locator('#password')).toHaveAttribute('type', 'password');
    await expect(form).not.toContainText(/sign[_ -]?o3|secret[_ -]?hex|bootstrap|vpn/i);

    await form.locator('#username').fill(providerCredentials.username);
    await form.locator('#password').fill(providerCredentials.password);
    const saveResponsePromise = page.waitForResponse(
      (response) => response.request().method() === 'POST'
        && new URL(response.url()).pathname.toLowerCase() === '/livetv/tunerhosts',
      { timeout: syncTimeoutMs },
    );
    await form.locator('#saveButton').click();
    const saveResponse = await saveResponsePromise;
    expect(saveResponse.status(), 'MAGSTV tuner save/import status').toBe(200);

    // The controller clears both fields before sending the request. This also keeps screenshots
    // and DOM snapshots from retaining credentials if a later catalogue assertion fails.
    await expect(form.locator('#username')).toHaveValue('');
    await expect(form.locator('#password')).toHaveValue('');
    await expect(form.locator('#status')).toHaveClass(/\bok\b/);
    await expect(form.locator('#refreshButton')).toBeEnabled();

    let explicitRefreshCounts = null;
    if (process.env.JELLYRIN_E2E_MAGSTV_CLICK_REFRESH === '1') {
      const refreshResponsePromise = page.waitForResponse(
        (response) => response.request().method() === 'POST'
          && new URL(response.url()).pathname.toLowerCase()
            === `/plugins/${PLUGIN_ID.toLowerCase()}/vodlibrary/refresh`,
        { timeout: syncTimeoutMs },
      );
      await form.locator('#refreshButton').click();
      const refreshResponse = await refreshResponsePromise;
      expect(refreshResponse.status(), 'explicit MAGSTV VOD refresh status').toBe(200);
      explicitRefreshCounts = await refreshResponse.json();
      await expect(form.locator('#status')).toContainText('Catálogo actualizado');
    }

    const persisted = await verifyEncryptedTunerConfiguration(request, auth.AccessToken);
    const minima = expectedCatalogueMinima();
    const snapshot = await waitForCompleteCatalogue(
      request,
      auth,
      minima,
      syncTimeoutMs,
    );
    expect(snapshot.channels.total).toBe(Number(persisted.PersistedChannelCount));

    if (explicitRefreshCounts) {
      expect(snapshot.movies.total).toBe(Number(explicitRefreshCounts.MovieCount));
      expect(snapshot.series.total).toBe(Number(explicitRefreshCounts.SeriesCount));
      expect(snapshot.episodes.total).toBe(Number(explicitRefreshCounts.EpisodeCount));
    }

    await verifyHomeAndOpenViews(page, snapshot.views);

    const liveResult = await probeFirstPlayable(
      snapshot.channels.items,
      (channel) => probeLiveTvHls(request, baseURL, auth, channel),
      'Mags Live TV',
    );
    const movieResult = await probeFirstPlayable(
      snapshot.movies.items,
      (movie) => probeVodStream(baseURL, auth, movie),
      'Mags Movies',
    );
    const episodeResult = await probeFirstPlayable(
      snapshot.episodes.items,
      (episode) => probeVodStream(baseURL, auth, episode),
      'Mags Series episode',
    );

    expect(liveResult.bytes).toBeGreaterThan(0);
    expect(movieResult.bytes).toBeGreaterThan(0);
    expect(episodeResult.bytes).toBeGreaterThan(0);
    expect(browserFailures.chunkErrors, 'Jellyfin Web chunk loading errors').toEqual([]);
    expect(browserFailures.pageErrors, 'Jellyfin Web page errors').toEqual([]);

    console.log(JSON.stringify({
      status: 'magstv-e2e-passed',
      counts: {
        channels: snapshot.channels.total,
        movies: snapshot.movies.total,
        series: snapshot.series.total,
        episodes: snapshot.episodes.total,
      },
      playback: {
        liveTvBytes: liveResult.bytes,
        movieBytes: movieResult.bytes,
        episodeBytes: episodeResult.bytes,
      },
      credentialsPersistedAsEncryptedReference: true,
    }));
  });
});

function loadProviderCredentials() {
  const username = process.env.JELLYRIN_MAGSTV_USERNAME;
  const password = process.env.JELLYRIN_MAGSTV_PASSWORD;
  if (typeof username !== 'string' || !username.trim() || typeof password !== 'string' || !password) {
    throw new Error('Set JELLYRIN_MAGSTV_USERNAME and JELLYRIN_MAGSTV_PASSWORD for the opt-in QA');
  }
  return { username: username.trim(), password };
}

async function loadAdministratorAuthentication(request) {
  const token = await loadAdministratorToken();
  if (token) {
    const user = await getJson(request, '/Users/Me', token);
    if (user?.Policy?.IsAdministrator !== true) {
      throw new Error('The supplied Jellyrin token is not an administrator token');
    }
    return { AccessToken: token, User: user };
  }

  const username = process.env.JELLYRIN_E2E_ADMIN_USER;
  const password = process.env.JELLYRIN_E2E_ADMIN_PASSWORD;
  if (!username || !password) {
    throw new Error('Set an administrator token/file or JELLYRIN_E2E_ADMIN_USER and JELLYRIN_E2E_ADMIN_PASSWORD');
  }
  const response = await request.post('/Users/AuthenticateByName', {
    headers: {
      Authorization: `MediaBrowser Client="Jellyfin Web", Device="Playwright MAGSTV QA", DeviceId="${DEVICE_ID}", Version="dev"`,
    },
    data: { Username: username, Pw: password },
    timeout: 30_000,
  });
  if (response.status() !== 200) {
    throw new Error(`Jellyrin administrator authentication returned HTTP ${response.status()}`);
  }
  const auth = await response.json();
  if (auth?.User?.Policy?.IsAdministrator !== true || !auth.AccessToken) {
    throw new Error('Jellyrin authentication did not return an administrator session');
  }
  return auth;
}

async function loadAdministratorToken() {
  const environmentToken = (
    process.env.JELLYRIN_E2E_API_TOKEN
      || process.env.JELLYRIN_API_TOKEN
      || ''
  ).trim();
  if (environmentToken) return environmentToken;

  const filePath = process.env.JELLYRIN_E2E_API_TOKEN_FILE
    || process.env.JELLYRIN_API_TOKEN_FILE;
  if (!filePath) return null;
  let handle;
  try {
    handle = await fs.open(filePath, fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW || 0));
    const stat = await handle.stat();
    if (!stat.isFile() || (process.platform !== 'win32' && (stat.mode & 0o077) !== 0)) {
      throw new Error('The Jellyrin token file must be a regular file with mode 0600 or stricter');
    }
    const raw = (await handle.readFile('utf8')).trim();
    if (!raw) throw new Error('The Jellyrin token file is empty');
    if (!raw.startsWith('{')) return raw;
    const value = JSON.parse(raw);
    const token = value.AccessToken || value.accessToken || value.token;
    if (typeof token !== 'string' || !token.trim()) {
      throw new Error('The Jellyrin token file has no supported token field');
    }
    return token.trim();
  } finally {
    await handle?.close();
  }
}

async function discoverConfigurationPage(request, token) {
  const pages = await getJson(request, '/web/ConfigurationPages', token);
  const page = pages.find((candidate) => candidate?.PluginId?.toLowerCase() === PLUGIN_ID);
  if (!page?.Name) throw new Error('The active MAGSTV plugin configuration page was not discovered');
  return page;
}

async function installBrowserSession(page, baseURL, publicInfo, auth) {
  const address = new URL(baseURL).origin;
  const credentials = {
    Servers: [{
      Id: publicInfo.Id,
      Name: publicInfo.ServerName || 'Jellyrin',
      Version: publicInfo.Version,
      LocalAddress: address,
      ManualAddress: address,
      RemoteAddress: address,
      LastConnectionMode: 2,
      DateLastAccessed: Date.now(),
      UserId: auth.User.Id,
      AccessToken: auth.AccessToken,
    }],
  };
  await page.addInitScript((storedCredentials) => {
    localStorage.setItem('jellyfin_credentials', JSON.stringify(storedCredentials));
  }, credentials);
}

function monitorBrowserFailures(page) {
  const failures = { chunkErrors: [], pageErrors: [] };
  page.on('response', (response) => {
    const url = response.url();
    if (response.status() >= 400 && /(?:chunk|bundle)\.js(?:\?|$)/i.test(url)) {
      failures.chunkErrors.push(`${response.status()} ${new URL(url).pathname}`);
    }
  });
  page.on('pageerror', (error) => {
    const message = String(error?.message || error);
    if (/ChunkLoadError|Loading chunk .* failed/i.test(message)) {
      failures.chunkErrors.push(message);
    } else {
      failures.pageErrors.push(message);
    }
  });
  return failures;
}

async function verifyEncryptedTunerConfiguration(request, token) {
  const configuration = await getJson(request, '/System/Configuration/livetv', token);
  const tuner = configuration?.TunerHosts?.find((candidate) => candidate?.Id === TUNER_ID);
  if (!tuner) throw new Error('The persisted MAGSTV tuner is missing after the settings save');
  const serialized = JSON.stringify(tuner).toLowerCase();
  expect(serialized.includes('"username"')).toBe(false);
  expect(serialized.includes('"password"')).toBe(false);
  expect(tuner.Type).toBe(`plugin:${PLUGIN_ID}`);
  expect(tuner.FriendlyName).toBe(REQUIRED_VIEWS.liveTv);
  expect(tuner.JellyrinProviderSecretRef?.Id).toBeTruthy();
  expect(tuner.JellyrinProviderSecretRef?.Provider).toBeTruthy();
  expect(Number(tuner.JellyrinProviderSecretRef?.Revision)).toBeGreaterThan(0);
  expect(Number(tuner.PersistedChannelCount)).toBeGreaterThan(0);
  return tuner;
}

function expectedCatalogueMinima() {
  return {
    channels: positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_MIN_CHANNELS', 1000),
    movies: positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_MIN_MOVIES', 30000),
    series: positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_MIN_SERIES', 20000),
    // This must stay above the retired 100k import ceiling: a capped snapshot is not "full".
    episodes: positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_MIN_EPISODES', 100001),
  };
}

async function waitForCompleteCatalogue(request, auth, minima, timeoutMs) {
  const startedAt = Date.now();
  let last = null;
  while (Date.now() - startedAt < timeoutMs) {
    try {
      last = await catalogueSnapshot(request, auth);
      if (
        last.channels.total >= minima.channels
        && last.movies.total >= minima.movies
        && last.series.total >= minima.series
        && last.episodes.total >= minima.episodes
      ) {
        return last;
      }
      console.log(JSON.stringify({
        status: 'magstv-sync-pending',
        elapsedSeconds: Math.floor((Date.now() - startedAt) / 1000),
        counts: {
          channels: last.channels.total,
          movies: last.movies.total,
          series: last.series.total,
          episodes: last.episodes.total,
        },
      }));
    } catch (error) {
      console.log(JSON.stringify({
        status: 'magstv-sync-pending',
        elapsedSeconds: Math.floor((Date.now() - startedAt) / 1000),
        reason: safeErrorReason(error),
      }));
    }
    await delay(positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_POLL_MS', 30_000));
  }
  const counts = last
    ? `${last.channels.total}/${last.movies.total}/${last.series.total}/${last.episodes.total}`
    : 'unavailable';
  throw new Error(`MAGSTV catalogue did not reach the required channel/movie/series/episode minima; final counts ${counts}`);
}

async function catalogueSnapshot(request, auth) {
  const headers = { 'X-Emby-Token': auth.AccessToken };
  const viewsBody = await getJson(
    request,
    `/UserViews?UserId=${encodeURIComponent(auth.User.Id)}`,
    auth.AccessToken,
  );
  const views = {};
  for (const [kind, requiredName] of Object.entries(REQUIRED_VIEWS)) {
    const view = viewsBody?.Items?.find((candidate) => candidate?.Name === requiredName);
    if (!view?.Id) throw new Error(`Required MAGSTV view is not ready: ${requiredName}`);
    views[kind] = view;
  }

  const [moviesResponse, seriesResponse, episodesResponse, channelsResponse] = await Promise.all([
    request.get(itemsPath(auth.User.Id, views.movies.Id, 'Movie'), { headers, timeout: 60_000 }),
    request.get(itemsPath(auth.User.Id, views.series.Id, 'Series'), { headers, timeout: 60_000 }),
    request.get(itemsPath(auth.User.Id, views.series.Id, 'Episode'), { headers, timeout: 60_000 }),
    request.get(
      `/LiveTv/Channels?UserId=${encodeURIComponent(auth.User.Id)}&ParentId=${encodeURIComponent(views.liveTv.Id)}&StartIndex=0&Limit=5`,
      { headers, timeout: 60_000 },
    ),
  ]);
  for (const [label, response] of [
    ['movies', moviesResponse],
    ['series', seriesResponse],
    ['episodes', episodesResponse],
    ['channels', channelsResponse],
  ]) {
    if (response.status() !== 200) throw new Error(`MAGSTV ${label} query returned HTTP ${response.status()}`);
  }
  const [movies, series, episodes, channels] = await Promise.all([
    moviesResponse.json(),
    seriesResponse.json(),
    episodesResponse.json(),
    channelsResponse.json(),
  ]);
  const normalized = {
    views,
    movies: normalizePage(movies),
    series: normalizePage(series),
    episodes: normalizePage(episodes),
    channels: normalizePage(channels),
  };
  expect(normalized.channels.items.every((item) => item.TunerHostId === TUNER_ID)).toBe(true);
  return normalized;
}

function itemsPath(userId, parentId, itemType) {
  return `/Items?UserId=${encodeURIComponent(userId)}`
    + `&ParentId=${encodeURIComponent(parentId)}`
    + `&Recursive=true&IncludeItemTypes=${encodeURIComponent(itemType)}`
    + '&Fields=MediaSources,MediaStreams&StartIndex=0&Limit=5&SortBy=SortName';
}

function normalizePage(body) {
  return {
    total: Number(body?.TotalRecordCount) || 0,
    items: Array.isArray(body?.Items) ? body.Items.filter((item) => item?.Id) : [],
  };
}

async function verifyHomeAndOpenViews(page, views) {
  await page.goto('/web/#/home', { waitUntil: 'domcontentloaded' });
  for (const name of Object.values(REQUIRED_VIEWS)) {
    await expect(
      page.locator('.homeLibraryButton').filter({ hasText: name }).first(),
      `${name} home section`,
    ).toBeVisible({ timeout: 60_000 });
  }

  for (const [kind, name] of Object.entries(REQUIRED_VIEWS)) {
    await page.goto('/web/#/home', { waitUntil: 'domcontentloaded' });
    const link = page.locator('.homeLibraryButton').filter({ hasText: name }).first();
    await expect(link).toBeVisible({ timeout: 60_000 });
    await link.click();
    await expect(page).not.toHaveURL(/\/web\/#\/home(?:$|\?)/, { timeout: 30_000 });
    await expect(
      page.locator('.card, .listItem').first(),
      `${name} first rendered item`,
    ).toBeVisible({ timeout: 60_000 });
    expect(views[kind].Id).toBeTruthy();
  }
}

async function probeFirstPlayable(items, probe, label) {
  if (!items.length) throw new Error(`${label} has no candidate items`);
  const failures = [];
  for (const item of items.slice(0, 5)) {
    try {
      return await probe(item);
    } catch (error) {
      failures.push(safeErrorReason(error));
    }
  }
  throw new Error(`${label} did not yield a playable item (${failures.join('; ')})`);
}

async function probeLiveTvHls(request, baseURL, auth, channel) {
  const playback = await requestPlaybackInfo(request, auth, channel.Id, true);
  const source = playback.MediaSources?.[0];
  if (!source?.TranscodingUrl || !playback.PlaySessionId) {
    throw new Error('Live TV PlaybackInfo did not return an HLS session');
  }
  let segmentBytes = 0;
  try {
    const masterUrl = sameOriginUrl(baseURL, source.TranscodingUrl);
    const masterText = await getTextWithRetry(request, masterUrl, 'live master playlist');
    let mediaUrl = masterUrl;
    let mediaText = masterText;
    if (masterText.includes('#EXT-X-STREAM-INF')) {
      const mediaReference = firstPlaylistReference(masterText);
      if (!mediaReference) throw new Error('Live HLS master has no media playlist');
      mediaUrl = new URL(mediaReference, masterUrl).toString();
      mediaText = await getTextWithRetry(request, mediaUrl, 'live media playlist');
    }
    const segmentReference = firstPlaylistReference(mediaText);
    if (!segmentReference) throw new Error('Live HLS media playlist has no segment');
    const segmentResponse = await request.get(new URL(segmentReference, mediaUrl).toString(), {
      timeout: 45_000,
    });
    if (segmentResponse.status() !== 200) {
      throw new Error(`Live HLS segment returned HTTP ${segmentResponse.status()}`);
    }
    segmentBytes = (await segmentResponse.body()).length;
    if (segmentBytes < 1) throw new Error('Live HLS segment is empty');
    return { bytes: segmentBytes };
  } finally {
    await request.post('/Sessions/Playing/Stopped', {
      headers: { 'X-Emby-Token': auth.AccessToken },
      data: {
        ItemId: channel.Id,
        MediaSourceId: source?.Id || channel.Id,
        PlayMethod: 'Transcode',
        PlaySessionId: playback.PlaySessionId,
        PositionTicks: 0,
        CanSeek: true,
        IsPaused: false,
      },
      timeout: 15_000,
    }).catch(() => {});
    if (playback.PlaySessionId) {
      await request.delete(
        `/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(playback.PlaySessionId)}&DeviceId=${DEVICE_ID}`,
        { headers: { 'X-Emby-Token': auth.AccessToken }, timeout: 15_000 },
      ).catch(() => {});
    }
  }
}

async function probeVodStream(baseURL, auth, item) {
  const url = new URL(`/Videos/${encodeURIComponent(item.Id)}/stream.ts`, baseURL);
  url.searchParams.set('Static', 'true');
  url.searchParams.set('MediaSourceId', item.Id);
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 45_000);
  try {
    const response = await fetch(url, {
      headers: {
        'X-Emby-Token': auth.AccessToken,
        Range: 'bytes=0-1315',
      },
      redirect: 'error',
      signal: controller.signal,
    });
    if (![200, 206].includes(response.status)) {
      throw new Error(`VOD stream returned HTTP ${response.status}`);
    }
    const contentType = response.headers.get('content-type') || '';
    if (!/(?:video|octet-stream)/i.test(contentType)) {
      throw new Error(`VOD stream returned unexpected content type ${contentType || 'missing'}`);
    }
    const reader = response.body?.getReader();
    if (!reader) throw new Error('VOD stream has no response body');
    const first = await reader.read();
    await reader.cancel().catch(() => {});
    const bytes = first.value?.byteLength || 0;
    if (bytes < 1) throw new Error('VOD stream returned no media bytes');
    return { bytes };
  } finally {
    clearTimeout(timeout);
    controller.abort();
  }
}

async function requestPlaybackInfo(request, auth, itemId, live) {
  const response = await request.post(`/Items/${encodeURIComponent(itemId)}/PlaybackInfo`, {
    headers: { 'X-Emby-Token': auth.AccessToken },
    data: {
      UserId: auth.User.Id,
      IsPlayback: true,
      AutoOpenLiveStream: live,
      EnableDirectPlay: true,
      EnableDirectStream: true,
      EnableTranscoding: true,
      DeviceProfile: hlsDeviceProfile(),
    },
    timeout: 45_000,
  });
  if (response.status() !== 200) {
    throw new Error(`PlaybackInfo returned HTTP ${response.status()}`);
  }
  return response.json();
}

function hlsDeviceProfile() {
  return {
    Name: 'Jellyfin Web',
    MaxStreamingBitrate: 120_000_000,
    DirectPlayProfiles: [
      { Container: 'webm', Type: 'Video', VideoCodec: 'vp8,vp9,av1', AudioCodec: 'vorbis,opus' },
      { Container: 'mp4,m4v', Type: 'Video', VideoCodec: 'h264,av1', AudioCodec: 'aac,mp3,opus,flac,vorbis' },
    ],
    TranscodingProfiles: [{
      Container: 'ts',
      Type: 'Video',
      VideoCodec: 'h264',
      AudioCodec: 'aac',
      Protocol: 'hls',
      Context: 'Streaming',
      EnableMpegtsM2TsMode: true,
      CopyTimestamps: true,
    }],
    ContainerProfiles: [],
    CodecProfiles: [],
    SubtitleProfiles: [],
  };
}

async function getTextWithRetry(request, url, label) {
  let lastStatus = 0;
  for (let attempt = 0; attempt < 30; attempt += 1) {
    const response = await request.get(url, { timeout: 30_000 });
    lastStatus = response.status();
    const text = await response.text();
    if (lastStatus === 200 && text.includes('#EXTM3U')) return text;
    await delay(500);
  }
  throw new Error(`${label} did not become ready; final HTTP status ${lastStatus}`);
}

function firstPlaylistReference(playlist) {
  return playlist
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => line && !line.startsWith('#'));
}

function sameOriginUrl(baseURL, pathOrUrl) {
  const base = new URL(baseURL);
  const value = new URL(pathOrUrl, base);
  if (value.origin !== base.origin) {
    throw new Error('PlaybackInfo attempted to expose an external provider URL');
  }
  return value.toString();
}

async function getJson(request, route, token) {
  const response = await request.get(route, {
    headers: token ? { 'X-Emby-Token': token } : undefined,
    timeout: 60_000,
  });
  if (response.status() !== 200) {
    throw new Error(`${new URL(route, 'http://qa.invalid').pathname} returned HTTP ${response.status()}`);
  }
  return response.json();
}

function positiveIntegerEnvironment(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new Error(`${name} must be a positive integer`);
  }
  return value;
}

function safeErrorReason(error) {
  const message = String(error?.message || error);
  const status = message.match(/HTTP \d{3}/i)?.[0];
  if (status) return status.toUpperCase();
  if (/timeout|timed out/i.test(message)) return 'timeout';
  if (/abort/i.test(message)) return 'aborted';
  return 'probe failed';
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
