const { test, expect } = require('@playwright/test');

const DEFAULT_DEVICE_ID = 'deployed-playback-hls';
const TICKS_PER_SECOND = 10_000_000;

test.describe('deployed HLS playback compatibility', () => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');

  test('serves a seekable VOD HLS playlist and buffered/seek segments', async ({ browser, page, request, baseURL }) => {
    const username = process.env.JELLYRIN_E2E_ADMIN_USER || process.env.JELLYRIN_E2E_USER;
    const password = process.env.JELLYRIN_E2E_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_PASSWORD;
    test.skip(!username || !password, 'Requires JELLYRIN_E2E_USER/JELLYRIN_E2E_PASSWORD or admin equivalents');

    const auth = await authenticate(request, username, password);
    const item = await resolveVideoItem(request, auth);
    const streams = mediaStreams(item);
    const audioStreamIndex = optionalEnvInt('JELLYRIN_E2E_AUDIO_STREAM_INDEX')
      ?? firstStreamIndex(streams, 'Audio')
      ?? 1;
    const subtitleStreamIndex = optionalEnvInt('JELLYRIN_E2E_SUBTITLE_STREAM_INDEX') ?? -1;
    const startPositionTicks = optionalEnvInt('JELLYRIN_E2E_START_POSITION_TICKS') ?? 0;

    const playSessionIds = [];
    try {
      const playbackInfo = await requestPlaybackInfo(request, auth, item, {
        audioStreamIndex,
        subtitleStreamIndex,
        startPositionTicks,
      });
      playSessionIds.push(playbackInfo.PlaySessionId);
      const mediaSource = playbackInfo.MediaSources?.[0];
      expect(playbackInfo.PlaySessionId).toBeTruthy();
      expect(mediaSource?.TranscodingUrl).toBeTruthy();
      expect(mediaSource?.TranscodingSubProtocol).toBe('hls');

      const { masterText, mainText, mainUrl } = await loadHlsPlaylists(request, baseURL, mediaSource.TranscodingUrl);
      expect(masterText).toContain('#EXT-X-STREAM-INF');
      expect(mainUrl).toContain('main.m3u8');

      const parsed = parseMediaPlaylist(mainText);
      expect(parsed.isVod, mainText.slice(0, 500)).toBe(true);
      expect(parsed.hasEndList, mainText.slice(-500)).toBe(true);
      expect(parsed.discontinuities, 'VOD should not add discontinuity between normal sequential segments').toBe(0);
      expect(parsed.segments.length).toBeGreaterThan(3);

      const targetDuration = parsed.targetDuration ?? 3;
      const expectedRemainingSeconds = Math.max(0, ((item.RunTimeTicks ?? 0) - startPositionTicks) / TICKS_PER_SECOND);
      if (expectedRemainingSeconds > 0) {
        expect(parsed.segments.length * targetDuration).toBeGreaterThanOrEqual(Math.floor(expectedRemainingSeconds * 0.85));
      }

      const bufferedSegments = parsed.segments.slice(0, Math.min(4, parsed.segments.length));
      for (const [index, segment] of bufferedSegments.entries()) {
        const response = await request.get(resolvePlaylistUrl(baseURL, mainUrl, segment.uri), {
          timeout: 20_000,
        });
        expect(response.status(), `buffer segment ${index} ${segment.uri}`).toBe(200);
        expect((await response.body()).length, `buffer segment ${index} has bytes`).toBeGreaterThan(0);
      }

      const seekSegmentIndex = Math.min(
        parsed.segments.length - 1,
        optionalEnvInt('JELLYRIN_E2E_SEEK_SEGMENT_INDEX') ?? Math.max(1, Math.floor(parsed.segments.length / 2)),
      );
      const seekSegment = parsed.segments[seekSegmentIndex];
      const seekResponse = await request.get(resolvePlaylistUrl(baseURL, mainUrl, seekSegment.uri), {
        timeout: 30_000,
      });
      expect(seekResponse.status(), `seek segment ${seekSegmentIndex} ${seekSegment.uri}`).toBe(200);
      expect((await seekResponse.body()).length, `seek segment ${seekSegmentIndex} has bytes`).toBeGreaterThan(0);

      const browserResult = await runBrowserFetchProbe(browser, page, baseURL, auth, item, {
        audioStreamIndex,
        subtitleStreamIndex,
        startPositionTicks,
        seekSegmentIndex,
      });
      playSessionIds.push(browserResult.playSessionId);
      expect(browserResult.playbackInfoStatus).toBe(200);
      expect(browserResult.mainStatus).toBe(200);
      expect(browserResult.isVod).toBe(true);
      expect(browserResult.discontinuities).toBe(0);
      expect(browserResult.bufferStatuses).toEqual(expect.arrayContaining([200]));
      expect(browserResult.seekStatus).toBe(200);
      expect(browserResult.seekBytes).toBeGreaterThan(0);
    } finally {
      await stopPlaySessions(request, auth, playSessionIds);
    }
  });

  test('serves a playable final HLS segment when resume starts past the end', async ({ request, baseURL }) => {
    const username = process.env.JELLYRIN_E2E_ADMIN_USER || process.env.JELLYRIN_E2E_USER;
    const password = process.env.JELLYRIN_E2E_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_PASSWORD;
    test.skip(!username || !password, 'Requires JELLYRIN_E2E_USER/JELLYRIN_E2E_PASSWORD or admin equivalents');

    const auth = await authenticate(request, username, password);
    const item = await resolveVideoItem(request, auth);
    const streams = mediaStreams(item);
    const audioStreamIndex = optionalEnvInt('JELLYRIN_E2E_AUDIO_STREAM_INDEX')
      ?? firstStreamIndex(streams, 'Audio')
      ?? 1;
    const subtitleStreamIndex = optionalEnvInt('JELLYRIN_E2E_SUBTITLE_STREAM_INDEX') ?? -1;
    const runtimeTicks = itemRuntimeTicks(item);
    test.skip(runtimeTicks <= TICKS_PER_SECOND, 'Requires a video with runtime metadata longer than one second');

    const playSessionIds = [];
    try {
      const playbackInfo = await requestPlaybackInfo(request, auth, item, {
        audioStreamIndex,
        subtitleStreamIndex,
        startPositionTicks: runtimeTicks + 1,
      });
      playSessionIds.push(playbackInfo.PlaySessionId);
      const mediaSource = playbackInfo.MediaSources?.[0];
      expect(playbackInfo.PlaySessionId).toBeTruthy();
      expect(mediaSource?.TranscodingUrl).toBeTruthy();
      expect(mediaSource?.TranscodingSubProtocol).toBe('hls');

      const { mainText, mainUrl } = await loadHlsPlaylists(request, baseURL, mediaSource.TranscodingUrl);
      const parsed = parseMediaPlaylist(mainText);
      expect(parsed.isVod, mainText.slice(0, 500)).toBe(true);
      expect(parsed.hasEndList, mainText.slice(-500)).toBe(true);
      expect(parsed.segments.length, mainText).toBeGreaterThan(0);
      expect(parsed.segments[0].duration, mainText).toBeGreaterThan(0);

      const finalResumeResponse = await request.get(resolvePlaylistUrl(baseURL, mainUrl, parsed.segments[0].uri), {
        timeout: 30_000,
      });
      expect(finalResumeResponse.status(), `final resume segment ${parsed.segments[0].uri}`).toBe(200);
      expect((await finalResumeResponse.body()).length, 'final resume segment has bytes').toBeGreaterThan(0);
    } finally {
      await stopPlaySessions(request, auth, playSessionIds);
    }
  });
});

