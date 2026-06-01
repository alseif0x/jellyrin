#!/usr/bin/env node

const fs = require('node:fs/promises');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '../..');
const upstreamDll = process.env.JELLYFIN_UPSTREAM_DLL
  || '/home/cdmonio/dev/jellyfin/Jellyfin.Server/bin/Release/net10.0/jellyfin.dll';
const upstreamWebDir = process.env.JELLYFIN_WEB_DIR || '/home/cdmonio/dev/jellyfin-web/dist';
const upstreamFfmpeg = process.env.JELLYFIN_FFMPEG || '/usr/lib/jellyfin-ffmpeg/ffmpeg';
const jellyrinServer = process.env.JELLYRIN_SERVER_BIN
  || path.join(repoRoot, 'target/debug/jellyrin-server');
const mediaFixtureDir = process.env.JELLYRIN_MEDIA_FIXTURE_DIR
  || path.join(repoRoot, 'var/fixtures/m2-movies');
const outputRoot = process.env.JELLYRIN_CLIENTS_RUNNER_TMP
  || path.join(os.tmpdir(), 'jellyrin-clients-');
const adminPassword = process.env.JELLYRIN_CLIENTS_ADMIN_PASSWORD || 'e6-clients-secret';

async function main() {
  await assertPath(upstreamDll, 'upstream Jellyfin DLL');
  await assertPath(upstreamWebDir, 'Jellyfin web dir');
  await assertPath(upstreamFfmpeg, 'Jellyfin ffmpeg');
  await assertPath(jellyrinServer, 'Jellyrin server binary');
  await assertPath(mediaFixtureDir, 'movie media fixture dir');

  const root = await fs.mkdtemp(outputRoot);
  const upstreamPort = await freePort();
  const jellyrinPort = await freePort();
  const children = [];

  try {
    const upstreamDirs = await prepareUpstream(root, upstreamPort);
    const jellyrinDirs = await prepareDirs(path.join(root, 'jellyrin'));

    children.push(spawn('/usr/bin/dotnet', [
      upstreamDll,
      '--webdir',
      upstreamWebDir,
      '--datadir',
      upstreamDirs.data,
      '--configdir',
      upstreamDirs.config,
      '--cachedir',
      upstreamDirs.cache,
      '--logdir',
      upstreamDirs.log,
      '--ffmpeg',
      upstreamFfmpeg,
      '--published-server-url',
      `http://127.0.0.1:${upstreamPort}`,
      '--nonetchange',
    ], childOptions()));

    children.push(spawn(jellyrinServer, [
      '--host',
      '127.0.0.1',
      '--port',
      String(jellyrinPort),
      '--data-dir',
      jellyrinDirs.data,
      '--config-dir',
      jellyrinDirs.config,
      '--cache-dir',
      jellyrinDirs.cache,
      '--log-dir',
      jellyrinDirs.log,
      '--web-dir',
      upstreamWebDir,
    ], childOptions()));

    const upstreamUrl = `http://127.0.0.1:${upstreamPort}`;
    const jellyrinUrl = `http://127.0.0.1:${jellyrinPort}`;
    await Promise.all([
      waitForStartupTarget(upstreamUrl, 'upstream'),
      waitForStartupTarget(jellyrinUrl, 'jellyrin'),
    ]);

    await completeWizard(upstreamUrl, 'E6 Upstream', 'e6upstream');
    await completeWizard(jellyrinUrl, 'E6 Jellyrin', 'e6admin');
    await Promise.all([
      waitForCompletedTarget(upstreamUrl, 'upstream'),
      waitForCompletedTarget(jellyrinUrl, 'jellyrin'),
    ]);

    await Promise.all([
      prepareMovieLibrary(upstreamUrl, 'e6upstream', 'Golden Non-Web Movies'),
      prepareMovieLibrary(jellyrinUrl, 'e6admin', 'Golden Non-Web Movies'),
    ]);

    await runClientsGolden(upstreamUrl, jellyrinUrl);
  } finally {
    await Promise.all(children.map(stopChild));
    if (!process.env.JELLYRIN_KEEP_CLIENTS_RUNNER_TMP) {
      await fs.rm(root, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`kept non-web clients runner temp dir: ${root}`);
    }
  }
}

