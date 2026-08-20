#!/usr/bin/env node

const fs = require('node:fs/promises');
const fsConstants = require('node:fs').constants;
const path = require('node:path');

const PLUGIN_ID = '7a7a8541-29f8-4c35-99b1-66df55f8399e';
const repoRoot = path.resolve(__dirname, '..');

if (require.main === module) {
  main().catch((error) => {
    // Do not append response bodies or configuration values: provider errors can reflect input.
    console.error(error instanceof SafeError ? error.message : 'MAGSTV configuration failed safely');
    process.exitCode = 1;
  });
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const baseUrl = validateJellyrinBase(process.env.JELLYRIN_BASE_URL);
  const config = validateConfig(await loadConfig());
  const token = await loadToken();
  const payload = buildPayload(config);

  if (options.validateOnly) {
    printResult({
      status: 'magstv-input-valid',
      mode: 'validate-only',
      tunerId: payload.Id,
      pluginId: PLUGIN_ID,
      providerRoute: 'resolved-inside-plugin',
    });
    return;
  }

  const user = await getJson('/Users/Me', token, baseUrl);
  if (user?.Policy?.IsAdministrator !== true) {
    throw new SafeError('Jellyrin token is authenticated but is not an administrator token');
  }

  if (options.dryRun) {
    await getJson('/LiveTv/TunerHosts/Types', token, baseUrl);
    printResult({
      status: 'magstv-preflight-ok',
      mode: 'dry-run',
      tunerId: payload.Id,
      pluginId: PLUGIN_ID,
      authenticatedUserId: user.Id,
      providerRoute: 'resolved-inside-plugin',
      mutationPerformed: false,
    });
    return;
  }

  const tuner = options.verifyOnly
    ? null
    : await requestJson('/LiveTv/TunerHosts', token, baseUrl, {
      method: 'POST',
      body: payload,
    });
  if (tuner) verifyPersistedTuner(tuner, payload);

  const liveTv = await getJson('/System/Configuration/livetv', token, baseUrl);
  const persisted = liveTv?.TunerHosts?.find((candidate) => candidate?.Id === payload.Id);
  if (!persisted) {
    throw new SafeError('MAGSTV tuner was not found in the persisted Live TV configuration');
  }
  verifyPersistedTuner(persisted, payload);

  const tunerChannels = [];
  let channelStartIndex = 0;
  let totalChannelCount = 0;
  do {
    const channels = await getJson(
      `/LiveTv/Channels?Limit=500&StartIndex=${channelStartIndex}`,
      token,
      baseUrl,
    );
    const pageItems = Array.isArray(channels?.Items) ? channels.Items : [];
    tunerChannels.push(...pageItems.filter((item) => item?.TunerHostId === payload.Id));
    totalChannelCount = integerOrZero(channels?.TotalRecordCount);
    channelStartIndex += pageItems.length;
    if (pageItems.length === 0) break;
  } while (channelStartIndex < totalChannelCount);
  const importedCount = integerOrZero((tuner || persisted).PersistedChannelCount);
  if (importedCount < 1 || tunerChannels.length < 1) {
    throw new SafeError('MAGSTV import completed without a verifiable indexed channel');
  }

  printResult({
    status: 'magstv-configured-and-indexed',
    mode: options.verifyOnly ? 'verify-only' : 'configure',
    tunerId: payload.Id,
    pluginId: PLUGIN_ID,
    authenticatedUserId: user.Id,
    providerRoute: 'resolved-inside-plugin',
    storage: (tuner || persisted).Storage,
    importedChannelCount: importedCount,
    importedCategoryCount: integerOrZero((tuner || persisted).PersistedCategoryCount),
    visibleTunerChannelsInProbePage: tunerChannels.length,
    firstChannelIds: tunerChannels.slice(0, 5).map((item) => item.Id),
    credentialsPersistedAsEncryptedReference: true,
    nextCheck: 'Run the deployed Live TV HLS suite with a returned channel id.',
  });
}

function parseArgs(args) {
  const known = new Set(['--dry-run', '--validate-only', '--verify-only']);
  const unknown = args.filter((arg) => !known.has(arg));
  if (unknown.length > 0) {
    throw new SafeError(`Unknown option: ${unknown[0]}`);
  }
  const selectedModes = ['--dry-run', '--validate-only', '--verify-only']
    .filter((mode) => args.includes(mode));
  if (selectedModes.length > 1) {
    throw new SafeError('Choose only one execution mode');
  }
  return {
    dryRun: args.includes('--dry-run'),
    validateOnly: args.includes('--validate-only'),
    verifyOnly: args.includes('--verify-only'),
  };
}

async function loadConfig() {
  const filePath = process.env.JELLYRIN_MAGSTV_CONFIG
    || path.join(repoRoot, 'var', 'secrets', 'magstv.json');
  let fromFile = {};
  try {
    fromFile = JSON.parse(await readSecureFile(filePath));
  } catch (error) {
    if (error?.code !== 'ENOENT') {
      throw error instanceof SafeError ? error : new SafeError('MAGSTV secure configuration is not valid JSON');
    }
  }
  return {
    ...fromFile,
    username: process.env.JELLYRIN_MAGSTV_USERNAME || fromFile.username,
    password: process.env.JELLYRIN_MAGSTV_PASSWORD || fromFile.password,
  };
}

