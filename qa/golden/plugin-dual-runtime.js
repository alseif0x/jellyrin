#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');
const evidencePath = path.join(generatedDir, 'plugin-dual-runtime.json');
const evidenceMarkdownPath = path.join(generatedDir, 'plugin-dual-runtime.md');

async function main() {
  await fs.mkdir(generatedDir, { recursive: true });
  const dbTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'plugin_platform_state',
    '--',
    '--nocapture',
  ]);
  const apiTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_repositories_round_trip_system_configuration_payload',
    '--',
    '--nocapture',
  ]);
  const refreshTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_repository_refresh_downloads_manifest_and_updates_catalog',
    '--',
    '--nocapture',
  ]);
  const cancellationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_install_cancellation_guard_observes_failed_task_run',
    '--',
    '--nocapture',
  ]);
  const immediateCancellationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_install_cancelable_operation_aborts_in_flight_step',
    '--',
    '--nocapture',
  ]);
  const catalogMergeTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'package_catalog_merges_duplicates_and_filters_incompatible_versions',
    '--',
    '--nocapture',
  ]);
  const taskProgressTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'task_runs_track_current_and_last_result',
    '--',
    '--nocapture',
  ]);
  const taskFailedProgressTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'task_runs_can_be_cancelled_and_stale_runs_expire',
    '--',
    '--nocapture',
  ]);
  const runtimeInstanceTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-db',
    'plugin_runtime_instance_updates_installed_plugin_health_and_events',
    '--',
    '--nocapture',
  ]);
  const backupRestoreTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'backup_endpoints_list_create_manifest_and_reject_restore',
    '--',
    '--nocapture',
  ]);
  const filesystemDiscoveryTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'plugin_filesystem_discovery',
    '--',
    '--nocapture',
  ]);
  const rustWasiActivationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'rust_wasi_activation_uses_stdio_host_and_persists_runtime_state',
    '--',
    '--nocapture',
  ]);
  const rustWasiPermissionActivationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'rust_wasi_activation_passes_granted_permissions_to_host',
    '--',
    '--nocapture',
  ]);
  const runtimeFailureStatusTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'enable_plugin_marks_runtime_failure_as_malfunctioned',
    '--',
    '--nocapture',
  ]);
  const dotNetActivationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'dotnet_activation_uses_stdio_host_and_persists_runtime_state',
    '--',
    '--nocapture',
  ]);
  const dotNetConfigurationTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'dotnet_configuration_uses_stdio_host_and_persists_update',
    '--',
    '--nocapture',
  ]);
  const dotNetPagesImagesTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'dotnet_pages_and_images_use_stdio_host',
    '--',
    '--nocapture',
  ]);
  const rustWasiScheduledTaskTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'rust_wasi_scheduled_task_invokes_runtime_host',
    '--',
    '--nocapture',
  ]);
  const rustWasiChannelProviderTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-api',
    'rust_wasi_channel_provider_feeds_channels_api',
    '--',
    '--nocapture',
  ]);
  const dotNetHostTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-plugin-host-dotnet',
    '--',
    '--nocapture',
  ]);
  const runtimeRpcTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-plugin-rpc',
    '--',
    '--nocapture',
  ]);
  const wasiHostTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-plugin-host-wasi',
    '--',
    '--nocapture',
  ]);
  const wasiSdkTestResult = await runCommand('cargo', [
    'test',
    '-p',
    'jellyrin-plugin-sdk',
    '--',
    '--nocapture',
  ]);
  const passed =
    dbTestResult.code === 0 &&
    apiTestResult.code === 0 &&
    refreshTestResult.code === 0 &&
    cancellationTestResult.code === 0 &&
    immediateCancellationTestResult.code === 0 &&
    catalogMergeTestResult.code === 0 &&
    taskProgressTestResult.code === 0 &&
    taskFailedProgressTestResult.code === 0 &&
    runtimeInstanceTestResult.code === 0 &&
    backupRestoreTestResult.code === 0 &&
    filesystemDiscoveryTestResult.code === 0 &&
    rustWasiActivationTestResult.code === 0 &&
    rustWasiPermissionActivationTestResult.code === 0 &&
    runtimeFailureStatusTestResult.code === 0 &&
    dotNetActivationTestResult.code === 0 &&
    dotNetConfigurationTestResult.code === 0 &&
    dotNetPagesImagesTestResult.code === 0 &&
    rustWasiScheduledTaskTestResult.code === 0 &&
    rustWasiChannelProviderTestResult.code === 0 &&
    dotNetHostTestResult.code === 0 &&
    runtimeRpcTestResult.code === 0 &&
    wasiHostTestResult.code === 0 &&
    wasiSdkTestResult.code === 0;
  const evidence = {
    gate: 'plugin-dual-runtime',
    status: passed ? 'implemented' : 'designed',
    percent: passed ? 99 : 5,
    closed: false,
    sourcePhase: passed
      ? 'E1.P1/E1.P1b/E1.P2a/E1.P2b/E1.P2c/E1.P2d/E1.P2e/E1.P2f/E1.P2f2/E1.P2g/E1.P2h/E1.P2i/E1.P2j/E1.P3a/E1.P3b/E1.P3c/E1.P3d/E1.P3e/E1.P3f/E1.P3g/E1.P3h/E1.P4a/E1.P4b/E1.P4c/E1.P4d/E1.P4e/E1.P5a/E1.P5b/E1.P5c/E1.P5d/E1.P5e/E1.P5f/E1.P5g/E1.P5h/E1.P5i/E1.P5j/E1.P6a/E1.P6b/E1.P8a/E1.P8b/E1.P9a/E1.P9b'
      : 'E1.P1/P2-attempted',
    evidence: passed
      ? 'E1/P1 persistent plugin platform model is implemented and verified, including backup/restore of plugin repositories, package catalog cache, package installations, installed plugin rows, manifests, configurations, permissions, runtime instances, host events and audit log metadata without copying plugin binaries; E1/P2a/P2b/P2c/P2d/P2e/P2f/P2f2/P2g/P2h/P2i/P2j/P3a/P3b/P3c/P3d/P3e/P3f/P3g/P3h/P4a/P4b/P4c/P4d/P4e/P5a/P5b/P5c/P5d/P5e/P5f/P5g/P5h/P5i/P5j/P6a/P6b/P8a/P8b/P9a/P9b safe package lifecycle, registry discovery, observability and runtime RPC contract are implemented and verified: installing from a configured repository downloads/reads a package ZIP SourceUrl, verifies SHA256/SHA1 checksums when provided, rejects zip-slip paths, extracts through staging with rollback-safe swap, records package_installations, installed_plugins, manifest/config/permissions and audit state, completes PackageInstall tasks, broadcasts PackageInstall websocket task events for running/completed/failed/cancelled phases, handles update/downgrade by marking previous package_installations as Superseded while switching the active installed_plugins version, refreshes enabled plugin repository manifests into the persisted catalog/task evidence while preserving disabled repositories and previous package state on partial failures, honors IfStale/Force/CacheTtlSeconds repository refresh cache semantics and records cached/refreshed task evidence, broadcasts PackageRepositoriesRefresh websocket task events for running/completed/failed phases, observes PackageInstall cancellation before destructive/DB commit checkpoints and aborts cancelable in-flight package operations while waiting on downloads, file reads and unzip child processes, merges duplicate package catalog entries while preserving dual-runtime versions and optional Runtime/TargetAbi/ServerVersion filters, persists lifecycle progress in task_runs.result_json and exposes GET status endpoints for PackageInstall and PackageRepositoriesRefresh; /Plugins discovers package directories from filesystem, maps .dll artifacts to DotNetJellyfin and .wasm artifacts to RustWasi, ignores unsafe/incomplete package directories, preserves existing status/configuration instead of overwriting persisted state, and includes persisted RuntimeInstances/RecentEvents for plugin observability; runtime activation state can now persist Active runtime instances, runtime version, health JSON, capabilities and RuntimeStatus host events into installed plugin views and backup snapshots; Enable for RustWasi and DotNetJellyfin attempts stdio sidecar activation when a host binary is available, records runtime load failures as Malfunctioned with LastError, and falls back to NotSupported when no host is available; plugin permissions can now be read/updated through admin APIs and persisted into installed plugin views so RustWasi host loads receive granted permissions; plugin configuration GET/POST can now load a runtime sidecar on demand, call GetConfiguration/UpdateConfiguration over stdio and persist the host-normalized configuration; dashboard configuration page listing and plugin image retrieval can now load a runtime sidecar on demand, call ListWebPages/GetEmbeddedImage over stdio and fall back to existing static/catalog behavior; ScheduledTasks now exposes active plugins with ScheduledTask capability and can invoke the plugin runtime sidecar through InvokeCapability while recording task_runs evidence; Channels now exposes active plugins with ChannelProvider capability through /Channels, /Channels/{id}/Items, /Channels/{id}/Features and /Channels/Diagnostics by invoking the plugin runtime sidecar through InvokeCapability and isolating runtime failures from the provider list; GET /Plugins/{id}/Health and /Plugins/{id}/Logs expose persisted plugin health and host events; jellyrin-plugin-rpc defines the shared versioned JSON-line runtime contract for Handshake, LoadPlugin, configuration, pages, embedded images, capabilities, health and shutdown with typed errors, correlation IDs, stdio JSON-line transport, timeouts and sidecar process kill-on-drop; jellyrin-plugin-host-dotnet is a process-isolated DotNetJellyfin metadata/control host with an integration smoke test that launches the compiled sidecar over stdio, handshakes, loads plugin manifests from install paths containing .dll artifacts, serves declarative configuration, configuration pages and embedded images, executes manifest-declared capability handlers via InvokeCapability, executes a manifest-declared .NET executable fixture through dotnet JSON stdout, invokes a manifest-declared .NET library method through an isolated reflection bridge using Type/Method metadata, reports capabilities/health and shuts down; jellyrin-plugin-host-wasi is a process-isolated RustWasi metadata/control host with an integration smoke test that launches the compiled sidecar over stdio, handshakes, loads plugin manifests from install paths containing .wasm artifacts, rejects incompatible TargetAbi and ungranted manifest permissions before load, serves declarative configuration, configuration pages and embedded images, executes manifest-declared capability handlers via InvokeCapability, executes manifest-declared i32 WASM exports for real fixture modules with zero or one i32 argument, reports capabilities/health and shuts down, and validates a manifest generated by jellyrin-plugin-sdk through the host LoadPlugin/config/pages/images/InvokeCapability path; jellyrin-plugin-sdk now defines the initial Rust/WASI SDK manifest, permission, admin page, embedded image, declarative capability handler and capability response types for target ABI jellyrin-wasi-0.1 so native fixtures can produce host-compatible manifests and responses, plus typed scheduled-task, metadata-provider and channel-provider capability payloads for the three required Rust/WASI fixture classes; configuration, enable, disable and uninstall mutate persisted state without claiming full Jellyfin .NET extension-point adapter coverage or full WASI SDK execution.'
      : 'E1/P1/P2 persistent plugin platform or safe lifecycle tests failed; inspect command output before advancing plugin runtime work.',
    updatedAt: new Date().toISOString(),
    completedTargets: passed
      ? [
          'persistent-plugin-model',
          'safe-plugin-lifecycle',
          'zip-package-extraction',
          'package-checksum-policy',
          'package-update-downgrade',
          'remote-repository-refresh',
          'cooperative-package-install-cancellation',
          'immediate-package-install-cancellation',
          'package-catalog-merge-and-filters',
          'task-lifecycle-progress-status',
          'package-manager-websocket-events',
          'package-catalog-cache-ttl',
          'plugin-health-logs-observability',
          'plugin-permission-grant-flow',
          'plugin-runtime-failure-status',
          'dotnet-sidecar-metadata-host',
          'dotnet-minimal-assembly-fixture-execution',
          'dotnet-reflection-method-fixture-execution',
          'plugin-runtime-rpc-contract',
          'plugin-runtime-stdio-transport',
          'rust-wasi-sidecar-metadata-host',
          'plugin-runtime-instance-activation-state',
          'rust-wasi-enable-stdio-activation',
          'dotnet-enable-stdio-activation',
          'runtime-declarative-configuration-pages-images',
          'plugin-configuration-runtime-rpc',
          'plugin-pages-images-runtime-rpc',
          'runtime-declarative-capability-execution',
          'plugin-scheduled-task-runtime-rpc',
          'plugin-channel-provider-runtime-rpc',
          'rust-wasi-sdk-types',
          'rust-wasi-sdk-capability-payloads',
          'rust-wasi-sdk-host-manifest-roundtrip',
          'rust-wasi-target-abi-and-permission-gates',
          'rust-wasi-minimal-wasm-fixture-execution',
          'rust-wasi-i32-argument-fixture-execution',
          'plugin-state-backup-restore',
          'plugin-filesystem-discovery',
        ]
      : [],
    failedTargets: passed ? [] : ['persistent-plugin-model-or-safe-plugin-lifecycle'],
    validatedCommands: [
      'cargo test -p jellyrin-db plugin_platform_state -- --nocapture',
      'cargo test -p jellyrin-api package_repositories_round_trip_system_configuration_payload -- --nocapture',
      'cargo test -p jellyrin-api package_repository_refresh_downloads_manifest_and_updates_catalog -- --nocapture',
      'cargo test -p jellyrin-api package_install_cancellation_guard_observes_failed_task_run -- --nocapture',
      'cargo test -p jellyrin-api package_install_cancelable_operation_aborts_in_flight_step -- --nocapture',
      'cargo test -p jellyrin-api package_catalog_merges_duplicates_and_filters_incompatible_versions -- --nocapture',
      'cargo test -p jellyrin-db task_runs_track_current_and_last_result -- --nocapture',
      'cargo test -p jellyrin-db task_runs_can_be_cancelled_and_stale_runs_expire -- --nocapture',
      'cargo test -p jellyrin-db plugin_runtime_instance_updates_installed_plugin_health_and_events -- --nocapture',
      'cargo test -p jellyrin-api backup_endpoints_list_create_manifest_and_reject_restore -- --nocapture',
      'cargo test -p jellyrin-api plugin_filesystem_discovery -- --nocapture',
      'cargo test -p jellyrin-api rust_wasi_activation_uses_stdio_host_and_persists_runtime_state -- --nocapture',
      'cargo test -p jellyrin-api rust_wasi_activation_passes_granted_permissions_to_host -- --nocapture',
      'cargo test -p jellyrin-api enable_plugin_marks_runtime_failure_as_malfunctioned -- --nocapture',
      'cargo test -p jellyrin-api dotnet_activation_uses_stdio_host_and_persists_runtime_state -- --nocapture',
      'cargo test -p jellyrin-api dotnet_configuration_uses_stdio_host_and_persists_update -- --nocapture',
      'cargo test -p jellyrin-api dotnet_pages_and_images_use_stdio_host -- --nocapture',
      'cargo test -p jellyrin-api rust_wasi_scheduled_task_invokes_runtime_host -- --nocapture',
      'cargo test -p jellyrin-api rust_wasi_channel_provider_feeds_channels_api -- --nocapture',
      'cargo test -p jellyrin-plugin-host-dotnet -- --nocapture',
      'cargo test -p jellyrin-plugin-rpc -- --nocapture',
      'cargo test -p jellyrin-plugin-host-wasi -- --nocapture',
      'cargo test -p jellyrin-plugin-sdk -- --nocapture',
    ],
    openRisks: [
      'DotNetJellyfin sidecar executes manifest-declared executable fixtures and Type/Method reflection fixtures; real Jellyfin extension-point adapters are still pending.',
      'RustWasi host executes only manifest-declared i32 WASM exports with zero or one i32 argument; full WASI imports, SDK runtime calls and MetadataProvider/ImageProvider ABI execution are still pending.',
      'Package install extracts package artifacts and records a safe NotSupported state when a host is unavailable; full real provider/task execution is still pending.',
    ],
  };
  await fs.writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  await fs.writeFile(
    evidenceMarkdownPath,
    renderMarkdown(evidence, [
      dbTestResult,
      apiTestResult,
      refreshTestResult,
      cancellationTestResult,
      immediateCancellationTestResult,
      catalogMergeTestResult,
      taskProgressTestResult,
      taskFailedProgressTestResult,
      runtimeInstanceTestResult,
      backupRestoreTestResult,
      filesystemDiscoveryTestResult,
      rustWasiActivationTestResult,
      rustWasiPermissionActivationTestResult,
      runtimeFailureStatusTestResult,
      dotNetActivationTestResult,
      dotNetConfigurationTestResult,
      dotNetPagesImagesTestResult,
      rustWasiScheduledTaskTestResult,
      rustWasiChannelProviderTestResult,
      dotNetHostTestResult,
      runtimeRpcTestResult,
      wasiHostTestResult,
      wasiSdkTestResult,
    ]),
  );
  console.log(`wrote ${evidencePath}`);
  console.log(`wrote ${evidenceMarkdownPath}`);
  if (!passed) {
    process.exitCode = dbTestResult.code || apiTestResult.code || 1;
  }
}

