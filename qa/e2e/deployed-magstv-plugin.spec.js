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

const catalogueTimeoutMs = positiveIntegerEnvironment(
  'JELLYRIN_E2E_MAGSTV_VERIFY_TIMEOUT_MS',
  positiveIntegerEnvironment('JELLYRIN_E2E_MAGSTV_SYNC_TIMEOUT_MS', 20 * 60 * 1000),
);
const catalogueCandidateLimit = positiveIntegerEnvironment(
  'JELLYRIN_E2E_MAGSTV_CANDIDATE_LIMIT',
  8,
);
const verifyOnly = process.env.JELLYRIN_E2E_MAGSTV_VERIFY_ONLY === '1';

// Unlike ordinary deployed tests, every failure artifact is disabled here: authenticated
// settings and playback responses must never be retained in a screenshot, video, or trace.
test.use({ trace: 'off', video: 'off', screenshot: 'off' });

test.describe('deployed MAGSTV plugin settings, catalogue, and playback', () => {
  test.skip(
    process.env.JELLYRIN_E2E_DEPLOYED !== '1'
      || process.env.JELLYRIN_E2E_MAGSTV_QA !== '1'
      || !verifyOnly,
    'This opt-in suite only verifies an already-published MAGSTV catalogue',
  );

  test('verifies settings, three scoped views, metadata, artwork, tracks, and Web HLS without synchronizing', async ({ page, request, baseURL }) => {
    test.setTimeout(catalogueTimeoutMs + 20 * 60 * 1000);
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
    // Verify-only is intentionally non-mutating: neither Save nor Refresh is clicked and no
    // provider account value is loaded into this process or into the page DOM.
    await expect(form.locator('#username')).toHaveValue('');
    await expect(form.locator('#password')).toHaveValue('');
    await expect(form.locator('#saveButton')).toBeVisible();
    await expect(form.locator('#refreshButton')).toBeVisible();

    const persisted = await verifyEncryptedTunerConfiguration(request, auth.AccessToken);
    const minima = expectedCatalogueMinima();
    const snapshot = await waitForCompleteCatalogue(
      request,
      auth,
      minima,
      catalogueTimeoutMs,
    );
    expect(snapshot.channels.total).toBe(Number(persisted.PersistedChannelCount));

    await verifyHomeAndOpenViews(page, snapshot.views);

    const movieResult = await probeFirstPlayable(
      snapshot.movies.items,
      (movie) => probeVodWebExperience(request, baseURL, auth, movie, 'movie'),
      'Mags Movies',
    );
    const episodeResult = await probeFirstPlayable(
      snapshot.episodes.items,
      (episode) => probeVodWebExperience(request, baseURL, auth, episode, 'episode'),
      'Mags Series episode',
    );

    expect(movieResult.hlsSegmentBytes).toBeGreaterThan(0);
    expect(episodeResult.hlsSegmentBytes).toBeGreaterThan(0);
    expect(
      browserFailures.catalogueMutations,
      'verify-only browser session must not save or refresh the MAGSTV catalogue',
    ).toEqual([]);
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
        movieHlsSegmentBytes: movieResult.hlsSegmentBytes,
        episodeHlsSegmentBytes: episodeResult.hlsSegmentBytes,
        movieAudioTracks: movieResult.audioTracks,
        episodeAudioTracks: episodeResult.audioTracks,
        movieAudioLanguageTracks: movieResult.audioLanguageTracks,
        episodeAudioLanguageTracks: episodeResult.audioLanguageTracks,
        movieSubtitleTracks: movieResult.subtitleTracks,
        episodeSubtitleTracks: episodeResult.subtitleTracks,
        movieSubtitleVerified: movieResult.subtitleVerified,
        episodeSubtitleVerified: episodeResult.subtitleVerified,
      },
      credentialsPersistedAsEncryptedReference: true,
      catalogueMutationPerformed: false,
    }));
  });
});