async function authenticate(request, username, password) {
  const response = await request.post('/Users/AuthenticateByName', {
    headers: {
      Authorization: `MediaBrowser Client="Jellyfin Web", Device="Playwright", DeviceId="${DEFAULT_DEVICE_ID}", Version="dev"`,
    },
    data: { Username: username, Pw: password },
  });
  expect(response.status()).toBe(200);
  return response.json();
}

async function resolveVideoItem(request, auth) {
  const configuredItemId = process.env.JELLYRIN_E2E_ITEM_ID;
  if (configuredItemId) {
    const response = await request.get(`/Items/${configuredItemId}?Fields=MediaSources,MediaStreams`, {
      headers: { 'X-Emby-Token': auth.AccessToken },
    });
    expect(response.status()).toBe(200);
    return response.json();
  }

  const response = await request.get(
    `/Items?UserId=${auth.User.Id}&Recursive=true&IncludeItemTypes=Episode,Movie&MediaTypes=Video&Fields=MediaSources,MediaStreams&StartIndex=0&Limit=50`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  expect(response.status()).toBe(200);
  const body = await response.json();
  const item = body.Items?.find(candidate => (candidate.RunTimeTicks ?? 0) > 0 && mediaStreams(candidate).some(stream => stream.Type === 'Video'));
  expect(item, 'at least one playable video item').toBeTruthy();
  return item;
}

async function requestPlaybackInfo(request, auth, item, options) {
  const response = await request.post(`/Items/${item.Id}/PlaybackInfo?UserId=${auth.User.Id}`, {
    headers: {
      'Content-Type': 'application/json',
      'X-Emby-Token': auth.AccessToken,
    },
    data: {
      UserId: auth.User.Id,
      StartTimeTicks: options.startPositionTicks,
      AudioStreamIndex: options.audioStreamIndex,
      SubtitleStreamIndex: options.subtitleStreamIndex,
      EnableDirectPlay: false,
      EnableDirectStream: false,
      EnableTranscoding: true,
      DeviceProfile: hlsTranscodeDeviceProfile(),
    },
  });
  expect(response.status()).toBe(200);
  return response.json();
}

async function loadHlsPlaylists(request, baseURL, transcodingUrl) {
  const masterUrl = absoluteUrl(baseURL, transcodingUrl);
  const masterText = await getTextWithRetry(request, masterUrl, 'master playlist');
  const mainRelative = masterText.split('\n').find(line => line.startsWith('main.m3u8'));
  expect(mainRelative, masterText).toBeTruthy();
  const prefix = transcodingUrl.slice(0, transcodingUrl.lastIndexOf('/') + 1);
  const mainUrl = `${prefix}${mainRelative}`;
  const mainText = await getTextWithRetry(request, absoluteUrl(baseURL, mainUrl), 'media playlist');
  return { masterText, mainText, mainUrl };
}

async function getTextWithRetry(request, url, label) {
  let lastStatus = 0;
  let lastBody = '';
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const response = await request.get(url);
    lastStatus = response.status();
    lastBody = await response.text();
    if (response.status() === 200) {
      return lastBody;
    }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  expect(lastStatus, `${label} ${url}\n${lastBody.slice(0, 1000)}`).toBe(200);
  return lastBody;
}

function parseMediaPlaylist(playlist) {
  const lines = playlist.split(/\r?\n/);
  const segments = [];
  let pendingDuration = null;
  let discontinuities = 0;
  let targetDuration = null;
  for (const line of lines) {
    if (line.startsWith('#EXTINF:')) {
      pendingDuration = Number(line.slice('#EXTINF:'.length).split(',')[0]);
    } else if (line.startsWith('#EXT-X-DISCONTINUITY')) {
      discontinuities += 1;
    } else if (line.startsWith('#EXT-X-TARGETDURATION:')) {
      targetDuration = Number(line.slice('#EXT-X-TARGETDURATION:'.length));
    } else if (line && !line.startsWith('#')) {
      segments.push({ uri: line, duration: pendingDuration });
      pendingDuration = null;
    }
  }
  return {
    isVod: playlist.includes('#EXT-X-PLAYLIST-TYPE:VOD'),
    hasEndList: playlist.includes('#EXT-X-ENDLIST'),
    discontinuities,
    targetDuration,
    segments,
  };
}

async function runBrowserFetchProbe(browser, page, baseURL, auth, item, options) {
  let lastError;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const probePage = attempt === 0 ? page : await browser.newPage();
    try {
      return await runBrowserFetchProbeOnce(probePage, baseURL, auth, item, options);
    } catch (error) {
      lastError = error;
      if (!String(error?.message || error).includes('Target page, context or browser has been closed')) {
        throw error;
      }
    } finally {
      if (attempt > 0) {
        await probePage.close().catch(() => {});
      }
    }
  }
  throw lastError;
}

