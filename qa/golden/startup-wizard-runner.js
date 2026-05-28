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
const outputRoot = process.env.JELLYRIN_STARTUP_RUNNER_TMP
  || path.join(os.tmpdir(), 'jellyrin-startup-wizard-');

async function main() {
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
    ], childOptions('upstream')));

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
    ], childOptions('jellyrin')));

    const upstreamUrl = `http://127.0.0.1:${upstreamPort}`;
    const jellyrinUrl = `http://127.0.0.1:${jellyrinPort}`;
    await Promise.all([
      waitForStartupTarget(upstreamUrl, 'upstream'),
      waitForStartupTarget(jellyrinUrl, 'jellyrin'),
    ]);

    await runBrowserTrace(upstreamUrl, jellyrinUrl);
  } finally {
    await Promise.all(children.map(stopChild));
    if (!process.env.JELLYRIN_KEEP_STARTUP_RUNNER_TMP) {
      await fs.rm(root, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`kept startup runner temp dir: ${root}`);
    }
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

function childOptions(name) {
  void name;
  return {
    cwd: repoRoot,
    stdio: 'ignore',
    env: {
      ...process.env,
      ASPNETCORE_ENVIRONMENT: 'Development',
    },
  };
}

async function waitForStartupTarget(baseUrl, name) {
  const deadline = Date.now() + 90_000;
  let lastError = '';
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/System/Info/Public`);
      if (response.ok) {
        const json = await response.json();
        if (json.StartupWizardCompleted === false) {
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
  throw new Error(`Timed out waiting for ${name} startup target: ${lastError}`);
}

async function runBrowserTrace(upstreamUrl, jellyrinUrl) {
  await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(repoRoot, 'qa/golden/browser-trace.js')], {
      cwd: repoRoot,
      stdio: 'inherit',
      env: {
        ...process.env,
        JELLYRIN_BROWSER_FLOW: 'startup-wizard',
        JELLYFIN_UPSTREAM_URL: upstreamUrl,
        JELLYRIN_URL: jellyrinUrl,
        JELLYRIN_BROWSER_TARGETS: 'upstream,jellyrin',
      },
    });
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`browser trace exited with ${signal || code}`));
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
  process.exitCode = 1;
});