test.describe('MAGSTV home view locator contract', () => {
  test('selects the exact accessible links inside My Media only', async ({ page }) => {
    await page.setContent(`
      <nav aria-label="Media">
        <a href="#/wrong-movies">Mags Movies</a>
        <a href="#/wrong-series">Mags Series</a>
        <a href="#/wrong-live">Mags Live TV</a>
      </nav>
      <main>
        <section>
          <h2>My Media</h2>
          <div>
            <a href="#/movies?topParentId=movies-view&amp;collectionType=movies">Mags Movies</a>
            <a href="#/tv?topParentId=series-view&amp;collectionType=tvshows">Mags Series</a>
            <a href="#/list?serverId=server&amp;parentId=live-view">Mags Live TV</a>
          </div>
        </section>
        <section>
          <h2>Continue Watching</h2>
          <a href="#/wrong-later-movies">Mags Movies</a>
        </section>
      </main>
    `);

    const expectedHrefs = {
      [REQUIRED_VIEWS.movies]: '#/movies?topParentId=movies-view&collectionType=movies',
      [REQUIRED_VIEWS.series]: '#/tv?topParentId=series-view&collectionType=tvshows',
      [REQUIRED_VIEWS.liveTv]: '#/list?serverId=server&parentId=live-view',
    };
    for (const [name, href] of Object.entries(expectedHrefs)) {
      const link = myMediaViewLink(page, name);
      await expect(link).toHaveCount(1);
      await expect(link).toHaveAttribute('href', href);
    }
  });
});

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
  const failures = { catalogueMutations: [], chunkErrors: [], pageErrors: [] };
  page.on('request', (request) => {
    if (request.method() === 'GET' || request.method() === 'HEAD') return;
    const path = new URL(request.url()).pathname.toLowerCase();
    if (path === '/livetv/tunerhosts'
      || path === `/plugins/${PLUGIN_ID}/vodlibrary/refresh`) {
      failures.catalogueMutations.push(`${request.method()} ${path}`);
    }
  });
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
    + '&Fields=MediaSources,MediaStreams,Overview,PrimaryImageAspectRatio'
    + `&StartIndex=0&Limit=${catalogueCandidateLimit}&SortBy=SortName`;
}

function normalizePage(body) {
  return {
    total: Number(body?.TotalRecordCount) || 0,
    items: Array.isArray(body?.Items) ? body.Items.filter((item) => item?.Id) : [],
  };
}

