#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const dockerfile = await read('Dockerfile');
  const runtimeDockerfile = dockerfile.slice(dockerfile.lastIndexOf('FROM ${RUNTIME_IMAGE}'));
  const dockerignore = await read('.dockerignore');
  const supplyChainLock = await read('ops/supply-chain.lock.env');
  const sbomGenerator = await read('ops/generate-sbom.sh');
  const supplyChainQa = await read('qa/supply-chain.js');
  const ci = await read('.github/workflows/ci.yml');
  const compose = await read('docker-compose.yml');
  const nginx = await read('ops/nginx-jellyrin.test.kode.live.conf.example');
  const nginxAcmeBootstrap = await read('ops/nginx-jellyrin-acme-bootstrap.conf.example');
  const gitignore = await read('.gitignore');
  const jellyfinWebBuilder = await read('ops/build-jellyfin-web.sh');
  const deploymentPreflight = await read('ops/deployment-preflight.sh');
  const infrastructureCompose = await read('docker-compose.infrastructure.yml');
  const dlnaCompose = await read('docker-compose.dlna.yml');
  const systemd = await read('ops/jellyrin.service');
  const migrationSystemd = await read('ops/jellyrin-migrate.service');
  const env = await read('ops/jellyrin.env.example');
  const migrationEnv = await read('ops/jellyrin-migrate.env.example');
  const checklist = await read('ops/release-checklist.md');
  const readme = await read('README.md');
  const api = await read('crates/jellyrin-api/src/lib.rs');
  const transcode = await read('crates/jellyrin-transcode/src/lib.rs');
  const server = await read('crates/jellyrin-server/src/main.rs');
  const migrator = await read('crates/jellyrin-migrate/src/main.rs');
  const serverTree = await cargoTree('jellyrin-server');
  const migratorTree = await cargoTree('jellyrin-migrate');
  const sqliteDependency = /(?:^|\s)(?:sqlx-sqlite|libsqlite3-sys)(?:\s|$)/im;
  const nginxLogDefinition = nginx.slice(0, nginx.indexOf('server {'));

  const checks = [
    check('docker-release-build', dockerfile.includes('cargo build --locked --release -p jellyrin-server -p jellyrin-migrate') && dockerfile.includes('RUNTIME_IMAGE=docker.io/library/debian:bookworm-slim@sha256:')),
    check('docker-migrator-binary', dockerfile.includes('/usr/local/bin/jellyrin-migrate')),
    check('docker-runtime-ffmpeg', dockerfile.includes('ffmpeg') && dockerfile.includes('USER 10001:10001')),
    check('docker-stable-secret-reader-identity', dockerfile.includes('USER 10001:10001') && dockerfile.includes('--chown=10001:10001')),
    check(
      'docker-safe-ffmpeg-default',
      dockerfile.includes('JELLYRIN_FFMPEG_MODE=enabled') &&
        env.includes('JELLYRIN_MAX_FFMPEG_JOBS=2') &&
        env.includes('JELLYRIN_MAX_VIDEO_TRANSCODES=1') &&
        env.includes('JELLYRIN_MAX_AUDIO_TRANSCODES=1') &&
        env.includes('JELLYRIN_MAX_REMUXES=1'),
    ),
    check(
      'docker-bounded-build-jobs',
      dockerfile.includes('ARG CARGO_BUILD_JOBS=1') &&
        /CARGO_BUILD_JOBS=\$\{CARGO_BUILD_JOBS\}[\s\\]*cargo build --locked --release/.test(dockerfile),
    ),
    check('docker-locked-base-images', dockerfile.includes('RUST_IMAGE=docker.io/library/rust:1.94.0-bookworm@sha256:') && dockerfile.includes('RUNTIME_IMAGE=docker.io/library/debian:bookworm-slim@sha256:') && dockerfile.includes('DISTROLESS_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:')),
    check('docker-locked-ffmpeg-source', dockerfile.includes('ARG DEBIAN_SNAPSHOT=') && dockerfile.includes('ARG FFMPEG_SOURCE_REVISION=') && dockerfile.includes('ARG FFMPEG_SOURCE_SHA256=') && dockerfile.includes('code.ffmpeg.org/FFmpeg/FFmpeg/archive/${FFMPEG_SOURCE_REVISION}.tar.gz') && dockerfile.includes('git -C /tmp/ffmpeg-source apply --reverse --check') && dockerfile.includes('sha256sum --check --strict') && dockerfile.includes('--disable-everything') && dockerfile.includes('COPY --from=ffmpeg-builder')),
    check(
      'docker-build-context-excludes-secrets',
      dockerignore.includes('**/.env') && dockerignore.includes('ops/*.env') && dockerignore.includes('*.pem') && dockerignore.includes('*.key'),
    ),
    check(
      'docker-healthcheck-without-runtime-http-client',
      dockerfile.includes('CMD ["/usr/local/bin/jellyrin-server", "--healthcheck"]') &&
        dockerfile.includes('HEALTHCHECK') &&
        !/apt-get install[^\n]*\bcurl\b/.test(runtimeDockerfile) &&
        server.includes('run_container_healthcheck()') &&
        server.includes('GET /healthz HTTP/1.1') &&
        server.includes('TcpStream::connect_timeout'),
    ),
    check('compose-service', compose.includes('jellyrin:') && compose.includes('${JELLYRIN_HOST_PORT:-8096}:8096') && compose.includes('JELLYRIN_PORT: "8096"') && compose.includes('postgres:')),
    check('compose-backend-loopback-only', compose.includes('${JELLYRIN_PUBLISH_ADDRESS:-127.0.0.1}:${JELLYRIN_HOST_PORT:-8096}:8096')),
    check('nginx-path-only-access-log', nginxLogDefinition.includes('$request_method $uri $server_protocol') && !nginxLogDefinition.includes('$request_uri') && !nginxLogDefinition.includes('$args') && !nginxLogDefinition.includes(' $request ') && !nginxLogDefinition.includes('$http_referer') && nginxAcmeBootstrap.includes('$request_method $uri $server_protocol') && nginxAcmeBootstrap.includes('access_log /var/log/nginx/jellyrin.access.log jellyrin_path_only;') && !nginxAcmeBootstrap.includes('$request_uri') && !nginxAcmeBootstrap.includes('$args')),
    check('nginx-http-redirect-preserves-query', nginx.includes('return 308 https://$host$request_uri;')),
    check('compose-migration-gate', compose.includes('jellyrin-migrate:') && compose.includes('condition: service_completed_successfully')),
    check('compose-distroless-healthcheck', compose.includes('test: ["CMD", "/usr/local/bin/jellyrin-server", "--healthcheck"]') && !compose.includes('test: ["CMD", "curl"')),
    check('compose-postgres-private-network', infrastructureCompose.includes('${POSTGRES_IMAGE:-docker.io/library/postgres:') && infrastructureCompose.includes('internal: true')),
    check('compose-infrastructure-images-locked', infrastructureCompose.includes('docker.io/library/postgres:17.10-bookworm@sha256:') && infrastructureCompose.includes('docker.io/library/redis:7.2.14-bookworm@sha256:')),
    check('compose-persistent-volumes', compose.includes('jellyrin-postgres') && compose.includes('jellyrin-data') && compose.includes('jellyrin-config') && compose.includes('jellyrin-cache')),
    check('systemd-production-unit', systemd.includes('EnvironmentFile=/etc/jellyrin/jellyrin.env') && systemd.includes('ExecStart=/usr/local/bin/jellyrin-server') && systemd.includes('Requires=jellyrin-migrate.service')),
    check('systemd-migration-unit', migrationSystemd.includes('Type=oneshot') && migrationSystemd.includes('ExecStart=/usr/local/bin/jellyrin-migrate schema') && migrationSystemd.includes('EnvironmentFile=/etc/jellyrin-migrate.env')),
    check('systemd-hardening', systemd.includes('NoNewPrivileges=true') && systemd.includes('ProtectSystem=strict') && systemd.includes('PrivateDevices=true') && systemd.includes('ProtectKernelTunables=true') && systemd.includes('ProtectKernelModules=true') && systemd.includes('ProtectControlGroups=true') && systemd.includes('RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6') && systemd.includes('CapabilityBoundingSet=') && systemd.includes('LoadCredential=provider-secret-keyring.json:/etc/jellyrin-secrets/provider-secret-keyring.json') && systemd.includes('Environment=JELLYRIN_PROVIDER_SECRET_KEYRING_FILE=%d/provider-secret-keyring.json')),
    check('systemd-network-online', systemd.includes('After=network-online.target') && systemd.includes('Wants=network-online.target')),
    check('systemd-restart-and-state-dirs', systemd.includes('Restart=on-failure') && systemd.includes('StateDirectory=jellyrin') && systemd.includes('CacheDirectory=jellyrin') && systemd.includes('LogsDirectory=jellyrin')),
    check('systemd-dlna-writable-paths', systemd.includes('ReadWritePaths=/var/lib/jellyrin /var/cache/jellyrin /var/log/jellyrin /etc/jellyrin')),
    check('compose-dlna-host-network', dlnaCompose.includes('network_mode: host') && dlnaCompose.includes('ports: !reset []')),
    check('compose-dlna-ssdp-guidance', dlnaCompose.includes('multicast UDP 1900') && dlnaCompose.includes('advertised LOCATION')),
    check('config-dirs-env', env.includes('JELLYRIN_DATA_DIR=/var/lib/jellyrin') && env.includes('DATABASE_URL=postgresql://jellyrin_runtime:')),
    check('separate-postgres-credentials', migrationEnv.includes('DATABASE_URL=postgresql://jellyrin_migrator:') && !env.includes('jellyrin_migrator')),
    check('runtime-config-excludes-sqlite', !env.includes('DATABASE_URL=sqlite:') && !compose.includes('DATABASE_URL: sqlite:')),
    check('runtime-ffmpeg-default-fails-safe', api.includes('None | Some("remux-only") => Ok(FfmpegMode::RemuxOnly)')),
    check('runtime-ffmpeg-aggregate-cap', env.includes('JELLYRIN_MAX_FFMPEG_JOBS=2') && env.includes('JELLYRIN_MAX_PROBE_JOBS=1') && env.includes('JELLYRIN_MAX_QUEUED_PROBES=8') && env.includes('JELLYRIN_PROBE_QUEUE_TIMEOUT_SECONDS=10') && env.includes('Aggregate cap shared by encode, remux, auxiliary FFmpeg and ffprobe lanes') && transcode.includes('"JELLYRIN_MAX_FFMPEG_JOBS"') && transcode.includes('"JELLYRIN_MAX_PROBE_JOBS"') && transcode.includes('MULTIMEDIA_PROCESS_COORDINATOR')),
    check('runtime-transcode-disk-reservation', env.includes('JELLYRIN_TRANSCODE_RESERVATION_BYTES=67108864') && api.includes('"JELLYRIN_TRANSCODE_RESERVATION_BYTES"') && api.includes('reserve_transcode_disk_capacity()')),
    check('server-health-routes', api.includes('route("/healthz", get(health))') && api.includes('route("/readyz", get(ready))')),
    check('external-schema-migrations', migrator.includes('apply_schema(&args.target)') && server.includes('db.schema_health()')),
    check('production-server-tree-command', serverTree.code === 0),
    check('production-server-excludes-sqlite', serverTree.code === 0 && !sqliteDependency.test(serverTree.stdout)),
    check('sqlite-confined-to-migrator', migratorTree.code === 0 && sqliteDependency.test(migratorTree.stdout)),
    check('release-checklist-fresh-upgrade-rollback', checklist.includes('## Fresh Install') && checklist.includes('## Upgrade') && checklist.includes('## Rollback')),
    check('release-checklist-ffmpeg-safe-upgrade', checklist.includes('ffprobe -version') && checklist.includes('The default `JELLYRIN_FFMPEG_MODE=enabled`') && checklist.includes('use `remux-only`') && checklist.includes('only for a measured direct-play-compatible client fleet')),
    check('supply-chain-lock-present', supplyChainLock.includes('RUST_IMAGE=docker.io/library/rust:1.94.0-bookworm@sha256:') && supplyChainLock.includes('DISTROLESS_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:') && supplyChainLock.includes('SYFT_LINUX_AMD64_SHA256=') && supplyChainLock.includes('JELLYFIN_WEB_VERSION=10.11.11') && supplyChainLock.includes('JELLYFIN_WEB_COMMIT=35c0793ece3adbd247eab290ae1effab851f3d37') && supplyChainLock.includes('JELLYFIN_WEB_TARBALL_SHA256=1dd84a8bf4aaa90b12ca38e72e68a554826ab0ea28bb354bcc3212f579e0a337') && supplyChainLock.includes('JELLYFIN_WEB_SWIPER_VERSION=12.1.2') && supplyChainLock.includes('JELLYFIN_WEB_SWIPER_PATCH_COMMIT=3cb38a0ac319edfcbcd331e3818cc9f6dec3e334') && supplyChainLock.includes('JELLYFIN_WEB_SWIPER_PATCH_SHA256=76e80a084337162a24dba022760492fa52f102fbf88747ff895b7076fc17f4b4')),
    check('jellyfin-web-build-verified-atomic-untracked', jellyfinWebBuilder.includes('sha256sum --check --strict') && jellyfinWebBuilder.includes('commit/${JELLYFIN_WEB_SWIPER_PATCH_COMMIT}.patch') && jellyfinWebBuilder.includes('Swiper security patch must modify only package.json and package-lock.json') && jellyfinWebBuilder.includes('EXPECTED_SWIPER_VERSION') && jellyfinWebBuilder.includes('npm ci --omit=optional') && jellyfinWebBuilder.includes('node_modules/pdfjs-dist/node_modules/canvas') && jellyfinWebBuilder.includes('node_modules/@mapbox/node-pre-gyp') && jellyfinWebBuilder.includes('node_modules/tar') && jellyfinWebBuilder.includes('npm run build:production') && jellyfinWebBuilder.includes('refusing to overwrite existing Jellyfin Web output') && jellyfinWebBuilder.includes('mv -T') && gitignore.split(/\r?\n/).includes('/web/')),
    check('deployment-preflight-fail-closed', deploymentPreflight.includes('never reads env/key contents') && deploymentPreflight.includes('web output is missing regular index.html') && deploymentPreflight.includes('--require-provider-keyring') && deploymentPreflight.includes("stat -c '%a'") && deploymentPreflight.includes("stat -c '%u'") && deploymentPreflight.includes("stat -c '%g'")),
    check('sbom-generator-verifies-output', sbomGenerator.includes('jellyrin-image.spdx.json') && sbomGenerator.includes('jellyrin-source.cyclonedx.json') && sbomGenerator.includes('sha256sum --check --strict SHA256SUMS')),
    check('supply-chain-qa-and-ci', supplyChainQa.includes('ci-actions-are-commit-pinned') && ci.includes('node qa/supply-chain.js') && ci.includes('ops/generate-sbom.sh jellyrin:ci supply-chain-artifacts')),
    check('readme-release-entrypoint', readme.includes('## Release Packaging') && readme.includes('npm run qa:packaging-release')),
    check('jellyfin-web-hardening-documentation', readme.includes('Swiper 12.1.2') && readme.includes('canvas`/`node-pre-gyp`/`tar') && checklist.includes('--omit=optional') && checklist.includes('slideshow and comics')),
  ];

  const failed = checks.filter((item) => item.status !== 'passed');
  const result = {
    generatedAt: new Date().toISOString(),
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: {
      passed: checks.length - failed.length,
      failed: failed.length,
      total: checks.length,
    },
    checks,
    dependencyBoundary: {
      serverCommand: 'cargo tree -p jellyrin-server -e normal',
      serverExitCode: serverTree.code,
      migratorCommand: 'cargo tree -p jellyrin-migrate -e normal',
      migratorExitCode: migratorTree.code,
    },
  };

  await fs.mkdir(generatedDir, { recursive: true });
  await fs.writeFile(
    path.join(generatedDir, 'packaging-release.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  await fs.writeFile(path.join(generatedDir, 'packaging-release.md'), renderMarkdown(result));
  console.log(`wrote ${path.join(generatedDir, 'packaging-release.md')}`);

  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

async function read(relativePath) {
  return fs.readFile(path.join(repoRoot, relativePath), 'utf8');
}

function cargoTree(packageName) {
  return new Promise((resolve) => {
    const child = spawn(
      'cargo',
      ['tree', '-p', packageName, '-e', 'normal', '--prefix', 'none'],
      { cwd: repoRoot, stdio: ['ignore', 'pipe', 'pipe'] },
    );
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    child.on('error', (error) => resolve({ code: 1, stdout, stderr: error.message }));
    child.on('close', (code) => resolve({ code: code ?? 1, stdout, stderr }));
  });
}

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Packaging Release Matrix');
  lines.push('');
  lines.push(`- Status: ${result.status}`);
  lines.push(`- Passed: ${result.summary.passed}/${result.summary.total}`);
  lines.push('');
  lines.push('| Check | Status |');
  lines.push('| --- | --- |');
  for (const item of result.checks) {
    lines.push(`| ${item.id} | ${item.status} |`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