async function assertPath(filePath, label) {
  try {
    await fs.access(filePath);
  } catch {
    throw new Error(`Missing ${label}: ${filePath}`);
  }
}

async function prepareDirs(root) {
  const dirs = {
    data: path.join(root, 'data'),
    config: path.join(root, 'config'),
    cache: path.join(root, 'cache'),
    log: path.join(root, 'log'),
  };
  await Promise.all(Object.values(dirs).map((dir) => fs.mkdir(dir, { recursive: true })));
  return dirs;
}

async function prepareUpstream(root, port) {
  const dirs = await prepareDirs(path.join(root, 'upstream'));
  await fs.writeFile(path.join(dirs.config, 'network.xml'), networkXml(port));
  return dirs;
}

function networkXml(port) {
  return `<?xml version="1.0" encoding="utf-8"?>
<NetworkConfiguration xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <BaseUrl />
  <EnableHttps>false</EnableHttps>
  <RequireHttps>false</RequireHttps>
  <CertificatePath />
  <CertificatePassword />
  <InternalHttpPort>${port}</InternalHttpPort>
  <InternalHttpsPort>0</InternalHttpsPort>
  <PublicHttpPort>${port}</PublicHttpPort>
  <PublicHttpsPort>0</PublicHttpsPort>
  <AutoDiscovery>false</AutoDiscovery>
  <EnableUPnP>false</EnableUPnP>
  <EnableIPv4>true</EnableIPv4>
  <EnableIPv6>false</EnableIPv6>
  <EnableRemoteAccess>true</EnableRemoteAccess>
  <LocalNetworkSubnets />
  <LocalNetworkAddresses />
  <KnownProxies />
  <IgnoreVirtualInterfaces>true</IgnoreVirtualInterfaces>
  <VirtualInterfaceNames>
    <string>veth</string>
  </VirtualInterfaceNames>
  <EnablePublishedServerUriByRequest>false</EnablePublishedServerUriByRequest>
  <PublishedServerUriBySubnet />
  <RemoteIPFilter />
  <IsRemoteIPFilterBlacklist>false</IsRemoteIPFilterBlacklist>
</NetworkConfiguration>
`;
}

function childOptions(extraEnv = {}) {
  return {
    cwd: repoRoot,
    stdio: 'ignore',
    env: {
      ...process.env,
      ASPNETCORE_ENVIRONMENT: 'Development',
      ...extraEnv,
    },
  };
}

async function waitForStartupTarget(baseUrl, name) {
  await waitForPublicInfo(baseUrl, name, (json) => json.StartupWizardCompleted === false, 'StartupWizardCompleted=false');
}

async function waitForCompletedTarget(baseUrl, name) {
  await waitForPublicInfo(baseUrl, name, (json) => json.StartupWizardCompleted === true, 'StartupWizardCompleted=true');
}

async function waitForPublicInfo(baseUrl, name, predicate, expected) {
  const deadline = Date.now() + 90_000;
  let lastError = '';
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/System/Info/Public`);
      if (response.ok) {
        const json = await response.json();
        if (predicate(json)) {
          return;
        }
        lastError = `${name} StartupWizardCompleted=${json.StartupWizardCompleted}`;
      } else {
        lastError = `${name} HTTP ${response.status}`;
      }
    } catch (error) {
      lastError = `${name} ${error.message}`;
    }
    await delay(1000);
  }
  throw new Error(`Timed out waiting for ${name} ${expected}: ${lastError}`);
}

async function completeWizard(baseUrl, serverName, username) {
  await getJson(baseUrl, '/Startup/Configuration');
  await postJson(baseUrl, '/Startup/Configuration', {
    ServerName: serverName,
    UICulture: 'en-US',
    MetadataCountryCode: 'US',
    PreferredMetadataLanguage: 'en',
  });
  await getJson(baseUrl, '/Startup/User');
  await postJson(baseUrl, '/Startup/User', {
    Name: username,
    Password: adminPassword,
  });
  await postJson(baseUrl, '/Startup/RemoteAccess', {
    EnableRemoteAccess: true,
  });
  await postJson(baseUrl, '/Startup/Complete');
}

async function getJson(baseUrl, route) {
  const response = await fetch(`${baseUrl}${route}`);
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`${route} returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  }
  return response.json();
}