async function verifyHomeAndOpenViews(page, views) {
  await page.goto('/web/#/home', { waitUntil: 'domcontentloaded' });
  await expect(myMediaHeading(page)).toBeVisible({ timeout: 60_000 });
  for (const name of Object.values(REQUIRED_VIEWS)) {
    const link = myMediaViewLink(page, name);
    await expect(link, `${name} home section`).toHaveCount(1, { timeout: 60_000 });
    await expect(link, `${name} home section`).toBeVisible({ timeout: 60_000 });
  }

  for (const [kind, name] of Object.entries(REQUIRED_VIEWS)) {
    await page.goto('/web/#/home', { waitUntil: 'domcontentloaded' });
    await expect(myMediaHeading(page)).toBeVisible({ timeout: 60_000 });
    const link = myMediaViewLink(page, name);
    await expect(link).toHaveCount(1, { timeout: 60_000 });
    await expect(link).toBeVisible({ timeout: 60_000 });
    let scopedChannelsResponsePromise = null;
    if (kind === 'liveTv') {
      const href = await link.getAttribute('href');
      expect(href, 'Mags Live TV link href').toBeTruthy();
      expect(hashRoutePath(href), 'Mags Live TV route').toBe('/list');
      expect(hashRouteParameter(href, 'parentId'), 'Mags Live TV route ParentId')
        .toBe(views.liveTv.Id);
      scopedChannelsResponsePromise = page.waitForResponse((response) => {
        if (response.request().method() !== 'GET') return false;
        const url = new URL(response.url());
        return /\/Items$/i.test(url.pathname)
          && searchParameterCaseInsensitive(url.searchParams, 'ParentId') === views.liveTv.Id;
      }, { timeout: 60_000 });
    }
    await link.click();
    await expect(page).not.toHaveURL(/\/web\/#\/home(?:$|\?)/, { timeout: 30_000 });
    const firstRenderedItem = page.locator('.card, .listItem').first();
    await expect(
      firstRenderedItem,
      `${name} first rendered item`,
    ).toBeVisible({ timeout: 60_000 });
    if (kind === 'liveTv') {
      await expect(page).toHaveURL(/\/web\/#\/list\?/);
      expect(hashRouteParameter(page.url(), 'parentId'), 'Mags Live TV loaded ParentId')
        .toBe(views.liveTv.Id);

      const scopedChannelsResponse = await scopedChannelsResponsePromise;
      expect(scopedChannelsResponse.status(), 'Mags Live TV scoped /Items status').toBe(200);
      const scopedChannels = normalizePage(await scopedChannelsResponse.json());
      expect(scopedChannels.items.length, 'Mags Live TV scoped response items').toBeGreaterThan(0);
      expect(
        scopedChannels.items.every((item) => item.TunerHostId === TUNER_ID),
        'Mags Live TV scoped response must contain only MAGSTV channels',
      ).toBe(true);
      const renderedIds = await page.locator('.card[data-id], .listItem[data-id]')
        .evaluateAll((elements) => elements.map((element) => element.getAttribute('data-id')));
      const scopedIds = new Set(scopedChannels.items.map((item) => item.Id));
      expect(
        renderedIds.some((id) => scopedIds.has(id)),
        'Mags Live TV rendered cards must come from the scoped MAGSTV response',
      ).toBe(true);
    }
    expect(views[kind].Id).toBeTruthy();
  }
}

function myMediaHeading(page) {
  return page.getByRole('heading', { name: 'My Media', exact: true, level: 2 });
}

function myMediaViewLink(page, name) {
  const section = myMediaHeading(page).locator('xpath=ancestor::*[.//a][1]');
  return section.getByRole('link', { name, exact: true });
}

function hashRoutePath(url) {
  const hash = new URL(url, 'https://jellyrin.invalid').hash;
  const queryOffset = hash.indexOf('?');
  return hash.slice(1, queryOffset < 0 ? undefined : queryOffset);
}

function hashRouteParameter(url, name) {
  const hash = new URL(url, 'https://jellyrin.invalid').hash;
  const queryOffset = hash.indexOf('?');
  if (queryOffset < 0) return null;
  return new URLSearchParams(hash.slice(queryOffset + 1)).get(name);
}

function searchParameterCaseInsensitive(searchParams, name) {
  const normalizedName = name.toLowerCase();
  for (const [key, value] of searchParams) {
    if (key.toLowerCase() === normalizedName) return value;
  }
  return null;
}

async function probeFirstPlayable(items, probe, label) {
  if (!items.length) throw new Error(`${label} has no candidate items`);
  const failures = new Map();
  for (const item of items.slice(0, catalogueCandidateLimit)) {
    try {
      return await probe(item);
    } catch (error) {
      const reason = safeErrorReason(error);
      failures.set(reason, (failures.get(reason) || 0) + 1);
    }
  }
  const summary = [...failures].map(([reason, count]) => `${reason} x${count}`).join('; ');
  throw new Error(
    `${label} did not yield a complete candidate within the bounded sample (${summary})`,
  );
}

async function probeVodWebExperience(request, baseURL, auth, item, expectedType) {
  const sessions = [];
  try {
    const detail = await getJson(
      request,
      `/Users/${encodeURIComponent(auth.User.Id)}/Items/${encodeURIComponent(item.Id)}`
        + '?Fields=MediaSources,MediaStreams,Overview,PrimaryImageAspectRatio',
      auth.AccessToken,
    );
    if (String(detail?.Type || '').toLowerCase() !== expectedType) {
      throw new Error('VOD metadata type is invalid');
    }
    if (typeof detail?.Overview !== 'string' || !detail.Overview.trim()) {
      throw new Error('VOD metadata overview is unavailable');
    }
    await verifyRealPrimaryArtwork(request, auth, detail.Id);

    // This resolves safe stream descriptors without opening the media URL. Native MPEG-TS is
    // only the discovery fallback; the actual Web request below must negotiate HLS.
    const discovery = await requestPlaybackInfo(request, auth, detail.Id, {
      enableDirectPlay: false,
      enableDirectStream: true,
      enableTranscoding: false,
      subtitleStreamIndex: -1,
      deviceProfile: nativeMpegTsDeviceProfile(),
    });
    const discoverySource = discovery.MediaSources?.[0];
    sessions.push({ item: detail, playback: discovery, source: discoverySource, method: 'DirectStream' });
    if (!discovery.PlaySessionId || !discoverySource) {
      throw new Error('VOD PlaybackInfo track discovery is unavailable');
    }

    const discoveryStreams = Array.isArray(discoverySource.MediaStreams)
      ? discoverySource.MediaStreams
      : [];
    const audioStreams = selectableStreams(discoveryStreams, 'Audio');
    if (audioStreams.length < 2) throw new Error('VOD alternative audio track is unavailable');
    const audioLanguageTracks = verifyOptionalLanguages(audioStreams, 'audio');
    if (!audioLanguageTracks) throw new Error('VOD audio language labels are unavailable');
    const defaultAudioStreamIndex = Number(discoverySource.DefaultAudioStreamIndex);
    if (!Number.isSafeInteger(defaultAudioStreamIndex)
      || !audioStreams.some((stream) => Number(stream.Index) === defaultAudioStreamIndex)) {
      throw new Error('VOD default audio track is unavailable');
    }
    const selectedAudio = selectNonDefaultStream(
      audioStreams,
      defaultAudioStreamIndex,
    );
    if (!selectedAudio?.Language) {
      throw new Error('VOD alternative audio language label is unavailable');
    }
    const subtitleStreams = selectableStreams(discoveryStreams, 'Subtitle');
    if (!subtitleStreams.length) throw new Error('VOD subtitle track is unavailable');
    const subtitleLanguageTracks = verifyOptionalLanguages(subtitleStreams, 'subtitle');
    if (!subtitleLanguageTracks) throw new Error('VOD subtitle language labels are unavailable');
    const selectedSubtitle = subtitleStreams.find((stream) => stream.Language);

    const playback = await requestPlaybackInfo(request, auth, detail.Id, {
      enableDirectPlay: true,
      enableDirectStream: true,
      enableTranscoding: true,
      audioStreamIndex: selectedAudio.Index,
      subtitleStreamIndex: selectedSubtitle.Index,
      deviceProfile: webHlsDeviceProfile(),
    });
    const source = playback.MediaSources?.[0];
    sessions.push({ item: detail, playback, source, method: 'Transcode' });
    if (!playback.PlaySessionId || !source) {
      throw new Error('VOD Web PlaybackInfo is unavailable');
    }
    if (source.SupportsDirectPlay !== false
      || source.SupportsTranscoding !== true
      || source.TranscodingSubProtocol !== 'hls'
      || source.DirectStreamUrl) {
      throw new Error('VOD Web PlaybackInfo did not select HLS');
    }
    if (Number(source.DefaultAudioStreamIndex) !== Number(selectedAudio.Index)) {
      throw new Error('VOD selected audio track was not preserved');
    }
    const selectedPlaybackAudio = selectableStreams(source.MediaStreams || [], 'Audio')
      .find((stream) => Number(stream.Index) === Number(selectedAudio.Index));
    if (!selectedPlaybackAudio
      || (selectedAudio.Language
        && selectedPlaybackAudio.Language !== selectedAudio.Language)) {
      throw new Error('VOD selected audio language was not preserved');
    }
    if (!source.TranscodingUrl) throw new Error('VOD Web HLS URL is unavailable');

    const hlsSegmentBytes = await probeVodHls(
      request,
      baseURL,
      auth.AccessToken,
      source.TranscodingUrl,
    );

    if (Number(source.DefaultSubtitleStreamIndex) !== Number(selectedSubtitle.Index)) {
      throw new Error('VOD selected subtitle track was not preserved');
    }
    const selectedPlaybackSubtitle = selectableStreams(source.MediaStreams || [], 'Subtitle')
      .find((stream) => Number(stream.Index) === Number(selectedSubtitle.Index));
    if (!selectedPlaybackSubtitle
      || selectedPlaybackSubtitle.Language !== selectedSubtitle.Language) {
      throw new Error('VOD selected subtitle language was not preserved');
    }
    await verifyJitSubtitle(request, baseURL, auth.AccessToken, selectedPlaybackSubtitle);

    return {
      hlsSegmentBytes,
      audioTracks: audioStreams.length,
      audioLanguageTracks,
      subtitleTracks: subtitleStreams.length,
      subtitleLanguageTracks,
      subtitleVerified: true,
    };
  } finally {
    await stopPlaybackSessions(request, auth, sessions);
  }
}

function selectableStreams(streams, type) {
  if (!Array.isArray(streams)) return [];
  return streams.filter((stream) => (
    stream?.Type === type
      && Number.isSafeInteger(Number(stream.Index))
      && Number(stream.Index) >= 0
  ));
}

function verifyOptionalLanguages(streams, type) {
  const withLanguage = streams.filter((stream) => stream.Language !== undefined);
  if (withLanguage.some((stream) => (
    typeof stream.Language !== 'string'
      || !stream.Language.trim()
      || stream.Language.length > 64
  ))) {
    throw new Error(`VOD ${type} language metadata is invalid`);
  }
  return withLanguage.length;
}

function selectNonDefaultStream(streams, defaultIndex) {
  const alternatives = streams.filter(
    (stream) => Number(stream.Index) !== Number(defaultIndex),
  );
  return alternatives.find((stream) => typeof stream.Language === 'string') || alternatives[0];
}

async function verifyRealPrimaryArtwork(request, auth, itemId) {
  const response = await request.get(
    `/Items/${encodeURIComponent(itemId)}/Images/Primary`,
    {
      headers: { 'X-Emby-Token': auth.AccessToken },
      timeout: 45_000,
    },
  );
  if (response.status() !== 200) {
    throw new Error(`VOD artwork returned HTTP ${response.status()}`);
  }
  const contentType = response.headers()['content-type'] || '';
  if (!contentType.toLowerCase().startsWith('image/')) {
    throw new Error('VOD artwork content type is invalid');
  }
  const bytes = await response.body();
  const dimensions = imageDimensions(bytes);
  if (!dimensions || dimensions.width <= 1 || dimensions.height <= 1) {
    throw new Error('VOD artwork is a placeholder or invalid image');
  }
}

function imageDimensions(bytes) {
  if (bytes.length >= 24
    && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) {
    return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
  }
  if (bytes.length >= 10 && ['GIF87a', 'GIF89a'].includes(bytes.subarray(0, 6).toString('ascii'))) {
    return { width: bytes.readUInt16LE(6), height: bytes.readUInt16LE(8) };
  }
  if (bytes.length >= 30
    && bytes.subarray(0, 4).toString('ascii') === 'RIFF'
    && bytes.subarray(8, 12).toString('ascii') === 'WEBP') {
    const chunk = bytes.subarray(12, 16).toString('ascii');
    if (chunk === 'VP8X') {
      return {
        width: bytes.readUIntLE(24, 3) + 1,
        height: bytes.readUIntLE(27, 3) + 1,
      };
    }
    if (chunk === 'VP8 ' && bytes.subarray(23, 26).equals(Buffer.from([0x9d, 0x01, 0x2a]))) {
      return {
        width: bytes.readUInt16LE(26) & 0x3fff,
        height: bytes.readUInt16LE(28) & 0x3fff,
      };
    }
    if (chunk === 'VP8L' && bytes[20] === 0x2f) {
      const packed = bytes.readUInt32LE(21);
      return {
        width: (packed & 0x3fff) + 1,
        height: ((packed >>> 14) & 0x3fff) + 1,
      };
    }
  }
  if (bytes.length >= 4 && bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2;
    while (offset + 4 <= bytes.length) {
      if (bytes[offset] !== 0xff) {
        offset += 1;
        continue;
      }
      const marker = bytes[offset + 1];
      offset += 2;
      if (marker === 0xd8 || marker === 0xd9 || (marker >= 0xd0 && marker <= 0xd7)) continue;
      if (offset + 2 > bytes.length) return null;
      const segmentLength = bytes.readUInt16BE(offset);
      if (segmentLength < 2 || offset + segmentLength > bytes.length) return null;
      if ([
        0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7,
        0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
      ].includes(marker) && segmentLength >= 7) {
        return {
          width: bytes.readUInt16BE(offset + 5),
          height: bytes.readUInt16BE(offset + 3),
        };
      }
      offset += segmentLength;
    }
  }
  return null;
}

async function requestPlaybackInfo(request, auth, itemId, options) {
  const data = {
    UserId: auth.User.Id,
    IsPlayback: true,
    StartTimeTicks: 0,
    EnableDirectPlay: options.enableDirectPlay,
    EnableDirectStream: options.enableDirectStream,
    EnableTranscoding: options.enableTranscoding,
    DeviceProfile: options.deviceProfile,
  };
  if (Number.isSafeInteger(options.audioStreamIndex)) {
    data.AudioStreamIndex = options.audioStreamIndex;
  }
  if (Number.isSafeInteger(options.subtitleStreamIndex)) {
    data.SubtitleStreamIndex = options.subtitleStreamIndex;
  }
  const response = await request.post(
    `/Items/${encodeURIComponent(itemId)}/PlaybackInfo?UserId=${encodeURIComponent(auth.User.Id)}`,
    {
      headers: {
        'Content-Type': 'application/json',
        'X-Emby-Token': auth.AccessToken,
      },
      data,
      timeout: 45_000,
    },
  );
  if (response.status() !== 200) {
    throw new Error(`VOD PlaybackInfo returned HTTP ${response.status()}`);
  }
  return response.json();
}

function nativeMpegTsDeviceProfile() {
  return {
    Name: 'MAGSTV QA native MPEG-TS discovery',
    DirectPlayProfiles: [{
      Container: 'ts,mpegts',
      Type: 'Video',
      VideoCodec: 'h264,hevc,mpeg2video',
      AudioCodec: 'aac,mp2,mp3,ac3,eac3',
    }],
    TranscodingProfiles: [],
    ContainerProfiles: [],
    CodecProfiles: [],
    SubtitleProfiles: [{ Format: 'vtt', Method: 'External' }],
  };
}

function webHlsDeviceProfile() {
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
      MinSegments: 1,
      BreakOnNonKeyFrames: false,
    }],
    ContainerProfiles: [],
    CodecProfiles: [],
    SubtitleProfiles: [{ Format: 'vtt', Method: 'External' }],
  };
}

