#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const xtreamConfigPath = process.env.JELLYRIN_XTREAM_CONFIG
  || path.join(repoRoot, 'var', 'secrets', 'xtream.json');
const jellyrinConfigPath = process.env.JELLYRIN_ADMIN_CONFIG
  || path.join(repoRoot, 'var', 'secrets', 'jellyrin-admin.json');
const baseUrl = normalizeBase(process.env.JELLYRIN_BASE_URL || 'http://127.0.0.1:8097');

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const xtream = await readJson(xtreamConfigPath);
  const admin = await readJson(jellyrinConfigPath);
  const auth = await authenticate(admin.username, admin.password);

  const tuner = await postJson('/LiveTv/TunerHosts', auth.AccessToken, {
    Id: 'xtream-dev',
    Type: 'xtream',
    FriendlyName: 'Xtream Dev',
    Url: xtream.dns,
    Username: xtream.username,
    Password: xtream.password,
    TunerCount: Number(xtream.tunerCount || 1),
    AllowStreamSharing: true,
    ...(options.categoryIds.length ? { CategoryIds: options.categoryIds } : {}),
    ...(options.excludeCategoryIds.length ? { ExcludeCategoryIds: options.excludeCategoryIds } : {}),
    ...(options.channelLimit ? { Limit: options.channelLimit } : {}),
  });
  // Request an explicit large page: /LiveTv/Channels now defaults to 100 items
  // when Limit is omitted, so probe scripts must opt into a full scan.
  const channels = await getJson(`/LiveTv/Channels?UserId=${encodeURIComponent(auth.User.Id)}&Limit=500`, auth.AccessToken);
  const items = Array.isArray(channels.Items) ? channels.Items : [];
  const guideProbeChannelIds = selectGuideProbeChannels(items, options.guideChannels);
  const listing = options.skipGuide
    ? null
    : await postJson('/LiveTv/ListingProviders', auth.AccessToken, {
        Id: 'xtream-dev-guide',
        Type: 'xtream',
        Name: 'Xtream Dev Guide',
        Url: xtream.dns,
        Username: xtream.username,
        Password: xtream.password,
        ChannelIds: guideProbeChannelIds,
        EpgLimit: options.epgLimit,
      });
  const streamProbe = options.probeStream
    ? await probeFirstStream(items, auth.AccessToken, options.streamBytes)
    : { skipped: true };
  const firstItems = items.slice(0, 8).map((item) => ({
    Id: item.Id,
    Name: item.Name,
    Number: item.Number,
    GuideChannelId: item.GuideChannelId,
    HasMediaSources: Array.isArray(item.MediaSources) && item.MediaSources.length > 0,
    DirectStreamUrl: item.MediaSources?.[0]?.DirectStreamUrl,
  }));

  console.log(JSON.stringify({
    status: 'jellyrin-xtream-configured',
    baseUrl: baseUrl.toString().replace(/\/$/, ''),
    userId: auth.User.Id,
    tuner: summarizeProvider(tuner),
    categories: {
      total: Number(tuner.PersistedCategoryCount ?? (Array.isArray(tuner.Categories) ? tuner.Categories.length : 0)),
      firstItems: Array.isArray(tuner.Categories) ? tuner.Categories.slice(0, 8) : [],
      activeFilters: {
        categoryIds: options.categoryIds,
        excludeCategoryIds: options.excludeCategoryIds,
        channelLimit: options.channelLimit,
      },
    },
    listingProvider: listing ? summarizeProvider(listing) : { skipped: true },
    guideProbe: {
      requestedChannelIds: guideProbeChannelIds,
      programCount: Array.isArray(listing?.Programs) ? listing.Programs.length : 0,
      providerReturnedPrograms: Array.isArray(listing?.Programs) && listing.Programs.length > 0,
    },
    streamProbe,
    channels: {
      total: channels.TotalRecordCount ?? items.length,
      firstItems,
    },
    nextChecks: [
      'Open Jellyrin Web -> Live TV and verify the channel list.',
      'Run with --probe-stream for a controlled DirectStreamUrl read; this consumes the provider connection briefly.',
    ],
  }, null, 2));
}

function parseArgs(args) {
  return {
    skipGuide: args.includes('--skip-guide'),
    probeStream: args.includes('--probe-stream'),
    epgLimit: numberArg(args, '--epg-limit', 6),
    guideChannels: numberArg(args, '--guide-channels', 3),
    streamBytes: numberArg(args, '--stream-bytes', 256 * 1024),
    channelLimit: optionalNumberArg(args, '--channel-limit'),
    categoryIds: listArg(args, '--category-ids'),
    excludeCategoryIds: listArg(args, '--exclude-category-ids'),
  };
}

