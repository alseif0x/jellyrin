const { test, expect } = require('@playwright/test');

const DEFAULT_DEVICE_ID = 'deployed-live-tv-hls';

test.describe('deployed Live TV HLS compatibility', () => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');

  test('serves Live TV channels through HLS and releases tuner leases on stop', async ({ request, baseURL }) => {
    const username = process.env.JELLYRIN_E2E_ADMIN_USER || process.env.JELLYRIN_E2E_USER;
    const password = process.env.JELLYRIN_E2E_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_PASSWORD;
    test.skip(!username || !password, 'Requires JELLYRIN_E2E_USER/JELLYRIN_E2E_PASSWORD or admin equivalents');

    const auth = await authenticate(request, username, password);
    const channels = await resolveLiveTvChannels(request, auth);
    expect(channels.length, 'playable Live TV channels discovered').toBeGreaterThan(0);
    const baselineLeaseCount = await activeTunerLeaseCount(request, auth);

    const results = [];
    for (const channel of channels) {
      const playbackInfo = await requestPlaybackInfo(request, auth, channel.Id);
      try {
        const mediaSource = playbackInfo.MediaSources?.[0];
        expect(playbackInfo.PlaySessionId, `${channel.Id} PlaySessionId`).toBeTruthy();
        expect(mediaSource?.Id, `${channel.Id} MediaSource.Id`).toBe(channel.Id);
        expect(mediaSource?.SupportsTranscoding, `${channel.Id} SupportsTranscoding`).toBe(true);
        expect(mediaSource?.SupportsDirectStream, `${channel.Id} raw TS is not selected for Web`).toBe(false);
        expect(mediaSource?.DirectStreamUrl, `${channel.Id} does not expose a competing Web TS URL`).toBeFalsy();
        expect(mediaSource?.TranscodingUrl, `${channel.Id} TranscodingUrl`).toBeTruthy();

        const { mainUrl, mainText } = await loadHlsPlaylists(request, baseURL, mediaSource.TranscodingUrl);
        const segmentUri = firstSegmentUri(mainText);
        expect(segmentUri, `${channel.Id} media playlist has a segment`).toBeTruthy();

        const segmentUrl = resolvePlaylistUrl(baseURL, mainUrl, segmentUri);
        const segmentResponse = await request.get(segmentUrl, { timeout: 30_000 });
        const segmentBytes = await segmentResponse.body();
        expect(segmentResponse.status(), `${channel.Id} first segment status`).toBe(200);
        expect(segmentBytes.length, `${channel.Id} first segment bytes`).toBeGreaterThan(0);

        results.push({
          id: channel.Id,
          name: channel.Name,
          segmentBytes: segmentBytes.length,
        });
      } finally {
        const stopped = await request.post('/Sessions/Playing/Stopped', {
          headers: { 'X-Emby-Token': auth.AccessToken },
          data: {
            ItemId: channel.Id,
            MediaSourceId: channel.Id,
            PlayMethod: 'Transcode',
            PlaySessionId: playbackInfo.PlaySessionId,
            PositionTicks: 0,
            CanSeek: true,
            IsPaused: false,
          },
        });
        expect(stopped.status(), `${channel.Id} stopped report`).toBe(204);

        await expect.poll(
          async () => (await activeTunerLeaseCount(request, auth)),
          { timeout: 10_000 },
        ).toBe(baselineLeaseCount);
      }
    }

    expect(results).toHaveLength(channels.length);
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

async function resolveLiveTvChannels(request, auth) {
  const configuredIds = (process.env.JELLYRIN_E2E_LIVE_TV_ITEM_IDS || process.env.JELLYRIN_E2E_LIVE_TV_ITEM_ID || '')
    .split(',')
    .map(value => value.trim())
    .filter(Boolean);
  if (configuredIds.length) {
    const channels = [];
    for (const id of configuredIds) {
      const response = await request.get(`/Items/${id}`, {
        headers: { 'X-Emby-Token': auth.AccessToken },
      });
      expect(response.status(), `configured Live TV item ${id}`).toBe(200);
      channels.push(await response.json());
    }
    return channels;
  }

  const startIndex = Number(process.env.JELLYRIN_E2E_LIVE_TV_START_INDEX || 0);
  const limit = Number(process.env.JELLYRIN_E2E_LIVE_TV_LIMIT || 1);
  const response = await request.get(
    `/LiveTv/Channels?UserId=${auth.User.Id}&StartIndex=${startIndex}&Limit=${limit}`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  expect(response.status()).toBe(200);
  const body = await response.json();
  return (body.Items || []).filter(channel => channel.Id && channel.Name);
}

async function requestPlaybackInfo(request, auth, itemId) {
  const response = await request.post(`/Items/${itemId}/PlaybackInfo`, {
    headers: { 'X-Emby-Token': auth.AccessToken },
    data: {
      UserId: auth.User.Id,
      IsPlayback: true,
      AutoOpenLiveStream: true,
      EnableDirectPlay: true,
      EnableDirectStream: true,
      EnableTranscoding: true,
      DeviceProfile: {
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
      },
    },
  });
  expect(response.status(), `${itemId} PlaybackInfo`).toBe(200);
  return response.json();
}

async function loadHlsPlaylists(request, baseURL, transcodingUrl) {
  const masterUrl = absoluteUrl(baseURL, transcodingUrl);
  const masterText = await getTextWithRetry(request, masterUrl, 'master playlist');
  expect(masterText).toContain('#EXT-X-STREAM-INF');

  const mainRelative = masterText.split(/\r?\n/).find(line => line.startsWith('main.m3u8'));
  expect(mainRelative, masterText).toBeTruthy();
  const mainUrl = resolvePlaylistUrl(baseURL, transcodingUrl, mainRelative);
  const mainText = await getTextWithRetry(request, mainUrl, 'media playlist');
  expect(mainText).toContain('#EXTINF');
  return { mainUrl, mainText };
}

async function getTextWithRetry(request, url, label) {
  let lastStatus = 0;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const response = await request.get(url, { timeout: 20_000 });
    lastStatus = response.status();
    const body = await response.text();
    if (lastStatus === 200) {
      return body;
    }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  expect(lastStatus, `${label} did not become ready`).toBe(200);
  return '';
}

function firstSegmentUri(playlist) {
  return playlist
    .split(/\r?\n/)
    .map(line => line.trim())
    .find(line => line && !line.startsWith('#') && line.includes('.ts'));
}

function resolvePlaylistUrl(baseURL, playlistUrl, relativeOrAbsolute) {
  if (/^https?:\/\//i.test(relativeOrAbsolute)) {
    return relativeOrAbsolute;
  }
  if (relativeOrAbsolute.startsWith('/')) {
    return absoluteUrl(baseURL, relativeOrAbsolute);
  }
  const parent = playlistUrl.includes('/')
    ? playlistUrl.slice(0, playlistUrl.lastIndexOf('/') + 1)
    : '/';
  return absoluteUrl(baseURL, `${parent}${relativeOrAbsolute}`);
}

function absoluteUrl(baseURL, value) {
  return new URL(value, baseURL).toString();
}

async function activeTunerLeaseCount(request, auth) {
  const response = await request.get('/System/Diagnostics', {
    headers: { 'X-Emby-Token': auth.AccessToken },
  });
  expect(response.status()).toBe(200);
  const diagnostics = await response.json();
  return diagnostics.LiveTv?.ActiveTunerLeases?.Count ?? 0;
}