async function probeVodHls(request, baseURL, token, transcodingUrl) {
  const masterUrl = sameOriginUrl(baseURL, transcodingUrl);
  const masterText = await getPlaylistWithRetry(
    request,
    masterUrl,
    token,
    'VOD HLS master playlist',
    true,
  );
  let mediaUrl = masterUrl;
  let mediaText = masterText;
  if (masterText.includes('#EXT-X-STREAM-INF')) {
    const mediaReference = firstPlaylistReference(masterText);
    if (!mediaReference) throw new Error('VOD HLS master playlist has no media reference');
    mediaUrl = new URL(mediaReference, masterUrl).toString();
    mediaText = await getPlaylistWithRetry(
      request,
      mediaUrl,
      token,
      'VOD HLS media playlist',
      true,
    );
  }
  const segmentReference = firstPlaylistReference(mediaText);
  if (!segmentReference) throw new Error('VOD HLS media playlist has no segment');
  const segmentUrl = sameOriginUrl(baseURL, new URL(segmentReference, mediaUrl).toString());
  const segmentResponse = await request.get(segmentUrl, {
    headers: { 'X-Emby-Token': token },
    timeout: 45_000,
  });
  if (segmentResponse.status() !== 200) {
    throw new Error(`VOD HLS segment returned HTTP ${segmentResponse.status()}`);
  }
  const bytes = await segmentResponse.body();
  if (bytes.length < 188) throw new Error('VOD HLS segment is empty or truncated');
  return bytes.length;
}