function optionalNumberArg(args, name) {
  const index = args.indexOf(name);
  if (index === -1 || index + 1 >= args.length) {
    return null;
  }
  const value = Number(args[index + 1]);
  return Number.isFinite(value) && value > 0 ? value : null;
}

function numberArg(args, name, fallback) {
  const index = args.indexOf(name);
  if (index === -1 || index + 1 >= args.length) {
    return fallback;
  }
  const value = Number(args[index + 1]);
  return Number.isFinite(value) && value > 0 ? value : fallback;
}

function listArg(args, name) {
  const index = args.indexOf(name);
  if (index === -1 || index + 1 >= args.length) {
    return [];
  }
  return args[index + 1]
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean);
}

async function readJson(filePath) {
  return JSON.parse(await fs.readFile(filePath, 'utf8'));
}

async function authenticate(username, password) {
  const response = await fetch(new URL('/Users/AuthenticateByName', baseUrl), {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-emby-authorization': 'MediaBrowser Client="Codex", Device="Codex", DeviceId="codex-xtream-config", Version="1.0"',
    },
    body: JSON.stringify({ Username: username, Pw: password }),
  });
  if (!response.ok) {
    throw new Error(`Jellyrin auth failed: HTTP ${response.status}`);
  }
  return response.json();
}

async function postJson(route, token, payload) {
  const response = await fetch(new URL(route, baseUrl), {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-emby-token': token,
    },
    body: JSON.stringify(payload),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${route} failed: HTTP ${response.status} ${text.slice(0, 240)}`);
  }
  return text ? JSON.parse(text) : {};
}

async function getJson(route, token) {
  const response = await fetch(new URL(route, baseUrl), {
    headers: {
      'x-emby-token': token,
    },
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${route} failed: HTTP ${response.status} ${text.slice(0, 240)}`);
  }
  return text ? JSON.parse(text) : {};
}

function selectGuideProbeChannels(items, limit) {
  return items
    .filter((item) => typeof item.GuideChannelId === 'string' && item.GuideChannelId.length > 0)
    .slice(0, limit)
    .map((item) => item.Id);
}

async function probeFirstStream(items, token, byteLimit) {
  const item = items.find((candidate) => candidate.MediaSources?.[0]?.DirectStreamUrl);
  if (!item) {
    return { ok: false, error: 'No channel with DirectStreamUrl found' };
  }
  const route = item.MediaSources[0].DirectStreamUrl;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 15000);
  try {
    const response = await fetch(new URL(route, baseUrl), {
      headers: { 'x-emby-token': token },
      signal: controller.signal,
    });
    const reader = response.body?.getReader();
    let bytes = 0;
    if (reader) {
      while (bytes < byteLimit) {
        const { done, value } = await reader.read();
        if (done) {
          break;
        }
        bytes += value.byteLength;
      }
      await reader.cancel();
    } else {
      const arrayBuffer = await response.arrayBuffer();
      bytes = arrayBuffer.byteLength;
    }
    return {
      ok: response.ok,
      status: response.status,
      channelId: item.Id,
      channelName: item.Name,
      contentType: response.headers.get('content-type'),
      bytesRead: bytes,
    };
  } catch (error) {
    return {
      ok: false,
      channelId: item.Id,
      channelName: item.Name,
      error: {
        name: error.name,
        code: error.code,
        message: error.message,
        causeCode: error.cause?.code,
      },
    };
  } finally {
    clearTimeout(timeout);
  }
}

function summarizeProvider(provider) {
  return {
    Id: provider.Id,
    Type: provider.Type,
    FriendlyName: provider.FriendlyName,
    Name: provider.Name,
    channelCount: Number(provider.PersistedChannelCount ?? (Array.isArray(provider.Channels) ? provider.Channels.length : 0)),
    categoryCount: Number(provider.PersistedCategoryCount ?? (Array.isArray(provider.Categories) ? provider.Categories.length : 0)),
    storage: provider.Storage,
    programCount: Array.isArray(provider.Programs) ? provider.Programs.length : undefined,
  };
}

function buildXtreamUrls(baseUrl, username, password) {
  const base = normalizeBase(baseUrl);
  return {
    m3u: withQuery(new URL('/get.php', base), {
      username,
      password,
      type: 'm3u_plus',
      output: 'ts',
    }),
    xmltv: withQuery(new URL('/xmltv.php', base), { username, password }),
  };
}

function normalizeBase(value) {
  const url = new URL(value);
  url.pathname = '/';
  url.search = '';
  url.hash = '';
  return url;
}

function withQuery(url, params) {
  for (const [key, value] of Object.entries(params)) {
    url.searchParams.set(key, value);
  }
  return url;
}