async function postJson(baseUrl, route, body) {
  const response = await fetch(`${baseUrl}${route}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!response.ok && response.status !== 204) {
    const text = await response.text().catch(() => '');
    throw new Error(`${route} returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  }
}

async function prepareMovieLibrary(baseUrl, username, name) {
  const auth = await authenticate(baseUrl, username);
  await createVirtualFolder(baseUrl, auth.AccessToken, name, 'movies', mediaFixtureDir);
  await postJsonAuth(baseUrl, '/Library/Refresh', auth.AccessToken, {});
  await waitForMovie(baseUrl, auth.AccessToken, auth.User.Id);
}

async function authenticate(baseUrl, username) {
  const response = await fetch(`${baseUrl}/Users/AuthenticateByName`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: 'MediaBrowser Client="Jellyrin E6 Runner", Device="Harness", DeviceId="non-web-clients-runner", Version="dev"',
    },
    body: JSON.stringify({
      Username: username,
      Pw: adminPassword,
    }),
  });
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`AuthenticateByName returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  }
  return response.json();
}

async function createVirtualFolder(baseUrl, token, name, collectionType, location) {
  const folders = await getJsonAuth(baseUrl, '/Library/VirtualFolders', token);
  const exists = folders.some((folder) => (
    folder.Name === name
    || (folder.Locations || []).includes(location)
    || (folder.LibraryOptions?.PathInfos || []).some((pathInfo) => pathInfo.Path === location)
  ));
  if (exists) {
    return;
  }
  const route = `/Library/VirtualFolders?name=${encodeURIComponent(name)}&collectionType=${encodeURIComponent(collectionType)}&paths=${encodeURIComponent(location)}`;
  await postJsonAuth(baseUrl, route, token, {});
}

async function waitForMovie(baseUrl, token, userId) {
  const deadline = Date.now() + 60_000;
  let lastTotal = 0;
  while (Date.now() < deadline) {
    const result = await getJsonAuth(
      baseUrl,
      `/Items?UserId=${encodeURIComponent(userId)}&Recursive=true&IncludeItemTypes=Movie&Fields=MediaSources,RunTimeTicks,Path&Limit=20`,
      token,
    );
    lastTotal = result.TotalRecordCount || 0;
    if ((result.Items || []).some((item) => item.Type === 'Movie' && item.Id)) {
      return;
    }
    await delay(1000);
  }
  throw new Error(`movie fixture not found after library refresh; last count=${lastTotal}`);
}

async function getJsonAuth(baseUrl, route, token) {
  const response = await fetch(`${baseUrl}${route}`, {
    headers: {
      'X-Emby-Token': token,
    },
  });
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`${route} returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  }
  return response.json();
}

async function postJsonAuth(baseUrl, route, token, body) {
  const response = await fetch(`${baseUrl}${route}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-Emby-Token': token,
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!response.ok && response.status !== 204) {
    const text = await response.text().catch(() => '');
    throw new Error(`${route} returned HTTP ${response.status}: ${text.slice(0, 300)}`);
  }
}

async function runClientsGolden(upstreamUrl, jellyrinUrl) {
  await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(repoRoot, 'qa/golden/non-web-clients.js')], {
      cwd: repoRoot,
      stdio: 'inherit',
      env: {
        ...process.env,
        JELLYRIN_BROWSER_TARGETS: 'upstream,jellyrin',
        JELLYFIN_UPSTREAM_URL: upstreamUrl,
        JELLYRIN_URL: jellyrinUrl,
        JELLYFIN_ADMIN_USER: 'e6upstream',
        JELLYFIN_ADMIN_PASSWORD: adminPassword,
        JELLYRIN_ADMIN_USER: 'e6admin',
        JELLYRIN_ADMIN_PASSWORD: adminPassword,
      },
    });
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`non-web clients golden exited with ${signal || code}`));
      }
    });
  });
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) {
    return;
  }
  child.kill('SIGINT');
  await Promise.race([
    new Promise((resolve) => child.once('exit', resolve)),
    delay(8_000).then(() => {
      if (child.exitCode === null) {
        child.kill('SIGKILL');
      }
    }),
  ]);
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