async function loadToken() {
  const environmentToken = process.env.JELLYRIN_API_TOKEN?.trim();
  if (environmentToken) {
    return environmentToken;
  }
  const tokenPath = process.env.JELLYRIN_API_TOKEN_FILE;
  if (!tokenPath) {
    throw new SafeError('Set JELLYRIN_API_TOKEN or JELLYRIN_API_TOKEN_FILE');
  }
  const value = (await readSecureFile(tokenPath)).trim();
  let token = value;
  if (value.startsWith('{')) {
    try {
      const parsed = JSON.parse(value);
      token = parsed.AccessToken || parsed.accessToken || parsed.token;
    } catch {
      throw new SafeError('Jellyrin token file is not valid JSON or plain text');
    }
  }
  if (typeof token !== 'string' || token.trim().length < 8) {
    throw new SafeError('Jellyrin token is missing or invalid');
  }
  return token.trim();
}

async function readSecureFile(filePath) {
  let handle;
  try {
    handle = await fs.open(filePath, fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW || 0));
    const stat = await handle.stat();
    if (!stat.isFile() || (process.platform !== 'win32' && (stat.mode & 0o077) !== 0)) {
      throw new SafeError('Secret files must be regular files with mode 0600 or stricter');
    }
    return await handle.readFile('utf8');
  } finally {
    await handle?.close();
  }
}

function validateJellyrinBase(value) {
  if (!value) {
    throw new SafeError('Set JELLYRIN_BASE_URL explicitly');
  }
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new SafeError('JELLYRIN_BASE_URL is invalid');
  }
  const local = ['localhost', '127.0.0.1', '::1'].includes(url.hostname);
  if (url.protocol !== 'https:' && !(local && url.protocol === 'http:')) {
    throw new SafeError('Jellyrin must use HTTPS except on loopback');
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new SafeError('JELLYRIN_BASE_URL must not contain credentials, query, or fragment');
  }
  url.pathname = `${url.pathname.replace(/\/+$/, '')}/`;
  return url;
}

function validateConfig(config) {
  for (const field of ['username', 'password']) {
    if (typeof config[field] !== 'string' || config[field].trim() === '') {
      throw new SafeError(`MAGSTV ${field} is required`);
    }
  }
  return {
    username: config.username.trim(),
    password: config.password,
  };
}

function buildPayload(config) {
  return {
    Id: 'magstv',
    Type: `plugin:${PLUGIN_ID}`,
    PluginId: PLUGIN_ID,
    FriendlyName: 'Mags Live TV',
    Username: config.username,
    Password: config.password,
    PageSize: 200,
    TunerCount: 1,
  };
}

function verifyPersistedTuner(tuner, submitted) {
  if (!tuner || tuner.Id !== submitted.Id || tuner.Type !== submitted.Type
      || tuner.PluginId !== submitted.PluginId) {
    throw new SafeError('Persisted MAGSTV tuner does not match the submitted route');
  }
  if (containsKey(tuner, new Set(['username', 'password']))) {
    throw new SafeError('Persisted MAGSTV tuner unexpectedly contains plaintext credential fields');
  }
  const reference = tuner.JellyrinProviderSecretRef;
  if (!reference || typeof reference.Id !== 'string' || !reference.Id
      || typeof reference.Provider !== 'string' || !reference.Provider
      || !Number.isInteger(reference.Revision) || reference.Revision < 1) {
    throw new SafeError('Persisted MAGSTV tuner is missing its encrypted provider-secret reference');
  }
}

function containsKey(value, names) {
  if (Array.isArray(value)) return value.some((item) => containsKey(item, names));
  if (!value || typeof value !== 'object') return false;
  return Object.entries(value).some(([key, child]) => names.has(key.toLowerCase()) || containsKey(child, names));
}

async function getJson(route, token, baseUrl) {
  return requestJson(route, token, baseUrl, { method: 'GET' });
}

async function requestJson(route, token, baseUrl, options) {
  let response;
  try {
    response = await fetch(new URL(route, baseUrl), {
      method: options.method,
      headers: {
        accept: 'application/json',
        'x-emby-token': token,
        ...(options.body ? { 'content-type': 'application/json' } : {}),
      },
      ...(options.body ? { body: JSON.stringify(options.body) } : {}),
      signal: AbortSignal.timeout(options.method === 'POST' ? 130000 : 15000),
    });
  } catch {
    throw new SafeError(`${route} could not be reached`);
  }
  if (!response.ok) {
    throw new SafeError(`${route} failed with HTTP ${response.status}`);
  }
  try {
    return await response.json();
  } catch {
    throw new SafeError(`${route} returned invalid JSON`);
  }
}

function integerOrZero(value) {
  return Number.isInteger(value) && value >= 0 ? value : 0;
}

function printResult(result) {
  console.log(JSON.stringify(result, null, 2));
}

class SafeError extends Error {}

module.exports = {
  PLUGIN_ID,
  SafeError,
  buildPayload,
  containsKey,
  validateConfig,
  validateJellyrinBase,
  verifyPersistedTuner,
};
