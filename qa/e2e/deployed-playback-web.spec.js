const { test, expect } = require('@playwright/test');

const DEFAULT_DEVICE_ID = 'deployed-playback-web';

test.describe('deployed Jellyfin Web playback', () => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');

  test('plays a video in Jellyfin Web and can seek without HLS request failures', async ({ page, request, baseURL }) => {
    const username = process.env.JELLYRIN_E2E_ADMIN_USER || process.env.JELLYRIN_E2E_USER;
    const password = process.env.JELLYRIN_E2E_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_PASSWORD;
    test.skip(!username || !password, 'Requires JELLYRIN_E2E_USER/JELLYRIN_E2E_PASSWORD or admin equivalents');

    const auth = await authenticate(request, username, password);
    const item = await resolveVideoItem(request, auth);
    const startPositionTicks = Number(
      process.env.JELLYRIN_E2E_WEB_START_POSITION_TICKS
        || process.env.JELLYRIN_E2E_START_POSITION_TICKS
        || 600_000_000,
    );
    await seedPlaybackPosition(request, auth, item.Id, startPositionTicks);
    const publicInfo = await (await request.get('/System/Info/Public')).json();
    const failedResponses = [];
    const hlsResponses = [];
    const playSessionIds = [];

    page.on('response', async response => {
      const url = response.url();
      const status = response.status();
      if (url.includes('/PlaybackInfo') && status === 200) {
        try {
          const body = await response.json();
          if (body?.PlaySessionId) {
            playSessionIds.push(body.PlaySessionId);
          }
          const transcodeUrl = body?.MediaSources?.[0]?.TranscodingUrl;
          const playSessionId = transcodeUrl ? new URL(transcodeUrl, baseURL).searchParams.get('PlaySessionId') : null;
          if (playSessionId) {
            playSessionIds.push(playSessionId);
          }
        } catch {
          // Network listeners should not make the test flaky if a body is unavailable.
        }
      }
      if (isHlsUrl(url)) {
        hlsResponses.push({
          status,
          url,
          contentType: response.headers()['content-type'] || '',
        });
      }
      if (status >= 400 && !url.includes('/Branding/Splashscreen')) {
        failedResponses.push(`${status} ${url}`);
      }
    });

    try {
      await loginThroughWeb(page, baseURL, publicInfo.Id, username, password);
      await page.goto(`${baseURL}/web/#/details?id=${item.Id}`, { waitUntil: 'domcontentloaded' });
      const playButton = await playbackButton(page);
      await page.waitForLoadState('networkidle').catch(() => {});

      const playbackInfoPromise = page.waitForResponse(response =>
        response.url().includes(`/Items/${item.Id}/PlaybackInfo`) && response.status() === 200,
        { timeout: 30_000 },
      );
      const sessionStarted = page.waitForResponse(response =>
        response.url().includes('/Sessions/Playing') && response.request().method() === 'POST' && response.status() === 204,
        { timeout: 30_000 },
      ).catch(() => null);
      const firstHlsSegment = page.waitForResponse(response =>
        isHlsSegmentUrl(response.url()) && response.status() === 200,
        { timeout: 45_000 },
      );

      await playButton.click();
      const playbackInfoResponse = await playbackInfoPromise;
      const playbackInfo = await playbackInfoResponse.json();
      if (playbackInfo?.PlaySessionId) {
        playSessionIds.push(playbackInfo.PlaySessionId);
      }
      const transcodeUrl = playbackInfo?.MediaSources?.[0]?.TranscodingUrl;
      const transcodePlaySessionId = transcodeUrl ? new URL(transcodeUrl, baseURL).searchParams.get('PlaySessionId') : null;
      if (transcodePlaySessionId) {
        playSessionIds.push(transcodePlaySessionId);
      }
      await firstHlsSegment;
      await sessionStarted;

      await page.locator('video').first().waitFor({ state: 'attached', timeout: 20_000 });
      const beforeSeek = await waitForVideoReady(page);
      expect(beforeSeek.duration, 'video duration').toBeGreaterThan(30);
      expect(beforeSeek.seekableEnd, 'video seekable end').toBeGreaterThan(30);

      const targetTime = Math.min(
        Math.max(20, beforeSeek.duration * 0.45),
        Math.max(20, beforeSeek.duration - 15),
      );
      const segmentCountBeforeSeek = hlsResponses.filter(entry => isHlsSegmentUrl(entry.url)).length;
      const seekResult = await seekVideo(page, targetTime);
      expect(seekResult.currentTime, `currentTime after seek to ${targetTime}`).toBeGreaterThan(targetTime - 8);

      await expect.poll(
        () => hlsResponses.filter(entry => isHlsSegmentUrl(entry.url)).length,
        { timeout: 45_000 },
      ).toBeGreaterThan(segmentCountBeforeSeek);

      const hlsFailures = hlsResponses.filter(entry => entry.status >= 400);
      expect(hlsFailures).toEqual([]);
      expect(failedResponses).toEqual([]);
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
    const response = await request.get(`/Items/${configuredItemId}`, {
      headers: { 'X-Emby-Token': auth.AccessToken },
    });
    expect(response.status()).toBe(200);
    return response.json();
  }

  const response = await request.get(
    `/Items?UserId=${auth.User.Id}&Recursive=true&IncludeItemTypes=Episode,Movie&MediaTypes=Video&Fields=RunTimeTicks&StartIndex=0&Limit=50`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  expect(response.status()).toBe(200);
  const body = await response.json();
  const item = body.Items?.find(candidate => (candidate.RunTimeTicks ?? 0) > 0);
  expect(item, 'at least one playable video item').toBeTruthy();
  return item;
}

async function seedPlaybackPosition(request, auth, itemId, positionTicks) {
  const stopped = await request.post('/Sessions/Playing/Stopped', {
    headers: { 'X-Emby-Token': auth.AccessToken },
    data: {
      ItemId: itemId,
      MediaSourceId: itemId,
      PlaySessionId: `playwright-seed-${Date.now()}`,
      PositionTicks: positionTicks,
      Failed: false,
    },
  });
  if (stopped.status() >= 200 && stopped.status() < 300) {
    return;
  }

  const updated = await request.post(`/Items/Users/${auth.User.Id}/Items/${itemId}/UserData`, {
    headers: { 'X-Emby-Token': auth.AccessToken },
    data: {
      PlaybackPositionTicks: positionTicks,
      Played: false,
    },
  });
  expect(
    updated.status(),
    `seed playback position through /Sessions/Playing/Stopped (${stopped.status()}) or UserData`,
  ).toBeLessThan(300);
}

async function loginThroughWeb(page, baseURL, serverId, username, password) {
  await page.goto(`${baseURL}/web/#/login?serverid=${serverId}&url=%2Fhome`, { waitUntil: 'domcontentloaded' });
  const manualName = page.locator('#txtManualName');
  const manualFormVisible = await manualName.waitFor({ state: 'visible', timeout: 5_000 })
    .then(() => true)
    .catch(() => false);
  if (!manualFormVisible) {
    await page.getByRole('button', { name: 'Manual Login' }).waitFor({ state: 'visible', timeout: 15_000 });
    await page.getByRole('button', { name: 'Manual Login' }).click();
  }
  await manualName.fill(username);
  await page.locator('#txtManualPassword').fill(password);
  const authResponse = page.waitForResponse(response =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200,
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authResponse;
  await expect(page).toHaveURL(/\/web\/#\/home/, { timeout: 20_000 });
}

async function playbackButton(page) {
  for (const name of ['Play', 'Resume']) {
    const buttons = page.getByRole('button', { name: new RegExp(`^${name}$`) });
    const count = await buttons.count();
    for (let index = 0; index < count; index += 1) {
      const button = buttons.nth(index);
      if (await button.isVisible()) {
        await button.waitFor({ state: 'visible', timeout: 20_000 });
        return button;
      }
    }
  }
  const fallback = page.locator('.btnPlay:not(.hide), .btnReplay:not(.hide)').first();
  await fallback.waitFor({ state: 'visible', timeout: 20_000 });
  return fallback;
}

async function waitForVideoReady(page) {
  return page.waitForFunction(() => {
    const video = document.querySelector('video');
    if (!video || !Number.isFinite(video.duration) || video.duration <= 0) {
      return false;
    }
    const seekableEnd = video.seekable.length ? video.seekable.end(video.seekable.length - 1) : 0;
    if (!Number.isFinite(seekableEnd) || seekableEnd <= 0) {
      return false;
    }
    return {
      currentTime: video.currentTime,
      duration: video.duration,
      readyState: video.readyState,
      seekableEnd,
      paused: video.paused,
    };
  }, { timeout: 45_000 }).then(handle => handle.jsonValue());
}

async function seekVideo(page, targetTime) {
  await page.evaluate(target => {
    const video = document.querySelector('video');
    if (!video) {
      throw new Error('video element not found');
    }
    video.currentTime = target;
  }, targetTime);

  return page.waitForFunction(target => {
    const video = document.querySelector('video');
    if (!video || !Number.isFinite(video.currentTime)) {
      return false;
    }
    if (video.currentTime < target - 8) {
      return false;
    }
    return {
      currentTime: video.currentTime,
      readyState: video.readyState,
      networkState: video.networkState,
      paused: video.paused,
    };
  }, targetTime, { timeout: 45_000 }).then(handle => handle.jsonValue());
}

async function stopPlaySessions(request, auth, playSessionIds) {
  for (const playSessionId of new Set(playSessionIds.filter(Boolean))) {
    await request.delete(`/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(playSessionId)}`, {
      headers: { 'X-Emby-Token': auth.AccessToken },
    }).catch(() => {});
  }
}

function isHlsUrl(url) {
  return /\.m3u8(?:\?|$)/i.test(url) || isHlsSegmentUrl(url);
}

function isHlsSegmentUrl(url) {
  return /\/(?:hls|hls1)\//i.test(url) && /\.(?:ts|mp4|aac|mp3)(?:\?|$)/i.test(url);
}