async function runBrowserFetchProbeOnce(page, baseURL, auth, item, options) {
  await page.goto(`${baseURL}/web/`);
  return page.evaluate(async ({ auth, item, options, deviceProfile }) => {
    const absoluteUrl = path => new URL(path, location.origin).toString();
    const fetchTextWithRetry = async (url, headers = {}) => {
      let response;
      let text = '';
      for (let attempt = 0; attempt < 20; attempt += 1) {
        response = await fetch(url, { headers });
        text = await response.text();
        if (response.status === 200) {
          return { response, text };
        }
        await new Promise(resolve => setTimeout(resolve, 250));
      }
      return { response, text };
    };
    const playbackInfoResponse = await fetch(absoluteUrl(`/Items/${item.Id}/PlaybackInfo?UserId=${auth.User.Id}`), {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Emby-Token': auth.AccessToken,
      },
      body: JSON.stringify({
        UserId: auth.User.Id,
        StartTimeTicks: options.startPositionTicks,
        AudioStreamIndex: options.audioStreamIndex,
        SubtitleStreamIndex: options.subtitleStreamIndex,
        EnableDirectPlay: false,
        EnableDirectStream: false,
        EnableTranscoding: true,
        DeviceProfile: deviceProfile,
      }),
    });
    const playbackInfo = await playbackInfoResponse.json();
    const transcodingUrl = playbackInfo.MediaSources?.[0]?.TranscodingUrl;
    const { response: masterResponse, text: masterText } = await fetchTextWithRetry(absoluteUrl(transcodingUrl), {
      'X-Emby-Token': auth.AccessToken,
    });
    const mainRelative = masterText.split('\n').find(line => line.startsWith('main.m3u8'));
    const mainPrefix = transcodingUrl.slice(0, transcodingUrl.lastIndexOf('/') + 1);
    const { response: mainResponse, text: mainText } = await fetchTextWithRetry(absoluteUrl(`${mainPrefix}${mainRelative}`), {
      'X-Emby-Token': auth.AccessToken,
    });
    const mainAbsoluteUrl = absoluteUrl(`${mainPrefix}${mainRelative}`);
    const segmentUris = mainText.split(/\r?\n/).filter(line => line && !line.startsWith('#'));
    const resolveSegmentUrl = uri => new URL(uri, mainAbsoluteUrl).toString();
    const bufferStatuses = [];
    for (const uri of segmentUris.slice(0, Math.min(3, segmentUris.length))) {
      const response = await fetch(resolveSegmentUrl(uri));
      await response.arrayBuffer();
      bufferStatuses.push(response.status);
    }
    const seekUri = segmentUris[Math.min(options.seekSegmentIndex, segmentUris.length - 1)];
    const seekResponse = await fetch(resolveSegmentUrl(seekUri));
    const seekBytes = (await seekResponse.arrayBuffer()).byteLength;
    return {
      playbackInfoStatus: playbackInfoResponse.status,
      playSessionId: playbackInfo.PlaySessionId,
      masterStatus: masterResponse.status,
      mainStatus: mainResponse.status,
      isVod: mainText.includes('#EXT-X-PLAYLIST-TYPE:VOD'),
      discontinuities: (mainText.match(/#EXT-X-DISCONTINUITY/g) ?? []).length,
      segmentCount: segmentUris.length,
      bufferStatuses,
      seekStatus: seekResponse.status,
      seekBytes,
    };
  }, { auth, item, options, deviceProfile: hlsTranscodeDeviceProfile() });
}

async function stopPlaySessions(request, auth, playSessionIds) {
  for (const playSessionId of new Set(playSessionIds.filter(Boolean))) {
    await request.delete(`/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(playSessionId)}`, {
      headers: { 'X-Emby-Token': auth.AccessToken },
    }).catch(() => {});
  }
}

function mediaStreams(item) {
  return item.MediaStreams ?? item.MediaSources?.[0]?.MediaStreams ?? [];
}

function itemRuntimeTicks(item) {
  return item.RunTimeTicks ?? item.MediaSources?.[0]?.RunTimeTicks ?? 0;
}

function firstStreamIndex(streams, type) {
  return streams.find(stream => stream.Type === type)?.Index;
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

function optionalEnvInt(name) {
  const value = process.env[name];
  if (value === undefined || value === '') {
    return undefined;
  }
  const parsed = Number(value);
  if (!Number.isInteger(parsed)) {
    throw new Error(`${name} must be an integer, got ${value}`);
  }
  return parsed;
}

function absoluteUrl(baseURL, pathOrUrl) {
  return new URL(pathOrUrl, baseURL).toString();
}

function resolvePlaylistUrl(baseURL, playlistUrl, segmentUri) {
  return new URL(segmentUri, absoluteUrl(baseURL, playlistUrl)).toString();
}
