#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const defaultConfigPath = path.join(repoRoot, 'var', 'secrets', 'xtream.json');
const configPath = process.env.JELLYRIN_XTREAM_CONFIG || defaultConfigPath;
const byteLimit = Number(process.env.JELLYRIN_XTREAM_PROBE_BYTES || 1024 * 1024);
const timeoutMs = Number(process.env.JELLYRIN_XTREAM_PROBE_TIMEOUT_MS || 45000);

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});

async function main() {
  const config = await readConfig(configPath);
  const bases = [
    { label: 'dns', url: config.dns },
    { label: 'samsungLgDns', url: config.samsungDns },
  ].filter((entry) => entry.url);

  const results = [];
  for (const base of bases) {
    results.push(await probeBase(base, config));
  }
  console.log(JSON.stringify({
    status: 'xtream-probe-complete',
    configPath: path.relative(repoRoot, configPath),
    byteLimit,
    results,
  }, null, 2));
}

async function readConfig(filePath) {
  const raw = await fs.readFile(filePath, 'utf8');
  const config = JSON.parse(raw);
  for (const field of ['username', 'password', 'dns']) {
    if (!config[field] || typeof config[field] !== 'string') {
      throw new Error(`${filePath} must contain string field ${field}`);
    }
  }
  return config;
}

async function probeBase(base, config) {
  const urls = buildXtreamUrls(base.url, config.username, config.password);
  const playerApi = await capture(() => fetchJson(urls.playerApi));
  const categories = await capture(() => fetchJson(urls.liveCategories));
  const streams = await capture(() => fetchJson(urls.liveStreams));
  const epgStreamId = streams.ok ? firstEpgStreamId(streams.value.json) : null;
  const epg = epgStreamId
    ? await capture(() => fetchJson(buildXtreamActionUrl(base.url, config.username, config.password, 'get_short_epg', {
        stream_id: epgStreamId,
        limit: '3',
      })))
    : { ok: false, error: { message: 'No stream with epg_channel_id found in live stream payload' } };
  const m3u = await capture(() => fetchTextPrefix(urls.m3u));
  const xmltv = await capture(() => fetchTextPrefix(urls.xmltv));
  return {
    label: base.label,
    baseUrl: redactUrl(base.url),
    playerApi: playerApi.ok ? summarizePlayerApi(playerApi.value) : playerApi,
    liveCategories: categories.ok ? summarizeArrayResponse(categories.value) : categories,
    liveStreams: streams.ok ? summarizeArrayResponse(streams.value) : streams,
    shortEpg: epg.ok ? summarizeEpg(epg.value, epgStreamId) : epg,
    m3u: m3u.ok ? summarizeM3u(m3u.value) : m3u,
    xmltv: xmltv.ok ? summarizeXmltv(xmltv.value) : xmltv,
    jellyrinConfigCandidate: {
      tunerHost: {
        Type: 'xtream',
        Url: redactUrl(base.url),
      },
      listingProvider: {
        Type: 'xtream',
        ChannelIds: epgStreamId ? [`xtream_${epgStreamId}`] : [],
      },
    },
  };
}

async function capture(operation) {
  try {
    return { ok: true, value: await operation() };
  } catch (error) {
    return {
      ok: false,
      error: {
        name: error.name,
        code: error.code,
        message: error.message,
        causeCode: error.cause?.code,
        causeMessage: error.cause?.message,
      },
    };
  }
}

function buildXtreamUrls(baseUrl, username, password) {
  const base = normalizeBase(baseUrl);
  return {
    playerApi: withQuery(new URL('/player_api.php', base), { username, password }),
    liveCategories: buildXtreamActionUrl(baseUrl, username, password, 'get_live_categories'),
    liveStreams: buildXtreamActionUrl(baseUrl, username, password, 'get_live_streams'),
    m3u: withQuery(new URL('/get.php', base), {
      username,
      password,
      type: 'm3u_plus',
      output: 'ts',
    }),
    xmltv: withQuery(new URL('/xmltv.php', base), { username, password }),
  };
}

