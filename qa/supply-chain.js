#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');

async function main() {
  const [
    lockText,
    dockerfile,
    dockerignore,
    compose,
    infrastructureCompose,
    composeEnv,
    workflow,
    generator,
    rustsecRunner,
    vulnerabilityScanner,
    vulnerabilityRenderer,
    vulnerabilityExceptionsText,
    supplyChainDocumentation,
    readme,
    checklist,
    gitignore,
    jellyfinWebBuilder,
    deploymentPreflight,
    nginx,
    ffmpegSmoke,
    runtimeSmoke,
    runtimeHygieneAudit,
    ffmpegSecurityBaseline,
  ] = await Promise.all([
    read('ops/supply-chain.lock.env'),
    read('Dockerfile'),
    read('.dockerignore'),
    read('docker-compose.yml'),
    read('docker-compose.infrastructure.yml'),
    read('ops/compose.env.example'),
    read('.github/workflows/ci.yml'),
    read('ops/generate-sbom.sh'),
    read('ops/audit-rustsec.sh'),
    read('ops/scan-vulnerabilities.sh'),
    read('ops/render-vulnerability-ignores.js'),
    read('ops/vulnerability-exceptions.json'),
    read('ops/supply-chain.md'),
    read('README.md'),
    read('ops/release-checklist.md'),
    read('.gitignore'),
    read('ops/build-jellyfin-web.sh'),
    read('ops/deployment-preflight.sh'),
    read('ops/nginx-jellyrin.test.kode.live.conf.example'),
    read('qa/ffmpeg-remux-smoke.sh'),
    read('qa/runtime-container-smoke.sh'),
    read('ops/audit-runtime-hygiene.sh'),
    read('ops/ffmpeg-security-baseline.txt'),
  ]);
  const cargoLock = await read('Cargo.lock');
  const lock = parseLock(lockText);
  const runtimeDockerfile = dockerfile.slice(dockerfile.lastIndexOf('FROM runtime-smoke'));
  const vulnerabilityExceptions = JSON.parse(vulnerabilityExceptionsText);
  const exceptionErrors = validateVulnerabilityExceptions(vulnerabilityExceptions);
  const exceptionValidatorSelfTest = validateExceptionValidator();
  const requiredKeys = [
    'RUST_IMAGE',
    'RUNTIME_IMAGE',
    'DISTROLESS_IMAGE',
    'POSTGRES_IMAGE',
    'REDIS_IMAGE',
    'DEBIAN_SNAPSHOT',
    'FFMPEG_SOURCE_REVISION',
    'FFMPEG_SOURCE_VERSION',
    'FFMPEG_NVD_BASELINE_VERSION',
    'FFMPEG_SOURCE_SHA256',
    'SYFT_VERSION',
    'SYFT_LINUX_AMD64_SHA256',
    'SYFT_LINUX_ARM64_SHA256',
    'CARGO_AUDIT_VERSION',
    'CARGO_AUDIT_CRATE_SHA256',
    'RUSTSEC_ADVISORY_DB_REVISION',
    'TRIVY_VERSION',
    'TRIVY_LINUX_AMD64_SHA256',
    'TRIVY_LINUX_ARM64_SHA256',
    'ACTIONS_CHECKOUT_SHA',
    'DTOLNAY_RUST_TOOLCHAIN_SHA',
    'ACTIONS_UPLOAD_ARTIFACT_SHA',
    'JELLYFIN_WEB_VERSION',
    'JELLYFIN_WEB_COMMIT',
    'JELLYFIN_WEB_TARBALL_SHA256',
    'JELLYFIN_WEB_SWIPER_VERSION',
    'JELLYFIN_WEB_SWIPER_PATCH_COMMIT',
    'JELLYFIN_WEB_SWIPER_PATCH_SHA256',
  ];
  const imageKeys = ['RUST_IMAGE', 'RUNTIME_IMAGE', 'DISTROLESS_IMAGE', 'POSTGRES_IMAGE', 'REDIS_IMAGE'];
  const actionReferences = [...workflow.matchAll(/uses:\s*[^@\s]+@([^\s#]+)/g)].map(
    (match) => match[1],
  );
  const rustToolchainUses = [
    ...workflow.matchAll(/uses:\s*dtolnay\/rust-toolchain@([^\s#]+)/g),
  ];
  const rustToolchainSteps = workflow
    .split(/(?=^      - )/m)
    .filter((step) => step.includes('uses: dtolnay/rust-toolchain@'));
  const checks = [
    check('lock-required-fields', requiredKeys.every((key) => nonEmpty(lock[key]))),
    check(
      'lock-images-use-tag-and-digest',
      imageKeys.every((key) =>
        /^[a-z0-9][a-z0-9./_-]*:[A-Za-z0-9._-]+@sha256:[a-f0-9]{64}$/.test(lock[key] || ''),
      ),
    ),
    check('lock-debian-snapshot', /^\d{8}T\d{6}Z$/.test(lock.DEBIAN_SNAPSHOT || '')),
    check(
      'lock-ffmpeg-source',
      /^[a-f0-9]{40}$/.test(lock.FFMPEG_SOURCE_REVISION || '') &&
        /^\d+\.\d+-dev-git-[a-f0-9]{12}$/.test(lock.FFMPEG_SOURCE_VERSION || '') &&
        /^\d+\.\d+\.\d+$/.test(lock.FFMPEG_NVD_BASELINE_VERSION || '') &&
        /^[a-f0-9]{64}$/.test(lock.FFMPEG_SOURCE_SHA256 || ''),
    ),
    check(
      'lock-syft-checksums',
      /^\d+\.\d+\.\d+$/.test(lock.SYFT_VERSION || '') &&
        /^[a-f0-9]{64}$/.test(lock.SYFT_LINUX_AMD64_SHA256 || '') &&
        /^[a-f0-9]{64}$/.test(lock.SYFT_LINUX_ARM64_SHA256 || ''),
    ),
    check(
      'lock-vulnerability-scanners',
      /^\d+\.\d+\.\d+$/.test(lock.CARGO_AUDIT_VERSION || '') &&
        /^[a-f0-9]{64}$/.test(lock.CARGO_AUDIT_CRATE_SHA256 || '') &&
        /^[a-f0-9]{40}$/.test(lock.RUSTSEC_ADVISORY_DB_REVISION || '') &&
        /^\d+\.\d+\.\d+$/.test(lock.TRIVY_VERSION || '') &&
        /^[a-f0-9]{64}$/.test(lock.TRIVY_LINUX_AMD64_SHA256 || '') &&
        /^[a-f0-9]{64}$/.test(lock.TRIVY_LINUX_ARM64_SHA256 || ''),
    ),
    check(
      'lock-actions-immutable',
      ['ACTIONS_CHECKOUT_SHA', 'DTOLNAY_RUST_TOOLCHAIN_SHA', 'ACTIONS_UPLOAD_ARTIFACT_SHA'].every(
        (key) => /^[a-f0-9]{40}$/.test(lock[key] || ''),
      ),
    ),
    check(
      'lock-jellyfin-web-immutable',
      lock.JELLYFIN_WEB_VERSION === '10.11.11' &&
        lock.JELLYFIN_WEB_COMMIT === '35c0793ece3adbd247eab290ae1effab851f3d37' &&
        lock.JELLYFIN_WEB_TARBALL_SHA256 ===
          '1dd84a8bf4aaa90b12ca38e72e68a554826ab0ea28bb354bcc3212f579e0a337' &&
        lock.JELLYFIN_WEB_SWIPER_VERSION === '12.1.2' &&
        lock.JELLYFIN_WEB_SWIPER_PATCH_COMMIT ===
          '3cb38a0ac319edfcbcd331e3818cc9f6dec3e334' &&
        lock.JELLYFIN_WEB_SWIPER_PATCH_SHA256 ===
          '76e80a084337162a24dba022760492fa52f102fbf88747ff895b7076fc17f4b4' &&
        /^[a-f0-9]{40}$/.test(lock.JELLYFIN_WEB_COMMIT) &&
        /^[a-f0-9]{64}$/.test(lock.JELLYFIN_WEB_TARBALL_SHA256) &&
        /^[a-f0-9]{40}$/.test(lock.JELLYFIN_WEB_SWIPER_PATCH_COMMIT) &&
        /^[a-f0-9]{64}$/.test(lock.JELLYFIN_WEB_SWIPER_PATCH_SHA256),
    ),
    check(
      'jellyfin-web-build-is-verified-and-atomic',
        jellyfinWebBuilder.includes('archive/${JELLYFIN_WEB_COMMIT}.tar.gz') &&
        jellyfinWebBuilder.includes(
          'commit/${JELLYFIN_WEB_SWIPER_PATCH_COMMIT}.patch',
        ) &&
        jellyfinWebBuilder.includes('sha256sum --check --strict') &&
        jellyfinWebBuilder.includes('Swiper security patch must modify only package.json and package-lock.json') &&
        jellyfinWebBuilder.includes('patch --batch --forward --strip=1') &&
        jellyfinWebBuilder.includes('EXPECTED_SWIPER_VERSION') &&
        jellyfinWebBuilder.includes('npm ci --omit=optional') &&
        jellyfinWebBuilder.includes('node_modules/pdfjs-dist/node_modules/canvas') &&
        jellyfinWebBuilder.includes('node_modules/@mapbox/node-pre-gyp') &&
        jellyfinWebBuilder.includes('node_modules/tar') &&
        jellyfinWebBuilder.includes('npm run build:production') &&
        jellyfinWebBuilder.includes('dist/index.html') &&
        jellyfinWebBuilder.includes('refusing to overwrite existing Jellyfin Web output') &&
        jellyfinWebBuilder.includes('mktemp -d') &&
        jellyfinWebBuilder.includes('mv -T'),
    ),
    check(
      'jellyfin-web-output-is-untracked',
      gitignore.split(/\r?\n/).includes('/web/'),
    ),
    check(
      'deployment-preflight-is-metadata-only-and-fail-closed',
      deploymentPreflight.includes('never reads env/key contents') &&
        deploymentPreflight.includes('--require-provider-keyring') &&
        deploymentPreflight.includes('web output is missing regular index.html') &&
        deploymentPreflight.includes("stat -c '%a'") &&
        deploymentPreflight.includes("stat -c '%u'") &&
        deploymentPreflight.includes("stat -c '%g'") &&
        !deploymentPreflight.includes('cat "${compose_env}"') &&
        !deploymentPreflight.includes('cat "${runtime_env}"') &&
        !deploymentPreflight.includes('cat "${provider_keyring}"'),
    ),
    check(
      'nginx-redirect-preserves-query-with-path-only-logs',
      nginx.includes('"$request_method $uri $server_protocol"') &&
        nginx.includes('return 308 https://$host$request_uri;') &&
        !nginx.slice(0, nginx.indexOf('server {')).includes('$request_uri') &&
        !nginx.slice(0, nginx.indexOf('server {')).includes('$args'),
    ),
    check(
      'docker-base-images-match-lock',
      dockerfile.includes(`ARG RUST_IMAGE=${lock.RUST_IMAGE}`) &&
        dockerfile.includes(`ARG RUNTIME_IMAGE=${lock.RUNTIME_IMAGE}`) &&
        dockerfile.includes(`ARG DISTROLESS_IMAGE=${lock.DISTROLESS_IMAGE}`) &&
        dockerfile.includes('FROM ${DISTROLESS_IMAGE} AS runtime-smoke') &&
        dockerfile.includes('org.opencontainers.image.base.name="${DISTROLESS_IMAGE}"'),
    ),
    check(
      'docker-cargo-lock-enforced',
      dockerfile.includes('cargo build --locked --release -p jellyrin-server -p jellyrin-migrate'),
    ),
    check(
      'docker-build-context-excludes-sbom-output',
      ['/web/', '/supply-chain-artifacts/', '/vulnerability-artifacts/', '/rustsec-audit-artifacts/'].every(
        (output) => dockerignore.includes(output),
      ),
    ),
    check(
      'docker-debian-snapshot-matches-lock',
      dockerfile.includes(`ARG DEBIAN_SNAPSHOT=${lock.DEBIAN_SNAPSHOT}`) &&
        dockerfile.includes(`archive/debian/\${DEBIAN_SNAPSHOT}`) &&
        dockerfile.includes(`archive/debian-security/\${DEBIAN_SNAPSHOT}`),
    ),
    check(
      'docker-ffmpeg-pin-and-verification',
      dockerfile.includes(`ARG FFMPEG_SOURCE_SHA256=${lock.FFMPEG_SOURCE_SHA256}`) &&
        dockerfile.includes(`ARG FFMPEG_SOURCE_REVISION=${lock.FFMPEG_SOURCE_REVISION}`) &&
        dockerfile.includes(`ARG FFMPEG_SOURCE_VERSION=${lock.FFMPEG_SOURCE_VERSION}`) &&
        dockerfile.includes(`ARG FFMPEG_NVD_BASELINE_VERSION=${lock.FFMPEG_NVD_BASELINE_VERSION}`) &&
        dockerfile.includes('code.ffmpeg.org/FFmpeg/FFmpeg/archive/${FFMPEG_SOURCE_REVISION}.tar.gz') &&
        dockerfile.includes('code.ffmpeg.org/FFmpeg/FFmpeg/commit/${fix_commit}.patch') &&
        dockerfile.includes('git -C /tmp/ffmpeg-source apply --reverse --check') &&
        dockerfile.includes("sha256sum --check --strict") &&
        dockerfile.includes('--disable-everything') &&
        dockerfile.includes('--enable-muxer=hls,mpegts,mov,mp4') &&
        dockerfile.includes('COPY --from=ffmpeg-builder') &&
        dockerfile.includes('RUN ["/usr/local/bin/ffmpeg", "-version"]') &&
        !runtimeDockerfile.includes('apt-get install -y --no-install-recommends ffmpeg') &&
        dockerfile.includes('ffprobe -version'),
    ),
    check(
      'docker-runtime-excludes-general-http-client',
      !/apt-get install[^\n]*\bcurl\b/.test(runtimeDockerfile) &&
        dockerfile.includes('CMD ["/usr/local/bin/jellyrin-server", "--healthcheck"]') &&
        !dockerfile.includes('CMD curl'),
    ),
    check(
      'compose-build-lock-defaults',
      compose.includes(`RUST_IMAGE: \${RUST_IMAGE:-${lock.RUST_IMAGE}}`) &&
        compose.includes(`RUNTIME_IMAGE: \${RUNTIME_IMAGE:-${lock.RUNTIME_IMAGE}}`) &&
        compose.includes(`DISTROLESS_IMAGE: \${DISTROLESS_IMAGE:-${lock.DISTROLESS_IMAGE}}`) &&
        compose.includes(`DEBIAN_SNAPSHOT: \${DEBIAN_SNAPSHOT:-${lock.DEBIAN_SNAPSHOT}}`) &&
        compose.includes(`FFMPEG_SOURCE_REVISION: \${FFMPEG_SOURCE_REVISION:-${lock.FFMPEG_SOURCE_REVISION}}`) &&
        compose.includes(`FFMPEG_SOURCE_VERSION: \${FFMPEG_SOURCE_VERSION:-${lock.FFMPEG_SOURCE_VERSION}}`) &&
        compose.includes(`FFMPEG_NVD_BASELINE_VERSION: \${FFMPEG_NVD_BASELINE_VERSION:-${lock.FFMPEG_NVD_BASELINE_VERSION}}`) &&
        compose.includes(`FFMPEG_SOURCE_SHA256: \${FFMPEG_SOURCE_SHA256:-${lock.FFMPEG_SOURCE_SHA256}}`),
    ),
    check(
      'compose-infrastructure-lock-defaults',
      infrastructureCompose.includes(`\${POSTGRES_IMAGE:-${lock.POSTGRES_IMAGE}}`) &&
        infrastructureCompose.includes(`\${REDIS_IMAGE:-${lock.REDIS_IMAGE}}`),
    ),
    check(
      'compose-example-matches-lock',
      imageKeys.every((key) => composeEnv.includes(`${key}=${lock[key]}`)) &&
        composeEnv.includes(`DEBIAN_SNAPSHOT=${lock.DEBIAN_SNAPSHOT}`) &&
        composeEnv.includes(`FFMPEG_SOURCE_REVISION=${lock.FFMPEG_SOURCE_REVISION}`) &&
        composeEnv.includes(`FFMPEG_SOURCE_VERSION=${lock.FFMPEG_SOURCE_VERSION}`) &&
        composeEnv.includes(`FFMPEG_NVD_BASELINE_VERSION=${lock.FFMPEG_NVD_BASELINE_VERSION}`) &&
        composeEnv.includes(`FFMPEG_SOURCE_SHA256=${lock.FFMPEG_SOURCE_SHA256}`),
    ),
    check(
      'ci-actions-are-commit-pinned',
      actionReferences.length > 0 && actionReferences.every((reference) => /^[a-f0-9]{40}$/.test(reference)),
    ),
    check(
      'ci-action-pins-match-lock',
      workflow.includes(`actions/checkout@${lock.ACTIONS_CHECKOUT_SHA}`) &&
        workflow.includes(`dtolnay/rust-toolchain@${lock.DTOLNAY_RUST_TOOLCHAIN_SHA}`) &&
        workflow.includes(`actions/upload-artifact@${lock.ACTIONS_UPLOAD_ARTIFACT_SHA}`),
    ),
    check(
      'ci-rust-toolchain-is-explicit',
      rustToolchainUses.length > 0 &&
        rustToolchainSteps.length === rustToolchainUses.length &&
        rustToolchainSteps.every((step) => /^\s{10}toolchain:\s*1\.94\.0\s*$/m.test(step)),
    ),
    check(
      'rust-lock-excludes-rsa',
      cargoLock.includes('name = "sqlx"\nversion = "0.9.0"') &&
        !cargoLock.includes('name = "rsa"\n'),
    ),
    check(
      'ci-rust-commands-are-locked-and-cover-all-features',
      workflow.includes('cargo check --locked -p jellyrin-server') &&
        workflow.includes('cargo check --locked --workspace --all-targets --all-features') &&
        workflow.includes('cargo clippy --locked --workspace --all-targets --all-features -- -D warnings') &&
        workflow.includes('cargo test --locked --workspace --all-targets --all-features') &&
        workflow.includes('cargo test --locked -p jellyrin-db --all-features') &&
        workflow.includes('cargo test --locked -p jellyrin-migrate --all-features'),
    ),
    check('ci-postgres-image-matches-lock', workflow.includes(`image: ${lock.POSTGRES_IMAGE}`)),
    check(
      'ci-builds-and-uploads-sbom',
      workflow.includes('node qa/supply-chain.js') &&
        workflow.includes('ops/generate-sbom.sh jellyrin:ci supply-chain-artifacts') &&
        workflow.includes('name: jellyrin-supply-chain-${{ github.sha }}'),
    ),
    check(
      'ci-requires-native-amd64-image',
      workflow.includes("test \"$(docker image inspect --format '{{.Architecture}}' jellyrin:ci)\" = amd64"),
    ),
    check(
      'ci-validates-locked-compose-topology',
      workflow.includes('name: Validate locked Compose topology') &&
        workflow.includes('docker compose config --quiet') &&
        workflow.includes('POSTGRES_MIGRATOR_PASSWORD: ci-compose-migrator') &&
        workflow.includes('POSTGRES_RUNTIME_PASSWORD: ci-compose-runtime'),
    ),
    check(
      'ci-runs-provider-url-retention-gate',
      workflow.includes('jellyrin-migrate -- audit-source-hygiene') &&
        workflow.includes('JELLYRIN_TEST_POSTGRES_URL'),
    ),
    check(
      'runtime-hygiene-audit-is-counts-only-and-fail-closed',
      runtimeHygieneAudit.includes('audit-runtime-hygiene') &&
        runtimeHygieneAudit.includes('--relay-port') &&
        runtimeHygieneAudit.includes('journalctl') &&
        runtimeHygieneAudit.includes('cgroup.procs') &&
        runtimeHygieneAudit.includes('status=3') &&
        workflow.includes('node qa/runtime-hygiene-smoke.js') &&
        checklist.includes('ops/audit-runtime-hygiene.sh'),
    ),
    check(
      'vulnerability-exceptions-are-governed',
      exceptionErrors.length === 0,
      exceptionErrors.join('; '),
    ),
    check('vulnerability-exception-validator-self-test', exceptionValidatorSelfTest),
    check(
      'vulnerability-rendering-is-scoped',
      vulnerabilityRenderer.includes("mode === 'rustsec-ids'") &&
        vulnerabilityRenderer.includes("mode === 'trivy-yaml'") &&
        vulnerabilityRenderer.includes("process.stdout.write('    purls:\\n')") &&
        vulnerabilityRenderer.includes('entry.expires_on') &&
        vulnerabilityRenderer.includes('entry.tracking_issue'),
    ),
    check(
      'cargo-audit-is-locked-and-offline-to-pinned-db',
      [rustsecRunner, vulnerabilityScanner].every((runner) => runner.includes(
        'https://crates.io/api/v1/crates/cargo-audit/${CARGO_AUDIT_VERSION}/download',
      ) &&
        runner.includes('"${CARGO_AUDIT_CRATE_SHA256}"') &&
        runner.includes('cargo install --locked') &&
        runner.includes('fetch --quiet --depth=1 origin "${RUSTSEC_ADVISORY_DB_REVISION}"') &&
        runner.includes('--no-fetch') &&
        runner.includes('--no-yanked') &&
        runner.includes('--deny unsound') &&
        runner.includes('--json')) &&
        rustsecRunner.includes('node "${repo_root}/qa/supply-chain.js"') &&
        rustsecRunner.includes('rustsec-ignores.txt') &&
        rustsecRunner.includes('rustsec-status.json') &&
        rustsecRunner.includes('> SHA256SUMS') &&
        !rustsecRunner.includes('required_command in docker') &&
        !rustsecRunner.includes('required_command in cargo curl docker'),
    ),
    check(
      'vulnerability-policy-is-strict-and-evidenced',
      vulnerabilityScanner.includes(
        'aquasecurity/trivy/releases/download/v${TRIVY_VERSION}/trivy_${TRIVY_VERSION}_Linux-${trivy_arch}.tar.gz',
      ) &&
        vulnerabilityScanner.includes('TRIVY_LINUX_AMD64_SHA256') &&
        vulnerabilityScanner.includes('TRIVY_LINUX_ARM64_SHA256') &&
        vulnerabilityScanner.includes('--severity HIGH,CRITICAL') &&
        vulnerabilityScanner.includes('--show-suppressed') &&
        vulnerabilityScanner.includes('--exit-code 1') &&
        !vulnerabilityScanner.includes('--ignore-unfixed') &&
        vulnerabilityScanner.includes('trivy-version.txt') &&
        vulnerabilityScanner.includes('trivy-image.json') &&
        vulnerabilityScanner.includes('services.nvd.nist.gov/rest/json/cves/2.0') &&
        vulnerabilityScanner.includes('FFMPEG_NVD_BASELINE_VERSION') &&
        vulnerabilityScanner.includes('nvd-ffmpeg-high-critical.txt') &&
        vulnerabilityScanner.includes('nvd-ffmpeg-unmapped.txt') &&
        vulnerabilityScanner.includes('NVD reports unmapped HIGH/CRITICAL FFmpeg vulnerabilities') &&
        ffmpegSecurityBaseline.split(/\r?\n/).filter((line) => line.startsWith('CVE-')).length === 16 &&
        vulnerabilityScanner.includes('scan-status.json'),
    ),
    check(
      'ci-runs-minimal-ffmpeg-corpus',
      workflow.includes('qa/ffmpeg-remux-smoke.sh jellyrin:ci') &&
        ffmpegSmoke.includes('source.mp4 source.mkv source.ts') &&
        ffmpegSmoke.includes('-show_format -show_streams -of json') &&
        ffmpegSmoke.includes('-c copy -f hls') &&
        ffmpegSmoke.includes('[[ -z "${encoder_names}" ]]') &&
        ffmpegSmoke.includes('[[ "${decoder_names}" == "aac" ]]'),
    ),
    check(
      'ci-runs-distroless-runtime-smoke',
      workflow.includes('qa/runtime-container-smoke.sh jellyrin:ci') &&
        runtimeSmoke.includes('/usr/local/bin/jellyrin-migrate') &&
        runtimeSmoke.includes('/usr/local/bin/jellyrin-server --healthcheck') &&
        runtimeSmoke.includes('--read-only') &&
        runtimeSmoke.includes('10001:10001'),
    ),
    check(
      'ci-runs-and-retains-vulnerability-gate',
      workflow.includes('schedule:') &&
        workflow.includes("github.event_name == 'schedule'") &&
        workflow.includes("needs.format.result == 'success'") &&
        workflow.includes("needs['postgres-schema'].result == 'success'") &&
        workflow.includes('ops/scan-vulnerabilities.sh jellyrin:ci vulnerability-artifacts') &&
        workflow.includes('vulnerability-artifacts/') &&
        workflow.includes('if: ${{ always() }}'),
    ),
    check(
      'sbom-tool-download-is-verified',
      generator.includes('node "${repo_root}/qa/supply-chain.js"') &&
        generator.includes('required_command in curl docker jq node sha256sum tar') &&
        generator.includes('SYFT_LINUX_AMD64_SHA256') &&
        generator.includes('SYFT_LINUX_ARM64_SHA256') &&
        generator.includes('sha256sum --check --strict'),
    ),
    check(
      'sbom-covers-image-and-rust-source',
      generator.includes('jellyrin-image.spdx.json') &&
      generator.includes('jellyrin-image.cyclonedx.json') &&
        generator.includes('jellyrin-source.spdx.json') &&
        generator.includes('jellyrin-source.cyclonedx.json') &&
        generator.includes('cpe:2.3:a:ffmpeg:ffmpeg:') &&
        generator.includes('SPDXRef-Package-FFmpeg'),
    ),
    check(
      'sbom-output-is-verified',
        generator.includes('FFmpeg source digest drift') &&
        generator.includes('general-purpose packaged FFmpeg is present') &&
        generator.includes('distroless runtime package surface drifted above its reviewed bound') &&
        generator.includes('remux-only image decoder allowlist drift') &&
        generator.includes('ffmpeg-source.spdx.json') &&
        generator.includes('ffmpeg-source.cyclonedx.json') &&
        generator.includes("grep -o -- '--enable-parser=[^[:space:]]*'") &&
        generator.includes('ffmpeg-parsers.txt') &&
        generator.includes('remux-only image parser allowlist drift') &&
        generator.includes('release image has no immutable VCS revision label') &&
        generator.includes('.name == "ffmpeg" and .versionInfo == $version') &&
        generator.includes('> SHA256SUMS') &&
        generator.includes('sha256sum --check --strict SHA256SUMS') &&
        generator.includes('sha256sum --check --strict binaries.sha256'),
    ),
    check(
      'release-documentation-covers-sbom',
      readme.includes('ops/supply-chain.lock.env') &&
        readme.includes('ops/generate-sbom.sh') &&
        checklist.includes('jellyrin-image.spdx.json') &&
      checklist.includes('SHA256SUMS'),
    ),
    check(
      'release-documentation-covers-web-build-and-preflight',
      supplyChainDocumentation.includes('ops/build-jellyfin-web.sh') &&
        supplyChainDocumentation.includes('official minimal PR #7617') &&
        supplyChainDocumentation.includes('npm ci --omit=optional') &&
        supplyChainDocumentation.includes('slideshow and comics') &&
        readme.includes('ops/build-jellyfin-web.sh ./web') &&
        readme.includes('Swiper 12.1.2') &&
        readme.includes('ops/deployment-preflight.sh') &&
        checklist.includes('ops/build-jellyfin-web.sh') &&
        checklist.includes('slideshow and comics') &&
        checklist.includes('--omit=optional') &&
        checklist.includes('ops/deployment-preflight.sh'),
    ),
    check(
      'release-documentation-covers-vulnerability-policy',
      supplyChainDocumentation.includes('ops/scan-vulnerabilities.sh') &&
        supplyChainDocumentation.includes('ops/vulnerability-exceptions.json') &&
        supplyChainDocumentation.includes('30 days') &&
        checklist.includes('cargo-audit.json') &&
        checklist.includes('trivy-image.json'),
    ),
  ];

  const failed = checks.filter((item) => item.status === 'failed');
  const result = {
    status: failed.length === 0 ? 'passed' : 'failed',
    summary: { passed: checks.length - failed.length, failed: failed.length, total: checks.length },
    checks,
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

function parseLock(text) {
  const values = {};
  for (const [index, sourceLine] of text.split('\n').entries()) {
    const line = sourceLine.trim();
    if (!line || line.startsWith('#')) {
      continue;
    }
    const match = line.match(/^([A-Z][A-Z0-9_]*)=(\S+)$/);
    if (!match) {
      throw new Error(`invalid supply-chain lock line ${index + 1}`);
    }
    if (Object.hasOwn(values, match[1])) {
      throw new Error(`duplicate supply-chain lock key ${match[1]}`);
    }
    values[match[1]] = match[2];
  }
  return values;
}

function nonEmpty(value) {
  return typeof value === 'string' && value.length > 0;
}

function validateVulnerabilityExceptions(policy) {
  const errors = [];
  if (!policy || typeof policy !== 'object' || Array.isArray(policy)) {
    return ['policy must be an object'];
  }
  if (policy.schema_version !== 1) {
    errors.push('schema_version must be 1');
  }
  if (!Array.isArray(policy.exceptions)) {
    return [...errors, 'exceptions must be an array'];
  }

  const todayText = new Date().toISOString().slice(0, 10);
  const today = parseIsoDate(todayText);
  const seen = new Set();
  const allowedFields = [
    'approved_on',
    'components',
    'expires_on',
    'id',
    'owner',
    'reason',
    'scanner',
    'tracking_issue',
  ];

  for (const [index, entry] of policy.exceptions.entries()) {
    const label = `exceptions[${index}]`;
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      errors.push(`${label} must be an object`);
      continue;
    }
    const fields = Object.keys(entry).sort();
    if (fields.join('\0') !== allowedFields.join('\0')) {
      errors.push(`${label} must contain exactly: ${allowedFields.join(', ')}`);
    }
    if (!['rustsec', 'trivy'].includes(entry.scanner)) {
      errors.push(`${label}.scanner must be rustsec or trivy`);
    }
    const validId =
      entry.scanner === 'rustsec'
        ? /^RUSTSEC-\d{4}-\d{4}$/.test(entry.id || '')
        : /^[A-Za-z0-9][A-Za-z0-9._-]{5,127}$/.test(entry.id || '');
    if (!validId) {
      errors.push(`${label}.id is invalid for ${entry.scanner || 'unknown scanner'}`);
    }
    const identity = `${entry.scanner}:${entry.id}`;
    if (seen.has(identity)) {
      errors.push(`${label} duplicates ${identity}`);
    }
    seen.add(identity);

    if (
      !Array.isArray(entry.components) ||
      entry.components.length === 0 ||
      new Set(entry.components).size !== entry.components.length ||
      entry.components.some((component) => typeof component !== 'string' || component.length === 0)
    ) {
      errors.push(`${label}.components must be a non-empty unique string array`);
    } else if (
      entry.scanner === 'rustsec' &&
      entry.components.some((component) => !/^crate:[A-Za-z0-9_-]+@\S+$/.test(component))
    ) {
      errors.push(`${label}.components must use crate:<name>@<version-or-range>`);
    } else if (
      entry.scanner === 'trivy' &&
      entry.components.some((component) => !/^pkg:[a-z0-9.+-]+\/\S+$/.test(component))
    ) {
      errors.push(`${label}.components must contain exact package URLs (purls)`);
    }
    if (typeof entry.reason !== 'string' || entry.reason.trim().length < 32) {
      errors.push(`${label}.reason must explain the accepted risk (at least 32 characters)`);
    }
    if (typeof entry.owner !== 'string' || !/^@[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)?$/.test(entry.owner)) {
      errors.push(`${label}.owner must be an @user or @organization/team`);
    }
    if (typeof entry.tracking_issue !== 'string' || !/^https:\/\/\S+$/.test(entry.tracking_issue)) {
      errors.push(`${label}.tracking_issue must be an HTTPS URL`);
    }

    const approved = parseIsoDate(entry.approved_on);
    const expires = parseIsoDate(entry.expires_on);
    if (!approved) {
      errors.push(`${label}.approved_on must be YYYY-MM-DD`);
    }
    if (!expires) {
      errors.push(`${label}.expires_on must be YYYY-MM-DD`);
    }
    if (approved && expires) {
      const lifetimeDays = (expires.getTime() - approved.getTime()) / 86_400_000;
      if (lifetimeDays <= 0 || lifetimeDays > 30) {
        errors.push(`${label} must expire 1 to 30 days after approval`);
      }
      if (approved > today) {
        errors.push(`${label}.approved_on cannot be in the future`);
      }
      if (expires <= today) {
        errors.push(`${label} is expired`);
      }
    }
  }
  return errors;
}

function validateExceptionValidator() {
  const approved = new Date();
  const expires = new Date(approved.getTime() + 14 * 86_400_000);
  const validEntry = {
    scanner: 'trivy',
    id: 'CVE-2099-0001',
    components: ['pkg:deb/debian/example@1.0.0'],
    reason: 'The affected code path is disabled while the tracked upgrade is prepared.',
    owner: '@jellyrin/security',
    tracking_issue: 'https://github.com/alseif0x/jellyrin/issues/1',
    approved_on: approved.toISOString().slice(0, 10),
    expires_on: expires.toISOString().slice(0, 10),
  };
  const validPolicy = { schema_version: 1, exceptions: [validEntry] };
  const expiredPolicy = {
    schema_version: 1,
    exceptions: [{ ...validEntry, expires_on: validEntry.approved_on }],
  };
  return (
    validateVulnerabilityExceptions(validPolicy).length === 0 &&
    validateVulnerabilityExceptions(expiredPolicy).some((error) => error.includes('expired'))
  );
}

function parseIsoDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return null;
  }
  const date = new Date(`${value}T00:00:00.000Z`);
  return Number.isNaN(date.getTime()) || date.toISOString().slice(0, 10) !== value ? null : date;
}

function check(id, passed, detail = '') {
  const result = { id, status: passed ? 'passed' : 'failed' };
  if (!passed && detail) {
    result.detail = detail;
  }
  return result;
}

function read(relativePath) {
  return fs.readFile(path.join(repoRoot, relativePath), 'utf8');
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