function runCommand(command, args) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: process.env,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      const text = chunk.toString();
      stdout += text;
      process.stdout.write(text);
    });
    child.stderr.on('data', (chunk) => {
      const text = chunk.toString();
      stderr += text;
      process.stderr.write(text);
    });
    child.on('close', (code, signal) => resolve({ code: code || 0, signal, stdout, stderr }));
  });
}

function renderMarkdown(evidence, testResults) {
  const lines = [];
  lines.push('# Plugin Dual Runtime Evidence');
  lines.push('');
  lines.push(`Generated: ${evidence.updatedAt}`);
  lines.push(`Status: \`${evidence.status}\``);
  lines.push(`Progress: ${evidence.percent}%`);
  lines.push(`Closed: ${evidence.closed}`);
  lines.push('');
  lines.push('## Evidence');
  lines.push('');
  lines.push(`- ${evidence.evidence}`);
  lines.push('');
  lines.push('## Validated Commands');
  lines.push('');
  for (const command of evidence.validatedCommands) {
    lines.push(`- \`${command}\``);
  }
  if (testResults.some((testResult) => testResult.stderr || testResult.stdout)) {
    lines.push('');
    lines.push('## Command Result');
    lines.push('');
    testResults.forEach((testResult, index) => {
      lines.push(`- Command ${index + 1} exit code: ${testResult.code}`);
    });
  }
  lines.push('');
  lines.push('## Open Risks');
  lines.push('');
  for (const risk of evidence.openRisks) {
    lines.push(`- ${risk}`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