function buildXtreamActionUrl(baseUrl, username, password, action, extra = {}) {
  const base = normalizeBase(baseUrl);
  const url = withQuery(new URL('/player_api.php', base), { username, password, action });
  for (const [key, value] of Object.entries(extra)) {
    url.searchParams.set(key, value);
  }
  return url;
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

async function fetchJson(url) {
  const response = await fetchWithTimeout(url);
  const contentType = response.headers.get('content-type') || '';
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch (_) {
    // Keep the probe useful when providers return HTML/plain errors.
  }
  return {
    status: response.status,
    ok: response.ok,
    contentType,
    bytes: Buffer.byteLength(text),
    json,
    textPrefix: text.slice(0, 160),
  };
}

async function fetchTextPrefix(url) {
  const response = await fetchWithTimeout(url);
  const contentType = response.headers.get('content-type') || '';
  const reader = response.body?.getReader();
  if (!reader) {
    const text = await response.text();
    return {
      status: response.status,
      ok: response.ok,
      contentType,
      truncated: false,
      bytes: Buffer.byteLength(text),
      text,
    };
  }
  const chunks = [];
  let bytes = 0;
  let truncated = false;
  try {
    while (bytes < byteLimit) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      chunks.push(value);
      bytes += value.byteLength;
    }
  } catch (error) {
    if (bytes === 0) {
      throw error;
    }
    truncated = true;
  }
  if (bytes >= byteLimit) {
    truncated = true;
    try {
      await reader.cancel();
    } catch (_) {
      // Some fetch implementations surface the intentional cancel as AbortError.
    }
  }
  const text = Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString('utf8');
  return {
    status: response.status,
    ok: response.ok,
    contentType,
    truncated,
    bytes: Buffer.byteLength(text),
    text,
  };
}

async function fetchWithTimeout(url) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, {
      signal: controller.signal,
      headers: {
        'user-agent': 'Jellyrin-Xtream-Probe/1.0',
      },
    });
  } finally {
    clearTimeout(timeout);
  }
}

function summarizePlayerApi(result) {
  const userInfo = result.json?.user_info || {};
  const serverInfo = result.json?.server_info || {};
  return {
    status: result.status,
    ok: result.ok,
    contentType: result.contentType,
    bytes: result.bytes,
    json: Boolean(result.json),
    auth: userInfo.auth,
    userStatus: userInfo.status,
    activeConnections: userInfo.active_cons,
    maxConnections: userInfo.max_connections,
    allowedOutputFormats: userInfo.allowed_output_formats || [],
    serverProtocol: serverInfo.server_protocol,
    serverPort: serverInfo.port,
    timezone: serverInfo.timezone,
    textPrefix: result.json ? undefined : result.textPrefix,
  };
}

function summarizeArrayResponse(result) {
  const array = Array.isArray(result.json) ? result.json : [];
  return {
    status: result.status,
    ok: result.ok,
    contentType: result.contentType,
    bytes: result.bytes,
    count: array.length,
    first: array.slice(0, 1),
  };
}

function firstEpgStreamId(streams) {
  if (!Array.isArray(streams)) {
    return null;
  }
  const stream = streams.find((item) => item && item.stream_id && item.epg_channel_id);
  return stream ? String(stream.stream_id) : null;
}

function summarizeEpg(result, streamId) {
  const listings = Array.isArray(result.json?.epg_listings)
    ? result.json.epg_listings
    : Array.isArray(result.json)
      ? result.json
      : [];
  return {
    status: result.status,
    ok: result.ok,
    streamId,
    contentType: result.contentType,
    bytes: result.bytes,
    listingCount: listings.length,
    providerError: !result.json && result.textPrefix ? result.textPrefix : undefined,
    first: listings.slice(0, 1),
  };
}

function summarizeM3u(result) {
  const text = result.text || '';
  const extinfCount = countMatches(text, /^#EXTINF:/gm);
  const streamUrlCount = text.split(/\r?\n/).filter((line) => /^https?:\/\//i.test(line.trim())).length;
  const firstNames = [];
  for (const match of text.matchAll(/^#EXTINF:[^\n,]*,(.+)$/gm)) {
    firstNames.push(match[1].trim());
    if (firstNames.length >= 5) {
      break;
    }
  }
  return {
    status: result.status,
    ok: result.ok,
    contentType: result.contentType,
    bytesRead: result.bytes,
    truncated: result.truncated,
    startsWithExtm3u: text.trimStart().startsWith('#EXTM3U'),
    extinfCount,
    streamUrlCount,
    firstNames,
  };
}

function summarizeXmltv(result) {
  const text = result.text || '';
  return {
    status: result.status,
    ok: result.ok,
    contentType: result.contentType,
    bytesRead: result.bytes,
    truncated: result.truncated,
    startsWithTv: /<tv[\s>]/i.test(text.slice(0, 512)),
    channelCountInPrefix: countMatches(text, /<channel\b/gi),
    programmeCountInPrefix: countMatches(text, /<programme\b/gi),
  };
}

function countMatches(text, pattern) {
  return Array.from(text.matchAll(pattern)).length;
}

function redactUrl(value) {
  const url = new URL(value);
  for (const key of ['username', 'password']) {
    if (url.searchParams.has(key)) {
      url.searchParams.set(key, '<redacted>');
    }
  }
  return url.toString();
}