async function verifyJitSubtitle(request, baseURL, token, stream) {
  if (stream.DeliveryMethod !== 'External'
    || !['vtt', 'webvtt'].includes(String(stream.Codec || '').toLowerCase())
    || !stream.DeliveryUrl) {
    throw new Error('VOD selected subtitle is not externally selectable');
  }
  const url = sameOriginUrl(baseURL, stream.DeliveryUrl);
  const response = await request.get(url, {
    headers: { 'X-Emby-Token': token },
    timeout: 45_000,
  });
  if (response.status() !== 200) {
    throw new Error(`VOD subtitle returned HTTP ${response.status()}`);
  }
  const text = await response.text();
  if (!text.startsWith('WEBVTT') || !text.includes('-->')) {
    throw new Error('VOD subtitle has no WebVTT cue content');
  }
}

async function stopPlaybackSessions(request, auth, sessions) {
  for (const { item, playback, source, method } of sessions.reverse()) {
    if (!playback?.PlaySessionId) continue;
    await request.post('/Sessions/Playing/Stopped', {
      headers: { 'X-Emby-Token': auth.AccessToken },
      data: {
        ItemId: item.Id,
        MediaSourceId: source?.Id || item.Id,
        PlayMethod: method,
        PlaySessionId: playback.PlaySessionId,
        PositionTicks: 0,
        CanSeek: true,
        IsPaused: false,
      },
      timeout: 15_000,
    }).catch(() => {});
    await request.delete(
      `/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(playback.PlaySessionId)}`,
      { headers: { 'X-Emby-Token': auth.AccessToken }, timeout: 15_000 },
    ).catch(() => {});
  }
}

async function getPlaylistWithRetry(request, url, token, label, requireReference) {
  let lastStatus = 0;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const response = await request.get(url, {
      headers: { 'X-Emby-Token': token },
      timeout: 45_000,
    });
    lastStatus = response.status();
    const text = await response.text();
    if (lastStatus === 200
      && text.includes('#EXTM3U')
      && (!requireReference || firstPlaylistReference(text))) {
      return text;
    }
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
  const categorized = [
    [/metadata|overview/i, 'metadata unavailable'],
    [/artwork|image/i, 'artwork unavailable'],
    [/audio/i, 'alternate audio/language unavailable'],
    [/subtitle/i, 'subtitle/language unavailable'],
    [/hls/i, 'Web HLS unavailable'],
    [/PlaybackInfo/i, 'PlaybackInfo unavailable'],
    [/external provider URL/i, 'unsafe playback URL rejected'],
  ].find(([pattern]) => pattern.test(message));
  if (categorized) {
    return status ? `${categorized[1]} (${status.toUpperCase()})` : categorized[1];
  }
  if (status) return status.toUpperCase();
  if (/timeout|timed out/i.test(message)) return 'timeout';
  if (/abort/i.test(message)) return 'aborted';
  return 'probe failed';
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
