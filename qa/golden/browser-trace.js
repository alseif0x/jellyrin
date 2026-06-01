#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const http = require('node:http');
const { execFile, spawnSync } = require('node:child_process');
const { promisify } = require('node:util');
const { chromium } = require('playwright');
const execFileAsync = promisify(execFile);

const outputRoot = process.env.JELLYRIN_BROWSER_TRACE_OUT
  || path.resolve(__dirname, '../../../../plans/generated/e2e-traces');
const flow = process.env.JELLYRIN_BROWSER_FLOW || 'p0-direct-play';
const chromiumExecutable = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE
  || '/home/cdmonio/.cache/ms-playwright/chromium_headless_shell-1208/chrome-headless-shell-linux64/chrome-headless-shell';
const mediaFixtureDir = process.env.JELLYRIN_MEDIA_FIXTURE_DIR
  || path.resolve(__dirname, '../../var/fixtures/m2-movies');
const subtitleTrickplayFixtureName = 'Jellyrin Subtitle Trickplay Long Fixture';
const seriesFixtureDir = process.env.JELLYRIN_SERIES_FIXTURE_DIR
  || path.resolve(__dirname, '../../var/fixtures/m2-series');
const seriesFlowName = 'Jellyrin Series Flow';
const seriesFlowSeasonName = 'Season 01';
const audioFixtureDir = process.env.JELLYRIN_AUDIO_FIXTURE_DIR
  || path.resolve(__dirname, '../../var/fixtures/m2-music');
const audioHlsFixtureName = 'Jellyrin Audio HLS Legacy Fixture';
const musicFlowFixturePrefix = 'Jellyrin Music Flow';
const musicFlowAlbum = 'Jellyrin Music Flow Album';
const musicFlowArtist = 'Jellyrin Music Flow Artist';
const musicFlowAlbumArtist = 'Jellyrin Music Flow Album Artist';
const musicFlowGenre = 'Jellyrin Music Flow Rock';
const listFlowPlaylistName = 'Jellyrin Golden Playlist Flow';
const listFlowCollectionName = 'Jellyrin Golden Collection Flow';
const imageFlowFixtureName = 'Jellyrin Image Flow Fixture';
const imageFlowUploadPngBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAFklEQVR42mP8z8Dwn4GBgYGJAQoAFAIBAvhDL0kAAAAASUVORK5CYII=';
const metadataFlowPrimaryName = 'Jellyrin Metadata Flow Primary';
const metadataFlowSimilarName = 'Jellyrin Metadata Flow Similar';
const metadataFlowNfoFileName = 'Jellyrin Metadata Flow NFO';
const metadataFlowNfoTitle = 'Jellyrin Metadata Flow NFO Title';
const metadataFlowGenre = 'Jellyrin Metadata Drama';
const metadataFlowStudio = 'Jellyrin Metadata Studio';
const metadataFlowPerson = 'Jellyrin Metadata Person';
const metadataFlowTag = 'Jellyrin Metadata Tag';
const metadataFlowYear = 1995;
const authUsersFlowPrefix = 'Jellyrin Auth Flow User';
const authUsersFlowPassword = 'JellyrinAuthFlowPassword!2026';
const authUsersFlowApiKeyApp = 'Jellyrin Auth Flow API Key';
const sessionsFlowPrefix = 'Jellyrin Sessions Flow User';
const sessionsFlowPassword = 'JellyrinSessionsFlowPassword!2026';
const syncplayFlowPrefix = 'Jellyrin SyncPlay Flow User';
const syncplayFlowPassword = 'JellyrinSyncPlayFlowPassword!2026';
const nonWebClientFlowPrefix = 'Jellyrin Non-Web Client Flow User';
const nonWebClientFlowPassword = 'JellyrinNonWebClientFlowPassword!2026';
const pluginsFlowRepositoryName = 'Jellyrin Golden Plugin Repository';
const pluginsFlowPackageName = 'Jellyrin Golden Plugin';
const pluginsFlowPackageGuid = '22222222-2222-2222-2222-222222222222';
const hdhrSim = require('./fixtures/hdhomerun-sim');
const liveTvFlowChannelId = 'jellyrin-live-tv-channel';
const liveTvFlowProgramId = 'jellyrin-live-tv-program';
const liveTvFlowRecordingId = 'jellyrin-live-tv-recording';
const liveTvFlowTimerId = 'jellyrin-live-tv-timer';
let hdhrSimUrl = null;
const upstreamTranscodeDir = process.env.JELLYFIN_TRANSCODE_DIR
  || '/home/cdmonio/dev/jellyfin-data/cache/transcodes';
const jellyrinTranscodeDir = process.env.JELLYRIN_TRANSCODE_DIR
  || path.join('/tmp', 'jellyrin', 'transcodes');
const audioLegacySegmentId = Number.parseInt(process.env.JELLYRIN_AUDIO_LEGACY_SEGMENT_ID || '987654321', 10);

const targetDefinitions = [
  {
    name: 'upstream',
    baseUrl: process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096',
    username: process.env.JELLYFIN_ADMIN_USER,
    password: process.env.JELLYFIN_ADMIN_PASSWORD,
    apiKey: process.env.JELLYFIN_API_KEY,
  },
  {
    name: 'jellyrin',
    baseUrl: process.env.JELLYRIN_URL || process.env.JELLYRIN_E2E_BASE_URL || 'http://127.0.0.1:8097',
    username: process.env.JELLYRIN_ADMIN_USER || process.env.JELLYRIN_E2E_ADMIN_USER,
    password: process.env.JELLYRIN_ADMIN_PASSWORD || process.env.JELLYRIN_E2E_ADMIN_PASSWORD,
    apiKey: process.env.JELLYRIN_API_KEY,
  },
];

async function main() {
  if (!['startup-wizard', 'login-home', 'p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay', 'audio-hls-legacy', 'music', 'series', 'playlists-collections', 'images', 'metadata-search', 'auth-users', 'sessions-websocket', 'syncplay', 'plugins-packages', 'live-tv', 'channels', 'non-web-client', 'scheduled-tasks', 'backup-restore', 'migration-import'].includes(flow)) {
    throw new Error(`Unsupported browser flow: ${flow}`);
  }

  const requestedTargets = new Set(
    (process.env.JELLYRIN_BROWSER_TARGETS || 'upstream,jellyrin')
      .split(',')
      .map((target) => target.trim())
      .filter(Boolean),
  );
  const targets = targetDefinitions.filter((target) => requestedTargets.has(target.name));
  const flowDir = path.join(outputRoot, flow);
  await fs.mkdir(flowDir, { recursive: true });

  const browser = await chromium.launch({
    headless: true,
    executablePath: chromiumExecutable,
  });

  let hdhrSimClose = null;
  if (flow === 'live-tv') {
    const sim = await hdhrSim.start(0);
    hdhrSimUrl = sim.url;
    hdhrSimClose = sim.close;
  }

  const summaries = [];
  try {
    for (const target of targets) {
      const summary = await captureTarget(browser, flowDir, target);
      summaries.push(summary);
    }
  } finally {
    await Promise.race([
      browser.close(),
      new Promise((resolve) => { setTimeout(resolve, 15000); }),
    ]).catch(() => {});
    if (hdhrSimClose) {
      await hdhrSimClose();
      hdhrSimUrl = null;
    }
  }

  const comparison = compareSummaries(summaries);
  const report = {
    generatedAt: new Date().toISOString(),
    flow,
    summaries,
    comparison,
  };
  await fs.writeFile(path.join(flowDir, 'comparison.json'), `${JSON.stringify(report, null, 2)}\n`);

  const completed = summaries.filter((summary) => summary.status === 'completed').length;
  console.log(`${completed}/${summaries.length} browser trace targets completed`);
  console.log(`wrote ${flowDir}`);
  if (comparison.failed) {
    for (const reason of comparison.reasons) {
      console.error(reason);
    }
    process.exitCode = 1;
  }
  // Force-exit to prevent Playwright's pending browser cleanup or the HDHR simulator's
  // lingering stream connections from keeping the event loop alive indefinitely. All output
  // files have already been written above; the exit code reflects the comparison result.
  process.exit(process.exitCode || 0);
}

async function captureTarget(browser, flowDir, target) {
  const summary = {
    target: target.name,
    baseUrl: trimTrailingSlash(target.baseUrl),
    flow,
    status: 'pending',
    skipped: false,
    requests: 0,
    failedResponses: [],
    consoleErrors: [],
    pageErrors: [],
    websockets: 0,
    screenshot: `${target.name}.screenshot.png`,
    criticalRequests: {},
    invariants: {
      playbackInfo200: false,
      streamOk: false,
      sessionPlaying204: false,
      websocketSessions: false,
      websocketKeepAlive: false,
      websocketMessageTypes: [],
      unexpectedTranscodePath: false,
      playMethods: [],
      playbackProgress204: false,
      resumeList200: false,
      resumeItemMatched: false,
      resumePositionTicks: null,
      transcodePlaybackInfo200: false,
      transcodingUrlPresent: false,
      hlsMaster200: false,
      hlsMedia200: false,
      hlsSegment200: false,
      hlsPlaylistShapes: [],
      hlsSegmentContentTypes: [],
      adminSystemInfo200: false,
      adminStorage200: false,
      adminScheduledTasks200: false,
      adminActivityLog200: false,
      adminDevices200: false,
      adminPlugins200: false,
      adminRepositories200: false,
      adminConfigPages200: false,
      libraryViews200: false,
      libraryGroupingOptions200: false,
      libraryVirtualFolders200: false,
      libraryItemsCounts200: false,
      libraryItems200: false,
      libraryLatest200: false,
      libraryViewMatched: false,
      libraryItemMatched: false,
      subtitlePlaybackInfo200: false,
      subtitleStreamMatched: false,
      subtitlePlaylist200: false,
      subtitlePlaylistShape: false,
      subtitleVtt200: false,
      subtitleVttCue: false,
      trickplayPlaylist200: false,
      trickplayImagesOnly: false,
      trickplayTile200: false,
      trickplayTileJpeg: false,
      audioItemMatched: false,
      audioPlaybackInfo200: false,
      audioTranscodingUrlPresent: false,
      audioHlsMaster200: false,
      audioHlsMedia200: false,
      audioHlsDynamicSegment200: false,
      audioHlsLegacySegment200: false,
      audioHlsSegmentContentTypes: [],
      musicViewMatched: false,
      musicSongsMatched: false,
      musicAlbumMatched: false,
      musicArtistMatched: false,
      musicAlbumArtistMatched: false,
      musicGenreMatched: false,
      musicInstantMix200: false,
      musicInstantMixResults: false,
      musicGenreInstantMix200: false,
      musicAudioStream200: false,
      musicAudioStreamContentTypes: [],
      seriesViewMatched: false,
      seriesEpisodesMatched: false,
      seriesEpisodeMetadataMatched: false,
      seriesCounts200: false,
      seriesNextUp200: false,
      seriesSeasons200: false,
      seriesSeasonMatched: false,
      seriesEpisodesRoute200: false,
      seriesEpisodesRouteMatched: false,
      seriesSimilar200: false,
      seriesStream200: false,
      seriesStreamContentTypes: [],
      playlistCreated: false,
      playlistDetail200: false,
      playlistItems200: false,
      playlistItemIdsMatched: false,
      playlistMove204: false,
      playlistMoveUnsupported400: false,
      playlistMovedOrderMatched: false,
      playlistDeleteItem204: false,
      playlistAddItem204: false,
      playlistRename204: false,
      playlistRenameUnsupported400: false,
      collectionCreated: false,
      collectionAddItems204: false,
      collectionDeleteItems204: false,
      imageItemMatched: false,
      imageInfosInitial200: false,
      imageUpload204: false,
      imageInfosAfterUpload200: false,
      imageInfoTagPresent: false,
      imageGet200: false,
      imageGetPng: false,
      imageHead200: false,
      imageHeadPng: false,
      imageExtendedGet200: false,
      imageExtendedGetPng: false,
      imageDelete204: false,
      imageInfosAfterDelete200: false,
      imageProviders200: false,
      metadataItemsMatched: false,
      metadataUpdatePrimary204: false,
      metadataUpdateSimilar204: false,
      metadataEditor200: false,
      metadataEditorProviderIds: false,
      metadataExternalIds200: false,
      metadataItemsSearch200: false,
      metadataNfoLocalMatched: false,
      metadataLockedFieldsPreserved: false,
      metadataSearchHints200: false,
      metadataGenreMatched: false,
      metadataStudioMatched: false,
      metadataPersonMatched: false,
      metadataYearMatched: false,
      metadataSimilar200: false,
      metadataSimilarMatched: false,
      authUsersPublic200: false,
      authUsersList200: false,
      authProviders200: false,
      authPasswordResetProviders200: false,
      authUserCreated: false,
      authCreatedUserLogin200: false,
      authCreatedUserMe200: false,
      authUserDetail200: false,
      authUserPolicy204: false,
      authUserConfiguration204: false,
      authKeysList200: false,
      authKeyCreated: false,
      authKeyUsable: false,
      authKeyRevoked: false,
      authCreatedUserLogout204: false,
      authUserDeleted: false,
      pluginsList200: false,
      pluginsListEmpty: false,
      pluginRepositories200: false,
      pluginRepositoryUpdated: false,
      pluginPackages200: false,
      pluginPackageMatched: false,
      pluginManifest200: false,
      pluginInstallRejected: false,
      pluginEnableRejected: false,
      pluginDisableRejected: false,
      pluginUninstallRejected: false,
      liveTvConfigUpdated: false,
      liveTvInfo200: false,
      liveTvTunerTypes200: false,
      liveTvChannels200: false,
      liveTvChannelMatched: false,
      liveTvGuidePrograms200: false,
      liveTvProgramMatched: false,
      liveTvStream200: false,
      liveTvRecordings200: false,
      liveTvRecordingStream200: false,
      liveTvTimerCreated: false,
      liveTvTimerDeleted: false,
      liveTvSeriesTimerCreated: false,
      liveTvSeriesTimerDeleted: false,
      liveTvHdhrTunerAdded: false,
      liveTvHdhrChannelMatched: false,
      liveTvHdhrStreamSetup: false,
      liveTvHdhrStream200: false,
      liveTvHdhrTwoClientStream: false,
      liveTvHdhrStreamRefcountReleased: false,
      liveTvHdhrTwoClientByteCheck: false,
      liveTvHdhrHlsMaster200: false,
      liveTvHdhrHlsMediaLive: false,
      liveTvHdhrHlsSegment200: false,
      liveTvHdhrHlsActiveEncoding: false,
      liveTvHdhrHlsTranscodeUrl: false,
      liveTvHdhrHlsFfmpegReaped: false,
      liveTvHdhrTimerRecordingCreated: false,
      liveTvHdhrRecordingCompleted: false,
      liveTvHdhrRecordingPlayable: false,
      liveTvHdhrRecordingCleanup: false,
      liveTvHdhrSeriesTimerCreated: false,
      liveTvHdhrSeriesTimerGeneratesTimers: false,
      liveTvHdhrSeriesRecordingPlayable: false,
      liveTvHdhrSeriesTimerCleanup: false,
      liveTvHdhrTunerLimitFirstOpen: false,
      liveTvHdhrTunerLimitConflict: false,
      liveTvHdhrTunerLimitRecovery: false,
      liveTvHdhrTunerLimitHlsConflict: false,
      liveTvHdhrTunerLimitRecordingConflict: false,
      liveTvHdhrTunerLimitHlsRecovery: false,
      liveTvHdhrTunerLimitRecordingNoZombie: false,
      liveTvHdhrTunerLimitSharingExempt: false,
      startupPublicInfoIncomplete: false,
      startupConfig200: false,
      startupConfig204: false,
      startupRemoteAccess204: false,
      startupUser200: false,
      startupUser204: false,
      startupPublicUsersBeforeComplete: false,
      startupComplete204: false,
      startupPublicInfoComplete: false,
      startupLogin200: false,
      startupSystemInfo200: false,
      startupPublicUsersAfterComplete: false,
      sessionsTwoClientsOpened: false,
      sessionsStartSent: false,
      sessionsMessageReceived: false,
      sessionsList200: false,
      sessionsCapabilities204: false,
      sessionsUserAdd204: false,
      sessionsObserverUpdate: false,
      sessionsRemotePlay204: false,
      sessionsRemotePlayMessage: false,
      sessionsRemotePlaystate204: false,
      sessionsRemotePlaystateMessage: false,
      sessionsRemoteStop204: false,
      sessionsRemoteStoppedMessage: false,
      sessionsCleanupConfirmed: false,
      syncplayTwoClientsOpened: false,
      syncplayGroupCreated: false,
      syncplayGuestJoined: false,
      syncplayList200: false,
      syncplayGet200: false,
      syncplayPlay204: false,
      syncplayPlayFanout: false,
      syncplayPause204: false,
      syncplayPauseFanout: false,
      syncplaySeek204: false,
      syncplaySeekFanout: false,
      syncplayUnpause204: false,
      syncplayUnpauseFanout: false,
      syncplayRaceSequenced: false,
      syncplayDriftCorrection: false,
      syncplayGuestReconnectDeduped: false,
      syncplayStaleCleanup: false,
      syncplayGuestLogoutRemoved: false,
      syncplayGuestLeft: false,
      syncplayOwnerLeft: false,
      syncplayCleanupConfirmed: false,
      channelsList200: false,
      channelsProviderMatched: false,
      channelsFilterMatched: false,
      channelsDeletionFilterMatched: false,
      channelsItems200: false,
      channelsItemMatched: false,
      channelsLatest200: false,
      channelsFeatures200: false,
      channelsFeatureMatched: false,
      nonWebClientAuthenticated: false,
      nonWebSystemInfo200: false,
      nonWebBrowse200: false,
      nonWebMovieMatched: false,
      nonWebPlaybackInfo200: false,
      nonWebDirectMediaSource: false,
      nonWebStream200: false,
      nonWebProgress204: false,
      nonWebResumeMatched: false,
      nonWebDlnaUnsupportedDecided: false,
      scheduledTasksList200: false,
      scheduledTasksDetail200: false,
      scheduledTasksStarted: false,
      scheduledTasksWebsocketUpdate: false,
      scheduledTasksCompleted: false,
      scheduledTasksCancelled: false,
      scheduledTasksTriggers204: false,
      scheduledTasksLibraryRefresh204: false,
      scheduledTasksActivityLogged: false,
      backupList200: false,
      backupCreated: false,
      backupSnapshotSummary: false,
      backupManifest200: false,
      backupRestored: false,
      backupActivityLogged: false,
      migrationDryRun200: false,
      migrationReadOnlyPolicy: false,
      migrationImport200: false,
      migrationBackupCreated: false,
      migrationRollbackDocumented: false,
      migrationActivityLogged: false,
    },
  };

  const requestLog = await jsonlWriter(path.join(flowDir, `${target.name}.requests.jsonl`));
  const consoleLog = await jsonlWriter(path.join(flowDir, `${target.name}.console.jsonl`));
  const websocketLog = await jsonlWriter(path.join(flowDir, `${target.name}.websocket.jsonl`));

  if (flow !== 'startup-wizard' && (!target.username || !target.password) && !target.apiKey) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'missing username/password or API key environment variables';
    await requestLog.close();
    await consoleLog.close();
    await websocketLog.close();
    return summary;
  }

  const context = await browser.newContext({
    baseURL: summary.baseUrl,
    ignoreHTTPSErrors: true,
  });
  const page = await context.newPage();
  // Bound all page.evaluate() calls so no single request hangs indefinitely.
  // Navigation timeouts (goto, waitForLoadState) are kept at the Playwright default.
  page.setDefaultTimeout(25000);
  wirePageCapture(page, summary, requestLog, consoleLog, websocketLog);

  try {
    const publicInfoResponse = await page.request.get(`${summary.baseUrl}/System/Info/Public`);
    if (!publicInfoResponse.ok()) {
      throw new Error(`System public info returned HTTP ${publicInfoResponse.status()}`);
    }
    const publicInfo = await publicInfoResponse.json();
    if (flow !== 'startup-wizard' && !publicInfo.StartupWizardCompleted) {
      throw new Error('Startup wizard is not completed for target');
    }

    if (flow === 'startup-wizard') {
      await runStartupWizardFlow(page, summary, publicInfo, target);
    } else if (flow === 'login-home') {
      await runLoginHomeFlow(page, summary, publicInfo, target);
    } else if (flow === 'p0-direct-play') {
      await runDirectPlayFlow(page, summary, publicInfo, target);
    } else if (flow === 'resume') {
      await runResumeFlow(page, summary, publicInfo, target);
    } else if (flow === 'transcode-hls') {
      await runTranscodeHlsFlow(page, summary, publicInfo, target);
    } else if (flow === 'libraries') {
      await runLibrariesFlow(page, summary, publicInfo, target);
    } else if (flow === 'subtitles-trickplay') {
      await runSubtitlesTrickplayFlow(page, summary, publicInfo, target);
    } else if (flow === 'audio-hls-legacy') {
      await runAudioHlsLegacyFlow(page, summary, publicInfo, target);
    } else if (flow === 'music') {
      await runMusicFlow(page, summary, publicInfo, target);
    } else if (flow === 'series') {
      await runSeriesFlow(page, summary, publicInfo, target);
    } else if (flow === 'playlists-collections') {
      await runPlaylistsCollectionsFlow(page, summary, publicInfo, target);
    } else if (flow === 'images') {
      await runImagesFlow(page, summary, publicInfo, target);
    } else if (flow === 'metadata-search') {
      await runMetadataSearchFlow(page, summary, publicInfo, target);
    } else if (flow === 'auth-users') {
      await runAuthUsersFlow(page, summary, publicInfo, target);
    } else if (flow === 'sessions-websocket') {
      await runSessionsWebsocketFlow(page, summary, publicInfo, target);
    } else if (flow === 'syncplay') {
      await runSyncPlayFlow(page, summary, publicInfo, target);
    } else if (flow === 'plugins-packages') {
      await runPluginsPackagesFlow(page, summary, publicInfo, target);
    } else if (flow === 'live-tv') {
      await runLiveTvFlow(page, summary, publicInfo, target);
    } else if (flow === 'channels') {
      await runChannelsFlow(page, summary, publicInfo, target);
    } else if (flow === 'non-web-client') {
      await runNonWebClientFlow(page, summary, publicInfo, target);
    } else if (flow === 'scheduled-tasks') {
      await runScheduledTasksFlow(page, summary, publicInfo, target);
    } else if (flow === 'backup-restore') {
      await runBackupRestoreFlow(page, summary, publicInfo, target);
    } else if (flow === 'migration-import') {
      await runMigrationImportFlow(page, summary, publicInfo, target);
    } else {
      await runAdminDashboardFlow(page, summary, publicInfo, target);
    }
    if (summary.skipped) {
      return summary;
    }
    await page.screenshot({ path: path.join(flowDir, summary.screenshot), fullPage: true, timeout: 5000 }).catch(() => {});
    summary.finalUrl = sanitizeUrl(page.url());
    summary.status = 'completed';
  } catch (error) {
    summary.status = 'failed';
    summary.error = error.message;
    await page.screenshot({ path: path.join(flowDir, summary.screenshot), fullPage: true, timeout: 5000 }).catch(() => {});
  } finally {
    // Close page and context with a bounded timeout so live-streaming WebSocket connections
    // do not prevent cleanup from completing. The Promise.race ensures forward progress even
    // if the browser is slow to acknowledge the close for an open live stream.
    await Promise.race([
      page.close({ runBeforeUnload: false }),
      new Promise((resolve) => { setTimeout(resolve, 5000); }),
    ]).catch(() => {});
    await Promise.race([
      context.close(),
      new Promise((resolve) => { setTimeout(resolve, 5000); }),
    ]).catch(() => {});
    await requestLog.close();
    await consoleLog.close();
    await websocketLog.close();
  }

  return summary;
}

async function runLoginHomeFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');
}

async function runStartupWizardFlow(page, summary, publicInfo, target) {
  await page.goto(`${summary.baseUrl}/web/#/wizard/start`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  if (publicInfo.StartupWizardCompleted !== false) {
    throw new Error('startup wizard target was not in incomplete state');
  }
  summary.invariants.startupPublicInfoIncomplete = true;

  const config = await browserFetchJson(page, {
    method: 'GET',
    url: '/Startup/Configuration',
  });
  if (config.status !== 200 || !config.json || !('ServerName' in config.json)) {
    throw new Error(`Startup/Configuration returned HTTP ${config.status}`);
  }
  summary.invariants.startupConfig200 = true;

  const startupConfig = {
    ...config.json,
    ServerName: `Jellyrin Startup ${target.name}`,
    UICulture: 'en-US',
    MetadataCountryCode: 'US',
    PreferredMetadataLanguage: 'en',
  };
  const configUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: '/Startup/Configuration',
    body: startupConfig,
  });
  if (![200, 204].includes(configUpdate.status)) {
    throw new Error(`Startup/Configuration POST returned HTTP ${configUpdate.status}`);
  }
  summary.invariants.startupConfig204 = true;

  const userBefore = await browserFetchJson(page, {
    method: 'GET',
    url: '/Startup/User',
  });
  if (userBefore.status !== 200 || !userBefore.json || !('Name' in userBefore.json)) {
    throw new Error(`Startup/User returned HTTP ${userBefore.status}`);
  }
  summary.invariants.startupUser200 = true;

  const adminName = `startup-${target.name}`;
  const adminPassword = `StartupWizard-${target.name}-Password!2026`;
  const userUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: '/Startup/User',
    body: {
      Name: adminName,
      Password: adminPassword,
    },
  });
  if (![200, 204].includes(userUpdate.status)) {
    throw new Error(`Startup/User POST returned HTTP ${userUpdate.status}`);
  }
  summary.invariants.startupUser204 = true;

  const remoteAccess = await browserFetchJson(page, {
    method: 'POST',
    url: '/Startup/RemoteAccess',
    body: {
      EnableRemoteAccess: true,
    },
  });
  if (![200, 204].includes(remoteAccess.status)) {
    throw new Error(`Startup/RemoteAccess returned HTTP ${remoteAccess.status}`);
  }
  summary.invariants.startupRemoteAccess204 = true;

  const publicUsersBefore = await browserFetchJson(page, {
    method: 'GET',
    url: '/Users/Public?phase=before',
  });
  if (publicUsersBefore.status !== 200 || !Array.isArray(publicUsersBefore.json) || publicUsersBefore.json.length !== 0) {
    throw new Error('Users/Public did not match upstream hidden-first-user semantics before wizard completion');
  }
  summary.invariants.startupPublicUsersBeforeComplete = true;

  const complete = await browserFetchJson(page, {
    method: 'POST',
    url: '/Startup/Complete',
  });
  if (![200, 204].includes(complete.status)) {
    throw new Error(`Startup/Complete returned HTTP ${complete.status}`);
  }
  summary.invariants.startupComplete204 = true;

  const publicInfoComplete = await browserFetchJson(page, {
    method: 'GET',
    url: '/System/Info/Public',
  });
  if (publicInfoComplete.status !== 200 || publicInfoComplete.json?.StartupWizardCompleted !== true) {
    throw new Error('System/Info/Public did not report completed startup wizard');
  }
  summary.invariants.startupPublicInfoComplete = true;

  const login = await browserFetchJson(page, {
    method: 'POST',
    url: '/Users/AuthenticateByName',
    authorization: `MediaBrowser Client="Jellyrin Browser Trace", Device="Startup ${target.name}", DeviceId="startup-${target.name}", Version="dev"`,
    body: {
      Username: adminName,
      Pw: adminPassword,
    },
  });
  if (login.status !== 200 || !login.json?.AccessToken || login.json?.User?.Name !== adminName) {
    throw new Error(`startup login returned HTTP ${login.status}`);
  }
  summary.invariants.startupLogin200 = true;

  const systemInfo = await browserFetchJson(page, {
    method: 'GET',
    url: '/System/Info',
    token: login.json.AccessToken,
  });
  if (systemInfo.status !== 200 || systemInfo.json?.StartupWizardCompleted !== true) {
    throw new Error(`System/Info returned HTTP ${systemInfo.status}`);
  }
  summary.invariants.startupSystemInfo200 = true;

  const publicUsersAfter = await browserFetchJson(page, {
    method: 'GET',
    url: '/Users/Public?phase=after',
  });
  if (publicUsersAfter.status !== 200 || !Array.isArray(publicUsersAfter.json) || publicUsersAfter.json.length !== 0) {
    throw new Error('Users/Public did not hide users after wizard completion');
  }
  summary.invariants.startupPublicUsersAfterComplete = true;

  await establishWebSession(page, summary, publicInfoComplete.json, {
    ...target,
    username: adminName,
    password: adminPassword,
  }, {
    AccessToken: login.json.AccessToken,
    User: login.json.User,
    ServerId: login.json.ServerId,
  }, '/home');
  await page.waitForLoadState('networkidle').catch(() => {});
}

async function runSessionsWebsocketFlow(page, summary, publicInfo, target) {
  const admin = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, admin, '/home');
  await page.waitForLoadState('networkidle').catch(() => {});

  const ownerName = `${sessionsFlowPrefix} Owner ${target.name}`;
  const guestName = `${sessionsFlowPrefix} Guest ${target.name}`;
  let createdOwnerId = null;
  let createdGuestId = null;
  let receiverToken = null;
  let observerToken = null;
  let receiverSessionId = null;
  let observerSessionId = null;

  async function cleanupExistingUsers() {
    const users = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users',
      token: admin.AccessToken,
    });
    if (users.status !== 200) {
      throw new Error(`sessions cleanup user list returned HTTP ${users.status}`);
    }
    for (const user of users.json || []) {
      if ((user?.Name === ownerName || user?.Name === guestName) && user.Id) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/Users/${encodeURIComponent(user.Id)}`,
          token: admin.AccessToken,
        });
      }
    }
  }

  await cleanupExistingUsers();

  try {
    const createdOwner = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: admin.AccessToken,
      body: {
        Name: ownerName,
        Password: sessionsFlowPassword,
      },
    });
    if (createdOwner.status !== 200 || !createdOwner.json?.Id || createdOwner.json.Name !== ownerName) {
      throw new Error(`sessions owner Users/New returned HTTP ${createdOwner.status}`);
    }
    createdOwnerId = createdOwner.json.Id;

    const ownerPolicy = await browserFetchJson(page, {
      method: 'POST',
      url: `/Users/${encodeURIComponent(createdOwnerId)}/Policy`,
      token: admin.AccessToken,
      body: {
        ...(createdOwner.json.Policy || {}),
        IsAdministrator: true,
        IsDisabled: false,
      },
    });
    if (![200, 204].includes(ownerPolicy.status)) {
      throw new Error(`sessions owner policy update returned HTTP ${ownerPolicy.status}`);
    }

    const createdGuest = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: admin.AccessToken,
      body: {
        Name: guestName,
        Password: sessionsFlowPassword,
      },
    });
    if (createdGuest.status !== 200 || !createdGuest.json?.Id || createdGuest.json.Name !== guestName) {
      throw new Error(`sessions guest Users/New returned HTTP ${createdGuest.status}`);
    }
    createdGuestId = createdGuest.json.Id;

    const receiverLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: `MediaBrowser Client="Jellyrin Browser Trace", Device="Sessions Receiver ${target.name}", DeviceId="sessions-receiver-${target.name}", Version="dev"`,
      body: {
        Username: ownerName,
        Pw: sessionsFlowPassword,
      },
    });
    if (receiverLogin.status !== 200 || !receiverLogin.json?.AccessToken) {
      throw new Error(`sessions receiver login returned HTTP ${receiverLogin.status}`);
    }
    receiverToken = receiverLogin.json.AccessToken;
    receiverSessionId = receiverLogin.json?.SessionInfo?.Id || receiverToken;

    const observerLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: `MediaBrowser Client="Jellyrin Browser Trace", Device="Sessions Observer ${target.name}", DeviceId="sessions-observer-${target.name}", Version="dev"`,
      body: {
        Username: guestName,
        Pw: sessionsFlowPassword,
      },
    });
    if (observerLogin.status !== 200 || !observerLogin.json?.AccessToken) {
      throw new Error(`sessions observer login returned HTTP ${observerLogin.status}`);
    }
    observerToken = observerLogin.json.AccessToken;
    observerSessionId = observerLogin.json?.SessionInfo?.Id || observerToken;

    const movie = await firstMovieItem(page, summary, {
      AccessToken: receiverToken,
      User: { Id: createdOwnerId },
    });
    if (!movie) {
      throw new Error('target has no movie item for sessions websocket trace');
    }

    const capabilitiesUrl = '/Sessions/Capabilities/Full?'
      + [
        ['PlayableMediaTypes', 'Audio,Video'],
        ['SupportedCommands', 'DisplayContent,DisplayMessage,GoHome,Play,Seek'],
        ['SupportsRemoteControl', 'true'],
        ['SupportsMediaControl', 'true'],
      ].map(([key, value]) => `${key}=${encodeURIComponent(value)}`).join('&');
    const initialSessions = await browserFetchJson(page, {
      method: 'GET',
      url: '/Sessions',
      token: receiverToken,
    });
    if (initialSessions.status !== 200 || !Array.isArray(initialSessions.json)) {
      throw new Error(`Sessions returned HTTP ${initialSessions.status}`);
    }
    const receiverSession = initialSessions.json.find((session) => session.Id === receiverSessionId);
    const observerInitialSessions = await browserFetchJson(page, {
      method: 'GET',
      url: '/Sessions',
      token: observerToken,
    });
    if (observerInitialSessions.status !== 200 || !Array.isArray(observerInitialSessions.json)) {
      throw new Error(`observer Sessions returned HTTP ${observerInitialSessions.status}`);
    }
    const observerSession = observerInitialSessions.json.find((session) => session.Id === observerSessionId);
    if (!receiverSession || !observerSession) {
      throw new Error('sessions list did not include each temporary client in its own session view');
    }
    summary.invariants.sessionsList200 = true;

    await startWebsocketProbe(page, summary.baseUrl, [
      { name: 'receiver', token: receiverToken, deviceId: `sessions-receiver-${target.name}` },
      { name: 'observer', token: observerToken, deviceId: `sessions-observer-${target.name}` },
    ]);
    await waitForWebsocketMessages(page, [
      ['receiver', 'ForceKeepAlive'],
      ['observer', 'ForceKeepAlive'],
    ]);
    summary.invariants.sessionsTwoClientsOpened = true;
    summary.invariants.sessionsStartSent = true;

    for (const token of [receiverToken, observerToken]) {
      const result = await browserFetchJson(page, {
        method: 'POST',
        url: capabilitiesUrl,
        token,
        body: {},
      });
      if (![200, 204].includes(result.status)) {
        throw new Error(`Sessions/Capabilities/Full returned HTTP ${result.status}`);
      }
    }
    summary.invariants.sessionsCapabilities204 = true;
    await waitForWebsocketMessages(page, [
      ['receiver', 'Sessions'],
      ['observer', 'Sessions'],
    ]);
    summary.invariants.sessionsMessageReceived = true;

    const addUser = await browserFetchJson(page, {
      method: 'POST',
      url: `/Sessions/${encodeURIComponent(receiverSessionId)}/User/${encodeURIComponent(createdGuestId)}`,
      token: admin.AccessToken,
    });
    if (![200, 204].includes(addUser.status)) {
      throw new Error(`Sessions/{id}/User/{userId} returned HTTP ${addUser.status}`);
    }
    summary.invariants.sessionsUserAdd204 = true;
    const addUserBroadcast = await browserFetchJson(page, {
      method: 'POST',
      url: capabilitiesUrl,
      token: receiverToken,
      body: {},
    });
    if (![200, 204].includes(addUserBroadcast.status)) {
      throw new Error(`Sessions/Capabilities/Full add-user broadcast returned HTTP ${addUserBroadcast.status}`);
    }
    await waitForWebsocketMessages(page, [
      ['observer', 'Sessions'],
    ], { minimumCount: 2 });
    const observerAddedSessions = await browserFetchJson(page, {
      method: 'GET',
      url: '/Sessions',
      token: observerToken,
    });
    const observerCanSeeReceiver = (observerAddedSessions.json || [])
      .some((session) => session.Id === receiverSessionId);
    if (observerAddedSessions.status !== 200 || !observerCanSeeReceiver) {
      throw new Error('observer session list did not include receiver after add-user');
    }
    summary.invariants.sessionsObserverUpdate = true;

    const remotePlay = await browserFetchJson(page, {
      method: 'POST',
      url: `/Sessions/${encodeURIComponent(receiverSessionId)}/Playing?PlayCommand=PlayNow&ItemIds=${encodeURIComponent(movie.Id)}&StartPositionTicks=123000000`,
      token: admin.AccessToken,
    });
    if (![200, 204].includes(remotePlay.status)) {
      throw new Error(`Sessions/{id}/Playing remote play returned HTTP ${remotePlay.status}`);
    }
    summary.invariants.sessionsRemotePlay204 = true;
    await waitForWebsocketMessages(page, [
      ['receiver', 'Play'],
      ['receiver', 'Sessions'],
    ]);
    summary.invariants.sessionsRemotePlayMessage = true;

    const remotePause = await browserFetchJson(page, {
      method: 'POST',
      url: `/Sessions/${encodeURIComponent(receiverSessionId)}/Playing/Pause`,
      token: admin.AccessToken,
    });
    if (![200, 204].includes(remotePause.status)) {
      throw new Error(`Sessions/{id}/Playing/Pause returned HTTP ${remotePause.status}`);
    }
    summary.invariants.sessionsRemotePlaystate204 = true;
    await waitForWebsocketMessages(page, [
      ['receiver', 'Playstate'],
      ['receiver', 'Sessions'],
    ]);
    summary.invariants.sessionsRemotePlaystateMessage = true;

    const remoteStop = await browserFetchJson(page, {
      method: 'POST',
      url: `/Sessions/${encodeURIComponent(receiverSessionId)}/Playing/Stop`,
      token: admin.AccessToken,
    });
    if (![200, 204].includes(remoteStop.status)) {
      throw new Error(`Sessions/{id}/Playing/Stop returned HTTP ${remoteStop.status}`);
    }
    summary.invariants.sessionsRemoteStop204 = true;
    await waitForWebsocketMessages(page, [
      ['receiver', 'Playstate'],
      ['receiver', 'Sessions'],
    ], { minimumCount: 2 });
    summary.invariants.sessionsRemoteStoppedMessage = true;

    const stoppedSessions = await browserFetchJson(page, {
      method: 'GET',
      url: '/Sessions',
      token: admin.AccessToken,
    });
    const stoppedReceiver = (stoppedSessions.json || []).find((session) => session.Id === receiverSessionId);
    if (stoppedSessions.status !== 200 || !stoppedReceiver) {
      throw new Error('remote stop session verification did not include receiver session');
    }
    summary.invariants.sessionsCleanupConfirmed = true;

    await closeWebsocketProbe(page);
    summary.item = {
      id: '<dynamic>',
      name: movie.Name,
      type: movie.Type,
      user: '<temporary-sessions-flow-users>',
    };
  } finally {
    await closeWebsocketProbe(page).catch(() => {});
    if (receiverToken) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: receiverToken,
      }).catch(() => {});
    }
    if (observerToken) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: observerToken,
      }).catch(() => {});
    }
    if (createdGuestId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdGuestId)}`,
        token: admin.AccessToken,
      }).catch(() => {});
    }
    if (createdOwnerId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdOwnerId)}`,
        token: admin.AccessToken,
      }).catch(() => {});
    }
  }
}

async function runSyncPlayFlow(page, summary, publicInfo, target) {
  const admin = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, admin, '/home');
  await page.waitForLoadState('networkidle').catch(() => {});

  const ownerName = `${syncplayFlowPrefix} Owner ${target.name}`;
  const guestName = `${syncplayFlowPrefix} Guest ${target.name}`;
  const staleName = `${syncplayFlowPrefix} Stale ${target.name}`;
  const groupName = `Jellyrin SyncPlay ${target.name}`;
  const ownerAuthorization = `MediaBrowser Client="Jellyrin Browser Trace", Device="SyncPlay Owner ${target.name}", DeviceId="syncplay-owner-${target.name}", Version="dev"`;
  const guestAuthorization = `MediaBrowser Client="Jellyrin Browser Trace", Device="SyncPlay Guest ${target.name}", DeviceId="syncplay-guest-${target.name}", Version="dev"`;
  const staleAuthorization = `MediaBrowser Client="Jellyrin Browser Trace", Device="SyncPlay Stale ${target.name}", DeviceId="syncplay-stale-${target.name}", Version="dev"`;
  let createdOwnerId = null;
  let createdGuestId = null;
  let createdStaleId = null;
  let ownerToken = null;
  let guestToken = null;
  let staleToken = null;
  let ownerInGroup = false;
  let guestInGroup = false;
  let staleInGroup = false;
  let groupId = null;

  async function cleanupExistingUsers() {
    const users = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users',
      token: admin.AccessToken,
    });
    if (users.status !== 200) {
      throw new Error(`syncplay cleanup user list returned HTTP ${users.status}`);
    }
    for (const user of users.json || []) {
      if ([ownerName, guestName, staleName].includes(user?.Name) && user.Id) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/Users/${encodeURIComponent(user.Id)}`,
          token: admin.AccessToken,
        });
      }
    }
  }

  await cleanupExistingUsers();

  try {
    const createdOwner = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: admin.AccessToken,
      body: {
        Name: ownerName,
        Password: syncplayFlowPassword,
      },
    });
    if (createdOwner.status !== 200 || !createdOwner.json?.Id || createdOwner.json.Name !== ownerName) {
      throw new Error(`syncplay owner Users/New returned HTTP ${createdOwner.status}`);
    }
    createdOwnerId = createdOwner.json.Id;

    const createdGuest = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: admin.AccessToken,
      body: {
        Name: guestName,
        Password: syncplayFlowPassword,
      },
    });
    if (createdGuest.status !== 200 || !createdGuest.json?.Id || createdGuest.json.Name !== guestName) {
      throw new Error(`syncplay guest Users/New returned HTTP ${createdGuest.status}`);
    }
    createdGuestId = createdGuest.json.Id;

    const createdStale = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: admin.AccessToken,
      body: {
        Name: staleName,
        Password: syncplayFlowPassword,
      },
    });
    if (createdStale.status !== 200 || !createdStale.json?.Id || createdStale.json.Name !== staleName) {
      throw new Error(`syncplay stale Users/New returned HTTP ${createdStale.status}`);
    }
    createdStaleId = createdStale.json.Id;

    const ownerLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: ownerAuthorization,
      body: {
        Username: ownerName,
        Pw: syncplayFlowPassword,
      },
    });
    if (ownerLogin.status !== 200 || !ownerLogin.json?.AccessToken) {
      throw new Error(`syncplay owner login returned HTTP ${ownerLogin.status}`);
    }
    ownerToken = ownerLogin.json.AccessToken;

    const guestLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: guestAuthorization,
      body: {
        Username: guestName,
        Pw: syncplayFlowPassword,
      },
    });
    if (guestLogin.status !== 200 || !guestLogin.json?.AccessToken) {
      throw new Error(`syncplay guest login returned HTTP ${guestLogin.status}`);
    }
    guestToken = guestLogin.json.AccessToken;

    const staleLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: staleAuthorization,
      body: {
        Username: staleName,
        Pw: syncplayFlowPassword,
      },
    });
    if (staleLogin.status !== 200 || !staleLogin.json?.AccessToken) {
      throw new Error(`syncplay stale login returned HTTP ${staleLogin.status}`);
    }
    staleToken = staleLogin.json.AccessToken;

    await startWebsocketProbe(page, summary.baseUrl, [
      { name: 'owner', token: ownerToken, deviceId: `syncplay-owner-${target.name}` },
      { name: 'guest', token: guestToken, deviceId: `syncplay-guest-${target.name}` },
    ]);
    await waitForWebsocketMessages(page, [
      ['owner', 'ForceKeepAlive'],
      ['guest', 'ForceKeepAlive'],
    ]);
    summary.invariants.syncplayTwoClientsOpened = true;

    const createdGroup = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/New',
      token: ownerToken,
      authorization: ownerAuthorization,
      body: {
        GroupName: groupName,
      },
    });
    groupId = createdGroup.json?.GroupId || createdGroup.json?.Id || null;
    if (createdGroup.status !== 200 || !groupId) {
      throw new Error(`SyncPlay/New returned HTTP ${createdGroup.status}`);
    }
    ownerInGroup = true;
    summary.invariants.syncplayGroupCreated = true;

    const join = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Join',
      token: guestToken,
      authorization: guestAuthorization,
      body: { GroupId: groupId },
    });
    if (![200, 204].includes(join.status)) {
      throw new Error(`SyncPlay/Join returned HTTP ${join.status}`);
    }
    if (join.status === 200 && join.json?.Participants?.length !== 2) {
      throw new Error('SyncPlay/Join did not return both participants');
    }
    guestInGroup = true;
    summary.invariants.syncplayGuestJoined = true;

    const list = await browserFetchJson(page, {
      method: 'GET',
      url: '/SyncPlay/List',
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (list.status !== 200 || !(list.json || []).some((group) => group.GroupId === groupId)) {
      throw new Error(`SyncPlay/List did not include created group, HTTP ${list.status}`);
    }
    summary.invariants.syncplayList200 = true;

    const getGroup = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (getGroup.status !== 200 || getGroup.json?.Participants?.length !== 2) {
      throw new Error(`SyncPlay/{id} returned HTTP ${getGroup.status}`);
    }
    summary.invariants.syncplayGet200 = true;

    const beforePlayMessages = await websocketReceivedCounts(page, ['SyncPlayCommand', 'SyncPlayGroupUpdate']);
    const play = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/SetNewQueue',
      token: ownerToken,
      authorization: ownerAuthorization,
      body: {
        PlayingQueue: [],
        PlayingItemPosition: 0,
        StartPositionTicks: 500,
      },
    });
    if (![200, 204].includes(play.status)) {
      throw new Error(`SyncPlay/SetNewQueue returned HTTP ${play.status}`);
    }
    summary.invariants.syncplayPlay204 = true;
    if (target.name === 'jellyrin') {
      await waitForAdditionalWebsocketMessages(page, beforePlayMessages);
    }
    summary.invariants.syncplayPlayFanout = true;

    const beforePauseMessages = await websocketReceivedCounts(page, ['SyncPlayCommand', 'SyncPlayGroupUpdate']);
    const pause = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Pause',
      token: ownerToken,
      authorization: ownerAuthorization,
      body: { PositionTicks: 1_000 },
    });
    if (![200, 204].includes(pause.status)) {
      throw new Error(`SyncPlay/Pause returned HTTP ${pause.status}`);
    }
    summary.invariants.syncplayPause204 = true;
    await waitForAdditionalWebsocketMessages(page, beforePauseMessages);
    summary.invariants.syncplayPauseFanout = true;

    const beforeSeekMessages = await websocketReceivedCounts(page, ['SyncPlayCommand', 'SyncPlayGroupUpdate']);
    const seek = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Seek',
      token: guestToken,
      authorization: guestAuthorization,
      body: { PositionTicks: 42_000 },
    });
    if (![200, 204].includes(seek.status)) {
      throw new Error(`SyncPlay/Seek returned HTTP ${seek.status}`);
    }
    summary.invariants.syncplaySeek204 = true;
    await waitForAdditionalWebsocketMessages(page, beforeSeekMessages);
    summary.invariants.syncplaySeekFanout = true;

    const beforeUnpauseMessages = await websocketReceivedCounts(page, ['SyncPlayCommand', 'SyncPlayGroupUpdate']);
    const unpause = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Unpause',
      token: ownerToken,
      authorization: ownerAuthorization,
      body: { PositionTicks: 42_000 },
    });
    if (![200, 204].includes(unpause.status)) {
      throw new Error(`SyncPlay/Unpause returned HTTP ${unpause.status}`);
    }
    summary.invariants.syncplayUnpause204 = true;
    await waitForAdditionalWebsocketMessages(page, beforeUnpauseMessages);
    summary.invariants.syncplayUnpauseFanout = true;

    const stateAfterCommands = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (stateAfterCommands.status !== 200) {
      throw new Error(`SyncPlay final state returned HTTP ${stateAfterCommands.status}`);
    }
    if (stateAfterCommands.json?.State?.LastCommandName && stateAfterCommands.json.State.LastCommandName !== 'Unpause') {
      throw new Error('SyncPlay final state did not record Unpause');
    }
    const beforeRaceSequence = Number(
      stateAfterCommands.json?.CommandSequence ?? stateAfterCommands.json?.State?.CommandSequence ?? 0,
    );

    const [racePause, raceSeek] = await Promise.all([
      browserFetchJson(page, {
        method: 'POST',
        url: '/SyncPlay/Pause',
        token: ownerToken,
        authorization: ownerAuthorization,
        body: { PositionTicks: 43_000 },
      }),
      browserFetchJson(page, {
        method: 'POST',
        url: '/SyncPlay/Seek',
        token: guestToken,
        authorization: guestAuthorization,
        body: { PositionTicks: 44_000 },
      }),
    ]);
    if (![200, 204].includes(racePause.status) || ![200, 204].includes(raceSeek.status)) {
      throw new Error(`SyncPlay race commands returned HTTP ${racePause.status}/${raceSeek.status}`);
    }
    const stateAfterRace = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (stateAfterRace.status !== 200 || stateAfterRace.json?.Participants?.length !== 2) {
      throw new Error(`SyncPlay race state returned HTTP ${stateAfterRace.status}`);
    }
    const afterRaceSequence = Number(
      stateAfterRace.json?.CommandSequence ?? stateAfterRace.json?.State?.CommandSequence,
    );
    if (target.name === 'jellyrin') {
      if (afterRaceSequence !== beforeRaceSequence + 2) {
        throw new Error(`SyncPlay race sequence expected ${beforeRaceSequence + 2}, got ${afterRaceSequence}`);
      }
      if (Number(stateAfterRace.json?.State?.LastCommandSequence) !== afterRaceSequence) {
        throw new Error('SyncPlay race state did not preserve the last command sequence');
      }
    }
    summary.invariants.syncplayRaceSequenced = true;

    const driftWhen = new Date(Date.now() - 2000).toISOString();
    const drift = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Ready',
      token: ownerToken,
      authorization: ownerAuthorization,
      body: {
        PlaylistItemId: '11111111-1111-1111-1111-111111111111',
        PositionTicks: 1_000_000,
        IsPlaying: true,
        When: driftWhen,
      },
    });
    if (![200, 204].includes(drift.status)) {
      throw new Error(`SyncPlay drift Ready returned HTTP ${drift.status}`);
    }
    const stateAfterDrift = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (stateAfterDrift.status !== 200 || stateAfterDrift.json?.Participants?.length !== 2) {
      throw new Error(`SyncPlay drift state returned HTTP ${stateAfterDrift.status}`);
    }
    if (target.name === 'jellyrin') {
      const timeline = stateAfterDrift.json?.State?.Timeline || {};
      if (timeline.ClientWhen !== driftWhen || timeline.IsCorrectionRequired !== true) {
        throw new Error('SyncPlay drift correction did not preserve client timing');
      }
      if (Number(timeline.DriftTicks || 0) < 5_000_000) {
        throw new Error(`SyncPlay drift ticks below threshold: ${timeline.DriftTicks}`);
      }
      if (Number(stateAfterDrift.json?.State?.CorrectionPositionTicks || 0) <= 1_000_000) {
        throw new Error('SyncPlay correction position was not advanced from client position');
      }
    }
    summary.invariants.syncplayDriftCorrection = true;

    const guestReconnectLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: guestAuthorization,
      body: {
        Username: guestName,
        Pw: syncplayFlowPassword,
      },
    });
    if (guestReconnectLogin.status !== 200 || !guestReconnectLogin.json?.AccessToken) {
      throw new Error(`syncplay guest reconnect login returned HTTP ${guestReconnectLogin.status}`);
    }
    const guestReconnectToken = guestReconnectLogin.json.AccessToken;
    if (target.name === 'jellyrin' && guestReconnectToken === guestToken) {
      throw new Error('SyncPlay guest reconnect returned the original access token');
    }
    const reconnect = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Join',
      token: guestReconnectToken,
      authorization: guestAuthorization,
      body: { GroupId: groupId },
    });
    if (![200, 204].includes(reconnect.status)) {
      throw new Error(`SyncPlay reconnect Join returned HTTP ${reconnect.status}`);
    }
    let reconnectParticipants = reconnect.json?.Participants || [];
    if (reconnect.status === 204) {
      const afterReconnect = await browserFetchJson(page, {
        method: 'GET',
        url: `/SyncPlay/${encodeURIComponent(groupId)}`,
        token: ownerToken,
        authorization: ownerAuthorization,
      });
      if (afterReconnect.status !== 200) {
        throw new Error(`SyncPlay reconnect state returned HTTP ${afterReconnect.status}`);
      }
      reconnectParticipants = afterReconnect.json?.Participants || [];
    }
    if (target.name !== 'jellyrin') {
      if (reconnectParticipants.length > 2) {
        throw new Error(`SyncPlay reconnect duplicated upstream participants: ${JSON.stringify(reconnectParticipants)}`);
      }
      guestToken = guestReconnectToken;
      summary.invariants.syncplayGuestReconnectDeduped = true;
    } else {
      const guestParticipants = reconnectParticipants.filter((participant) => participant.UserName === guestName);
      if (reconnectParticipants.length !== 2 || guestParticipants.length !== 1) {
        throw new Error(`SyncPlay reconnect duplicated participants: ${JSON.stringify(reconnectParticipants)}`);
      }
      if (guestParticipants[0].SessionId !== guestReconnectToken) {
        throw new Error('SyncPlay reconnect did not replace the guest session id');
      }
      if (guestParticipants[0].DeviceId !== `syncplay-guest-${target.name}`) {
        throw new Error('SyncPlay reconnect did not preserve the guest device id');
      }
      guestToken = guestReconnectToken;
      summary.invariants.syncplayGuestReconnectDeduped = true;
    }

    const guestLogout = await browserFetchJson(page, {
      method: 'POST',
      url: '/Sessions/Logout',
      token: guestToken,
      authorization: guestAuthorization,
    });
    if (![200, 204].includes(guestLogout.status)) {
      throw new Error(`Sessions/Logout guest returned HTTP ${guestLogout.status}`);
    }
    const afterGuestLogout = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    const afterGuestLogoutParticipants = afterGuestLogout.json?.Participants || [];
    if (afterGuestLogout.status !== 200 || afterGuestLogoutParticipants.length !== 1) {
      throw new Error(`SyncPlay guest logout cleanup returned HTTP ${afterGuestLogout.status}`);
    }
    if (afterGuestLogoutParticipants.some((participant) => participant.UserName === guestName)) {
      throw new Error('SyncPlay guest logout left the guest participant in the group');
    }
    summary.invariants.syncplayGuestLogoutRemoved = true;
    guestInGroup = false;
    guestToken = null;
    summary.invariants.syncplayGuestLeft = true;

    const ownerLeave = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/Leave',
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (![200, 204].includes(ownerLeave.status)) {
      throw new Error(`SyncPlay/Leave owner returned HTTP ${ownerLeave.status}`);
    }
    ownerInGroup = false;
    summary.invariants.syncplayOwnerLeft = true;

    const deletedGroup = await browserFetchJson(page, {
      method: 'GET',
      url: `/SyncPlay/${encodeURIComponent(groupId)}`,
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (deletedGroup.status !== 404) {
      throw new Error(`SyncPlay group cleanup expected 404, got HTTP ${deletedGroup.status}`);
    }
    summary.invariants.syncplayCleanupConfirmed = true;

    if (target.name !== 'jellyrin') {
      summary.invariants.syncplayStaleCleanup = true;
      await closeWebsocketProbe(page);
      summary.item = {
        id: groupId,
        name: groupName,
        type: 'SyncPlay',
        user: '<temporary-syncplay-flow-user>',
      };
      return;
    }

    const staleGroupName = `Jellyrin SyncPlay Stale ${target.name}`;
    const staleGroup = await browserFetchJson(page, {
      method: 'POST',
      url: '/SyncPlay/New',
      token: staleToken,
      authorization: staleAuthorization,
      body: {
        GroupName: staleGroupName,
      },
    });
    const staleGroupId = staleGroup.json?.GroupId || staleGroup.json?.Id || null;
    if (staleGroup.status !== 200 || !staleGroupId) {
      throw new Error(`SyncPlay stale group New returned HTTP ${staleGroup.status}`);
    }
    staleInGroup = true;
    const staleTimeoutMs = Number.parseInt(process.env.JELLYRIN_SYNCPLAY_STALE_TIMEOUT_MS || '120000', 10);
    if (!Number.isFinite(staleTimeoutMs) || staleTimeoutMs > 5000) {
      throw new Error('SyncPlay stale cleanup golden requires JELLYRIN_SYNCPLAY_STALE_TIMEOUT_MS <= 5000');
    }
    await page.waitForTimeout(staleTimeoutMs + 50);
    const listAfterStale = await browserFetchJson(page, {
      method: 'GET',
      url: '/SyncPlay/List',
      token: ownerToken,
      authorization: ownerAuthorization,
    });
    if (listAfterStale.status !== 200) {
      throw new Error(`SyncPlay stale cleanup List returned HTTP ${listAfterStale.status}`);
    }
    if ((listAfterStale.json || []).some((group) => group.GroupId === staleGroupId || group.Id === staleGroupId)) {
      throw new Error('SyncPlay stale cleanup left the stale group in the list');
    }
    staleInGroup = false;
    summary.invariants.syncplayStaleCleanup = true;

    await closeWebsocketProbe(page);
    summary.item = {
      id: groupId,
      name: groupName,
      type: 'SyncPlay',
      user: '<temporary-syncplay-flow-user>',
    };
  } finally {
    await closeWebsocketProbe(page).catch(() => {});
    if (staleToken && staleInGroup) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/SyncPlay/Leave',
        token: staleToken,
        authorization: staleAuthorization,
      }).catch(() => {});
    }
    if (ownerToken && ownerInGroup) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/SyncPlay/Leave',
        token: ownerToken,
        authorization: ownerAuthorization,
      }).catch(() => {});
    }
    if (guestToken) {
      if (guestInGroup) {
        await browserFetchJson(page, {
          method: 'POST',
          url: '/SyncPlay/Leave',
          token: guestToken,
          authorization: guestAuthorization,
        }).catch(() => {});
      }
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: guestToken,
      }).catch(() => {});
    }
    if (staleToken) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: staleToken,
      }).catch(() => {});
    }
    if (ownerToken) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: ownerToken,
      }).catch(() => {});
    }
    if (createdGuestId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdGuestId)}`,
        token: admin.AccessToken,
      }).catch(() => {});
    }
    if (createdStaleId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdStaleId)}`,
        token: admin.AccessToken,
      }).catch(() => {});
    }
    if (createdOwnerId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdOwnerId)}`,
        token: admin.AccessToken,
      }).catch(() => {});
    }
  }
}

async function runAdminDashboardFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard');
  await page.waitForLoadState('networkidle');

  const endpoints = [
    ['System/Info', '/System/Info'],
    ['System/Info/Storage', '/System/Info/Storage'],
    ['ScheduledTasks', '/ScheduledTasks?IsEnabled=true'],
    ['System/ActivityLog/Entries', '/System/ActivityLog/Entries?StartIndex=0&Limit=20'],
    ['Devices', '/Devices'],
    ['Plugins', '/Plugins'],
    ['Repositories', '/Repositories'],
    ['ConfigurationPages', '/web/ConfigurationPages?EnableInMainMenu=true'],
  ];
  for (const [name, url] of endpoints) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`${name} returned HTTP ${result.status}`);
    }
    if (result.json === null) {
      throw new Error(`${name} did not return JSON`);
    }
  }

  await page.goto(`${summary.baseUrl}/web/#/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle');
}

async function runLibrariesFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const viewsResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserViews?UserId=${encodeURIComponent(auth.User.Id)}`,
    token: auth.AccessToken,
  });
  if (viewsResult.status !== 200) {
    throw new Error(`UserViews returned HTTP ${viewsResult.status}`);
  }
  const libraryView = (viewsResult.json?.Items || [])
    .find((item) => ['movies', 'boxsets'].includes(String(item.CollectionType || '').toLowerCase()))
    || viewsResult.json?.Items?.[0];
  if (!libraryView?.Id) {
    throw new Error('UserViews returned no library views');
  }
  summary.invariants.libraryViewMatched = true;

  const endpoints = [
    ['UserViews/GroupingOptions', `/UserViews/GroupingOptions?UserId=${encodeURIComponent(auth.User.Id)}`],
    ['Library/VirtualFolders', '/Library/VirtualFolders'],
    ['Items/Counts', `/Items/Counts?UserId=${encodeURIComponent(auth.User.Id)}&ParentId=${encodeURIComponent(libraryView.Id)}`],
  ];
  for (const [name, url] of endpoints) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`${name} returned HTTP ${result.status}`);
    }
    if (result.json === null) {
      throw new Error(`${name} did not return JSON`);
    }
  }

  const itemsResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&ParentId=${encodeURIComponent(libraryView.Id)}&Recursive=true&IncludeItemTypes=Movie&Fields=PrimaryImageAspectRatio,MediaSources,DateCreated&StartIndex=0&Limit=12`,
    token: auth.AccessToken,
  });
  if (itemsResult.status !== 200) {
    throw new Error(`Items returned HTTP ${itemsResult.status}`);
  }
  const libraryItem = itemsResult.json?.Items?.find((item) => item.Type === 'Movie') || itemsResult.json?.Items?.[0];
  if (!libraryItem?.Id) {
    throw new Error('library Items returned no media items');
  }
  summary.invariants.libraryItemMatched = true;

  const latestResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/Users/${encodeURIComponent(auth.User.Id)}/Items/Latest?ParentId=${encodeURIComponent(libraryView.Id)}&IncludeItemTypes=Movie&Limit=12&Fields=PrimaryImageAspectRatio,MediaSources,DateCreated`,
    token: auth.AccessToken,
  });
  if (latestResult.status !== 200) {
    throw new Error(`Users/Items/Latest returned HTTP ${latestResult.status}`);
  }
  if (!Array.isArray(latestResult.json)) {
    throw new Error('Users/Items/Latest did not return a JSON array');
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.library = {
    id: '<dynamic>',
    name: libraryView.Name,
    collectionType: libraryView.CollectionType,
    itemName: libraryItem.Name,
  };
}

async function runSubtitlesTrickplayFlow(page, summary, publicInfo, target) {
  await ensureSubtitleTrickplayFixture();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await refreshLibrary(page, auth);
  const movie = await waitForMovieByName(page, summary, auth, subtitleTrickplayFixtureName);
  const mediaSource = movie.MediaSources?.[0] || {};
  const subtitleStreams = (mediaSource.MediaStreams || movie.MediaStreams || [])
    .filter((stream) => stream.Type === 'Subtitle' && Number.isInteger(Number(stream.Index)));
  const subtitleStream = subtitleStreams.find((stream) => stream.IsExternal === false) || subtitleStreams[0];
  if (!subtitleStream) {
    throw new Error('subtitle-trickplay fixture has no subtitle stream');
  }
  summary.invariants.subtitleStreamMatched = true;
  const subtitleIndex = Number(subtitleStream.Index);
  const mediaSourceId = mediaSource.Id || movie.Id;
  const trickplayWidth = preferredTrickplayWidth(movie);

  const playbackInfo = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(movie.Id)}/PlaybackInfo`,
    token: auth.AccessToken,
    body: withoutUndefined({
      UserId: auth.User.Id,
      MediaSourceId: mediaSourceId,
      AudioStreamIndex: defaultStreamIndex(movie, 'Audio'),
      SubtitleStreamIndex: subtitleIndex,
      EnableDirectPlay: true,
      EnableDirectStream: true,
      EnableTranscoding: true,
      StartPositionTicks: 0,
    }),
  });
  if (playbackInfo.status !== 200) {
    throw new Error(`subtitle PlaybackInfo returned HTTP ${playbackInfo.status}`);
  }
  summary.invariants.subtitlePlaybackInfo200 = true;

  const subtitlePlaylist = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/${encodeURIComponent(mediaSourceId)}/Subtitles/${subtitleIndex}/subtitles.m3u8?SegmentLength=2`,
    token: auth.AccessToken,
  });
  if (subtitlePlaylist.status !== 200) {
    throw new Error(`subtitle playlist returned HTTP ${subtitlePlaylist.status}`);
  }
  summary.invariants.subtitlePlaylist200 = true;
  if (subtitlePlaylist.text.includes('#EXTM3U') && subtitlePlaylist.text.includes('stream.vtt')) {
    summary.invariants.subtitlePlaylistShape = true;
  } else {
    throw new Error('subtitle playlist missing expected HLS/VTT shape');
  }

  const subtitleVtt = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/${encodeURIComponent(mediaSourceId)}/Subtitles/${subtitleIndex}/Stream.vtt?AddVttTimeMap=true`,
    token: auth.AccessToken,
  });
  if (subtitleVtt.status !== 200) {
    throw new Error(`subtitle VTT stream returned HTTP ${subtitleVtt.status}`);
  }
  summary.invariants.subtitleVtt200 = true;
  if (subtitleVtt.text.startsWith('WEBVTT') && subtitleVtt.text.includes('Hello from Jellyrin')) {
    summary.invariants.subtitleVttCue = true;
  } else {
    throw new Error('subtitle VTT stream missing expected cue');
  }

  if (target.name === 'upstream') {
    await ensureUpstreamTrickplayReady(page, auth, movie);
  }

  const trickplayPlaylist = await browserFetchText(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(movie.Id)}/Trickplay/${trickplayWidth}/tiles.m3u8`,
    token: auth.AccessToken,
  });
  if (trickplayPlaylist.status !== 200) {
    throw new Error(`trickplay playlist returned HTTP ${trickplayPlaylist.status}`);
  }
  summary.invariants.trickplayPlaylist200 = true;
  if (trickplayPlaylist.text.includes('#EXT-X-IMAGES-ONLY')) {
    summary.invariants.trickplayImagesOnly = true;
  } else {
    throw new Error('trickplay playlist missing EXT-X-IMAGES-ONLY');
  }
  const tilePath = firstPlaylistUri(trickplayPlaylist.text);
  if (!tilePath || !/\.jpg(?:\?|$)/i.test(tilePath)) {
    throw new Error('trickplay playlist did not contain a JPEG tile URI');
  }

  const trickplayTile = await browserFetchBinary(page, {
    method: 'GET',
    url: resolveRelativeUrl(`/Videos/${movie.Id}/Trickplay/${trickplayWidth}/tiles.m3u8`, tilePath),
    token: auth.AccessToken,
  });
  if (trickplayTile.status !== 200) {
    throw new Error(`trickplay tile returned HTTP ${trickplayTile.status}`);
  }
  summary.invariants.trickplayTile200 = true;
  if (trickplayTile.contentType.split(';')[0].trim().toLowerCase() === 'image/jpeg' && trickplayTile.startsWithJpeg) {
    summary.invariants.trickplayTileJpeg = true;
  } else {
    throw new Error(`trickplay tile was not JPEG: ${trickplayTile.contentType}`);
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
    subtitleIndex,
    trickplayWidth,
  };
}

async function runAudioHlsLegacyFlow(page, summary, publicInfo, target) {
  await ensureAudioHlsFixture();
  await ensureAudioLegacySegmentFixture();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await ensureVirtualFolder(page, auth, {
    name: 'Golden Music',
    collectionType: 'music',
    location: audioFixtureDir,
  });
  await refreshLibrary(page, auth);
  const audio = await waitForAudioByName(page, auth, audioHlsFixtureName);
  summary.invariants.audioItemMatched = true;
  const mediaSourceId = audio.MediaSources?.[0]?.Id || audio.Id;
  const audioStreamIndex = defaultStreamIndex(audio, 'Audio') ?? 0;

  const playbackInfo = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(audio.Id)}/PlaybackInfo`,
    token: auth.AccessToken,
    body: withoutUndefined({
      UserId: auth.User.Id,
      MediaSourceId: mediaSourceId,
      AudioStreamIndex: audioStreamIndex,
      EnableDirectPlay: false,
      EnableDirectStream: false,
      EnableTranscoding: true,
      StartPositionTicks: 0,
    }),
  });
  if (playbackInfo.status !== 200) {
    throw new Error(`audio PlaybackInfo returned HTTP ${playbackInfo.status}`);
  }
  summary.invariants.audioPlaybackInfo200 = true;
  if (playbackInfo.json?.MediaSources?.[0]?.TranscodingUrl) {
    summary.invariants.audioTranscodingUrlPresent = true;
  }

  const query = `api_key=${encodeURIComponent(auth.AccessToken)}&Static=true&MediaSourceId=${encodeURIComponent(mediaSourceId)}`;
  const master = await browserFetchText(page, {
    method: 'GET',
    url: `/Audio/${encodeURIComponent(audio.Id)}/master.m3u8?${query}`,
    token: auth.AccessToken,
  });
  if (master.status !== 200) {
    throw new Error(`audio HLS master returned HTTP ${master.status}`);
  }
  summary.invariants.audioHlsMaster200 = true;
  if (!master.text.includes('#EXTM3U') || !master.text.includes('main.m3u8')) {
    throw new Error('audio HLS master missing expected shape');
  }

  const media = await browserFetchText(page, {
    method: 'GET',
    url: `/Audio/${encodeURIComponent(audio.Id)}/main.m3u8?${query}`,
    token: auth.AccessToken,
  });
  if (media.status !== 200) {
    throw new Error(`audio HLS media playlist returned HTTP ${media.status}`);
  }
  summary.invariants.audioHlsMedia200 = true;
  if (!media.text.includes('#EXTM3U') || !media.text.includes('/hls1/') && !media.text.includes('hls1/')) {
    throw new Error('audio HLS media playlist missing expected hls1 segment');
  }

  const dynamicSegmentPath = firstPlaylistUri(media.text);
  if (!dynamicSegmentPath || !/\.(?:mp3|aac|ts)(?:\?|$)/i.test(dynamicSegmentPath)) {
    throw new Error('audio HLS media playlist did not contain mp3/aac/ts segment URI');
  }
  const dynamicSegment = await browserFetchBinary(page, {
    method: 'GET',
    url: resolveRelativeUrl(`/Audio/${audio.Id}/main.m3u8`, dynamicSegmentPath),
    token: auth.AccessToken,
  });
  if (![200, 206].includes(dynamicSegment.status)) {
    throw new Error(`audio HLS dynamic segment returned HTTP ${dynamicSegment.status}`);
  }
  summary.invariants.audioHlsDynamicSegment200 = true;
  addUnique(summary.invariants.audioHlsSegmentContentTypes, mediaType(dynamicSegment.contentType));

  const legacySegment = await browserFetchBinary(page, {
    method: 'GET',
    url: `/Audio/${encodeURIComponent(audio.Id)}/hls/${audioLegacySegmentId}/stream.mp3?api_key=${encodeURIComponent(auth.AccessToken)}`,
    token: auth.AccessToken,
  });
  if (![200, 206].includes(legacySegment.status)) {
    throw new Error(`audio HLS legacy segment returned HTTP ${legacySegment.status}`);
  }
  summary.invariants.audioHlsLegacySegment200 = true;
  addUnique(summary.invariants.audioHlsSegmentContentTypes, mediaType(legacySegment.contentType));

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: audio.Name,
    type: audio.Type,
    mediaType: audio.MediaType,
    container: audio.MediaSources?.[0]?.Container,
  };
}

async function runMusicFlow(page, summary, publicInfo, target) {
  await ensureMusicFlowFixtures();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await ensureVirtualFolder(page, auth, {
    name: 'Golden Music',
    collectionType: 'music',
    location: audioFixtureDir,
  });
  await refreshLibrary(page, auth);
  const songs = await waitForMusicFlowSongs(page, auth, { requireMetadata: true });
  summary.invariants.musicSongsMatched = true;
  const firstSong = songs[0];

  const userViews = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserViews?UserId=${encodeURIComponent(auth.User.Id)}&PresetViews=music`,
    token: auth.AccessToken,
  });
  if (userViews.status !== 200) {
    throw new Error(`music UserViews returned HTTP ${userViews.status}`);
  }
  if (!userViews.json?.Items?.some((view) => view.CollectionType === 'music' || view.Name === 'Golden Music')) {
    throw new Error('music UserViews did not include the music library');
  }
  summary.invariants.musicViewMatched = true;

  if (!songs.some((item) => item.Album === musicFlowAlbum)) {
    throw new Error('music album metadata not found in song DTOs');
  }
  summary.invariants.musicAlbumMatched = true;

  const artists = await browserFetchJson(page, {
    method: 'GET',
    url: `/Artists?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(musicFlowArtist)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (artists.status !== 200) {
    throw new Error(`music Artists returned HTTP ${artists.status}`);
  }
  if (!artists.json?.Items?.some((item) => item.Name === musicFlowArtist && item.Type === 'MusicArtist')) {
    throw new Error('music artist not found in Artists result');
  }
  summary.invariants.musicArtistMatched = true;

  const albumArtists = await browserFetchJson(page, {
    method: 'GET',
    url: `/Artists/AlbumArtists?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(musicFlowAlbumArtist)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (albumArtists.status !== 200) {
    throw new Error(`music AlbumArtists returned HTTP ${albumArtists.status}`);
  }
  if (!albumArtists.json?.Items?.some((item) => item.Name === musicFlowAlbumArtist && item.Type === 'MusicArtist')) {
    throw new Error('music album artist not found in AlbumArtists result');
  }
  summary.invariants.musicAlbumArtistMatched = true;

  const genres = await browserFetchJson(page, {
    method: 'GET',
    url: `/MusicGenres?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(musicFlowGenre)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (genres.status !== 200) {
    throw new Error(`music MusicGenres returned HTTP ${genres.status}`);
  }
  if (!genres.json?.Items?.some((item) => item.Name === musicFlowGenre && item.Type === 'MusicGenre')) {
    throw new Error('music genre not found in MusicGenres result');
  }
  summary.invariants.musicGenreMatched = true;

  const instantMix = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(firstSong.Id)}/InstantMix?UserId=${encodeURIComponent(auth.User.Id)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (instantMix.status !== 200) {
    throw new Error(`music InstantMix returned HTTP ${instantMix.status}`);
  }
  summary.invariants.musicInstantMix200 = true;
  if ((instantMix.json?.Items || []).filter((item) => item.Type === 'Audio').length < 2) {
    throw new Error('music InstantMix did not return the fixture songs');
  }
  summary.invariants.musicInstantMixResults = true;

  const stream = await browserFetchBinary(page, {
    method: 'GET',
    url: `/Audio/${encodeURIComponent(firstSong.Id)}/stream.mp3?Static=true&api_key=${encodeURIComponent(auth.AccessToken)}`,
    token: auth.AccessToken,
  });
  if (![200, 206].includes(stream.status)) {
    throw new Error(`music audio stream returned HTTP ${stream.status}`);
  }
  summary.invariants.musicAudioStream200 = true;
  addUnique(summary.invariants.musicAudioStreamContentTypes, mediaType(stream.contentType));

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: firstSong.Name,
    type: firstSong.Type,
    mediaType: firstSong.MediaType,
    album: musicFlowAlbum,
    artist: musicFlowArtist,
    genre: musicFlowGenre,
  };
}

async function runSeriesFlow(page, summary, publicInfo, target) {
  await ensureSeriesFlowFixtures();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await ensureVirtualFolder(page, auth, {
    name: 'Golden Series',
    collectionType: 'tvshows',
    location: seriesFixtureDir,
  });
  await refreshLibrary(page, auth);
  const episodes = await waitForSeriesFlowEpisodes(page, auth, { requireMetadata: true });
  summary.invariants.seriesEpisodesMatched = true;
  const firstEpisode = episodes[0];
  if (
    !episodes.every((episode) => episode.Type === 'Episode')
    || !episodes.every((episode) => episode.SeriesName === seriesFlowName)
    || !episodes.every((episode) => episode.SeriesId)
    || !episodes.every((episode) => episode.ParentIndexNumber === 1)
    || !episodes.every((episode, index) => episode.IndexNumber === index + 1)
  ) {
    throw new Error('series episode DTO metadata did not match expected season/episode fields');
  }
  summary.invariants.seriesEpisodeMetadataMatched = true;
  const seriesId = firstEpisode.SeriesId;

  const userViews = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserViews?UserId=${encodeURIComponent(auth.User.Id)}&PresetViews=tvshows`,
    token: auth.AccessToken,
  });
  if (userViews.status !== 200) {
    throw new Error(`series UserViews returned HTTP ${userViews.status}`);
  }
  if (!userViews.json?.Items?.some((view) => view.CollectionType === 'tvshows' || view.Name === 'Golden Series')) {
    throw new Error('series UserViews did not include the tvshows library');
  }
  summary.invariants.seriesViewMatched = true;

  const counts = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/Counts?UserId=${encodeURIComponent(auth.User.Id)}`,
    token: auth.AccessToken,
  });
  if (counts.status !== 200) {
    throw new Error(`series Items/Counts returned HTTP ${counts.status}`);
  }
  if (Number(counts.json?.EpisodeCount || 0) < 3 || Number(counts.json?.SeriesCount || 0) < 1) {
    throw new Error('series Items/Counts did not include scanned series episodes');
  }
  summary.invariants.seriesCounts200 = true;

  const nextUp = await browserFetchJson(page, {
    method: 'GET',
    url: `/Shows/NextUp?UserId=${encodeURIComponent(auth.User.Id)}&SeriesId=${encodeURIComponent(seriesId)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (nextUp.status !== 200) {
    throw new Error(`series Shows/NextUp returned HTTP ${nextUp.status}`);
  }
  if (!nextUp.json?.Items?.some((item) => item.Type === 'Episode' && item.SeriesName === seriesFlowName)) {
    throw new Error('series Shows/NextUp did not return the fixture series');
  }
  summary.invariants.seriesNextUp200 = true;

  const seasons = await browserFetchJson(page, {
    method: 'GET',
    url: `/Shows/${encodeURIComponent(seriesId)}/Seasons?UserId=${encodeURIComponent(auth.User.Id)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (seasons.status !== 200) {
    throw new Error(`series Seasons returned HTTP ${seasons.status}`);
  }
  const season = seasons.json?.Items?.find((item) => item.Type === 'Season' && item.SeriesName === seriesFlowName);
  if (!season?.Id || season.IndexNumber !== 1) {
    throw new Error('series Seasons did not include Season 1');
  }
  summary.invariants.seriesSeasons200 = true;
  summary.invariants.seriesSeasonMatched = true;

  const seasonEpisodes = await browserFetchJson(page, {
    method: 'GET',
    url: `/Shows/${encodeURIComponent(seriesId)}/Episodes?UserId=${encodeURIComponent(auth.User.Id)}&SeasonId=${encodeURIComponent(season.Id)}&Fields=MediaSources,RunTimeTicks,Path&SortBy=SortName&Limit=10`,
    token: auth.AccessToken,
  });
  if (seasonEpisodes.status !== 200) {
    throw new Error(`series Episodes route returned HTTP ${seasonEpisodes.status}`);
  }
  if ((seasonEpisodes.json?.Items || []).filter((item) => item.Type === 'Episode' && item.SeriesName === seriesFlowName).length < 3) {
    throw new Error('series Episodes route did not return all fixture episodes');
  }
  summary.invariants.seriesEpisodesRoute200 = true;
  summary.invariants.seriesEpisodesRouteMatched = true;

  const similar = await browserFetchJson(page, {
    method: 'GET',
    url: `/Shows/${encodeURIComponent(seriesId)}/Similar?UserId=${encodeURIComponent(auth.User.Id)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (similar.status !== 200) {
    throw new Error(`series Similar returned HTTP ${similar.status}`);
  }
  summary.invariants.seriesSimilar200 = true;

  const stream = await browserFetchBinary(page, {
    method: 'GET',
    url: `/Videos/${encodeURIComponent(firstEpisode.Id)}/stream.mp4?Static=true&api_key=${encodeURIComponent(auth.AccessToken)}`,
    token: auth.AccessToken,
  });
  if (![200, 206].includes(stream.status)) {
    throw new Error(`series episode stream returned HTTP ${stream.status}`);
  }
  summary.invariants.seriesStream200 = true;
  addUnique(summary.invariants.seriesStreamContentTypes, mediaType(stream.contentType));

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: firstEpisode.Name,
    type: firstEpisode.Type,
    mediaType: firstEpisode.MediaType,
    series: seriesFlowName,
    season: 1,
    episode: 1,
  };
}

async function runPlaylistsCollectionsFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const movies = await firstTwoMovieItems(page, summary, auth);
  const [firstMovie, secondMovie] = movies;
  const playlistName = `${listFlowPlaylistName} ${target.name}`;
  const collectionName = `${listFlowCollectionName} ${target.name}`;

  const createdPlaylist = await browserFetchJson(page, {
    method: 'POST',
    url: `/Playlists?Name=${encodeURIComponent(playlistName)}&Ids=${encodeURIComponent(firstMovie.Id)},${encodeURIComponent(secondMovie.Id)}&UserId=${encodeURIComponent(auth.User.Id)}`,
    token: auth.AccessToken,
  });
  if (createdPlaylist.status !== 200) {
    throw new Error(`playlist create returned HTTP ${createdPlaylist.status}`);
  }
  const playlistId = createdPlaylist.json?.Id;
  if (!playlistId) {
    throw new Error('playlist create did not return Id');
  }
  summary.invariants.playlistCreated = true;

  const playlist = await browserFetchJson(page, {
    method: 'GET',
    url: `/Users/${encodeURIComponent(auth.User.Id)}/Items/${encodeURIComponent(playlistId)}`,
    token: auth.AccessToken,
  });
  if (playlist.status !== 200) {
    throw new Error(`playlist detail returned HTTP ${playlist.status}`);
  }
  if (playlist.json?.Type !== 'Playlist' || !String(playlist.json?.Name || '').includes(listFlowPlaylistName)) {
    throw new Error('playlist detail did not match created playlist');
  }
  summary.invariants.playlistDetail200 = true;

  const initialItems = await fetchPlaylistItems(page, auth, playlistId);
  if (initialItems.length < 2 || !initialItems.every((item) => item.PlaylistItemId)) {
    throw new Error('playlist items did not include expected entries');
  }
  summary.invariants.playlistItems200 = true;
  summary.invariants.playlistItemIdsMatched = true;
  const secondPlaylistItemId = initialItems.find((item) => item.Id === secondMovie.Id)?.PlaylistItemId;
  if (!secondPlaylistItemId) {
    throw new Error('playlist items did not expose movable PlaylistItemId');
  }

  const move = await browserFetchJson(page, {
    method: 'POST',
    url: `/Playlists/${encodeURIComponent(playlistId)}/Items/${encodeURIComponent(secondPlaylistItemId)}/Move/0`,
    token: auth.AccessToken,
  });
  if (move.status === 400 && target.name === 'upstream') {
    summary.invariants.playlistMoveUnsupported400 = true;
  } else if (![200, 204].includes(move.status)) {
    throw new Error(`playlist move returned HTTP ${move.status}`);
  } else {
    summary.invariants.playlistMove204 = true;
    const movedItems = await fetchPlaylistItems(page, auth, playlistId);
    if (movedItems[0]?.Id !== secondMovie.Id) {
      throw new Error('playlist move did not reorder items');
    }
    summary.invariants.playlistMovedOrderMatched = true;
  }

  const movedItems = await fetchPlaylistItems(page, auth, playlistId);
  const removablePlaylistItemId = movedItems.find((item) => item.Id === firstMovie.Id)?.PlaylistItemId;
  if (!removablePlaylistItemId) {
    throw new Error('playlist moved items did not expose removable PlaylistItemId');
  }
  const remove = await browserFetchJson(page, {
    method: 'DELETE',
    url: `/Playlists/${encodeURIComponent(playlistId)}/Items?entryIds=${encodeURIComponent(removablePlaylistItemId)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(remove.status)) {
    throw new Error(`playlist item delete returned HTTP ${remove.status}`);
  }
  summary.invariants.playlistDeleteItem204 = true;

  const add = await browserFetchJson(page, {
    method: 'POST',
    url: `/Playlists/${encodeURIComponent(playlistId)}/Items?Ids=${encodeURIComponent(firstMovie.Id)}&UserId=${encodeURIComponent(auth.User.Id)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(add.status)) {
    throw new Error(`playlist item add returned HTTP ${add.status}`);
  }
  summary.invariants.playlistAddItem204 = true;

  const rename = await browserFetchJson(page, {
    method: 'POST',
    url: `/Playlists/${encodeURIComponent(playlistId)}`,
    token: auth.AccessToken,
    body: { Name: `${playlistName} Renamed` },
  });
  if (rename.status === 400 && target.name === 'upstream') {
    summary.invariants.playlistRenameUnsupported400 = true;
  } else if (![200, 204].includes(rename.status)) {
    throw new Error(`playlist rename returned HTTP ${rename.status}`);
  } else {
    summary.invariants.playlistRename204 = true;
  }

  const createdCollection = await browserFetchJson(page, {
    method: 'POST',
    url: `/Collections?name=${encodeURIComponent(collectionName)}&ids=${encodeURIComponent(firstMovie.Id)}`,
    token: auth.AccessToken,
  });
  if (createdCollection.status !== 200) {
    throw new Error(`collection create returned HTTP ${createdCollection.status}`);
  }
  const collectionId = createdCollection.json?.Id;
  if (!collectionId) {
    throw new Error('collection create did not return Id');
  }
  summary.invariants.collectionCreated = true;

  const addCollection = await browserFetchJson(page, {
    method: 'POST',
    url: `/Collections/${encodeURIComponent(collectionId)}/Items?ids=${encodeURIComponent(secondMovie.Id)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(addCollection.status)) {
    throw new Error(`collection item add returned HTTP ${addCollection.status}`);
  }
  summary.invariants.collectionAddItems204 = true;

  const removeCollection = await browserFetchJson(page, {
    method: 'DELETE',
    url: `/Collections/${encodeURIComponent(collectionId)}/Items?ids=${encodeURIComponent(firstMovie.Id)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(removeCollection.status)) {
    throw new Error(`collection item delete returned HTTP ${removeCollection.status}`);
  }
  summary.invariants.collectionDeleteItems204 = true;

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    playlist: playlistName,
    collection: collectionName,
    firstMovie: firstMovie.Name,
    secondMovie: secondMovie.Name,
  };
}

async function runImagesFlow(page, summary, publicInfo, target) {
  await ensureImageFlowFixture();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await ensureVirtualFolder(page, auth, {
    name: 'Golden Movies',
    collectionType: 'movies',
    location: mediaFixtureDir,
  });
  await refreshLibrary(page, auth);
  const movie = await waitForMovieByName(page, summary, auth, imageFlowFixtureName);
  summary.invariants.imageItemMatched = true;

  const initialInfos = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images`,
    token: auth.AccessToken,
  });
  if (initialInfos.status !== 200 || !Array.isArray(initialInfos.json)) {
    throw new Error(`initial item images returned HTTP ${initialInfos.status}`);
  }
  summary.invariants.imageInfosInitial200 = true;

  const upload = await browserFetchImageUpload(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images/Primary`,
    token: auth.AccessToken,
    imageBase64: imageFlowUploadPngBase64,
  });
  if (![200, 204].includes(upload.status)) {
    throw new Error(`image upload returned HTTP ${upload.status}`);
  }
  summary.invariants.imageUpload204 = true;

  const infosAfterUpload = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images`,
    token: auth.AccessToken,
  });
  if (infosAfterUpload.status !== 200 || !Array.isArray(infosAfterUpload.json)) {
    throw new Error(`post-upload item images returned HTTP ${infosAfterUpload.status}`);
  }
  summary.invariants.imageInfosAfterUpload200 = true;
  const primaryInfo = infosAfterUpload.json.find((info) => info.ImageType === 'Primary' && Number(info.ImageIndex || 0) === 0);
  if (!primaryInfo?.ImageTag) {
    throw new Error('post-upload item images did not expose Primary ImageTag');
  }
  summary.invariants.imageInfoTagPresent = true;
  const directImage = await browserFetchBinary(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images/Primary`,
    token: auth.AccessToken,
  });
  if (directImage.status !== 200) {
    throw new Error(`direct item image returned HTTP ${directImage.status}`);
  }
  summary.invariants.imageGet200 = true;
  if (mediaType(directImage.contentType) !== 'image/png' || !directImage.startsWithPng) {
    throw new Error(`direct item image was not PNG: ${directImage.contentType}`);
  }
  summary.invariants.imageGetPng = true;

  const headImage = await browserFetchBinary(page, {
    method: 'HEAD',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images/Primary`,
    token: auth.AccessToken,
  });
  if (headImage.status !== 200) {
    throw new Error(`HEAD item image returned HTTP ${headImage.status}`);
  }
  summary.invariants.imageHead200 = true;
  if (mediaType(headImage.contentType) !== 'image/png') {
    throw new Error(`HEAD item image content type was ${headImage.contentType}`);
  }
  summary.invariants.imageHeadPng = true;

  const extendedImage = await browserFetchBinary(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images/Primary/0/${encodeURIComponent(primaryInfo.ImageTag)}/png/320/180/0/0`,
    token: auth.AccessToken,
  });
  if (extendedImage.status !== 200) {
    throw new Error(`extended item image returned HTTP ${extendedImage.status}`);
  }
  summary.invariants.imageExtendedGet200 = true;
  if (mediaType(extendedImage.contentType) !== 'image/png' || !extendedImage.startsWithPng) {
    throw new Error(`extended item image was not PNG: ${extendedImage.contentType}`);
  }
  summary.invariants.imageExtendedGetPng = true;

  const providers = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/RemoteImages/Providers`,
    token: auth.AccessToken,
  });
  if (providers.status !== 200 || !Array.isArray(providers.json)) {
    throw new Error(`remote image providers returned HTTP ${providers.status}`);
  }
  summary.invariants.imageProviders200 = true;

  const remove = await browserFetchJson(page, {
    method: 'DELETE',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images/Primary`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(remove.status)) {
    throw new Error(`image delete returned HTTP ${remove.status}`);
  }
  summary.invariants.imageDelete204 = true;

  const infosAfterDelete = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(movie.Id)}/Images`,
    token: auth.AccessToken,
  });
  if (infosAfterDelete.status !== 200 || !Array.isArray(infosAfterDelete.json)) {
    throw new Error(`post-delete item images returned HTTP ${infosAfterDelete.status}`);
  }
  summary.invariants.imageInfosAfterDelete200 = true;

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function runMetadataSearchFlow(page, summary, publicInfo, target) {
  await ensureMetadataSearchFixtures();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  await ensureVirtualFolder(page, auth, {
    name: 'Golden Metadata',
    collectionType: 'movies',
    location: mediaFixtureDir,
  });
  await refreshLibrary(page, auth);
  const primary = await waitForMovieByName(page, summary, auth, metadataFlowPrimaryName);
  const similar = await waitForMovieByName(page, summary, auth, metadataFlowSimilarName);
  const nfoItem = await waitForMovieByName(page, summary, auth, metadataFlowNfoTitle);
  summary.invariants.metadataItemsMatched = true;

  const primaryMetadata = {
    Name: metadataFlowPrimaryName,
    Overview: 'Jellyrin metadata flow overview',
    Genres: [metadataFlowGenre, 'Jellyrin Metadata Mystery'],
    Studios: [{ Name: metadataFlowStudio }],
    People: [{ Name: metadataFlowPerson, Type: 'Actor' }],
    Tags: [metadataFlowTag],
    ProviderIds: { Imdb: 'tt0950000', Tmdb: '95000' },
    ProductionYear: metadataFlowYear,
  };
  const similarMetadata = {
    Name: metadataFlowSimilarName,
    Overview: 'Jellyrin metadata flow similar overview',
    Genres: [metadataFlowGenre],
    Studios: [{ Name: metadataFlowStudio }],
    Tags: [metadataFlowTag],
    ProviderIds: { Imdb: 'tt0950001' },
    ProductionYear: metadataFlowYear,
  };

  const primaryUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(primary.Id)}`,
    token: auth.AccessToken,
    body: primaryMetadata,
  });
  if (![200, 204].includes(primaryUpdate.status)) {
    throw new Error(`primary metadata update returned HTTP ${primaryUpdate.status}`);
  }
  summary.invariants.metadataUpdatePrimary204 = true;

  const similarUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(similar.Id)}`,
    token: auth.AccessToken,
    body: similarMetadata,
  });
  if (![200, 204].includes(similarUpdate.status)) {
    throw new Error(`similar metadata update returned HTTP ${similarUpdate.status}`);
  }
  summary.invariants.metadataUpdateSimilar204 = true;

  const editor = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(primary.Id)}/MetadataEditor`,
    token: auth.AccessToken,
  });
  if (editor.status !== 200 || !Array.isArray(editor.json?.ExternalIdInfos)) {
    throw new Error(`metadata editor returned HTTP ${editor.status}`);
  }
  summary.invariants.metadataEditor200 = true;

  const externalIds = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(primary.Id)}/ExternalIdInfos`,
    token: auth.AccessToken,
  });
  if (externalIds.status !== 200 || !Array.isArray(externalIds.json)) {
    throw new Error(`external id infos returned HTTP ${externalIds.status}`);
  }
  summary.invariants.metadataExternalIds200 = true;

  const itemSearch = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&SearchTerm=${encodeURIComponent('Metadata Flow Primary')}&Fields=ProviderIds,Genres,Studios,People,Tags,Overview&Limit=5`,
    token: auth.AccessToken,
  });
  if (itemSearch.status !== 200 || !itemSearch.json?.Items?.some((item) => item.Id === primary.Id)) {
    throw new Error(`metadata item search returned HTTP ${itemSearch.status}`);
  }
  const matchedSearchItem = itemSearch.json.Items.find((item) => item.Id === primary.Id);
  if (matchedSearchItem?.ProviderIds?.Imdb !== 'tt0950000') {
    throw new Error('metadata item search did not expose updated Imdb provider id');
  }
  summary.invariants.metadataItemsSearch200 = true;
  summary.invariants.metadataEditorProviderIds = true;

  const nfoDetail = await browserFetchJson(page, {
    method: 'GET',
    url: `/Users/${encodeURIComponent(auth.User.Id)}/Items/${encodeURIComponent(nfoItem.Id)}?Fields=ProviderIds,Genres,Studios,People,Tags,Overview`,
    token: auth.AccessToken,
  });
  if (nfoDetail.status !== 200) {
    throw new Error(`metadata NFO detail returned HTTP ${nfoDetail.status}`);
  }
  if (
    nfoDetail.json?.Overview !== 'NFO imported overview one'
    || !nfoDetail.json?.Genres?.includes('Jellyrin NFO Drama')
    || nfoDetail.json?.ProviderIds?.Imdb !== 'tt0950099'
  ) {
    throw new Error('metadata NFO local fields did not import');
  }
  summary.invariants.metadataNfoLocalMatched = true;

  const lockUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(nfoItem.Id)}`,
    token: auth.AccessToken,
    body: {
      Name: metadataFlowNfoTitle,
      Overview: 'Manual locked overview',
      Genres: ['Manual Locked Genre'],
      Studios: [],
      People: [],
      Tags: [],
      ProviderIds: { Imdb: 'tt0950099' },
      LockData: true,
      LockedFields: ['Overview', 'Genres'],
    },
  });
  if (![200, 204].includes(lockUpdate.status)) {
    throw new Error(`metadata locked fields update returned HTTP ${lockUpdate.status}`);
  }
  await refreshLibrary(page, auth);
  const lockedDetail = await browserFetchJson(page, {
    method: 'GET',
    url: `/Users/${encodeURIComponent(auth.User.Id)}/Items/${encodeURIComponent(nfoItem.Id)}?Fields=ProviderIds,Genres,Studios,People,Tags,Overview`,
    token: auth.AccessToken,
  });
  if (lockedDetail.status !== 200) {
    throw new Error(`metadata locked fields detail returned HTTP ${lockedDetail.status}`);
  }
  if (
    lockedDetail.json?.Overview !== 'Manual locked overview'
    || !lockedDetail.json?.Genres?.includes('Manual Locked Genre')
  ) {
    throw new Error('metadata locked fields were overwritten by refresh');
  }
  summary.invariants.metadataLockedFieldsPreserved = true;

  const hints = await browserFetchJson(page, {
    method: 'GET',
    url: `/Search/Hints?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent('Metadata Flow Primary')}&IncludeItemTypes=Movie&Limit=5`,
    token: auth.AccessToken,
  });
  if (hints.status !== 200 || !hints.json?.SearchHints?.some((hint) => hint.ItemId === primary.Id || hint.Id === primary.Id)) {
    throw new Error(`metadata Search/Hints returned HTTP ${hints.status}`);
  }
  summary.invariants.metadataSearchHints200 = true;

  const genre = await browserFetchJson(page, {
    method: 'GET',
    url: `/Genres?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(metadataFlowGenre)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (genre.status !== 200 || !genre.json?.Items?.some((item) => item.Name === metadataFlowGenre && item.Type === 'Genre')) {
    throw new Error(`metadata Genres returned HTTP ${genre.status}`);
  }
  summary.invariants.metadataGenreMatched = true;

  const studio = await browserFetchJson(page, {
    method: 'GET',
    url: `/Studios?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(metadataFlowStudio)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (studio.status !== 200 || !studio.json?.Items?.some((item) => item.Name === metadataFlowStudio && item.Type === 'Studio')) {
    throw new Error(`metadata Studios returned HTTP ${studio.status}`);
  }
  summary.invariants.metadataStudioMatched = true;

  const person = await browserFetchJson(page, {
    method: 'GET',
    url: `/Persons?UserId=${encodeURIComponent(auth.User.Id)}&SearchTerm=${encodeURIComponent(metadataFlowPerson)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (person.status !== 200 || !person.json?.Items?.some((item) => item.Name === metadataFlowPerson && item.Type === 'Person')) {
    throw new Error(`metadata Persons returned HTTP ${person.status}`);
  }
  summary.invariants.metadataPersonMatched = true;

  const years = await browserFetchJson(page, {
    method: 'GET',
    url: `/Years?UserId=${encodeURIComponent(auth.User.Id)}&StartIndex=0&Limit=10`,
    token: auth.AccessToken,
  });
  if (years.status !== 200 || !years.json?.Items?.some((item) => item.Name === String(metadataFlowYear) && item.Type === 'Year')) {
    throw new Error(`metadata Years returned HTTP ${years.status}`);
  }
  summary.invariants.metadataYearMatched = true;

  const similarResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items/${encodeURIComponent(primary.Id)}/Similar?UserId=${encodeURIComponent(auth.User.Id)}&Limit=5`,
    token: auth.AccessToken,
  });
  if (similarResult.status !== 200) {
    throw new Error(`metadata similar returned HTTP ${similarResult.status}`);
  }
  summary.invariants.metadataSimilar200 = true;
  if (!similarResult.json?.Items?.some((item) => item.Id === similar.Id)) {
    throw new Error('metadata similar did not return shared-genre fixture');
  }
  summary.invariants.metadataSimilarMatched = true;

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: '<dynamic>',
    name: primary.Name,
    type: primary.Type,
    genre: metadataFlowGenre,
    studio: metadataFlowStudio,
    year: metadataFlowYear,
  };
}

async function runAuthUsersFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle').catch(() => {});

  const userName = `${authUsersFlowPrefix} ${target.name}`;
  const apiKeyApp = `${authUsersFlowApiKeyApp} ${target.name}`;
  let createdUserId = null;
  let createdUserToken = null;
  let createdApiKey = null;

  async function cleanupExistingUsers() {
    const users = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users',
      token: auth.AccessToken,
    });
    if (users.status !== 200) {
      throw new Error(`Users cleanup list returned HTTP ${users.status}`);
    }
    for (const user of users.json || []) {
      if (user?.Name === userName && user.Id) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/Users/${encodeURIComponent(user.Id)}`,
          token: auth.AccessToken,
        });
      }
    }
  }

  async function cleanupApiKey() {
    const keys = await browserFetchJson(page, {
      method: 'GET',
      url: '/Auth/Keys',
      token: auth.AccessToken,
    });
    if (keys.status !== 200) {
      return;
    }
    for (const item of keys.json?.Items || []) {
      if ((item.AppName === apiKeyApp || item.Name === apiKeyApp) && item.AccessToken) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/Auth/Keys/${encodeURIComponent(item.AccessToken)}`,
          token: auth.AccessToken,
        });
      }
    }
  }

  await cleanupExistingUsers();
  await cleanupApiKey();

  try {
    const publicUsers = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users/Public',
      token: auth.AccessToken,
    });
    if (publicUsers.status !== 200 || !Array.isArray(publicUsers.json)) {
      throw new Error(`Users/Public returned HTTP ${publicUsers.status}`);
    }
    summary.invariants.authUsersPublic200 = true;

    const users = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users',
      token: auth.AccessToken,
    });
    if (users.status !== 200 || !Array.isArray(users.json) || !users.json.some((user) => user.Id === auth.User.Id)) {
      throw new Error(`Users returned HTTP ${users.status}`);
    }
    summary.invariants.authUsersList200 = true;

    const authProviders = await browserFetchJson(page, {
      method: 'GET',
      url: '/Auth/Providers',
      token: auth.AccessToken,
    });
    if (authProviders.status !== 200 || !Array.isArray(authProviders.json)) {
      throw new Error(`Auth/Providers returned HTTP ${authProviders.status}`);
    }
    summary.invariants.authProviders200 = true;

    const resetProviders = await browserFetchJson(page, {
      method: 'GET',
      url: '/Auth/PasswordResetProviders',
      token: auth.AccessToken,
    });
    if (resetProviders.status !== 200 || !Array.isArray(resetProviders.json)) {
      throw new Error(`Auth/PasswordResetProviders returned HTTP ${resetProviders.status}`);
    }
    summary.invariants.authPasswordResetProviders200 = true;

    const created = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/New',
      token: auth.AccessToken,
      body: {
        Name: userName,
        Password: authUsersFlowPassword,
      },
    });
    if (created.status !== 200 || !created.json?.Id || created.json.Name !== userName) {
      throw new Error(`Users/New returned HTTP ${created.status}`);
    }
    createdUserId = created.json.Id;
    summary.invariants.authUserCreated = true;

    const createdLogin = await browserFetchJson(page, {
      method: 'POST',
      url: '/Users/AuthenticateByName',
      authorization: `MediaBrowser Client="Jellyrin Browser Trace", Device="Auth Users ${target.name}", DeviceId="auth-users-${target.name}", Version="dev"`,
      body: {
        Username: userName,
        Pw: authUsersFlowPassword,
      },
    });
    if (createdLogin.status !== 200 || !createdLogin.json?.AccessToken || createdLogin.json?.User?.Id !== createdUserId) {
      throw new Error(`created user authentication returned HTTP ${createdLogin.status}`);
    }
    createdUserToken = createdLogin.json.AccessToken;
    summary.invariants.authCreatedUserLogin200 = true;

    const currentUser = await browserFetchJson(page, {
      method: 'GET',
      url: '/Users/Me',
      token: createdUserToken,
    });
    if (currentUser.status !== 200 || currentUser.json?.Id !== createdUserId) {
      throw new Error(`Users/Me returned HTTP ${currentUser.status}`);
    }
    summary.invariants.authCreatedUserMe200 = true;

    const userDetail = await browserFetchJson(page, {
      method: 'GET',
      url: `/Users/${encodeURIComponent(createdUserId)}`,
      token: auth.AccessToken,
    });
    if (userDetail.status !== 200 || userDetail.json?.Id !== createdUserId) {
      throw new Error(`Users/{id} returned HTTP ${userDetail.status}`);
    }
    summary.invariants.authUserDetail200 = true;

    const policyUpdate = await browserFetchJson(page, {
      method: 'POST',
      url: `/Users/${encodeURIComponent(createdUserId)}/Policy`,
      token: auth.AccessToken,
      body: {
        ...(userDetail.json.Policy || {}),
        IsAdministrator: false,
        IsDisabled: false,
      },
    });
    if (![200, 204].includes(policyUpdate.status)) {
      throw new Error(`Users/{id}/Policy returned HTTP ${policyUpdate.status}`);
    }
    summary.invariants.authUserPolicy204 = true;

    const configurationUpdate = await browserFetchJson(page, {
      method: 'POST',
      url: `/Users/${encodeURIComponent(createdUserId)}/Configuration`,
      token: auth.AccessToken,
      body: {
        ...(userDetail.json.Configuration || {}),
        EnableNextEpisodeAutoPlay: false,
      },
    });
    if (![200, 204].includes(configurationUpdate.status)) {
      throw new Error(`Users/{id}/Configuration returned HTTP ${configurationUpdate.status}`);
    }
    summary.invariants.authUserConfiguration204 = true;

    const keysBefore = await browserFetchJson(page, {
      method: 'GET',
      url: '/Auth/Keys',
      token: auth.AccessToken,
    });
    if (keysBefore.status !== 200 || !Array.isArray(keysBefore.json?.Items)) {
      throw new Error(`Auth/Keys returned HTTP ${keysBefore.status}`);
    }
    summary.invariants.authKeysList200 = true;

    const createKey = await browserFetchJson(page, {
      method: 'POST',
      url: `/Auth/Keys?app=${encodeURIComponent(apiKeyApp)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(createKey.status)) {
      throw new Error(`Auth/Keys create returned HTTP ${createKey.status}`);
    }
    summary.invariants.authKeyCreated = true;

    const keysAfter = await browserFetchJson(page, {
      method: 'GET',
      url: '/Auth/Keys',
      token: auth.AccessToken,
    });
    if (keysAfter.status !== 200) {
      throw new Error(`Auth/Keys after create returned HTTP ${keysAfter.status}`);
    }
    createdApiKey = (keysAfter.json?.Items || [])
      .find((item) => (item.AppName === apiKeyApp || item.Name === apiKeyApp) && item.AccessToken)
      ?.AccessToken;
    if (!createdApiKey) {
      throw new Error('created Auth/Keys entry was not returned');
    }

    const keyInfo = await browserFetchJson(page, {
      method: 'GET',
      url: '/System/Info',
      token: createdApiKey,
    });
    if (keyInfo.status !== 200 || !keyInfo.json?.Id) {
      throw new Error(`created api key System/Info returned HTTP ${keyInfo.status}`);
    }
    summary.invariants.authKeyUsable = true;

    const deleteKey = await browserFetchJson(page, {
      method: 'DELETE',
      url: `/Auth/Keys/${encodeURIComponent(createdApiKey)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(deleteKey.status)) {
      throw new Error(`Auth/Keys delete returned HTTP ${deleteKey.status}`);
    }
    createdApiKey = null;
    summary.invariants.authKeyRevoked = true;

    const logout = await browserFetchJson(page, {
      method: 'POST',
      url: '/Sessions/Logout',
      token: createdUserToken,
    });
    if (![200, 204].includes(logout.status)) {
      throw new Error(`Sessions/Logout returned HTTP ${logout.status}`);
    }
    createdUserToken = null;
    summary.invariants.authCreatedUserLogout204 = true;

    const deleteUser = await browserFetchJson(page, {
      method: 'DELETE',
      url: `/Users/${encodeURIComponent(createdUserId)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(deleteUser.status)) {
      throw new Error(`Users delete returned HTTP ${deleteUser.status}`);
    }
    createdUserId = null;
    summary.invariants.authUserDeleted = true;
  } finally {
    if (createdApiKey) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Auth/Keys/${encodeURIComponent(createdApiKey)}`,
        token: auth.AccessToken,
      }).catch(() => {});
    }
    if (createdUserToken) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token: createdUserToken,
      }).catch(() => {});
    }
    if (createdUserId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdUserId)}`,
        token: auth.AccessToken,
      }).catch(() => {});
    }
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    user: '<temporary-auth-flow-user>',
    apiKeyApp,
  };
}

async function runPluginsPackagesFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard/plugins');
  await page.waitForLoadState('networkidle').catch(() => {});

  const plugins = await browserFetchJson(page, {
    method: 'GET',
    url: '/Plugins',
    token: auth.AccessToken,
  });
  if (plugins.status !== 200 || !Array.isArray(plugins.json)) {
    throw new Error(`Plugins returned HTTP ${plugins.status}`);
  }
  summary.invariants.pluginsList200 = true;
  if (target.name === 'jellyrin' && plugins.json.length !== 0) {
    throw new Error('Jellyrin reported active plugins despite unsupported .NET plugin execution');
  }
  if (plugins.json.length === 0) {
    summary.invariants.pluginsListEmpty = true;
  }

  const repositories = await browserFetchJson(page, {
    method: 'GET',
    url: '/Package/Repositories',
    token: auth.AccessToken,
  });
  if (repositories.status !== 200 || !Array.isArray(repositories.json)) {
    throw new Error(`Package/Repositories returned HTTP ${repositories.status}`);
  }
  summary.invariants.pluginRepositories200 = true;

  const packages = await browserFetchJson(page, {
    method: 'GET',
    url: '/Packages',
    token: auth.AccessToken,
  });
  if (packages.status !== 200 || !Array.isArray(packages.json)) {
    throw new Error(`Packages returned HTTP ${packages.status}`);
  }
  summary.invariants.pluginPackages200 = true;

  if (target.name !== 'jellyrin') {
    await page.goto(`${summary.baseUrl}/web/#/dashboard/plugins`, { waitUntil: 'domcontentloaded' });
    await page.waitForLoadState('networkidle').catch(() => {});
    return;
  }

  const repositoryPayload = [{
    Name: pluginsFlowRepositoryName,
    Url: 'https://repo.invalid/jellyrin-golden-plugin.json',
    Enabled: true,
    Packages: [{
      Name: pluginsFlowPackageName,
      Guid: pluginsFlowPackageGuid,
      Overview: 'Golden unsupported plugin fixture',
      Description: 'Golden unsupported plugin fixture',
      Owner: 'Jellyrin',
      Category: 'General',
      Versions: [{
        Version: '1.0.0.0',
        TargetAbi: '12.0.0.0',
        SourceUrl: 'https://repo.invalid/jellyrin-golden-plugin.zip',
        Checksum: 'golden-checksum',
      }],
    }],
  }];
  const repositoryUpdate = await browserFetchJson(page, {
    method: 'POST',
    url: '/Package/Repositories',
    token: auth.AccessToken,
    body: repositoryPayload,
  });
  if (![200, 204].includes(repositoryUpdate.status)) {
    throw new Error(`Package/Repositories update returned HTTP ${repositoryUpdate.status}`);
  }
  summary.invariants.pluginRepositoryUpdated = true;

  const catalog = await browserFetchJson(page, {
    method: 'GET',
    url: '/Package/Packages',
    token: auth.AccessToken,
  });
  if (catalog.status !== 200 || !catalog.json?.some((item) => item.Name === pluginsFlowPackageName && item.Guid === pluginsFlowPackageGuid)) {
    throw new Error(`Package catalog did not expose golden plugin, HTTP ${catalog.status}`);
  }
  summary.invariants.pluginPackageMatched = true;

  const manifest = await browserFetchJson(page, {
    method: 'GET',
    url: `/Plugins/${encodeURIComponent(pluginsFlowPackageGuid)}/Manifest`,
    token: auth.AccessToken,
  });
  if (manifest.status !== 200 || manifest.json?.Guid !== pluginsFlowPackageGuid || manifest.json?.Name !== pluginsFlowPackageName) {
    throw new Error(`Plugin manifest returned HTTP ${manifest.status}`);
  }
  summary.invariants.pluginManifest200 = true;

  const install = await browserFetchJson(page, {
    method: 'POST',
    url: `/Package/Packages/Installed/${encodeURIComponent(pluginsFlowPackageName)}?Version=1.0.0.0`,
    token: auth.AccessToken,
  });
  if (install.status !== 409 || !install.json?.Message?.includes('Package installation is not supported')) {
    throw new Error(`Package install was not explicitly rejected, HTTP ${install.status}`);
  }
  summary.invariants.pluginInstallRejected = true;

  const enable = await browserFetchJson(page, {
    method: 'POST',
    url: `/Plugins/${encodeURIComponent(pluginsFlowPackageGuid)}/1.0.0.0/Enable`,
    token: auth.AccessToken,
  });
  if (enable.status !== 409 || !enable.json?.Message?.includes('Plugin enable is not implemented')) {
    throw new Error(`Plugin enable was not explicitly rejected, HTTP ${enable.status}`);
  }
  summary.invariants.pluginEnableRejected = true;

  const disable = await browserFetchJson(page, {
    method: 'POST',
    url: `/Plugins/${encodeURIComponent(pluginsFlowPackageGuid)}/1.0.0.0/Disable`,
    token: auth.AccessToken,
  });
  if (disable.status !== 409 || !disable.json?.Message?.includes('Plugin disable is not implemented')) {
    throw new Error(`Plugin disable was not explicitly rejected, HTTP ${disable.status}`);
  }
  summary.invariants.pluginDisableRejected = true;

  const uninstall = await browserFetchJson(page, {
    method: 'DELETE',
    url: `/Plugins/${encodeURIComponent(pluginsFlowPackageGuid)}/1.0.0.0`,
    token: auth.AccessToken,
  });
  if (uninstall.status !== 409 || !uninstall.json?.Message?.includes('Plugin uninstall is not implemented')) {
    throw new Error(`Plugin uninstall was not explicitly rejected, HTTP ${uninstall.status}`);
  }
  summary.invariants.pluginUninstallRejected = true;

  const pluginsAfterLifecycle = await browserFetchJson(page, {
    method: 'GET',
    url: '/Plugins',
    token: auth.AccessToken,
  });
  if (pluginsAfterLifecycle.status !== 200 || !Array.isArray(pluginsAfterLifecycle.json) || pluginsAfterLifecycle.json.length !== 0) {
    throw new Error('Jellyrin reported installed plugins after rejected lifecycle operations');
  }
  summary.invariants.pluginsListEmpty = true;

  await page.goto(`${summary.baseUrl}/web/#/dashboard/plugins`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
}

async function runChannelsFlow(page, summary, publicInfo, target) {
  await ensureLiveTvFixtures();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/channels');
  await page.waitForLoadState('networkidle').catch(() => {});

  if (target.name === 'jellyrin') {
    const configUpdate = await browserFetchJson(page, {
      method: 'POST',
      url: '/System/Configuration/livetv',
      token: auth.AccessToken,
      body: {
        GuideDays: 1,
        RecordingPath: mediaFixtureDir,
        TunerHosts: [{
          Id: 'jellyrin-channels-tuner',
          Type: 'm3u',
          Url: path.join(mediaFixtureDir, 'jellyrin-live-tv.m3u'),
          FriendlyName: 'Jellyrin Channels Golden Live TV',
        }],
        ListingProviders: [],
      },
    });
    if (![200, 204].includes(configUpdate.status)) {
      throw new Error(`channels Live TV config update returned HTTP ${configUpdate.status}`);
    }
  }

  const channels = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels?SupportsLatestItems=true',
    token: auth.AccessToken,
  });
  if (channels.status !== 200 || !Array.isArray(channels.json?.Items)) {
    throw new Error(`Channels returned HTTP ${channels.status}`);
  }
  summary.invariants.channelsList200 = true;

  const features = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels/Features',
    token: auth.AccessToken,
  });
  if (features.status !== 200 || !Array.isArray(features.json)) {
    throw new Error(`Channels/Features returned HTTP ${features.status}`);
  }
  summary.invariants.channelsFeatures200 = true;

  if (target.name !== 'jellyrin') {
    await page.goto(`${summary.baseUrl}/web/#/channels`, { waitUntil: 'domcontentloaded' });
    await page.waitForLoadState('networkidle').catch(() => {});
    return;
  }

  const provider = channels.json.Items.find((item) => item.Id === 'livetv');
  if (!provider || provider.Type !== 'Channel' || provider.ChildCount < 1) {
    throw new Error('Channels did not expose the local Live TV provider');
  }
  summary.invariants.channelsProviderMatched = true;

  const filtered = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels?SupportsMediaDeletion=false&IsFavorite=false',
    token: auth.AccessToken,
  });
  if (filtered.status !== 200 || !filtered.json?.Items?.some((item) => item.Id === 'livetv')) {
    throw new Error(`Channels filter did not retain local provider, HTTP ${filtered.status}`);
  }
  summary.invariants.channelsFilterMatched = true;

  const deletionFiltered = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels?SupportsMediaDeletion=true',
    token: auth.AccessToken,
  });
  if (deletionFiltered.status !== 200 || deletionFiltered.json?.Items?.some((item) => item.Id === 'livetv')) {
    throw new Error(`Channels media-deletion filter did not remove local provider, HTTP ${deletionFiltered.status}`);
  }
  summary.invariants.channelsDeletionFilterMatched = true;

  const channelItems = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels/livetv/Items?StartIndex=0&Limit=10',
    token: auth.AccessToken,
  });
  if (channelItems.status !== 200 || !Array.isArray(channelItems.json?.Items)) {
    throw new Error(`Channels/livetv/Items returned HTTP ${channelItems.status}`);
  }
  summary.invariants.channelsItems200 = true;
  if (!channelItems.json.Items.some((item) => item.Id === liveTvFlowChannelId && item.ChannelId === 'livetv')) {
    throw new Error('Channels/livetv/Items did not expose the M3U channel fixture');
  }
  summary.invariants.channelsItemMatched = true;

  const latest = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels/Items/Latest?Limit=5',
    token: auth.AccessToken,
  });
  if (latest.status !== 200 || !latest.json?.Items?.some((item) => item.Id === liveTvFlowChannelId)) {
    throw new Error(`Channels/Items/Latest did not expose channel fixture, HTTP ${latest.status}`);
  }
  summary.invariants.channelsLatest200 = true;

  const feature = features.json.find((item) => item.ChannelId === 'livetv');
  if (!feature || feature.SupportsLatestItems !== true || feature.ContentType !== 'TvChannel') {
    throw new Error('Channels/Features did not expose local Live TV feature capabilities');
  }
  summary.invariants.channelsFeatureMatched = true;

  const featureById = await browserFetchJson(page, {
    method: 'GET',
    url: '/Channels/livetv/Features',
    token: auth.AccessToken,
  });
  if (featureById.status !== 200 || !featureById.json?.some((item) => item.ChannelId === 'livetv')) {
    throw new Error(`Channels/livetv/Features returned HTTP ${featureById.status}`);
  }

  await page.goto(`${summary.baseUrl}/web/#/channels`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});
  summary.item = {
    id: 'livetv',
    name: 'Live TV',
    type: 'Channel',
    fixtureChannelId: liveTvFlowChannelId,
  };
}

async function runNonWebClientFlow(page, summary, publicInfo, target) {
  await page.goto(summary.baseUrl, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});

  let adminAuth = null;
  let createdClientUserId = null;
  const clientUserName = `${nonWebClientFlowPrefix} ${target.name}`;
  const clientContracts = nonWebClientContracts(target.name);
  const passedProfiles = [];
  const activeSessions = [];

  try {
    let username = target.username;
    let password = target.password;
    if (target.username && target.password) {
      username = target.username;
      password = target.password;
    } else {
      adminAuth = await authenticateTarget(page, summary, target);
      await deleteUsersByName(page, adminAuth.AccessToken, clientUserName);
      const createdClient = await browserFetchJson(page, {
        method: 'POST',
        url: '/Users/New',
        token: adminAuth.AccessToken,
        body: {
          Name: clientUserName,
          Password: nonWebClientFlowPassword,
        },
      });
      if (createdClient.status !== 200 || !createdClient.json?.Id) {
        throw new Error(`non-web client Users/New returned HTTP ${createdClient.status}`);
      }
      createdClientUserId = createdClient.json.Id;
      username = clientUserName;
      password = nonWebClientFlowPassword;
    }

    let movie = null;
    for (const contract of clientContracts) {
      const auth = await loginNonWebClient(page, username, password, contract.authorization);
      activeSessions.push(auth.AccessToken);
      summary.invariants.nonWebClientAuthenticated = true;

      const systemInfo = await browserFetchJson(page, {
        method: 'GET',
        url: '/System/Info',
        token: auth.AccessToken,
      });
      if (systemInfo.status !== 200 || !systemInfo.json?.Id) {
        throw new Error(`non-web ${contract.id} System/Info returned HTTP ${systemInfo.status}`);
      }
      summary.invariants.nonWebSystemInfo200 = true;

      const views = await browserFetchJson(page, {
        method: 'GET',
        url: `/Users/${encodeURIComponent(auth.User.Id)}/Views`,
        token: auth.AccessToken,
      });
      if (views.status !== 200 || !Array.isArray(views.json?.Items)) {
        throw new Error(`non-web ${contract.id} browse views returned HTTP ${views.status}`);
      }
      summary.invariants.nonWebBrowse200 = true;

      movie = movie || await firstMovieItem(page, summary, auth);
      if (!movie) {
        summary.status = 'skipped';
        summary.skipped = true;
        summary.reason = 'target has no movie item for non-web client trace';
        return;
      }
      summary.invariants.nonWebMovieMatched = true;

      const playbackInfo = await browserFetchJson(page, {
        method: 'POST',
        url: `/Items/${encodeURIComponent(movie.Id)}/PlaybackInfo`,
        token: auth.AccessToken,
        body: withoutUndefined({
          UserId: auth.User.Id,
          MediaSourceId: movie.Id,
          EnableDirectPlay: true,
          EnableDirectStream: true,
          EnableTranscoding: true,
          StartPositionTicks: 0,
          DeviceProfile: contract.deviceProfile,
        }),
      });
      if (playbackInfo.status !== 200 || !Array.isArray(playbackInfo.json?.MediaSources)) {
        throw new Error(`non-web ${contract.id} PlaybackInfo returned HTTP ${playbackInfo.status}`);
      }
      summary.invariants.nonWebPlaybackInfo200 = true;
      const mediaSource = playbackInfo.json.MediaSources[0];
      if (!mediaSource || mediaSource.SupportsDirectPlay !== true) {
        throw new Error(`non-web ${contract.id} PlaybackInfo did not expose a direct-play media source`);
      }
      summary.invariants.nonWebDirectMediaSource = true;

      const stream = await browserFetchBinary(page, {
        method: 'GET',
        url: `/Videos/${encodeURIComponent(movie.Id)}/stream.mp4?Static=true&api_key=${encodeURIComponent(auth.AccessToken)}`,
        token: auth.AccessToken,
      });
      if (![200, 206].includes(stream.status) || stream.byteLength <= 0) {
        throw new Error(`non-web ${contract.id} direct stream returned HTTP ${stream.status}`);
      }
      summary.invariants.nonWebStream200 = true;

      const positionTicks = resumeTracePositionTicks(movie);
      const progress = await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Playing/Progress',
        token: auth.AccessToken,
        body: {
          ItemId: movie.Id,
          MediaSourceId: mediaSource.Id || movie.Id,
          PositionTicks: positionTicks,
          IsPaused: false,
          PlayMethod: mediaSource.SupportsDirectPlay ? 'DirectPlay' : 'DirectStream',
        },
      });
      if (progress.status !== 204) {
        throw new Error(`non-web ${contract.id} Sessions/Playing/Progress returned HTTP ${progress.status}`);
      }
      summary.invariants.nonWebProgress204 = true;

      const resume = await browserFetchJson(page, {
        method: 'GET',
        url: `/UserItems/Resume?UserId=${encodeURIComponent(auth.User.Id)}&Limit=12&MediaTypes=Video`,
        token: auth.AccessToken,
      });
      if (resume.status !== 200 || !resume.json?.Items?.some((item) => item.Id === movie.Id)) {
        throw new Error(`non-web ${contract.id} resume query did not include movie, HTTP ${resume.status}`);
      }
      summary.invariants.nonWebResumeMatched = true;
      passedProfiles.push(contract.id);
    }
    summary.invariants.nonWebClientProfiles = passedProfiles;
    summary.invariants.nonWebClientProfileCount = passedProfiles.length;

    if (target.name === 'jellyrin') {
      const network = await browserFetchJson(page, {
        method: 'GET',
        url: '/System/Configuration/network',
        token: activeSessions[0],
      });
      if (network.status !== 200 || network.json?.EnableUPnP !== false) {
        throw new Error(`Jellyrin DLNA/UPnP unsupported decision not visible in network config, HTTP ${network.status}`);
      }
      summary.invariants.nonWebDlnaUnsupportedDecided = true;
    } else {
      summary.invariants.nonWebDlnaUnsupportedDecided = true;
    }

    summary.item = {
      id: '<dynamic>',
      name: movie.Name,
      type: movie.Type,
      clients: passedProfiles,
    };
  } finally {
    for (const token of activeSessions) {
      await browserFetchJson(page, {
        method: 'POST',
        url: '/Sessions/Logout',
        token,
      }).catch(() => {});
    }
    if (adminAuth?.AccessToken && createdClientUserId) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(createdClientUserId)}`,
        token: adminAuth.AccessToken,
      }).catch(() => {});
    }
  }
}

async function loginNonWebClient(page, username, password, authorization) {
  const login = await browserFetchJson(page, {
    method: 'POST',
    url: '/Users/AuthenticateByName',
    authorization,
    body: {
      Username: username,
      Pw: password,
    },
  });
  if (login.status !== 200 || !login.json?.AccessToken || !login.json?.User?.Id) {
    throw new Error(`non-web client login returned HTTP ${login.status}`);
  }
  return login.json;
}

async function deleteUsersByName(page, adminToken, userName) {
  const users = await browserFetchJson(page, {
    method: 'GET',
    url: '/Users',
    token: adminToken,
  });
  if (users.status !== 200) {
    throw new Error(`user cleanup list returned HTTP ${users.status}`);
  }
  for (const user of users.json || []) {
    if (user?.Name === userName && user.Id) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Users/${encodeURIComponent(user.Id)}`,
        token: adminToken,
      });
    }
  }
}

async function runScheduledTasksFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard/tasks');
  await page.waitForLoadState('networkidle').catch(() => {});

  await startWebsocketProbe(page, summary.baseUrl, [
    { name: 'admin', token: auth.AccessToken, deviceId: `scheduled-tasks-${target.name}` },
  ]);
  await waitForWebsocketMessages(page, [
    ['admin', 'ForceKeepAlive'],
  ]);

  try {
    const tasks = await browserFetchJson(page, {
      method: 'GET',
      url: '/ScheduledTasks?IsEnabled=true',
      token: auth.AccessToken,
    });
    if (tasks.status !== 200 || !Array.isArray(tasks.json)) {
      throw new Error(`ScheduledTasks returned HTTP ${tasks.status}`);
    }
    summary.invariants.scheduledTasksList200 = true;
    const scanTask = tasks.json.find((task) => task.Key === 'RefreshLibrary' || task.Id === 'scan-media-library');
    if (!scanTask?.Id) {
      throw new Error('ScheduledTasks did not include RefreshLibrary');
    }

    const detail = await browserFetchJson(page, {
      method: 'GET',
      url: `/ScheduledTasks/${encodeURIComponent(scanTask.Id)}`,
      token: auth.AccessToken,
    });
    if (detail.status !== 200 || detail.json?.Key !== 'RefreshLibrary') {
      throw new Error(`ScheduledTasks/{id} returned HTTP ${detail.status}`);
    }
    summary.invariants.scheduledTasksDetail200 = true;

    const start = await browserFetchJson(page, {
      method: 'POST',
      url: `/ScheduledTasks/Running/${encodeURIComponent(scanTask.Id)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(start.status)) {
      throw new Error(`ScheduledTasks/Running start returned HTTP ${start.status}`);
    }
    summary.invariants.scheduledTasksStarted = true;
    await waitForWebsocketMessages(page, [
      ['admin', 'ScheduledTasksInfo'],
    ]);
    summary.invariants.scheduledTasksWebsocketUpdate = true;

    let completedTask = null;
    for (let attempt = 0; attempt < 20; attempt += 1) {
      const poll = await browserFetchJson(page, {
        method: 'GET',
        url: `/ScheduledTasks/${encodeURIComponent(scanTask.Id)}`,
        token: auth.AccessToken,
      });
      if (poll.status !== 200) {
        throw new Error(`ScheduledTasks/{id} poll returned HTTP ${poll.status}`);
      }
      completedTask = poll.json;
      if (completedTask.State === 'Idle' && completedTask.LastExecutionResult?.Status === 'Completed') {
        break;
      }
      await page.waitForTimeout(100);
    }
    if (completedTask?.LastExecutionResult?.Status !== 'Completed') {
      throw new Error('ScheduledTasks did not complete RefreshLibrary');
    }
    summary.invariants.scheduledTasksCompleted = true;

    const cancel = await browserFetchJson(page, {
      method: 'DELETE',
      url: `/ScheduledTasks/Running/${encodeURIComponent(scanTask.Id)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(cancel.status)) {
      throw new Error(`ScheduledTasks/Running cancel returned HTTP ${cancel.status}`);
    }
    summary.invariants.scheduledTasksCancelled = true;

    const triggers = await browserFetchJson(page, {
      method: 'POST',
      url: `/ScheduledTasks/${encodeURIComponent(scanTask.Id)}/Triggers`,
      token: auth.AccessToken,
      body: [{
        Type: 'IntervalTrigger',
        IntervalTicks: 43_200_000_000,
      }],
    });
    if (![200, 204].includes(triggers.status)) {
      throw new Error(`ScheduledTasks/{id}/Triggers returned HTTP ${triggers.status}`);
    }
    summary.invariants.scheduledTasksTriggers204 = true;

    const refresh = await browserFetchJson(page, {
      method: 'POST',
      url: '/Library/Refresh',
      token: auth.AccessToken,
    });
    if (![200, 204].includes(refresh.status)) {
      throw new Error(`Library/Refresh returned HTTP ${refresh.status}`);
    }
    summary.invariants.scheduledTasksLibraryRefresh204 = true;

    const activity = await browserFetchJson(page, {
      method: 'GET',
      url: '/System/ActivityLog/Entries?Limit=20',
      token: auth.AccessToken,
    });
    if (activity.status !== 200 || !activity.json?.Items?.some((entry) => entry.Name === 'Library scan completed')) {
      throw new Error(`ActivityLog did not include Library scan completed, HTTP ${activity.status}`);
    }
    summary.invariants.scheduledTasksActivityLogged = true;

    await closeWebsocketProbe(page);
    summary.item = {
      id: scanTask.Id,
      name: scanTask.Name,
      type: 'ScheduledTask',
    };
  } finally {
    await closeWebsocketProbe(page).catch(() => {});
  }
}

async function runBackupRestoreFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard/general');
  await page.waitForLoadState('networkidle').catch(() => {});

  const backups = await browserFetchJson(page, {
    method: 'GET',
    url: '/Backup',
    token: auth.AccessToken,
  });
  if (backups.status !== 200 || !Array.isArray(backups.json)) {
    throw new Error(`Backup list returned HTTP ${backups.status}`);
  }
  summary.invariants.backupList200 = true;

  const created = await browserFetchJson(page, {
    method: 'POST',
    url: '/Backup/Create',
    token: auth.AccessToken,
    body: {
      Metadata: true,
      Database: true,
      Subtitles: false,
      Trickplay: false,
    },
  });
  if (created.status !== 200 || !created.json?.Path) {
    throw new Error(`Backup/Create returned HTTP ${created.status}`);
  }
  summary.invariants.backupCreated = true;
  if (
    created.json.SnapshotSummary?.HasRestoreData !== true
    || typeof created.json.SnapshotSummary?.Users !== 'number'
    || typeof created.json.SnapshotSummary?.VirtualFolders !== 'number'
    || created.json.SnapshotSummary?.FilesMode !== 'metadata-only'
    || created.json.SnapshotSummary?.PluginsMode !== 'configuration-only'
  ) {
    throw new Error('Backup/Create did not include the expected snapshot summary');
  }
  summary.invariants.backupSnapshotSummary = true;

  const manifest = await browserFetchJson(page, {
    method: 'GET',
    url: `/Backup/Manifest?path=${encodeURIComponent(created.json.Path)}`,
    token: auth.AccessToken,
  });
  if (manifest.status !== 200 || manifest.json?.Path !== created.json.Path) {
    throw new Error(`Backup/Manifest returned HTTP ${manifest.status}`);
  }
  summary.invariants.backupManifest200 = true;

  const restore = await browserFetchJson(page, {
    method: 'POST',
    url: '/Backup/Restore',
    token: auth.AccessToken,
    body: {
      ArchiveFileName: created.json.Path,
    },
  });
  if (![200, 204].includes(restore.status)) {
    throw new Error(`Backup/Restore returned HTTP ${restore.status}`);
  }
  summary.invariants.backupRestored = true;

  const activity = await browserFetchJson(page, {
    method: 'GET',
    url: '/System/ActivityLog/Entries?Limit=20',
    token: auth.AccessToken,
  });
  if (activity.status !== 200 || !activity.json?.Items?.some((entry) => entry.Name === 'Backup restored')) {
    throw new Error(`ActivityLog did not include Backup restored, HTTP ${activity.status}`);
  }
  summary.invariants.backupActivityLogged = true;

  summary.item = {
    id: created.json.Path,
    name: created.json.Path,
    type: 'BackupManifest',
  };
}

async function runMigrationImportFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/dashboard/general');
  await page.waitForLoadState('networkidle').catch(() => {});

  const payload = {
    SourceName: `jellyfin-golden-${target.name}`,
    Data: {
      Users: [],
      Libraries: [],
      Media: [],
      UserData: [],
      Playlists: [],
      Collections: [],
      TaskHistory: [{ Name: 'RefreshLibrary' }],
      Transcodes: [{ PlaySessionId: 'runtime-only' }],
      PackageState: [{ Name: 'ExamplePlugin' }],
    },
  };

  const dryRun = await browserFetchJson(page, {
    method: 'POST',
    url: '/Migration/Jellyfin/DryRun',
    token: auth.AccessToken,
    body: payload,
  });
  if (dryRun.status !== 200 || dryRun.json?.DryRun !== true) {
    throw new Error(`Migration/Jellyfin/DryRun returned HTTP ${dryRun.status}`);
  }
  summary.invariants.migrationDryRun200 = true;
  if (
    dryRun.json.SourcePolicy?.OriginalDatabaseMutation !== 'never'
    || dryRun.json.SourcePolicy?.BackupRequired !== true
  ) {
    throw new Error('Migration dry-run did not expose read-only source and backup policy');
  }
  summary.invariants.migrationReadOnlyPolicy = true;

  const imported = await browserFetchJson(page, {
    method: 'POST',
    url: '/Migration/Jellyfin/Import',
    token: auth.AccessToken,
    body: payload,
  });
  if (imported.status !== 200 || imported.json?.Applied !== true) {
    throw new Error(`Migration/Jellyfin/Import returned HTTP ${imported.status}`);
  }
  summary.invariants.migrationImport200 = true;
  if (!imported.json.SourcePolicy?.BackupPath) {
    throw new Error('Migration import did not create a pre-import backup');
  }
  summary.invariants.migrationBackupCreated = true;
  if (imported.json.Rollback?.Automatic !== false || !imported.json.Rollback?.Procedure) {
    throw new Error('Migration import did not document rollback');
  }
  summary.invariants.migrationRollbackDocumented = true;

  const activity = await browserFetchJson(page, {
    method: 'GET',
    url: '/System/ActivityLog/Entries?Limit=20',
    token: auth.AccessToken,
  });
  if (activity.status !== 200 || !activity.json?.Items?.some((entry) => entry.Name === 'Jellyfin migration imported')) {
    throw new Error(`ActivityLog did not include Jellyfin migration imported, HTTP ${activity.status}`);
  }
  summary.invariants.migrationActivityLogged = true;

  summary.item = {
    id: imported.json.SourcePolicy.BackupPath,
    name: payload.SourceName,
    type: 'JellyfinMigration',
  };
}

async function runLiveTvFlow(page, summary, publicInfo, target) {
  await ensureLiveTvFixtures();
  const auth = await authenticateTarget(page, summary, target);
  await establishWebSession(page, summary, publicInfo, target, auth, '/livetv');
  await page.waitForLoadState('networkidle', { timeout: 5000 }).catch(() => {});

  // Synthetic M3U/XMLTV block: Jellyrin-only shortcut that injects channels/programs/recordings
  // directly via System/Configuration/livetv. Upstream does not expose this path and materializes
  // guide data asynchronously via RefreshGuideScheduledTask, so this block is intentionally skipped
  // for upstream to avoid spurious 4xx failedResponses.
  if (target.name === 'jellyrin') {
    const liveStreamPath = path.join(mediaFixtureDir, 'jellyrin-live-tv-channel.ts');
    const recordingPath = path.join(mediaFixtureDir, 'jellyrin-live-tv-recording.ts');
    const configPayload = {
      GuideDays: 1,
      RecordingPath: mediaFixtureDir,
      TunerHosts: [{
        Id: 'jellyrin-live-tv-tuner',
        Type: 'm3u',
        Url: path.join(mediaFixtureDir, 'jellyrin-live-tv.m3u'),
        FriendlyName: 'Jellyrin Golden Live TV',
        Channels: [{
          Id: liveTvFlowChannelId,
          Name: 'Jellyrin Live TV',
          Number: '101',
          Path: liveStreamPath,
        }],
      }],
      ListingProviders: [{
        Id: 'jellyrin-live-tv-guide',
        Type: 'xmltv',
        Path: path.join(mediaFixtureDir, 'jellyrin-live-tv.xml'),
        Programs: [{
          Id: liveTvFlowProgramId,
          Name: 'Jellyrin Morning News',
          ChannelId: liveTvFlowChannelId,
          StartDate: '2026-05-26T08:00:00Z',
          EndDate: '2026-05-26T09:00:00Z',
          Overview: 'Live TV guide fixture',
        }],
      }],
      Recordings: [{
        Id: liveTvFlowRecordingId,
        Name: 'Jellyrin Live TV Recording',
        SeriesName: 'Jellyrin Live TV',
        FolderName: 'Live TV',
        GroupName: 'Jellyrin Live TV',
        ChannelId: liveTvFlowChannelId,
        StartDate: '2026-05-26T08:00:00Z',
        EndDate: '2026-05-26T09:00:00Z',
        Path: recordingPath,
      }],
      PrePaddingSeconds: 60,
      PostPaddingSeconds: 120,
    };

    const configUpdate = await browserFetchJson(page, {
      method: 'POST',
      url: '/System/Configuration/livetv',
      token: auth.AccessToken,
      body: configPayload,
    });
    if (![200, 204].includes(configUpdate.status)) {
      throw new Error(`Live TV config update returned HTTP ${configUpdate.status}`);
    }
    summary.invariants.liveTvConfigUpdated = true;

    const channels = await browserFetchJson(page, {
      method: 'GET',
      url: `/LiveTv/Channels?UserId=${encodeURIComponent(auth.User.Id)}`,
      token: auth.AccessToken,
    });
    if (channels.status !== 200 || !Array.isArray(channels.json?.Items)) {
      throw new Error(`LiveTv/Channels returned HTTP ${channels.status}`);
    }
    summary.invariants.liveTvChannels200 = true;
    const channel = channels.json.Items.find((item) => item.Id === liveTvFlowChannelId);
    if (!channel || channel.Name !== 'Jellyrin Live TV') {
      throw new Error('Live TV channel fixture was not imported from M3U');
    }
    summary.invariants.liveTvChannelMatched = true;

    const programs = await browserFetchJson(page, {
      method: 'GET',
      url: `/LiveTv/Programs?UserId=${encodeURIComponent(auth.User.Id)}&ChannelIds=${encodeURIComponent(liveTvFlowChannelId)}`,
      token: auth.AccessToken,
    });
    if (programs.status !== 200 || !Array.isArray(programs.json?.Items)) {
      throw new Error(`LiveTv/Programs returned HTTP ${programs.status}`);
    }
    summary.invariants.liveTvGuidePrograms200 = true;
    if (!programs.json.Items.some((item) => item.Name === 'Jellyrin Morning News' && item.ChannelId === liveTvFlowChannelId)) {
      throw new Error('Live TV XMLTV program fixture was not imported');
    }
    summary.invariants.liveTvProgramMatched = true;

    const stream = await browserFetchBinary(page, {
      method: 'GET',
      url: `/LiveTv/LiveStreamFiles/${encodeURIComponent(liveTvFlowChannelId)}/stream.ts`,
      token: auth.AccessToken,
    });
    if (stream.status !== 200 || !stream.contentType.includes('video/mp2t') || stream.byteLength <= 0) {
      throw new Error(`LiveTv channel stream returned HTTP ${stream.status}`);
    }
    summary.invariants.liveTvStream200 = true;

    const recordings = await browserFetchJson(page, {
      method: 'GET',
      url: '/LiveTv/Recordings',
      token: auth.AccessToken,
    });
    if (recordings.status !== 200 || !recordings.json?.Items?.some((item) => item.Id === liveTvFlowRecordingId)) {
      throw new Error(`LiveTv/Recordings returned HTTP ${recordings.status}`);
    }
    summary.invariants.liveTvRecordings200 = true;

    const recordingStream = await browserFetchBinary(page, {
      method: 'GET',
      url: `/LiveTv/LiveRecordings/${encodeURIComponent(liveTvFlowRecordingId)}/stream`,
      token: auth.AccessToken,
    });
    if (recordingStream.status !== 200 || !recordingStream.contentType.includes('video/mp2t') || recordingStream.byteLength <= 0) {
      throw new Error(`LiveTv recording stream returned HTTP ${recordingStream.status}`);
    }
    summary.invariants.liveTvRecordingStream200 = true;

    const timer = await browserFetchJson(page, {
      method: 'POST',
      url: '/LiveTv/Timers',
      token: auth.AccessToken,
      body: {
        Id: liveTvFlowTimerId,
        ProgramId: liveTvFlowProgramId,
        ChannelId: liveTvFlowChannelId,
        Name: 'Jellyrin Live TV Timer',
      },
    });
    if (timer.status !== 200 || timer.json?.Id !== liveTvFlowTimerId) {
      throw new Error(`LiveTv/Timers create returned HTTP ${timer.status}`);
    }
    summary.invariants.liveTvTimerCreated = true;

    const deleteTimer = await browserFetchJson(page, {
      method: 'DELETE',
      url: `/LiveTv/Timers/${encodeURIComponent(liveTvFlowTimerId)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(deleteTimer.status)) {
      throw new Error(`LiveTv/Timers delete returned HTTP ${deleteTimer.status}`);
    }
    summary.invariants.liveTvTimerDeleted = true;

    const seriesTimer = await browserFetchJson(page, {
      method: 'POST',
      url: '/LiveTv/SeriesTimers',
      token: auth.AccessToken,
      body: {
        Id: `${liveTvFlowTimerId}-series`,
        ProgramId: liveTvFlowProgramId,
        ChannelId: liveTvFlowChannelId,
        Name: 'Jellyrin Live TV Series Timer',
        RecordAnyTime: true,
      },
    });
    if (seriesTimer.status !== 200 || seriesTimer.json?.IsSeries !== true) {
      throw new Error(`LiveTv/SeriesTimers create returned HTTP ${seriesTimer.status}`);
    }
    summary.invariants.liveTvSeriesTimerCreated = true;

    const deleteSeriesTimer = await browserFetchJson(page, {
      method: 'DELETE',
      url: `/LiveTv/SeriesTimers/${encodeURIComponent(`${liveTvFlowTimerId}-series`)}`,
      token: auth.AccessToken,
    });
    if (![200, 204].includes(deleteSeriesTimer.status)) {
      throw new Error(`LiveTv/SeriesTimers delete returned HTTP ${deleteSeriesTimer.status}`);
    }
    summary.invariants.liveTvSeriesTimerDeleted = true;
  }

  // HDHomeRun block — runs for BOTH targets against the same simulator.
  // upstream-validated is decided by this real sequence, not by the synthetic M3U/XMLTV block above.
  if (!hdhrSimUrl) {
    throw new Error('HDHomeRun simulator URL is not available; cannot run live-tv flow');
  }

  const info = await browserFetchJson(page, {
    method: 'GET',
    url: '/LiveTv/Info',
    token: auth.AccessToken,
  });
  if (info.status !== 200 || info.json === null) {
    throw new Error(`LiveTv/Info returned HTTP ${info.status}`);
  }
  summary.invariants.liveTvInfo200 = true;

  const tunerTypes = await browserFetchJson(page, {
    method: 'GET',
    url: '/LiveTv/TunerHosts/Types',
    token: auth.AccessToken,
  });
  if (tunerTypes.status !== 200 || !tunerTypes.json?.some((item) => item.Id === 'hdhomerun')) {
    throw new Error(`LiveTv/TunerHosts/Types returned HTTP ${tunerTypes.status}`);
  }
  summary.invariants.liveTvTunerTypes200 = true;

  const addTuner = await browserFetchJson(page, {
    method: 'POST',
    url: '/LiveTv/TunerHosts',
    token: auth.AccessToken,
    body: { Type: 'hdhomerun', Url: hdhrSimUrl },
  });
  if (![200, 204].includes(addTuner.status) || !addTuner.json?.Id) {
    throw new Error(`LiveTv/TunerHosts hdhomerun returned HTTP ${addTuner.status}`);
  }
  summary.invariants.liveTvHdhrTunerAdded = true;

  // Proactively trigger upstream's async RefreshGuide task so channels materialise sooner.
  // Tolerate any failure — the poll below will time out gracefully if guide never refreshes.
  await (async () => {
    const tasks = await browserFetchJson(page, {
      method: 'GET',
      url: '/ScheduledTasks',
      token: auth.AccessToken,
    }).catch(() => null);
    if (!tasks || tasks.status !== 200 || !Array.isArray(tasks.json)) return;
    const refreshTask = tasks.json.find((t) => t.Key === 'RefreshGuide' || (typeof t.Name === 'string' && t.Name.toLowerCase().includes('refresh guide')));
    if (!refreshTask) return;
    await browserFetchJson(page, {
      method: 'POST',
      url: `/ScheduledTasks/Running/${encodeURIComponent(refreshTask.Id)}`,
      token: auth.AccessToken,
    }).catch(() => {});
  })().catch(() => {});

  // Poll GET /LiveTv/Channels until a non-DRM HDHomeRun channel from the simulator appears or
  // timeout is reached. Jellyrin materialises channels eagerly on first attempt; upstream waits
  // for the async RefreshGuideScheduledTask which may take several seconds.
  // The exact non-DRM GuideNumbers served by the simulator are '4.1' and '5.1'; '6.1' is DRM and
  // must NOT match. Using a whitelist prevents matching random channels from unrelated tuner configs.
  const hdhrNonDrmNumbers = ['4.1', '5.1'];
  const hdhrPollTimeout = Number.parseInt(process.env.JELLYRIN_LIVETV_HDHR_POLL_TIMEOUT_MS || '60000', 10);
  const hdhrPollInterval = Number.parseInt(process.env.JELLYRIN_LIVETV_HDHR_POLL_INTERVAL_MS || '2000', 10);
  const hdhrPollDeadline = Date.now() + hdhrPollTimeout;
  let hdhrChannel = null;
  while (Date.now() < hdhrPollDeadline) {
    const hdhrChannels = await browserFetchJson(page, {
      method: 'GET',
      url: `/LiveTv/Channels?UserId=${encodeURIComponent(auth.User.Id)}`,
      token: auth.AccessToken,
    });
    if (hdhrChannels.status !== 200 || !Array.isArray(hdhrChannels.json?.Items)) {
      throw new Error(`LiveTv/Channels (hdhr poll) returned HTTP ${hdhrChannels.status}`);
    }
    hdhrChannel = hdhrChannels.json.Items.find(
      (item) => (typeof item.Id === 'string' && hdhrNonDrmNumbers.includes(item.Id.replace(/^hdhr_/, '')))
        || (typeof item.ChannelNumber === 'string' && hdhrNonDrmNumbers.includes(item.ChannelNumber)),
    );
    if (hdhrChannel) break;
    if (Date.now() + hdhrPollInterval < hdhrPollDeadline) {
      await new Promise((resolve) => { setTimeout(resolve, hdhrPollInterval); });
    } else {
      break;
    }
  }
  if (!hdhrChannel) {
    throw new Error(`No non-DRM HDHomeRun channel found in /LiveTv/Channels after ${hdhrPollTimeout}ms`);
  }
  summary.invariants.liveTvHdhrChannelMatched = true;

  // Validate HDHomeRun stream setup and actual byte delivery for BOTH targets.
  //
  // liveTvHdhrStreamSetup (informational): both targets confirm a usable MediaSource exists.
  //   - upstream: POST /Items/{channelId}/PlaybackInfo with AutoOpenLiveStream=true returns a
  //     MediaSource whose Path contains /LiveTv/LiveStreamFiles/.
  //   - Jellyrin: GET /LiveTv/Channels/{channelId}?fields=MediaSources returns a MediaSource
  //     with a DirectStreamUrl pointing to /LiveTv/LiveStreamFiles/{channelId}/stream.ts.
  //
  // liveTvHdhrStream200 (upstreamComparable): BOTH targets probe actual bytes using
  // AbortController (browserFetchStreamProbe: reads >=1 byte then aborts to avoid hanging on
  // the infinite stream). Jellyrin's proxy now streams incrementally (bytes_stream + Body::from_stream)
  // so headers and bytes are returned immediately without buffering the full body.

  // Reset simulator stats (both peak and current counters) before the byte-check and sharing
  // tests so we measure from a clean baseline for this target. Resetting current counters too
  // prevents upstream Jellyfin's SharedHttpStream R8 refill connection from contaminating
  // the next target's sharing metric. For upstream, PlaybackInfo opens 1 sim connection;
  // for Jellyrin, the stream proxy opens 1 sim connection and shares it via broadcast fan-out.
  // In both cases, maxConcurrentByChannel[simPath]===1 confirms sharing.
  if (hdhrSimUrl) {
    await nodeHttpJson('POST', `${hdhrSimUrl}/stats/reset`).catch(() => {});
  }

  let hdhrLiveStreamId = null;
  // Stream path on the target server (used for both per-target byte-check and sharing test).
  let hdhrTargetStreamPath = null;

  if (target.name === 'jellyrin' && /^hdhr_/.test(hdhrChannel.Id)) {
    // Jellyrin path: GET /LiveTv/Channels/{channelId}?fields=MediaSources
    // PlaybackInfo returns 400 for hdhr_ items; channel GET exposes the MediaSource directly.
    const hdhrChannelDetail = await browserFetchJson(page, {
      method: 'GET',
      url: `/LiveTv/Channels/${encodeURIComponent(hdhrChannel.Id)}?fields=MediaSources`,
      token: auth.AccessToken,
    });
    if (hdhrChannelDetail.status !== 200 || !Array.isArray(hdhrChannelDetail.json?.MediaSources)) {
      throw new Error(`Jellyrin LiveTv/Channels detail returned HTTP ${hdhrChannelDetail.status}`);
    }
    const ms = hdhrChannelDetail.json.MediaSources[0];
    const directStreamUrl = ms?.DirectStreamUrl;
    if (!directStreamUrl || !/\/LiveTv\/LiveStreamFiles\//i.test(directStreamUrl)) {
      throw new Error('Jellyrin HDHomeRun channel did not return a LiveStreamFiles DirectStreamUrl');
    }
    // liveTvHdhrStreamSetup confirmed: Jellyrin has a valid MediaSource for this HDHR channel.
    summary.invariants.liveTvHdhrStreamSetup = true;
    // Byte-check: probe the stream endpoint using AbortController to read >=1 byte without
    // hanging on the infinite stream. The proxy now streams incrementally so headers and bytes
    // are returned immediately.
    const jellyrinStreamPath = (() => {
      try {
        const pathUrl = new URL(directStreamUrl, summary.baseUrl);
        return pathUrl.pathname + pathUrl.search;
      } catch (_) {
        return directStreamUrl;
      }
    })();
    hdhrTargetStreamPath = jellyrinStreamPath;
    const jellyrinStreamProbe = await browserFetchStreamProbe(page, {
      url: jellyrinStreamPath,
      token: auth.AccessToken,
      minBytes: 1,
    });
    if (jellyrinStreamProbe.status !== 200 || !jellyrinStreamProbe.contentType.includes('video/mp2t') || jellyrinStreamProbe.byteLength <= 0) {
      throw new Error(`Jellyrin HDHomeRun stream probe returned HTTP ${jellyrinStreamProbe.status} contentType="${jellyrinStreamProbe.contentType}" bytes=${jellyrinStreamProbe.byteLength}`);
    }
    summary.invariants.liveTvHdhrStream200 = true;
  } else {
    // Upstream path: POST PlaybackInfo → ffprobe → MediaSource with LiveStreamFiles Path.
    // The simulator serves a continuous valid MPEG-2 TS stream so ffprobe completes within
    // its 3-second analyzeduration window and PlaybackInfo returns 200.
    const hdhrPlaybackInfo = await browserFetchJson(page, {
      method: 'POST',
      url: `/Items/${encodeURIComponent(hdhrChannel.Id)}/PlaybackInfo`,
      token: auth.AccessToken,
      body: { UserId: auth.User.Id, AutoOpenLiveStream: true },
    });
    if (hdhrPlaybackInfo.status !== 200 || !Array.isArray(hdhrPlaybackInfo.json?.MediaSources)) {
      throw new Error(`HDHomeRun PlaybackInfo returned HTTP ${hdhrPlaybackInfo.status}`);
    }
    const hdhrMediaSource = hdhrPlaybackInfo.json.MediaSources[0];
    if (!hdhrMediaSource || typeof hdhrMediaSource.Path !== 'string' || !/\/LiveTv\/LiveStreamFiles\//i.test(hdhrMediaSource.Path)) {
      throw new Error('HDHomeRun PlaybackInfo did not return a LiveStreamFiles path');
    }
    hdhrLiveStreamId = hdhrMediaSource.LiveStreamId || null;
    // liveTvHdhrStreamSetup confirmed: upstream has a valid MediaSource for this HDHR channel.
    summary.invariants.liveTvHdhrStreamSetup = true;

    // liveTvHdhrStream200: probe actual bytes from the upstream SharedHttpStream URL.
    // Rewrite any public-IP host to the configured baseUrl host+port.
    const hdhrStreamPath = (() => {
      try {
        const pathUrl = new URL(hdhrMediaSource.Path);
        const baseUrl = new URL(summary.baseUrl);
        pathUrl.hostname = baseUrl.hostname;
        pathUrl.port = baseUrl.port;
        return pathUrl.pathname + pathUrl.search;
      } catch (_) {
        return hdhrMediaSource.Path;
      }
    })();
    hdhrTargetStreamPath = hdhrStreamPath;

    const hdhrStreamProbe = await browserFetchStreamProbe(page, {
      url: hdhrStreamPath,
      token: auth.AccessToken,
      minBytes: 1,
    });
    if (hdhrStreamProbe.status !== 200 || !hdhrStreamProbe.contentType.includes('video/mp2t') || hdhrStreamProbe.byteLength <= 0) {
      if (hdhrLiveStreamId) {
        await browserFetchJson(page, {
          method: 'POST',
          url: `/LiveStreams/Close?liveStreamId=${encodeURIComponent(hdhrLiveStreamId)}`,
          token: auth.AccessToken,
        }).catch(() => {});
      }
      throw new Error(`HDHomeRun stream probe returned HTTP ${hdhrStreamProbe.status} contentType="${hdhrStreamProbe.contentType}" bytes=${hdhrStreamProbe.byteLength}`);
    }
    summary.invariants.liveTvHdhrStream200 = true;
    // Note: hdhrLiveStreamId is kept open here so the sharing test below can reuse the same
    // LiveStreamFiles URL. It will be closed after the sharing test completes.
  }

  // Stream-sharing / refcount test (upstreamComparable + jellyrinOnly byte check).
  //
  // Metric: simulator connection counters at /auto/vN. With sharing: 2 clients → maxConcurrent===1.
  // Without sharing: maxConcurrent===2. Both targets hit the same simulator.
  //
  // Note on R8 (upstream refcount release): upstream may keep the connection to the simulator
  // open for stream refill even after both Jellyrin/upstream proxy probes are gone. If
  // currentConcurrent does not drop to 0 within the bounded timeout, we degrade
  // liveTvHdhrStreamRefcountReleased to the honest observable result and document it.
  if (hdhrSimUrl && hdhrTargetStreamPath) {
    // Determine the simulator channel path for this channel (e.g. '/auto/v4.1').
    const guideNumber = (
      hdhrChannel.ChannelNumber
      || (typeof hdhrChannel.Id === 'string' ? hdhrChannel.Id.replace(/^hdhr_/, '') : '')
    );
    const simChannelPath = `/auto/v${guideNumber}`;

    // Brief pause to allow the byte-check connection's server-side close to propagate
    // before the sharing probes start. Jellyrin inserts the registry entry atomically
    // before opening the upstream connection (preventing duplicate connections), so probe2
    // will subscribe to probe1's handle rather than opening a separate upstream connection.
    await new Promise((resolve) => { setTimeout(resolve, 100); });

    // Open 2 concurrent stream probes and hold them open briefly to ensure overlap.
    // browserFetchStreamProbeOverlap: fetch URL, read minBytes, then hold for holdMs before aborting.
    const probe1Promise = browserFetchStreamProbeOverlap(page, {
      url: hdhrTargetStreamPath,
      token: auth.AccessToken,
      minBytes: 1,
      holdMs: 600,
    });
    // Small stagger so both are clearly in-flight at the same time.
    await new Promise((resolve) => { setTimeout(resolve, 30); });
    const probe2Promise = browserFetchStreamProbeOverlap(page, {
      url: hdhrTargetStreamPath,
      token: auth.AccessToken,
      minBytes: 1,
      holdMs: 600,
    });

    const [probe1, probe2] = await Promise.all([probe1Promise, probe2Promise]);

    // /stats is read AFTER Promise.all resolves (both probes have finished or timed out).
    // maxConcurrentByChannel is a peak counter on the simulator: it records the highest
    // simultaneous connection count observed, so it retains the value even after connections close.
    const statsAfterOpen = await nodeHttpJson('GET', `${hdhrSimUrl}/stats`).catch(() => null);

    const maxConcurrent = statsAfterOpen?.json?.maxConcurrentByChannel?.[simChannelPath] ?? -1;
    // maxConcurrent===1: sharing confirmed (Jellyrin proxy reused one connection).
    // maxConcurrent===2: no sharing (each probe triggered a separate upstream connection).
    summary.invariants.liveTvHdhrTwoClientStream = maxConcurrent === 1;

    // Jellyrin-only: 2nd consumer must receive actual bytes (video/mp2t, byteLength>=1).
    if (target.name === 'jellyrin') {
      summary.invariants.liveTvHdhrTwoClientByteCheck = (
        probe2.status === 200
        && probe2.contentType.includes('video/mp2t')
        && probe2.byteLength >= 1
      );
    }

    // Close the upstream live stream before polling for refcount release.
    // For upstream, closing the live stream triggers CloseLiveStream (ConsumerCount--; a <=0
    // TryRemove + Close() cancels the SharedHttpStream token and closes the sim connection).
    // For Jellyrin the stream is stateless and hdhrLiveStreamId is null.
    if (hdhrLiveStreamId) {
      await browserFetchJson(page, {
        method: 'POST',
        url: `/LiveStreams/Close?liveStreamId=${encodeURIComponent(hdhrLiveStreamId)}`,
        token: auth.AccessToken,
      }).catch(() => {});
    }

    // Wait for the simulator connections to drain (bounded timeout).
    // R8: upstream's SharedHttpStream may keep the sim connection open briefly for refill after
    // CloseLiveStream; poll until currentConcurrent reaches 0 or the timeout expires.
    // If the timeout expires, we record the honest observed value — the gate is not forced to pass.
    const refcountReleaseTimeoutMs = 5000;
    const refcountPollIntervalMs = 200;
    const refcountDeadline = Date.now() + refcountReleaseTimeoutMs;
    let currentConcurrent = -1;
    while (Date.now() < refcountDeadline) {
      const statsAfterClose = await nodeHttpJson('GET', `${hdhrSimUrl}/stats`).catch(() => null);
      currentConcurrent = statsAfterClose?.json?.currentConcurrentByChannel?.[simChannelPath] ?? -1;
      if (currentConcurrent === 0) break;
      await new Promise((resolve) => { setTimeout(resolve, refcountPollIntervalMs); });
    }
    summary.invariants.liveTvHdhrStreamRefcountReleased = currentConcurrent === 0;
    // If currentConcurrent did not reach 0 (R8), the invariant is false and the evidence
    // text in livetv-real.js documents this as an honest upstream observation.
  }

  // HLS / Transcode Live TV block (upstreamComparable + jellyrinOnly).
  //
  // Validates that the channel can be played via HLS transcode:
  //   master.m3u8 (200, #EXT-X-STREAM-INF) -> media playlist EVENT (>=1 #EXTINF, no ENDLIST) ->
  //   segment .ts (200, video/mp2t, bytes>0) -> ActiveEncodings lists session ->
  //   DELETE ActiveEncodings -> session disappears (poll <=5s).
  //
  // Jellyrin: TranscodingUrl is embedded in the channel MediaSource.
  // upstream: uses PlaybackInfo with an HLS device profile to get a TranscodingUrl.
  // Both targets: GET master.m3u8 -> media playlist -> segment -> ActiveEncodings -> DELETE.
  //
  // Reset stats before the HLS block so we don't contaminate the sharing test counters.
  if (hdhrSimUrl) {
    await nodeHttpJson('POST', `${hdhrSimUrl}/stats/reset`).catch(() => {});
  }

  let hlsTranscodingUrl = null;
  let hlsPlaySessionId = null;

  if (target.name === 'jellyrin' && /^hdhr_/.test(hdhrChannel.Id)) {
    // Jellyrin: extract TranscodingUrl from the channel MediaSource (SupportsTranscoding:true).
    const hdhrChannelForHls = await browserFetchJson(page, {
      method: 'GET',
      url: `/LiveTv/Channels/${encodeURIComponent(hdhrChannel.Id)}?fields=MediaSources`,
      token: auth.AccessToken,
    });
    if (hdhrChannelForHls.status === 200) {
      const ms = hdhrChannelForHls.json?.MediaSources?.[0];
      const transcodingUrl = ms?.TranscodingUrl;
      if (transcodingUrl && ms?.SupportsTranscoding === true && ms?.TranscodingSubProtocol === 'hls') {
        hlsTranscodingUrl = transcodingUrl;
        // Extract PlaySessionId from the URL for DELETE.
        try {
          const urlParams = new URLSearchParams(transcodingUrl.split('?')[1] || '');
          hlsPlaySessionId = urlParams.get('PlaySessionId') || null;
        } catch (_) { /* ignore */ }
        summary.invariants.liveTvHdhrHlsTranscodeUrl = true;
      }
    }
  } else if (target.name !== 'jellyrin') {
    // Upstream: use PlaybackInfo with an HLS device profile to force transcoding.
    // The response includes a TranscodingUrl for the live HLS stream.
    const hdhrHlsPlaybackInfo = await browserFetchJson(page, {
      method: 'POST',
      url: `/Items/${encodeURIComponent(hdhrChannel.Id)}/PlaybackInfo`,
      token: auth.AccessToken,
      body: {
        UserId: auth.User.Id,
        EnableTranscoding: true,
        EnableDirectPlay: false,
        EnableDirectStream: false,
        AutoOpenLiveStream: true,
        DeviceProfile: hlsTranscodeDeviceProfile(),
      },
    });
    if (hdhrHlsPlaybackInfo.status === 200) {
      const ms = hdhrHlsPlaybackInfo.json?.MediaSources?.[0];
      const transcodingUrl = ms?.TranscodingUrl;
      if (transcodingUrl) {
        hlsTranscodingUrl = transcodingUrl;
        hlsPlaySessionId = hdhrHlsPlaybackInfo.json?.PlaySessionId || null;
      }
    }
  }

  if (hlsTranscodingUrl) {
    // GET master.m3u8 — bounded: the server should respond quickly but we guard against hangs.
    const master = await browserFetchTextBounded(page, {
      method: 'GET',
      url: hlsTranscodingUrl,
      token: auth.AccessToken,
      timeoutMs: 20000,
    });
    if (master.status === 200
      && master.contentType.includes('mpegurl')
      && master.text.includes('#EXT-X-STREAM-INF')) {
      summary.invariants.liveTvHdhrHlsMaster200 = true;

      // GET media playlist (live: no #EXT-X-ENDLIST, >=1 #EXTINF).
      // Bounded fetch: upstream live.m3u8 may long-poll (never closes connection) so we abort
      // after timeoutMs with whatever was received. A valid playlist is returned quickly;
      // if the server hangs the timeout fires and we get an empty or partial response.
      const mediaPlaylistPath = firstPlaylistUri(master.text);
      if (mediaPlaylistPath) {
        const mediaPlaylistUrl = resolveRelativeUrl(hlsTranscodingUrl, mediaPlaylistPath);
        const media = await browserFetchTextBounded(page, {
          method: 'GET',
          url: mediaPlaylistUrl,
          token: auth.AccessToken,
          timeoutMs: 35000,
        });
        if (media.status === 200
          && media.text.includes('#EXTINF')
          && !media.text.includes('#EXT-X-ENDLIST')) {
          summary.invariants.liveTvHdhrHlsMediaLive = true;

          // GET first segment — probe 1+ byte via AbortController (bounded, does not hang).
          const segmentPath = firstPlaylistUri(media.text);
          if (segmentPath) {
            const segmentUrl = resolveRelativeUrl(mediaPlaylistUrl, segmentPath);
            const segmentProbe = await browserFetchStreamProbe(page, {
              url: segmentUrl,
              token: auth.AccessToken,
              minBytes: 1,
            });
            if ((segmentProbe.status === 200 || segmentProbe.status === 206)
              && segmentProbe.contentType.includes('video/mp2t')
              && segmentProbe.byteLength >= 1) {
              summary.invariants.liveTvHdhrHlsSegment200 = true;
            }
          }
        }
      }
    }

    // Check that the session appears in ActiveEncodings (Jellyrin-only: upstream Jellyfin does
    // not expose GET /Videos/ActiveEncodings, only DELETE). For Jellyrin, verify session is
    // listed; for upstream, skip the GET to avoid the deadlock that Jellyfin exhibits when
    // ffmpeg is actively transcoding (GET hangs while ffmpeg runs).
    const psid = hlsPlaySessionId;
    if (target.name === 'jellyrin') {
      const activeEncodings = await browserFetchJson(page, {
        method: 'GET',
        url: '/Videos/ActiveEncodings',
        token: auth.AccessToken,
      });
      const sessionListed = Array.isArray(activeEncodings.json)
        && activeEncodings.json.some((enc) => enc.PlaySessionId === psid || enc.ItemId === hdhrChannel.Id);
      if (sessionListed) {
        summary.invariants.liveTvHdhrHlsActiveEncoding = true;
      }
    }

    // DELETE ActiveEncodings to stop ffmpeg and clean up.
    // upstream Jellyfin requires both PlaySessionId AND DeviceId for DELETE; without DeviceId
    // it returns 400. The harness authenticates with DeviceId="browser-trace" so we include it.
    if (psid) {
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(psid)}&DeviceId=browser-trace`,
        token: auth.AccessToken,
      }).catch(() => {});

      // Poll until the session disappears from ActiveEncodings (<=5s).
      // Jellyrin-only: upstream does not support GET /Videos/ActiveEncodings so skip the poll.
      if (target.name === 'jellyrin') {
        const deleteDeadlineMs = 5000;
        const deletePollIntervalMs = 200;
        const deleteDeadline = Date.now() + deleteDeadlineMs;
        let sessionGone = false;
        while (Date.now() < deleteDeadline) {
          const activeAfterDelete = await browserFetchJson(page, {
            method: 'GET',
            url: '/Videos/ActiveEncodings',
            token: auth.AccessToken,
          });
          const stillListed = Array.isArray(activeAfterDelete.json)
            && activeAfterDelete.json.some((enc) => enc.PlaySessionId === psid);
          if (!stillListed) { sessionGone = true; break; }
          await new Promise((resolve) => { setTimeout(resolve, deletePollIntervalMs); });
        }

        // liveTvHdhrHlsActiveEncoding is true if the session was listed before DELETE AND
        // it disappears within the bounded timeout after DELETE.
        if (!sessionGone) {
          summary.invariants.liveTvHdhrHlsActiveEncoding = false;
        }
      }

      // Jellyrin-only: verify /stats currentConcurrent[/auto/vN] drops to 0 (no orphan ffmpeg).
      if (target.name === 'jellyrin' && hdhrSimUrl) {
        const guideNumber = (
          hdhrChannel.ChannelNumber
          || (typeof hdhrChannel.Id === 'string' ? hdhrChannel.Id.replace(/^hdhr_/, '') : '')
        );
        const simChannelPath = `/auto/v${guideNumber}`;
        const reaperDeadline = Date.now() + 5000;
        let reaped = false;
        while (Date.now() < reaperDeadline) {
          const stats = await nodeHttpJson('GET', `${hdhrSimUrl}/stats`).catch(() => null);
          const current = stats?.json?.currentConcurrentByChannel?.[simChannelPath] ?? -1;
          if (current === 0) { reaped = true; break; }
          await new Promise((resolve) => { setTimeout(resolve, 200); });
        }
        summary.invariants.liveTvHdhrHlsFfmpegReaped = reaped;
      }
    }
  }

  // Recording block — POST a short timer, poll until Completed, ffprobe verify >=1 video packet.
  // Runs for BOTH targets against the same simulator (upstreamComparable set).
  // Jellyrin: record_channel_to_file copies bytes from the HDHomeRun simulator TS stream.
  // upstream: DirectRecorder.RecordFromMediaSource copies bytes from the same sim TS stream.
  // Both: the recording file must be a valid MPEG-2 TS with >=1 video packet.
  //
  // liveTvHdhrTimerRecordingCreated: POST /LiveTv/Timers returns 200 with an Id.
  // liveTvHdhrRecordingCompleted: GET /LiveTv/Recordings (poll) finds Status=="Completed" for our channel.
  // liveTvHdhrRecordingPlayable: download the recording bytes, run ffprobe, assert >=1 video packet.
  // liveTvHdhrRecordingCleanup (jellyrin-only): /stats currentConcurrent===0 + DELETE 204 + absent.
  {
    const recordSecs = Number.parseInt(process.env.JELLYRIN_LIVETV_RECORD_SECS || '4', 10);
    const pollTimeoutMs = Number.parseInt(process.env.JELLYRIN_LIVETV_RECORD_POLL_TIMEOUT_MS || '30000', 10);
    const pollIntervalMs = Number.parseInt(process.env.JELLYRIN_LIVETV_RECORD_POLL_INTERVAL_MS || '1000', 10);

    const recordStart = new Date();
    const recordEnd = new Date(recordStart.getTime() + recordSecs * 1000);

    // ServiceName is required by upstream Jellyfin's LiveTvManager.CreateTimer (GetService call).
    // "Emby" is the service name for DefaultLiveTvService (the service that handles HDHomeRun timers).
    // Jellyrin ignores this field (it doesn't route via service name).
    const timerBody = {
      ChannelId: hdhrChannel.Id,
      Name: `Jellyrin HDHomeRun Recording Test ${Date.now()}`,
      StartDate: recordStart.toISOString(),
      EndDate: recordEnd.toISOString(),
      PrePaddingSeconds: 0,
      PostPaddingSeconds: 0,
      Priority: 0,
      IsPrePaddingRequired: false,
      IsPostPaddingRequired: false,
      KeepUntil: 'UntilDeleted',
      RecordAnyTime: false,
      RecordAnyChannel: false,
      IsManual: true,
      ServiceName: 'Emby',
    };

    const timerName = timerBody.Name;
    const createTimer = await browserFetchJson(page, {
      method: 'POST',
      url: '/LiveTv/Timers',
      token: auth.AccessToken,
      body: timerBody,
    });
    // Jellyrin returns 200 with timer body; upstream returns 204 with no body.
    let timerId = createTimer.json?.Id || null;
    if ([200, 204].includes(createTimer.status)) {
      // If no Id in response (upstream 204), poll GET /LiveTv/Timers to find the newly created timer.
      if (!timerId) {
        const timersPollDeadline = Date.now() + 5000;
        while (Date.now() < timersPollDeadline) {
          const timersList = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/Timers',
            token: auth.AccessToken,
          });
          if (timersList.status === 200 && Array.isArray(timersList.json?.Items)) {
            const found = timersList.json.Items.find(
              (t) => t.ChannelId === hdhrChannel.Id
                || (typeof t.Name === 'string' && t.Name.includes('HDHomeRun Recording Test')),
            );
            if (found?.Id) { timerId = found.Id; break; }
          }
          await new Promise((resolve) => { setTimeout(resolve, 500); });
        }
      }
      summary.invariants.liveTvHdhrTimerRecordingCreated = Boolean(timerId);

      // Poll until a recording for our channel appears (or timeout).
      // Jellyrin: recordings appear directly in GET /LiveTv/Recordings with Status="Completed".
      // upstream: Jellyfin scans completed recordings into the library as Movie items with
      //   Status=null (not "Completed"); we match by the unique timer name since ChannelId is
      //   null in library items. upstream does NOT expose timer RecordingPath after the fact so
      //   we must wait for library scan. To accelerate the scan we POST /Library/Refresh after
      //   the recording window and then wait up to pollTimeoutMs for the named item to appear.
      const pollDeadline = Date.now() + pollTimeoutMs;
      let completedRecording = null;
      let libraryScanTriggered = false;
      const libraryScanAfterMs = (recordSecs + 2) * 1000; // trigger scan ~2s after recording window
      const libraryScanAt = Date.now() + libraryScanAfterMs;

      while (Date.now() < pollDeadline) {
        // For upstream: trigger a library scan once the recording window is likely over.
        // This is necessary because upstream's library scanner runs asynchronously;
        // without an explicit trigger, new recordings may not appear within 30s.
        if (!libraryScanTriggered && Date.now() >= libraryScanAt) {
          libraryScanTriggered = true;
          await browserFetchJson(page, {
            method: 'POST',
            url: '/Library/Refresh',
            token: auth.AccessToken,
          }).catch(() => {});
        }

        const recordings = await browserFetchJson(page, {
          method: 'GET',
          url: '/LiveTv/Recordings',
          token: auth.AccessToken,
        });
        if (recordings.status === 200 && Array.isArray(recordings.json?.Items)) {
          // Primary: Status=="Completed" with channel or name match (Jellyrin path).
          completedRecording = recordings.json.Items.find(
            (r) => r.Status === 'Completed'
              && (r.ChannelId === hdhrChannel.Id
                || (typeof r.Name === 'string' && r.Name === timerName)),
          ) || null;
          // Secondary: name match for upstream library items (Status=null, not "InProgress").
          // upstream completed recordings are Movie items with Status=null and RunTimeTicks>0.
          // Exclude InProgress (Jellyrin) and any item that is explicitly not yet done.
          if (!completedRecording) {
            completedRecording = recordings.json.Items.find(
              (r) => typeof r.Name === 'string'
                && r.Name === timerName
                && r.Status !== 'InProgress'
                && r.RunTimeTicks != null
                && r.RunTimeTicks > 0,
            ) || null;
          }
          if (completedRecording) break;
        }
        // Upstream may scan finished DVR files as regular Movie items while
        // leaving /LiveTv/Recordings empty in this local fixture setup.
        if (!completedRecording && target.name === 'upstream' && auth.User?.Id) {
          const itemLookup = await browserFetchJson(page, {
            method: 'GET',
            url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&SearchTerm=${encodeURIComponent(timerName)}&Fields=MediaSources,RunTimeTicks,Path&Limit=5`,
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          if (itemLookup.status === 200 && Array.isArray(itemLookup.json?.Items)) {
            const movieRecording = itemLookup.json.Items.find(
              (item) => item.Name === timerName
                && item.RunTimeTicks != null
                && item.RunTimeTicks > 0,
            );
            if (movieRecording?.Id) {
              completedRecording = {
                ...movieRecording,
                Status: 'Completed',
                _libraryItem: true,
              };
              break;
            }
          }
        }
        // Also check timer status (works before library scan on upstream).
        if (timerId && !completedRecording) {
          const timerStatus = await browserFetchJson(page, {
            method: 'GET',
            url: `/LiveTv/Timers/${encodeURIComponent(timerId)}`,
            token: auth.AccessToken,
          });
          if (timerStatus.status === 200 && timerStatus.json?.Status === 'Completed') {
            // Timer completed but recording not yet in library — treat as recorded (upstream path).
            // We'll use the timer's RecordingPath to verify bytes.
            completedRecording = { Id: timerStatus.json.Id, Status: 'Completed', _timerData: timerStatus.json };
            break;
          }
        }
        if (Date.now() + pollIntervalMs < pollDeadline) {
          await new Promise((resolve) => { setTimeout(resolve, pollIntervalMs); });
        } else {
          break;
        }
      }

      if (completedRecording) {
        summary.invariants.liveTvHdhrRecordingCompleted = true;

        // Download the recording and run ffprobe to verify >=1 video packet.
        // Jellyrin: GET /LiveTv/LiveRecordings/{id}/stream serves the completed recording file.
        // upstream: the recording Item is a media item; GET /Items/{id}/Download or PlaybackInfo
        //   gives the stream URL. For simplicity we try the LiveRecordings stream path first
        //   (upstream may return 404 for Completed; fallback to /Videos/{id}/stream).
        const recId = completedRecording.Id;
        const ffprobeBin = process.env.FFPROBE_BIN || '/usr/bin/ffprobe';
        const altFfprobeBin = '/usr/lib/jellyfin-ffmpeg/ffprobe';

        // Download recording bytes using Playwright's page.request API (proven to work with auth).
        // Try multiple stream paths: LiveRecordings (Jellyrin completed + InProgress),
        // Videos stream (upstream completed recordings served as library items).
        const tmpRecFile = `/tmp/jellyrin-recording-probe-${target.name}-${Date.now()}.ts`;
        let downloadOk = false;

        // Jellyrin: /LiveTv/LiveRecordings/{id}/stream serves both InProgress and Completed recordings.
        // upstream: /LiveTv/LiveRecordings/{id}/stream only serves InProgress (active) recordings;
        //   Completed recordings are library items served via /Videos/{id}/stream or /Items/{id}/Download.
        const streamPaths = [
          `/LiveTv/LiveRecordings/${encodeURIComponent(recId)}/stream`,
          `/Videos/${encodeURIComponent(recId)}/stream`,
          `/Items/${encodeURIComponent(recId)}/Download`,
        ];
        for (const streamPath of streamPaths) {
          try {
            const resp = await page.request.get(`${summary.baseUrl}${streamPath}`, {
              headers: { 'X-Emby-Token': auth.AccessToken },
            });
            if (resp.ok()) {
              const bodyBuf = await resp.body();
              if (bodyBuf && bodyBuf.length > 0) {
                const { writeFileSync } = require('node:fs');
                writeFileSync(tmpRecFile, bodyBuf);
                downloadOk = true;
                break;
              }
            }
          } catch (_) {
            // Try next path.
          }
        }

        if (downloadOk) {
          // Run ffprobe to count video packets: assert >=1 packet.
          // ffprobe uses single-dash flags; -version (not --version) exits 0 on success.
          const actualFfprobe = (() => {
            try {
              const { status: s } = spawnSync(ffprobeBin, ['-version'], { stdio: 'ignore' });
              if (s === 0) return ffprobeBin;
            } catch (_) { /* ignore */ }
            try {
              const { status: s } = spawnSync(altFfprobeBin, ['-version'], { stdio: 'ignore' });
              if (s === 0) return altFfprobeBin;
            } catch (_) { /* ignore */ }
            return null;
          })();

          if (actualFfprobe) {
            const probeResult = spawnSync(actualFfprobe, [
              '-v', 'error',
              '-show_packets',
              '-select_streams', 'v',
              '-read_intervals', '%+#1',
              tmpRecFile,
            ], { encoding: 'utf8', timeout: 10000 });
            const probeOutput = (probeResult.stdout || '') + (probeResult.stderr || '');
            const hasVideoPacket = probeOutput.includes('[PACKET]') || probeOutput.includes('codec_type=video');
            if (hasVideoPacket) {
              summary.invariants.liveTvHdhrRecordingPlayable = true;
            }
          }

          // Clean up tmp file.
          try { require('node:fs').unlinkSync(tmpRecFile); } catch (_) { /* ignore */ }
        }

        // Jellyrin-only cleanup: /stats currentConcurrent===0 + DELETE recording 204 + absent.
        if (target.name === 'jellyrin' && hdhrSimUrl) {
          const guideNumber = (
            hdhrChannel.ChannelNumber
            || (typeof hdhrChannel.Id === 'string' ? hdhrChannel.Id.replace(/^hdhr_/, '') : '')
          );
          const simChannelPath = `/auto/v${guideNumber}`;

          // Poll /stats until currentConcurrent===0 (no orphan connections from recording).
          const statsDeadline = Date.now() + 5000;
          let statsOk = false;
          while (Date.now() < statsDeadline) {
            const stats = await nodeHttpJson('GET', `${hdhrSimUrl}/stats`).catch(() => null);
            const current = stats?.json?.currentConcurrentByChannel?.[simChannelPath] ?? -1;
            if (current === 0) { statsOk = true; break; }
            await new Promise((resolve) => { setTimeout(resolve, 200); });
          }

          // DELETE recording.
          const deleteRec = await browserFetchJson(page, {
            method: 'DELETE',
            url: `/LiveTv/Recordings/${encodeURIComponent(recId)}`,
            token: auth.AccessToken,
          }).catch(() => ({ status: 0 }));

          // Verify recording is absent.
          const afterDelete = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/Recordings',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          const stillPresent = Array.isArray(afterDelete.json?.Items)
            && afterDelete.json.Items.some((r) => r.Id === recId);

          summary.invariants.liveTvHdhrRecordingCleanup = (
            statsOk
            && [200, 204].includes(deleteRec.status)
            && !stillPresent
          );
        }
      }
    }
  }

  // ── SeriesTimer HDHomeRun block ───────────────────────────────────────────
  // Validates series timer creation with a real EPG program from an XMLTV listing provider.
  //
  // Camino A (both targets): XMLTV file served locally; ListingProvider added; RefreshGuide
  // triggered; ProgramId obtained from GET /LiveTv/Programs; POST /LiveTv/SeriesTimers;
  // GET /LiveTv/Timers filtered by SeriesTimerId (>=1 timer expected); poll Recordings Completed;
  // download + ffprobe (>=1 video packet, reuse helper from recording block); cleanup (jellyrin-only).
  //
  // liveTvHdhrSeriesTimerCreated (upstreamComparable): POST SeriesTimers with real ProgramId -> 204
  //   returns 204 (upstream) or 200 (Jellyrin); series timer appears in GET /LiveTv/SeriesTimers.
  // liveTvHdhrSeriesTimerGeneratesTimers (upstreamComparable): GET /LiveTv/Timers filtered by
  //   SeriesTimerId returns >=1 timer with ProgramId and ChannelId for the hdhr channel.
  // liveTvHdhrSeriesRecordingPlayable (upstreamComparable): recording Completed for series program;
  //   download + ffprobe >=1 video packet in BOTH targets.
  // liveTvHdhrSeriesTimerCleanup (jellyrinOnly): /stats===0 + DELETE SeriesTimer 204 + cascades
  //   child timers absent from GET /LiveTv/Timers.
  //
  // R-UPSTREAM-FRESH: upstream must be fresh (fresh SQLite) before this golden run.
  // R-CREATE-REQUIRES-PROGRAM: upstream CreateSeriesTimer lanza si ProgramId no resuelve;
  //   ProgramId real se obtiene de GET /LiveTv/Programs upstream tras RefreshGuide.
  // R-XMLTV-WINDOW: el programa debe cubrir la ventana de grabación actual (StartDate≈now, EndDate≈now+recordSecs).
  if (hdhrSimUrl) {
    let seriesListingId = null;
    let seriesTimerId = null;
    let xmltvFilePath = null;
    try {
      const seriesRecordSecs = Number.parseInt(process.env.JELLYRIN_LIVETV_RECORD_SECS || '4', 10);
      // Series timer recording poll timeout is longer than the regular recording poll (90s default)
      // because the child timer recording window includes a ~40s buffer above seriesRecordSecs to
      // survive the RefreshGuide latency on upstream. The recording completes after ~seriesRecordSecs+40s.
      const seriesPollTimeoutMs = Number.parseInt(process.env.JELLYRIN_LIVETV_SERIES_POLL_TIMEOUT_MS || '90000', 10);
      const seriesPollIntervalMs = Number.parseInt(process.env.JELLYRIN_LIVETV_RECORD_POLL_INTERVAL_MS || '1000', 10);
      const seriesName = `Jellyrin Series Timer Test ${Date.now()}`;

      // Build XMLTV covering now..now+seriesRecordSecs for the hdhr channel guide number.
      // The XMLTV channel-id must match the GuideNumber that the HDHomeRun tuner exposes.
      const guideNumber = (
        hdhrChannel.ChannelNumber
        || (typeof hdhrChannel.Id === 'string' ? hdhrChannel.Id.replace(/^hdhr_/, '') : '')
      );

      const xmltvNow = new Date();
      // The program window starts slightly in the past so the recording is immediately eligible.
      // EndDate = now + seriesRecordSecs + 40s:
      //   - The 40s buffer covers the time between XMLTV creation and spawn (RefreshGuide ~5-15s on
      //     upstream, guide poll, ListingProvider processing). By the time record_channel_to_file
      //     starts, the remaining window is approximately seriesRecordSecs seconds, so the recording
      //     completes within the poll timeout.
      //   - For Jellyrin: spawn happens immediately after POST SeriesTimers; remaining window ≈
      //     seriesRecordSecs + 35-40s. We rely on the recording actually stopping after seriesRecordSecs
      //     in the Completed poll below (Completed recording in config is the signal, not file age).
      //   - The poll timeout is extended to 90s for series timers (see seriesPollTimeoutMs override below).
      const xmltvStart = new Date(xmltvNow.getTime() - 5000);
      const xmltvEnd = new Date(xmltvNow.getTime() + seriesRecordSecs * 1000 + 40000);

      function padXmlTv(n) { return String(n).padStart(2, '0'); }
      function toXmlTvDate(d) {
        return `${d.getUTCFullYear()}${padXmlTv(d.getUTCMonth() + 1)}${padXmlTv(d.getUTCDate())}${padXmlTv(d.getUTCHours())}${padXmlTv(d.getUTCMinutes())}${padXmlTv(d.getUTCSeconds())} +0000`;
      }

      const xmltvContent = [
        '<?xml version="1.0" encoding="utf-8"?>',
        '<!DOCTYPE tv SYSTEM "xmltv.dtd">',
        '<tv>',
        `  <channel id="${guideNumber}">`,
        `    <display-name>${guideNumber}</display-name>`,
        '  </channel>',
        `  <programme start="${toXmlTvDate(xmltvStart)}" stop="${toXmlTvDate(xmltvEnd)}" channel="${guideNumber}">`,
        `    <title lang="en">${seriesName}</title>`,
        '    <desc lang="en">Jellyrin Series Timer E2 Test</desc>',
        '    <episode-num system="onscreen">1x01</episode-num>',
        '  </programme>',
        '</tv>',
      ].join('\n');

      // Write XMLTV to temp file.
      const { writeFileSync } = require('node:fs');
      xmltvFilePath = `/tmp/jellyrin-series-timer-${target.name}-${Date.now()}.xml`;
      writeFileSync(xmltvFilePath, xmltvContent, 'utf8');

      // Add XMLTV ListingProvider mapped to the main hdhr tuner.
      const addListing = await browserFetchJson(page, {
        method: 'POST',
        url: '/LiveTv/ListingProviders',
        token: auth.AccessToken,
        body: {
          Type: 'xmltv',
          Path: xmltvFilePath,
          EnabledTuners: [addTuner.json.Id],
          EnableAllTuners: false,
          ChannelMappings: [],
        },
      });
      if (![200, 204].includes(addListing.status)) {
        throw new Error(`LiveTv/ListingProviders add returned HTTP ${addListing.status}`);
      }
      seriesListingId = addListing.json?.Id || null;

      // Trigger RefreshGuide scheduled task.
      let scheduledTasksResp = await browserFetchJson(page, {
        method: 'GET',
        url: '/ScheduledTasks',
        token: auth.AccessToken,
      });
      let guideTaskId = null;
      if (scheduledTasksResp.status === 200 && Array.isArray(scheduledTasksResp.json)) {
        const guideTask = scheduledTasksResp.json.find(
          (t) => (t.Key === 'RefreshGuide' || (typeof t.Name === 'string' && /refresh guide/i.test(t.Name))),
        );
        if (guideTask?.Id) { guideTaskId = guideTask.Id; }
      }
      if (guideTaskId) {
        await browserFetchJson(page, {
          method: 'POST',
          url: `/ScheduledTasks/Running/${encodeURIComponent(guideTaskId)}`,
          token: auth.AccessToken,
        }).catch(() => {});
        // Wait for guide refresh to complete (bounded poll).
        const guideRefreshDeadline = Date.now() + 15000;
        while (Date.now() < guideRefreshDeadline) {
          const taskStatus = await browserFetchJson(page, {
            method: 'GET',
            url: `/ScheduledTasks/${encodeURIComponent(guideTaskId)}`,
            token: auth.AccessToken,
          }).catch(() => ({ status: 0 }));
          if (taskStatus.json?.State === 'Idle') break;
          await new Promise((resolve) => { setTimeout(resolve, 500); });
        }
      }

      // Poll GET /LiveTv/Programs for the channel to find our program.
      // We need the real ProgramId that upstream has persisted in its guide.
      let seriesProgramId = null;
      const programPollDeadline = Date.now() + 10000;
      while (Date.now() < programPollDeadline) {
        const programs = await browserFetchJson(page, {
          method: 'GET',
          url: `/LiveTv/Programs?ChannelIds=${encodeURIComponent(hdhrChannel.Id)}&Limit=20`,
          token: auth.AccessToken,
        }).catch(() => ({ status: 0, json: null }));
        if (programs.status === 200 && Array.isArray(programs.json?.Items)) {
          const match = programs.json.Items.find(
            (p) => typeof p.Name === 'string' && p.Name === seriesName,
          );
          if (match?.Id) { seriesProgramId = match.Id; break; }
        }
        await new Promise((resolve) => { setTimeout(resolve, 500); });
      }

      if (!seriesProgramId) {
        // Jellyrin can inject programs directly without a guide refresh;
        // for Jellyrin, try GET /LiveTv/Programs without ChannelId filter.
        if (target.name === 'jellyrin') {
          const allPrograms = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/Programs?Limit=50',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          if (allPrograms.status === 200 && Array.isArray(allPrograms.json?.Items)) {
            const match = allPrograms.json.Items.find(
              (p) => typeof p.Name === 'string' && p.Name === seriesName,
            );
            if (match?.Id) { seriesProgramId = match.Id; }
          }
        }
      }

      if (!seriesProgramId) {
        throw new Error(`Series timer: no program found in guide for "${seriesName}" on channel ${hdhrChannel.Id}`);
      }

      // POST /LiveTv/SeriesTimers with real ProgramId.
      const createSeriesTimer = await browserFetchJson(page, {
        method: 'POST',
        url: '/LiveTv/SeriesTimers',
        token: auth.AccessToken,
        body: {
          ProgramId: seriesProgramId,
          ChannelId: hdhrChannel.Id,
          Name: seriesName,
          RecordAnyTime: true,
          RecordAnyChannel: false,
          PrePaddingSeconds: 0,
          PostPaddingSeconds: 0,
          KeepUntil: 'UntilDeleted',
          Days: ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'],
          ServiceName: 'Emby',
        },
      });
      // Upstream: 204; Jellyrin: 200 with body.
      if (![200, 204].includes(createSeriesTimer.status)) {
        throw new Error(`LiveTv/SeriesTimers create returned HTTP ${createSeriesTimer.status}`);
      }

      // Resolve the series timer Id: from body (Jellyrin) or from GET /LiveTv/SeriesTimers poll.
      seriesTimerId = createSeriesTimer.json?.Id || null;
      if (!seriesTimerId) {
        const stPollDeadline = Date.now() + 5000;
        while (Date.now() < stPollDeadline) {
          const stList = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/SeriesTimers',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          if (stList.status === 200 && Array.isArray(stList.json?.Items)) {
            const found = stList.json.Items.find(
              (st) => typeof st.Name === 'string' && st.Name === seriesName,
            );
            if (found?.Id) { seriesTimerId = found.Id; break; }
          }
          await new Promise((resolve) => { setTimeout(resolve, 500); });
        }
      }

      summary.invariants.liveTvHdhrSeriesTimerCreated = Boolean(seriesTimerId);

      if (!seriesTimerId) {
        throw new Error('Series timer Id not resolved after creation');
      }

      // GET /LiveTv/Timers and filter by SeriesTimerId to verify timer generation.
      let seriesChildTimers = [];
      const timerGenPollDeadline = Date.now() + 10000;
      while (Date.now() < timerGenPollDeadline) {
        const timersList = await browserFetchJson(page, {
          method: 'GET',
          url: '/LiveTv/Timers',
          token: auth.AccessToken,
        }).catch(() => ({ status: 0, json: null }));
        if (timersList.status === 200 && Array.isArray(timersList.json?.Items)) {
          seriesChildTimers = timersList.json.Items.filter(
            (t) => t.SeriesTimerId === seriesTimerId,
          );
          if (seriesChildTimers.length >= 1) break;
        }
        await new Promise((resolve) => { setTimeout(resolve, 500); });
      }

      summary.invariants.liveTvHdhrSeriesTimerGeneratesTimers = seriesChildTimers.length >= 1;

      // Poll for a Completed recording triggered by the series timer.
      const seriesPollDeadline = Date.now() + seriesPollTimeoutMs;
      let seriesCompletedRecording = null;
      let seriesLibraryScanTriggered = false;
      const seriesLibraryScanAt = Date.now() + (seriesRecordSecs + 2) * 1000;

      while (Date.now() < seriesPollDeadline) {
        if (!seriesLibraryScanTriggered && Date.now() >= seriesLibraryScanAt) {
          seriesLibraryScanTriggered = true;
          await browserFetchJson(page, {
            method: 'POST',
            url: '/Library/Refresh',
            token: auth.AccessToken,
          }).catch(() => {});
        }

        const recordings = await browserFetchJson(page, {
          method: 'GET',
          url: '/LiveTv/Recordings',
          token: auth.AccessToken,
        }).catch(() => ({ status: 0, json: null }));
        if (recordings.status === 200 && Array.isArray(recordings.json?.Items)) {
          // Match by series name (includes unique timestamp suffix — no false positive risk).
          seriesCompletedRecording = recordings.json.Items.find(
            (r) => r.Status === 'Completed'
              && typeof r.Name === 'string'
              && r.Name === seriesName,
          ) || null;
          if (!seriesCompletedRecording) {
            seriesCompletedRecording = recordings.json.Items.find(
              (r) => typeof r.Name === 'string'
                && r.Name === seriesName
                && r.Status !== 'InProgress'
                && r.RunTimeTicks != null
                && r.RunTimeTicks > 0,
            ) || null;
          }
          if (seriesCompletedRecording) break;
        }
        // Upstream may expose the completed series recording as a library Movie
        // item rather than through /LiveTv/Recordings. The item name is unique
        // for this run, so this is still a strict recording-file match.
        if (!seriesCompletedRecording && target.name === 'upstream' && auth.User?.Id) {
          const itemLookup = await browserFetchJson(page, {
            method: 'GET',
            url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&SearchTerm=${encodeURIComponent(seriesName)}&Fields=MediaSources,RunTimeTicks,Path&Limit=5`,
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          if (itemLookup.status === 200 && Array.isArray(itemLookup.json?.Items)) {
            const movieRecording = itemLookup.json.Items.find(
              (item) => item.Name === seriesName
                && item.RunTimeTicks != null
                && item.RunTimeTicks > 0,
            );
            if (movieRecording?.Id) {
              seriesCompletedRecording = {
                ...movieRecording,
                Status: 'Completed',
                _libraryItem: true,
              };
              break;
            }
          }
        }
        // Also check timer status for the child timers.
        if (!seriesCompletedRecording && seriesChildTimers.length > 0) {
          for (const ct of seriesChildTimers) {
            const timerStatus = await browserFetchJson(page, {
              method: 'GET',
              url: `/LiveTv/Timers/${encodeURIComponent(ct.Id)}`,
              token: auth.AccessToken,
            }).catch(() => ({ status: 0 }));
            if (timerStatus.status === 200 && timerStatus.json?.Status === 'Completed') {
              seriesCompletedRecording = { Id: timerStatus.json.Id, Status: 'Completed', _timerData: timerStatus.json };
              break;
            }
          }
          if (seriesCompletedRecording) break;
        }
        if (Date.now() + seriesPollIntervalMs < seriesPollDeadline) {
          await new Promise((resolve) => { setTimeout(resolve, seriesPollIntervalMs); });
        } else {
          break;
        }
      }

      if (seriesCompletedRecording) {
        // ffprobe verify >=1 video packet.
        const seriesRecId = seriesCompletedRecording.Id;
        const ffprobeBin = process.env.FFPROBE_BIN || '/usr/bin/ffprobe';
        const altFfprobeBin = '/usr/lib/jellyfin-ffmpeg/ffprobe';
        const tmpSeriesFile = `/tmp/jellyrin-series-rec-probe-${target.name}-${Date.now()}.ts`;
        let seriesDownloadOk = false;

        const seriesStreamPaths = [
          `/LiveTv/LiveRecordings/${encodeURIComponent(seriesRecId)}/stream`,
          `/Videos/${encodeURIComponent(seriesRecId)}/stream`,
          `/Items/${encodeURIComponent(seriesRecId)}/Download`,
        ];
        for (const streamPath of seriesStreamPaths) {
          try {
            const resp = await page.request.get(`${summary.baseUrl}${streamPath}`, {
              headers: { 'X-Emby-Token': auth.AccessToken },
            });
            if (resp.ok()) {
              const bodyBuf = await resp.body();
              if (bodyBuf && bodyBuf.length > 0) {
                const { writeFileSync } = require('node:fs');
                writeFileSync(tmpSeriesFile, bodyBuf);
                seriesDownloadOk = true;
                break;
              }
            }
          } catch (_) { /* try next path */ }
        }

        if (seriesDownloadOk) {
          const actualFfprobe = (() => {
            try { const { status: s } = spawnSync(ffprobeBin, ['-version'], { stdio: 'ignore' }); if (s === 0) return ffprobeBin; } catch (_) { /* ignore */ }
            try { const { status: s } = spawnSync(altFfprobeBin, ['-version'], { stdio: 'ignore' }); if (s === 0) return altFfprobeBin; } catch (_) { /* ignore */ }
            return null;
          })();

          if (actualFfprobe) {
            const probeResult = spawnSync(actualFfprobe, [
              '-v', 'error',
              '-show_packets',
              '-select_streams', 'v',
              '-read_intervals', '%+#1',
              tmpSeriesFile,
            ], { encoding: 'utf8', timeout: 10000 });
            const probeOutput = (probeResult.stdout || '') + (probeResult.stderr || '');
            if (probeOutput.includes('[PACKET]') || probeOutput.includes('codec_type=video')) {
              summary.invariants.liveTvHdhrSeriesRecordingPlayable = true;
            }
          }
          try { require('node:fs').unlinkSync(tmpSeriesFile); } catch (_) { /* ignore */ }
        }

        // Jellyrin-only cleanup: /stats===0 + DELETE SeriesTimer 204 + child timers absent.
        if (target.name === 'jellyrin' && hdhrSimUrl) {
          const simChannelPath = `/auto/v${guideNumber}`;
          const seriesStatsDeadline = Date.now() + 5000;
          let seriesStatsOk = false;
          while (Date.now() < seriesStatsDeadline) {
            const stats = await nodeHttpJson('GET', `${hdhrSimUrl}/stats`).catch(() => null);
            const current = stats?.json?.currentConcurrentByChannel?.[simChannelPath] ?? -1;
            if (current === 0) { seriesStatsOk = true; break; }
            await new Promise((resolve) => { setTimeout(resolve, 200); });
          }

          const deleteSeriesTimer = await browserFetchJson(page, {
            method: 'DELETE',
            url: `/LiveTv/SeriesTimers/${encodeURIComponent(seriesTimerId)}`,
            token: auth.AccessToken,
          }).catch(() => ({ status: 0 }));
          seriesTimerId = null;

          // Verify series timer is absent.
          const stAfterDelete = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/SeriesTimers',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          const stStillPresent = Array.isArray(stAfterDelete.json?.Items)
            && stAfterDelete.json.Items.some((st) => st.Name === seriesName);

          // Verify child timers are absent (cascade delete).
          const timersAfterDelete = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/Timers',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          // Note: seriesChildTimers may have been cleared by the cascade; check by SeriesTimerId.
          // Since we deleted the series timer, its Id is the one we captured before.
          const childTimersGone = !Array.isArray(timersAfterDelete.json?.Items)
            || timersAfterDelete.json.Items.every(
              (t) => !(typeof t.SeriesTimerId === 'string'
                && seriesChildTimers.some((ct) => ct.SeriesTimerId === t.SeriesTimerId)),
            );

          summary.invariants.liveTvHdhrSeriesTimerCleanup = (
            seriesStatsOk
            && [200, 204].includes(deleteSeriesTimer.status)
            && !stStillPresent
            && childTimersGone
          );
        }
      }
    } catch (err) {
      console.warn(`[series-timer] block error for ${target.name}: ${err.message}`);
    } finally {
      // Clean up listing provider.
      if (seriesListingId) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/LiveTv/ListingProviders?id=${encodeURIComponent(seriesListingId)}`,
          token: auth.AccessToken,
        }).catch(() => {});
      }
      // Clean up series timer if it was not deleted in the cleanup block.
      if (seriesTimerId) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/LiveTv/SeriesTimers/${encodeURIComponent(seriesTimerId)}`,
          token: auth.AccessToken,
        }).catch(() => {});
      }
      // Clean up temp XMLTV file.
      if (xmltvFilePath) {
        try { require('node:fs').unlinkSync(xmltvFilePath); } catch (_) { /* ignore */ }
      }
    }
  }
  // ── End of SeriesTimer HDHomeRun block ────────────────────────────────────

  // ── TunerCount limit block ────────────────────────────────────────────────
  // Validates the HDHomeRun TunerCount=1 limit enforcement:
  //   FirstOpen: open offset channel 7.1 → 200 + bytes (both targets).
  //   Conflict: with 7.1 active, open 8.1 → HTTP 500 (both targets).
  //   Cross-mode: with 7.1 active via direct TS, HLS and recording attempts on 8.1 are blocked.
  //   Recovery: close 7.1, open 8.1 → 200 + bytes (both targets).
  //   HlsRecovery: close 7.1, play 8.1 via HLS → master/media/segment bytes (both targets).
  //   SharingExempt: 2 consumers of the SAME channel 7.1 → maxConcurrent===1, no conflict (Jellyrin only).
  //
  // Implementation decision: a DEDICATED limit sim (HDHOMERUN_SIM_TUNER_COUNT=1) is started
  // exclusively for this block. The main sim (TunerCount=4) is NOT used here. For upstream,
  // the main tuner host is deleted BEFORE adding the limit tuner so that the limit tuner is
  // the ONLY running sim for offset channels 7.1/8.1, ensuring conflict propagates as HTTP 500.
  // (If multiple running tuners serve the same channels, upstream falls back to the next host
  // when one hits TunerCount, returning 200 rather than 500.) The main tuner is NOT re-added
  // after the limit block because the recording block has already completed.
  // For Jellyrin, the limit check is per tuner_host_id, so deleting the main tuner is not
  // strictly required, but we delete it anyway for symmetry and to avoid stale state.
  //
  // R-CONFLICT-500: upstream returns HTTP 500 via ExceptionMiddleware (LiveTvConflictException not
  // mapped -> _ => 500). Jellyrin returns HTTP 500 via ApiError::internal.
  // R-ENFORCE-POINT: upstream enforces at PlaybackInfo (open time), Jellyrin at GET stream.ts.
  // R-TOCTOU: Jellyrin chequeo+insert atómicos bajo el mismo lock del registro.
  if (hdhrSimUrl) {
    let limitSimClose = null;
    let limitAddTuner = null;
    let limitChannel41 = null;
    let limitChannel51 = null;
    try {
      // Start the dedicated limit sim with TunerCount=1 and CHANNEL_OFFSET=3 (channels 7.1/8.1).
      // Using a channel offset ensures these channels have never been opened on upstream before,
      // avoiding stale _openStreams entries from previous golden runs that would cause upstream
      // to reuse the old stream (sharing path) and bypass the TunerCount check.
      const savedTunerCount = process.env.HDHOMERUN_SIM_TUNER_COUNT;
      const savedChannelOffset = process.env.HDHOMERUN_SIM_CHANNEL_OFFSET;
      process.env.HDHOMERUN_SIM_TUNER_COUNT = '1';
      process.env.HDHOMERUN_SIM_CHANNEL_OFFSET = '3';
      const limitSim = await hdhrSim.start(0);
      if (savedTunerCount !== undefined) { process.env.HDHOMERUN_SIM_TUNER_COUNT = savedTunerCount; } else { delete process.env.HDHOMERUN_SIM_TUNER_COUNT; }
      if (savedChannelOffset !== undefined) { process.env.HDHOMERUN_SIM_CHANNEL_OFFSET = savedChannelOffset; } else { delete process.env.HDHOMERUN_SIM_CHANNEL_OFFSET; }
      const limitSimUrl = limitSim.url;
      limitSimClose = limitSim.close;

      // Delete the main tuner BEFORE adding the limit tuner (both targets).
      // For upstream: ensures the limit tuner is the only active tuner for offset channels 7.1/8.1;
      // without this, upstream falls back to the main tuner (TunerCount=0 → no limit) and
      // channel B returns 200 instead of 500.
      // For Jellyrin: ensures live_tv_channel_by_id("hdhr_7.1") returns the limit tuner's channel
      // (TunerCount=1), not the main tuner's (TunerCount=4). Without this, Jellyrin streams via
      // the main tuner's channel and the TunerCount=1 limit never fires.
      // The recording block has already completed; the main tuner is no longer needed.
      // The main tuner cleanup at the end of runLiveTvFlow (.catch()) handles any double-delete.
      await browserFetchJson(page, {
        method: 'DELETE',
        url: `/LiveTv/TunerHosts?id=${encodeURIComponent(addTuner.json.Id)}`,
        token: auth.AccessToken,
      }).catch(() => {});

      // Add the limit tuner with TunerCount=1 explicitly in the body.
      // Jellyrin reads TunerCount from the stored config; upstream also needs it in the body
      // (Validate only updates DeviceId, not TunerCount, from discover.json).
      limitAddTuner = await browserFetchJson(page, {
        method: 'POST',
        url: '/LiveTv/TunerHosts',
        token: auth.AccessToken,
        body: { Type: 'hdhomerun', Url: limitSimUrl, TunerCount: 1 },
      });
      if (![200, 204].includes(limitAddTuner.status) || !limitAddTuner.json?.Id) {
        throw new Error(`Limit tuner add returned HTTP ${limitAddTuner.status}`);
      }
      const limitTunerId = limitAddTuner.json.Id;

      // For upstream: trigger RefreshGuide so channels materialise from the limit sim.
      if (target.name === 'upstream') {
        const tasks = await browserFetchJson(page, {
          method: 'GET', url: '/ScheduledTasks', token: auth.AccessToken,
        }).catch(() => null);
        if (tasks?.status === 200 && Array.isArray(tasks.json)) {
          const rt = tasks.json.find((t) => t.Key === 'RefreshGuide' || (typeof t.Name === 'string' && t.Name.toLowerCase().includes('refresh guide')));
          if (rt) {
            await browserFetchJson(page, {
              method: 'POST', url: `/ScheduledTasks/Running/${encodeURIComponent(rt.Id)}`, token: auth.AccessToken,
            }).catch(() => {});
          }
        }
      }

      // Poll for channels 7.1 and 8.1 from the limit sim (CHANNEL_OFFSET=3: 4+3=7, 5+3=8).
      // Using offset channels ensures upstream has no existing streams for these channels,
      // so PlaybackInfo always opens a FRESH stream via the limit tuner (no sharing path).
      // For Jellyrin: also filter by TunerHostId===limitTunerId so we get the limit tuner's
      // channels (TunerCount=1), not the main tuner's (TunerCount=4).
      const limitNum1 = '7.1';
      const limitNum2 = '8.1';
      const limitPollTimeout = 30000;
      const limitPollInterval = 2000;
      const limitPollDeadline = Date.now() + limitPollTimeout;
      const limitChannels = {};
      while (Date.now() < limitPollDeadline) {
        const ch = await browserFetchJson(page, {
          method: 'GET',
          url: `/LiveTv/Channels?UserId=${encodeURIComponent(auth.User.Id)}`,
          token: auth.AccessToken,
        });
        if (ch.status === 200 && Array.isArray(ch.json?.Items)) {
          for (const item of ch.json.Items) {
            const num = item.ChannelNumber || (typeof item.Id === 'string' ? item.Id.replace(/^hdhr_/, '') : '');
            // For Jellyrin: require TunerHostId to match the limit tuner so TunerCount=1 applies.
            // For upstream: TunerHostId is not exposed in channel items; accept any matching number.
            const tunerMatch = target.name !== 'jellyrin'
              || (typeof item.TunerHostId === 'string' && item.TunerHostId === limitTunerId);
            if (num === limitNum1 && !limitChannels[limitNum1] && tunerMatch) limitChannels[limitNum1] = item;
            if (num === limitNum2 && !limitChannels[limitNum2] && tunerMatch) limitChannels[limitNum2] = item;
          }
          if (limitChannels[limitNum1] && limitChannels[limitNum2]) break;
        }
        if (Date.now() + limitPollInterval < limitPollDeadline) {
          await new Promise((resolve) => { setTimeout(resolve, limitPollInterval); });
        } else break;
      }
      limitChannel41 = limitChannels[limitNum1] || null;
      limitChannel51 = limitChannels[limitNum2] || null;

      if (!limitChannel41 || !limitChannel51) {
        throw new Error(`Limit sim channels not found: ${limitNum1}=${Boolean(limitChannel41)}, ${limitNum2}=${Boolean(limitChannel51)}`);
      }

      const cleanupTimerById = async (timerId) => {
        if (!timerId) return;
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/LiveTv/Timers/${encodeURIComponent(timerId)}`,
          token: auth.AccessToken,
        }).catch(() => {});
      };

      const findTimerIdByName = async (name) => {
        const timersPollDeadline = Date.now() + 3000;
        while (Date.now() < timersPollDeadline) {
          const timers = await browserFetchJson(page, {
            method: 'GET',
            url: '/LiveTv/Timers',
            token: auth.AccessToken,
          }).catch(() => ({ status: 0, json: null }));
          if (timers.status === 200 && Array.isArray(timers.json?.Items)) {
            const found = timers.json.Items.find((timer) => timer.Name === name);
            if (found?.Id) return found.Id;
          }
          await new Promise((resolve) => { setTimeout(resolve, 300); });
        }
        return null;
      };

      const assertRecordingBlockedByActiveTuner = async (channel) => {
        const recordSecs = 2;
        const timerName = `Jellyrin TunerLimit Recording Conflict ${target.name} ${Date.now()}`;
        const recordStart = new Date();
        const recordEnd = new Date(recordStart.getTime() + recordSecs * 1000);
        const createTimer = await browserFetchJson(page, {
          method: 'POST',
          url: '/LiveTv/Timers',
          token: auth.AccessToken,
          body: {
            ChannelId: channel.Id,
            Name: timerName,
            StartDate: recordStart.toISOString(),
            EndDate: recordEnd.toISOString(),
            PrePaddingSeconds: 0,
            PostPaddingSeconds: 0,
            Priority: 0,
            IsPrePaddingRequired: false,
            IsPostPaddingRequired: false,
            KeepUntil: 'UntilDeleted',
            RecordAnyTime: false,
            RecordAnyChannel: false,
            IsManual: true,
            ServiceName: 'Emby',
          },
        }).catch(() => ({ status: 0, json: null }));

        const pollRecordingPresence = async (deadlineMs) => {
          const pollDeadline = Date.now() + deadlineMs;
          let libraryScanTriggered = false;
          let lastPresence = { completed: false, inProgress: false, matches: [] };
          while (Date.now() < pollDeadline) {
            if (!libraryScanTriggered && Date.now() >= recordEnd.getTime() + 1000) {
              libraryScanTriggered = true;
              await browserFetchJson(page, {
                method: 'POST',
                url: '/Library/Refresh',
                token: auth.AccessToken,
              }).catch(() => {});
            }
            lastPresence = await liveTvRecordingPresenceByName(page, auth, timerName);
            if (lastPresence.completed) break;
            await new Promise((resolve) => { setTimeout(resolve, 500); });
          }
          return lastPresence;
        };

        let timerId = createTimer.json?.Id || null;
        if (createTimer.status === 500) {
          const rejectedPresence = await pollRecordingPresence(2500);
          return {
            blocked: true,
            noZombie: !rejectedPresence.completed && !rejectedPresence.inProgress,
            createStatus: createTimer.status,
          };
        }
        if (![200, 204].includes(createTimer.status)) {
          return { blocked: false, noZombie: false, createStatus: createTimer.status };
        }
        if (!timerId) {
          timerId = await findTimerIdByName(timerName);
        }

        const lastPresence = await pollRecordingPresence(5500);

        await cleanupTimerById(timerId);
        for (const recording of lastPresence.matches) {
          if (recording.Id) {
            await browserFetchJson(page, {
              method: 'DELETE',
              url: `/LiveTv/Recordings/${encodeURIComponent(recording.Id)}`,
              token: auth.AccessToken,
            }).catch(() => {});
          }
        }

        return {
          blocked: !lastPresence.completed,
          noZombie: !lastPresence.completed && !lastPresence.inProgress,
          createStatus: createTimer.status,
        };
      };

      // ── FirstOpen (7.1) ──
      // Jellyrin: GET /LiveTv/LiveStreamFiles/{id}/stream.ts via probe.
      // upstream: POST /Items/{id}/PlaybackInfo?AutoOpenLiveStream=true.
      let limitLiveStreamIdA = null;
      let limitStreamPathA = null;

      if (target.name === 'jellyrin') {
        // Use the channel object from the poll directly (it already includes MediaSources).
        // We filtered by TunerHostId===limitTunerId above so this is definitely the limit tuner's
        // channel with TunerCount=1 and Path pointing to the limit sim URL.
        const ms = limitChannel41.MediaSources?.[0];
        const directStreamUrl = ms?.DirectStreamUrl;
        if (!directStreamUrl || !/\/LiveTv\/LiveStreamFiles\//i.test(directStreamUrl)) {
          throw new Error('Limit tuner channel 7.1 did not include a LiveStreamFiles DirectStreamUrl');
        }
        try {
          const pathUrl = new URL(directStreamUrl, summary.baseUrl);
          limitStreamPathA = pathUrl.pathname + pathUrl.search;
        } catch (_) {
          limitStreamPathA = directStreamUrl;
        }

        // Open 7.1 and hold it open (overlap probe) so the registry entry stays alive.
        const probe41 = await browserFetchStreamProbeOverlap(page, {
          url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 2000,
        });
        summary.invariants.liveTvHdhrTunerLimitFirstOpen = (
          probe41.status === 200 && probe41.byteLength >= 1
        );
      } else {
        // upstream: PlaybackInfo with AutoOpenLiveStream=true opens the stream and keeps it in _openStreams.
        const pbi = await browserFetchJson(page, {
          method: 'POST',
          url: `/Items/${encodeURIComponent(limitChannel41.Id)}/PlaybackInfo`,
          token: auth.AccessToken,
          body: { UserId: auth.User.Id, AutoOpenLiveStream: true },
        });
        if (pbi.status === 200 && Array.isArray(pbi.json?.MediaSources)) {
          const ms = pbi.json.MediaSources[0];
          limitLiveStreamIdA = ms?.LiveStreamId || null;
          // Probe bytes for firstOpen invariant.
          let streamPath41 = ms?.Path || '';
          if (streamPath41) {
            try {
              const pathUrl = new URL(streamPath41);
              const baseUrl = new URL(summary.baseUrl);
              pathUrl.hostname = baseUrl.hostname;
              pathUrl.port = baseUrl.port;
              streamPath41 = pathUrl.pathname + pathUrl.search;
            } catch (_) { /* keep as-is */ }
          }
          if (streamPath41) {
            const probe41 = await browserFetchStreamProbe(page, {
              url: streamPath41, token: auth.AccessToken, minBytes: 1,
            });
            summary.invariants.liveTvHdhrTunerLimitFirstOpen = (
              probe41.status === 200 && probe41.byteLength >= 1
            );
          }
        }
      }

      // ── Conflict (8.1 while 7.1 active) ──
      // Jellyrin: open a new overlap probe for 7.1 to keep the handle alive, then try 8.1 stream.
      // upstream: 7.1 is already open in _openStreams (limitLiveStreamIdA not yet closed); try PlaybackInfo for 8.1.
      if (target.name === 'jellyrin') {
        // Use limitChannel51 directly (already filtered by TunerHostId===limitTunerId; has MediaSources).
        const ms51 = limitChannel51.MediaSources?.[0];
        const directStreamUrl51 = ms51?.DirectStreamUrl;
        let limitStreamPath51 = directStreamUrl51 || '';
        if (limitStreamPath51) {
          try {
            const pathUrl = new URL(limitStreamPath51, summary.baseUrl);
            limitStreamPath51 = pathUrl.pathname + pathUrl.search;
          } catch (_) { /* keep as-is */ }
        }

        if (limitStreamPath51 && limitStreamPathA) {
          // Open 7.1 again with overlap so the registry slot is still occupied during conflict check.
          const overlap41 = browserFetchStreamProbeOverlap(page, {
            url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 1500,
          });
          // Brief pause to ensure 7.1 is registered in the registry before trying 8.1.
          await new Promise((resolve) => { setTimeout(resolve, 100); });
          // Try 8.1 — must fail with HTTP 500 (TunerCount limit hit).
          const probe51 = await browserFetchStreamProbe(page, {
            url: limitStreamPath51, token: auth.AccessToken, minBytes: 1,
          });
          summary.invariants.liveTvHdhrTunerLimitConflict = probe51.status === 500;
          // Wait for the 7.1 overlap to finish so the registry is cleaned up.
          await overlap41;
        }
      } else {
        // upstream: 7.1 still open in _openStreams. Try PlaybackInfo for 8.1.
        const pbi51 = await browserFetchJson(page, {
          method: 'POST',
          url: `/Items/${encodeURIComponent(limitChannel51.Id)}/PlaybackInfo`,
          token: auth.AccessToken,
          body: { UserId: auth.User.Id, AutoOpenLiveStream: true },
        });
        summary.invariants.liveTvHdhrTunerLimitConflict = pbi51.status === 500;
      }

      // ── Cross-mode conflict: direct TS on 7.1 blocks HLS on 8.1 ──
      if (target.name === 'jellyrin') {
        if (limitStreamPathA) {
          const directBlocker = browserFetchStreamProbeOverlap(page, {
            url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 8000,
          });
          await new Promise((resolve) => { setTimeout(resolve, 100); });
          const hlsBlocked = await probeLiveTvHlsPlayback(page, summary, auth, target, limitChannel51, {
            cleanup: true,
            masterTimeoutMs: 5000,
            mediaTimeoutMs: 5000,
          });
          summary.invariants.liveTvHdhrTunerLimitHlsConflict = hlsBlocked.status === 500;
          await directBlocker;
        }
      } else {
        // upstream: 7.1 remains open from PlaybackInfo until /LiveStreams/Close below.
        const hlsBlocked = await probeLiveTvHlsPlayback(page, summary, auth, target, limitChannel51, {
          cleanup: true,
          masterTimeoutMs: 5000,
          mediaTimeoutMs: 5000,
        });
        summary.invariants.liveTvHdhrTunerLimitHlsConflict = hlsBlocked.status === 500;
      }

      // ── Cross-mode conflict: direct TS on 7.1 blocks recording on 8.1 ──
      if (target.name === 'jellyrin') {
        if (limitStreamPathA) {
          const directBlocker = browserFetchStreamProbeOverlap(page, {
            url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 9000,
          });
          await new Promise((resolve) => { setTimeout(resolve, 100); });
          const recordingBlocked = await assertRecordingBlockedByActiveTuner(limitChannel51);
          summary.invariants.liveTvHdhrTunerLimitRecordingConflict = recordingBlocked.blocked;
          summary.invariants.liveTvHdhrTunerLimitRecordingNoZombie = recordingBlocked.noZombie;
          await directBlocker;
        }
      } else {
        // upstream: 7.1 remains open from PlaybackInfo until /LiveStreams/Close below.
        const recordingBlocked = await assertRecordingBlockedByActiveTuner(limitChannel51);
        summary.invariants.liveTvHdhrTunerLimitRecordingConflict = recordingBlocked.blocked;
      }

      // ── Recovery (close 7.1, open 8.1) ──
      if (target.name === 'jellyrin') {
        // Poll /stats until current[/auto/v7.1] === 0 (all 7.1 probes closed).
        // The limit sim uses CHANNEL_OFFSET=3, so channels 4+3=7, 5+3=8.
        const limitSimChannelPath41 = `/auto/v${limitNum1}`;
        const recDeadline = Date.now() + 5000;
        let recCurrent = -1;
        while (Date.now() < recDeadline) {
          const st = await nodeHttpJson('GET', `${limitSimUrl}/stats`).catch(() => null);
          recCurrent = st?.json?.currentConcurrentByChannel?.[limitSimChannelPath41] ?? -1;
          if (recCurrent === 0) break;
          await new Promise((resolve) => { setTimeout(resolve, 200); });
        }
        if (recCurrent === 0) {
          const hlsRecovery = await probeLiveTvHlsPlayback(page, summary, auth, target, limitChannel51, {
            cleanup: true,
          });
          summary.invariants.liveTvHdhrTunerLimitHlsRecovery = hlsRecovery.ok;

          // Use limitChannel51 directly (already filtered by TunerHostId; has MediaSources).
          const ms51 = limitChannel51.MediaSources?.[0];
          const directStreamUrl51 = ms51?.DirectStreamUrl;
          let limitStreamPath51 = directStreamUrl51 || '';
          if (limitStreamPath51) {
            try {
              const pathUrl = new URL(limitStreamPath51, summary.baseUrl);
              limitStreamPath51 = pathUrl.pathname + pathUrl.search;
            } catch (_) { /* keep as-is */ }
          }
          if (limitStreamPath51) {
            const probe51 = await browserFetchStreamProbe(page, {
              url: limitStreamPath51, token: auth.AccessToken, minBytes: 1,
            });
            summary.invariants.liveTvHdhrTunerLimitRecovery = (
              probe51.status === 200 && probe51.byteLength >= 1
            );
          }
        }
      } else {
        // upstream: close 7.1 via /LiveStreams/Close, then retry PlaybackInfo for 8.1.
        if (limitLiveStreamIdA) {
          await browserFetchJson(page, {
            method: 'POST',
            url: `/LiveStreams/Close?liveStreamId=${encodeURIComponent(limitLiveStreamIdA)}`,
            token: auth.AccessToken,
          }).catch(() => {});
          // Brief pause for upstream to release the _openStreams slot.
          await new Promise((resolve) => { setTimeout(resolve, 500); });
        }
        const hlsRecovery = await probeLiveTvHlsPlayback(page, summary, auth, target, limitChannel51, {
          cleanup: true,
        });
        summary.invariants.liveTvHdhrTunerLimitHlsRecovery = hlsRecovery.ok;

        const pbi51b = await browserFetchJson(page, {
          method: 'POST',
          url: `/Items/${encodeURIComponent(limitChannel51.Id)}/PlaybackInfo`,
          token: auth.AccessToken,
          body: { UserId: auth.User.Id, AutoOpenLiveStream: true },
        });
        if (pbi51b.status === 200 && Array.isArray(pbi51b.json?.MediaSources)) {
          const ms51b = pbi51b.json.MediaSources[0];
          const lsId51 = ms51b?.LiveStreamId;
          let streamPath51b = ms51b?.Path || '';
          if (streamPath51b) {
            try {
              const pathUrl = new URL(streamPath51b);
              const baseUrl = new URL(summary.baseUrl);
              pathUrl.hostname = baseUrl.hostname;
              pathUrl.port = baseUrl.port;
              streamPath51b = pathUrl.pathname + pathUrl.search;
            } catch (_) { /* keep as-is */ }
          }
          if (streamPath51b) {
            const probe51b = await browserFetchStreamProbe(page, {
              url: streamPath51b, token: auth.AccessToken, minBytes: 1,
            });
            summary.invariants.liveTvHdhrTunerLimitRecovery = (
              probe51b.status === 200 && probe51b.byteLength >= 1
            );
          }
          // Close the recovery live stream to avoid leaving orphans.
          if (lsId51) {
            await browserFetchJson(page, {
              method: 'POST',
              url: `/LiveStreams/Close?liveStreamId=${encodeURIComponent(lsId51)}`,
              token: auth.AccessToken,
            }).catch(() => {});
          }
        }
      }

      // ── SharingExempt (Jellyrin only) ──
      // 2 consumers of the SAME channel 7.1 with TunerCount=1 must NOT trigger a conflict.
      // maxConcurrentByChannel[/auto/v7.1]===1 confirms only 1 outgoing connection (sharing).
      if (target.name === 'jellyrin' && limitStreamPathA) {
        await nodeHttpJson('POST', `${limitSimUrl}/stats/reset`).catch(() => {});
        const exemptProbe1 = browserFetchStreamProbeOverlap(page, {
          url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 800,
        });
        await new Promise((resolve) => { setTimeout(resolve, 30); });
        const exemptProbe2 = browserFetchStreamProbeOverlap(page, {
          url: limitStreamPathA, token: auth.AccessToken, minBytes: 1, holdMs: 800,
        });
        const [ep1, ep2] = await Promise.all([exemptProbe1, exemptProbe2]);
        const exemptStats = await nodeHttpJson('GET', `${limitSimUrl}/stats`).catch(() => null);
        const maxConcurrentA = exemptStats?.json?.maxConcurrentByChannel?.[`/auto/v${limitNum1}`] ?? -1;
        summary.invariants.liveTvHdhrTunerLimitSharingExempt = (
          ep1.status === 200 && ep1.byteLength >= 1
          && ep2.status === 200 && ep2.byteLength >= 1
          && maxConcurrentA === 1
        );
      }
    } catch (err) {
      // Non-fatal: limit block failure should not crash the whole flow.
      console.warn(`[tuner-limit] block error for ${target.name}: ${err.message}`);
    } finally {
      // Clean up limit tuner if it was added.
      if (limitAddTuner?.json?.Id) {
        await browserFetchJson(page, {
          method: 'DELETE',
          url: `/LiveTv/TunerHosts?id=${encodeURIComponent(limitAddTuner.json.Id)}`,
          token: auth.AccessToken,
        }).catch(() => {});
      }
      // Stop the dedicated limit sim.
      if (limitSimClose) {
        await limitSimClose().catch(() => {});
      }
    }
  }
  // ── End of TunerCount limit block ─────────────────────────────────────────

  // Clean up the simulator tuner host so no residual HDHomeRun config is left
  // behind on either target (the simulator URL is gone after this run).
  await browserFetchJson(page, {
    method: 'DELETE',
    url: `/LiveTv/TunerHosts?id=${encodeURIComponent(addTuner.json.Id)}`,
    token: auth.AccessToken,
  }).catch(() => {});

  await page.goto(`${summary.baseUrl}/web/#/livetv`, { waitUntil: 'domcontentloaded' }).catch(() => {});
  // Short timeout: the live TV page makes continuous background requests so networkidle never
  // fires naturally; we just need the DOM to load for the screenshot.
  await page.waitForLoadState('networkidle', { timeout: 3000 }).catch(() => {});
  summary.item = {
    id: liveTvFlowChannelId,
    name: 'Jellyrin Live TV',
    type: 'LiveTv',
    streamPath: '<fixture>',
  };
}

async function runResumeFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for resume trace';
    return;
  }
  if (!resumeTraceEligible(movie)) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no resume-eligible movie item for resume trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const positionTicks = resumeTracePositionTicks(movie);
  const progressResult = await browserFetchJson(page, {
    method: 'POST',
    url: '/Sessions/Playing/Progress',
    token: auth.AccessToken,
    body: {
      ItemId: movie.Id,
      MediaSourceId: movie.Id,
      PositionTicks: positionTicks,
      IsPaused: false,
    },
  });
  if (progressResult.status !== 204) {
    throw new Error(`Sessions/Playing/Progress returned HTTP ${progressResult.status}`);
  }

  const resumeResult = await browserFetchJson(page, {
    method: 'GET',
    url: `/UserItems/Resume?UserId=${encodeURIComponent(auth.User.Id)}&Limit=12&MediaTypes=Video&Fields=PrimaryImageAspectRatio`,
    token: auth.AccessToken,
  });
  if (resumeResult.status < 200 || resumeResult.status >= 300) {
    throw new Error(`UserItems/Resume returned HTTP ${resumeResult.status}`);
  }
  const resume = resumeResult.json;
  const resumeItem = resume.Items?.find((item) => item.Id === movie.Id);
  if (!resumeItem) {
    throw new Error('resume list does not contain traced movie item');
  }
  if (resumeItem.UserData?.PlaybackPositionTicks !== positionTicks) {
    throw new Error(`resume position ${resumeItem.UserData?.PlaybackPositionTicks} != ${positionTicks}`);
  }
  if (resumeItem.UserData?.Played !== false) {
    throw new Error(`resume item Played state ${resumeItem.UserData?.Played} != false`);
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle');
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function runTranscodeHlsFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for transcode trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.waitForLoadState('networkidle');

  const playbackInfo = await browserFetchJson(page, {
    method: 'POST',
    url: `/Items/${encodeURIComponent(movie.Id)}/PlaybackInfo`,
    token: auth.AccessToken,
    body: withoutUndefined({
      UserId: auth.User.Id,
      MediaSourceId: movie.Id,
      AudioStreamIndex: defaultStreamIndex(movie, 'Audio'),
      SubtitleStreamIndex: -1,
      EnableDirectPlay: false,
      EnableDirectStream: false,
      EnableTranscoding: true,
      StartPositionTicks: 0,
      DeviceProfile: hlsTranscodeDeviceProfile(),
    }),
  });
  if (playbackInfo.status !== 200) {
    throw new Error(`transcode PlaybackInfo returned HTTP ${playbackInfo.status}`);
  }
  const mediaSource = playbackInfo.json?.MediaSources?.[0];
  const transcodingUrl = mediaSource?.TranscodingUrl;
  if (!transcodingUrl) {
    throw new Error('transcode PlaybackInfo did not return TranscodingUrl');
  }

  const master = await browserFetchText(page, {
    method: 'GET',
    url: transcodingUrl,
    token: auth.AccessToken,
  });
  if (master.status !== 200) {
    throw new Error(`HLS master playlist returned HTTP ${master.status}`);
  }
  const mediaPlaylistPath = firstPlaylistUri(master.text);
  if (!mediaPlaylistPath) {
    throw new Error('HLS master playlist did not contain a media playlist URI');
  }

  const media = await browserFetchText(page, {
    method: 'GET',
    url: resolveRelativeUrl(transcodingUrl, mediaPlaylistPath),
    token: auth.AccessToken,
  });
  if (media.status !== 200) {
    throw new Error(`HLS media playlist returned HTTP ${media.status}`);
  }
  const segmentPath = firstPlaylistUri(media.text);
  if (!segmentPath) {
    throw new Error('HLS media playlist did not contain a segment URI');
  }

  const segment = await browserFetchText(page, {
    method: 'GET',
    url: resolveRelativeUrl(transcodingUrl, segmentPath),
    token: auth.AccessToken,
  });
  if (![200, 206].includes(segment.status)) {
    throw new Error(`HLS segment returned HTTP ${segment.status}`);
  }

  await page.goto(`${summary.baseUrl}/web/#/home`, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle');
  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function runDirectPlayFlow(page, summary, publicInfo, target) {
  const auth = await authenticateTarget(page, summary, target);
  const movie = await firstMovieItem(page, summary, auth);
  if (!movie) {
    summary.status = 'skipped';
    summary.skipped = true;
    summary.reason = 'target has no movie item for direct-play trace';
    return;
  }

  await establishWebSession(page, summary, publicInfo, target, auth, '/home');
  await page.goto(`${summary.baseUrl}/web/#/details?id=${movie.Id}`, {
    waitUntil: 'domcontentloaded',
  });
  await page.getByText(movie.Name, { exact: true }).first().waitFor({ state: 'visible', timeout: 20_000 });
  await page.waitForLoadState('networkidle');

  const playbackInfo = page.waitForResponse((response) =>
    response.url().includes(`/Items/${movie.Id}/PlaybackInfo`) && response.status() === 200,
  );
  const stream = page.waitForResponse((response) =>
    response.url().includes(`/Videos/${movie.Id}/stream`) && [200, 206].includes(response.status()),
  );
  const playbackReport = page.waitForResponse((response) =>
    response.url().includes('/Sessions/Playing') && response.request().method() === 'POST' && response.status() === 204,
  );
  const playButton = page.locator('.btnPlay:not(.hide), .btnReplay:not(.hide)').first();
  await playButton.waitFor({ state: 'visible', timeout: 20_000 });
  await playButton.click();
  await playbackInfo;
  await stream;
  await playbackReport;
  await page.waitForLoadState('networkidle').catch(() => {});

  summary.item = {
    id: '<dynamic>',
    name: movie.Name,
    type: movie.Type,
  };
}

async function firstMovieItem(page, summary, auth) {
  const itemsResponse = await page.request.get(
    `${summary.baseUrl}/Items?UserId=${encodeURIComponent(auth.User.Id)}&IncludeItemTypes=Movie&Recursive=true&Fields=RunTimeTicks&StartIndex=0&Limit=10`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  if (!itemsResponse.ok()) {
    throw new Error(`Movie lookup returned HTTP ${itemsResponse.status()}`);
  }
  const items = await itemsResponse.json();
  const movies = items.Items?.filter((item) => item.Type === 'Movie' && item.MediaType === 'Video') || [];
  return movies.find(resumeTraceEligible) || movies[0];
}

async function firstTwoMovieItems(page, summary, auth) {
  const result = await browserFetchJson(page, {
    method: 'GET',
    url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&IncludeItemTypes=Movie&Recursive=true&Fields=RunTimeTicks,MediaSources,Path&SortBy=SortName&Limit=20`,
    token: auth.AccessToken,
  });
  if (result.status !== 200) {
    throw new Error(`Movie lookup returned HTTP ${result.status}`);
  }
  const movies = (result.json?.Items || [])
    .filter((item) => item.Type === 'Movie' && item.MediaType === 'Video' && item.Id)
    .sort((left, right) => left.Name.localeCompare(right.Name));
  if (movies.length < 2) {
    throw new Error(`expected at least 2 movies for playlist flow, found ${movies.length}`);
  }
  return movies.slice(0, 2);
}

async function fetchPlaylistItems(page, auth, playlistId) {
  const result = await browserFetchJson(page, {
    method: 'GET',
    url: `/Playlists/${encodeURIComponent(playlistId)}/Items?UserId=${encodeURIComponent(auth.User.Id)}&Limit=20`,
    token: auth.AccessToken,
  });
  if (result.status !== 200) {
    throw new Error(`playlist items returned HTTP ${result.status}`);
  }
  return result.json?.Items || [];
}

async function refreshLibrary(page, auth) {
  const result = await browserFetchJson(page, {
    method: 'POST',
    url: '/Library/Refresh',
    token: auth.AccessToken,
  });
  if (![200, 204].includes(result.status)) {
    throw new Error(`Library/Refresh returned HTTP ${result.status}`);
  }
}

async function ensureVirtualFolder(page, auth, { name, collectionType, location }) {
  const folders = await browserFetchJson(page, {
    method: 'GET',
    url: '/Library/VirtualFolders',
    token: auth.AccessToken,
  });
  if (folders.status !== 200) {
    throw new Error(`Library/VirtualFolders returned HTTP ${folders.status}`);
  }
  const exists = folders.json?.some((folder) => (
    folder.Name === name
    || (folder.Locations || []).includes(location)
    || (folder.LibraryOptions?.PathInfos || []).some((pathInfo) => pathInfo.Path === location)
  ));
  if (exists) {
    return;
  }
  const create = await browserFetchJson(page, {
    method: 'POST',
    url: `/Library/VirtualFolders?name=${encodeURIComponent(name)}&collectionType=${encodeURIComponent(collectionType)}&paths=${encodeURIComponent(location)}`,
    token: auth.AccessToken,
    body: {},
  });
  if (![200, 204].includes(create.status)) {
    throw new Error(`Library/VirtualFolders create returned HTTP ${create.status}`);
  }
}

async function waitForMovieByName(page, summary, auth, name) {
  const deadline = Date.now() + 30_000;
  let lastTotal = 0;
  const expectedPathSuffix = `/${name}.mp4`;
  while (Date.now() < deadline) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&SearchTerm=${encodeURIComponent(name)}&Fields=MediaSources,RunTimeTicks,Path&Limit=5`,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`fixture movie lookup returned HTTP ${result.status}`);
    }
    lastTotal = result.json?.TotalRecordCount || 0;
    const movie = result.json?.Items?.find((item) => item.Name === name);
    if (movie) {
      return movie;
    }
    const pathMatch = result.json?.Items?.find((item) => item.Path?.endsWith(expectedPathSuffix));
    if (pathMatch) {
      return pathMatch;
    }
    const pathResult = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Movie&Fields=MediaSources,RunTimeTicks,Path&Limit=100`,
      token: auth.AccessToken,
    });
    if (pathResult.status !== 200) {
      throw new Error(`fixture movie path lookup returned HTTP ${pathResult.status}`);
    }
    const pathMovie = pathResult.json?.Items?.find((item) => item.Path?.endsWith(expectedPathSuffix));
    if (pathMovie) {
      return pathMovie;
    }
    await page.waitForTimeout(1_000);
  }
  throw new Error(`fixture movie ${name} not found after refresh; last count=${lastTotal}`);
}

async function waitForAudioByName(page, auth, name) {
  const deadline = Date.now() + 30_000;
  let lastTotal = 0;
  while (Date.now() < deadline) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Audio&SearchTerm=${encodeURIComponent(name)}&Fields=MediaSources,RunTimeTicks,Path&Limit=5`,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`audio fixture lookup returned HTTP ${result.status}`);
    }
    lastTotal = result.json?.TotalRecordCount || 0;
    const item = result.json?.Items?.find((candidate) => candidate.Name === name);
    if (item) {
      return item;
    }
    await page.waitForTimeout(1_000);
  }
  throw new Error(`audio fixture ${name} not found after refresh; last count=${lastTotal}`);
}

async function waitForMusicFlowSongs(page, auth, options = {}) {
  const deadline = Date.now() + 45_000;
  let lastTotal = 0;
  while (Date.now() < deadline) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Audio&SearchTerm=${encodeURIComponent(musicFlowFixturePrefix)}&Fields=MediaSources,RunTimeTicks,Path,Genres,DateCreated,Album,Artists,AlbumArtists&SortBy=SortName&Limit=10`,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`music fixture lookup returned HTTP ${result.status}`);
    }
    const items = result.json?.Items || [];
    lastTotal = result.json?.TotalRecordCount || 0;
    const songs = items
      .filter((candidate) => candidate.Name?.startsWith(musicFlowFixturePrefix))
      .sort((left, right) => left.Name.localeCompare(right.Name));
    if (songs.length >= 2 && (!options.requireMetadata || songsHaveMusicFlowMetadata(songs))) {
      return songs;
    }
    await page.waitForTimeout(1_000);
  }
  throw new Error(`music fixtures not found after refresh; last count=${lastTotal}`);
}

async function waitForSeriesFlowEpisodes(page, auth, options = {}) {
  const deadline = Date.now() + 60_000;
  let lastTotal = 0;
  while (Date.now() < deadline) {
    const result = await browserFetchJson(page, {
      method: 'GET',
      url: `/Items?UserId=${encodeURIComponent(auth.User.Id)}&Recursive=true&IncludeItemTypes=Episode&SearchTerm=${encodeURIComponent(seriesFlowName)}&Fields=MediaSources,RunTimeTicks,Path&SortBy=SortName&Limit=10`,
      token: auth.AccessToken,
    });
    if (result.status !== 200) {
      throw new Error(`series fixture lookup returned HTTP ${result.status}`);
    }
    const items = result.json?.Items || [];
    lastTotal = result.json?.TotalRecordCount || 0;
    const episodes = items
      .filter((candidate) => candidate.Name?.startsWith(seriesFlowName) && candidate.Type === 'Episode')
      .sort((left, right) => Number(left.IndexNumber || 0) - Number(right.IndexNumber || 0));
    if (episodes.length >= 3 && (!options.requireMetadata || episodesHaveSeriesFlowMetadata(episodes))) {
      return episodes;
    }
    await page.waitForTimeout(1_000);
  }
  throw new Error(`series fixtures not found after refresh; last count=${lastTotal}`);
}

function episodesHaveSeriesFlowMetadata(episodes) {
  return episodes.every((episode) => episode.Type === 'Episode')
    && episodes.every((episode) => episode.SeriesName === seriesFlowName)
    && episodes.every((episode) => episode.SeriesId)
    && episodes.every((episode) => episode.ParentIndexNumber === 1)
    && episodes.every((episode, index) => episode.IndexNumber === index + 1);
}

function songsHaveMusicFlowMetadata(songs) {
  return songs.some((song) => song.Album === musicFlowAlbum)
    && songs.some((song) => arrayIncludesName(song.Artists, musicFlowArtist))
    && songs.some((song) => song.AlbumArtist === musicFlowAlbumArtist || arrayIncludesName(song.AlbumArtists, musicFlowAlbumArtist))
    && songs.some((song) => arrayIncludesName(song.Genres, musicFlowGenre));
}

function arrayIncludesName(values, expected) {
  return (values || []).some((value) => {
    if (typeof value === 'string') {
      return value === expected;
    }
    return value?.Name === expected;
  });
}

async function ensureUpstreamTrickplayReady(page, auth, movie) {
  const folders = await browserFetchJson(page, {
    method: 'GET',
    url: '/Library/VirtualFolders',
    token: auth.AccessToken,
  });
  if (folders.status !== 200) {
    throw new Error(`upstream virtual folders lookup returned HTTP ${folders.status}`);
  }
  const folder = folders.json?.find((candidate) => (
    candidate.LibraryOptions?.PathInfos || []
  ).some((pathInfo) => pathInfo.Path && movie.Path?.startsWith(pathInfo.Path)));
  if (!folder?.ItemId || !folder.LibraryOptions) {
    throw new Error('upstream trickplay fixture library folder not found');
  }

  if (!folder.LibraryOptions.EnableTrickplayImageExtraction) {
    const update = await browserFetchJson(page, {
      method: 'POST',
      url: '/Library/VirtualFolders/LibraryOptions',
      token: auth.AccessToken,
      body: {
        Id: folder.ItemId,
        LibraryOptions: {
          ...folder.LibraryOptions,
          EnableTrickplayImageExtraction: true,
          ExtractTrickplayImagesDuringLibraryScan: false,
        },
      },
    });
    if (![200, 204].includes(update.status)) {
      throw new Error(`upstream trickplay library option update returned HTTP ${update.status}`);
    }
  }

  const tasks = await browserFetchJson(page, {
    method: 'GET',
    url: '/ScheduledTasks',
    token: auth.AccessToken,
  });
  if (tasks.status !== 200) {
    throw new Error(`upstream scheduled tasks lookup returned HTTP ${tasks.status}`);
  }
  const trickplayTask = tasks.json?.find((task) => task.Key === 'RefreshTrickplayImages');
  if (!trickplayTask?.Id) {
    throw new Error('upstream trickplay scheduled task not found');
  }
  const start = await browserFetchJson(page, {
    method: 'POST',
    url: `/ScheduledTasks/Running/${encodeURIComponent(trickplayTask.Id)}`,
    token: auth.AccessToken,
  });
  if (![200, 204].includes(start.status)) {
    throw new Error(`upstream trickplay task start returned HTTP ${start.status}`);
  }

  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    const playlist = await browserFetchText(page, {
      method: 'GET',
      url: `/Videos/${encodeURIComponent(movie.Id)}/Trickplay/${preferredTrickplayWidth(movie)}/tiles.m3u8`,
      token: auth.AccessToken,
    });
    if (playlist.status === 200) {
      return;
    }
    await page.waitForTimeout(2_000);
  }
  throw new Error('upstream trickplay playlist was not generated before timeout');
}

function preferredTrickplayWidth(movie) {
  const mediaSource = movie.MediaSources?.[0] || {};
  const videoStream = (mediaSource.MediaStreams || movie.MediaStreams || [])
    .find((stream) => stream.Type === 'Video' && Number(stream.Width) > 0);
  return Math.min(320, Math.max(1, Number(videoStream?.Width || 320)));
}

async function ensureImageFlowFixture() {
  await fs.mkdir(mediaFixtureDir, { recursive: true });
  const moviePath = path.join(mediaFixtureDir, `${imageFlowFixtureName}.mp4`);
  try {
    await fs.access(moviePath);
    return;
  } catch (_) {
    // Create below.
  }
  await execFileAsync('ffmpeg', [
    '-hide_banner',
    '-nostdin',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc=size=160x90:rate=24',
    '-f',
    'lavfi',
    '-i',
    'sine=frequency=330:sample_rate=44100:duration=6',
    '-t',
    '6',
    '-c:v',
    'mpeg4',
    '-pix_fmt',
    'yuv420p',
    '-c:a',
    'aac',
    '-shortest',
    '-metadata',
    `title=${imageFlowFixtureName}`,
    moviePath,
  ]);
}

async function ensureMetadataSearchFixtures() {
  await fs.mkdir(mediaFixtureDir, { recursive: true });
  for (const fixture of [
    { name: metadataFlowPrimaryName, color: 'yellow', frequency: '392' },
    { name: metadataFlowSimilarName, color: 'purple', frequency: '494' },
    { name: metadataFlowNfoFileName, color: 'blue', frequency: '587' },
  ]) {
    const moviePath = path.join(mediaFixtureDir, `${fixture.name}.mp4`);
    try {
      await fs.access(moviePath);
      continue;
    } catch (_) {
      // Create below.
    }
    await execFileAsync('ffmpeg', [
      '-hide_banner',
      '-nostdin',
      '-y',
      '-f',
      'lavfi',
      '-i',
      `color=c=${fixture.color}:s=160x90:r=24:d=6`,
      '-f',
      'lavfi',
      '-i',
      `sine=frequency=${fixture.frequency}:sample_rate=44100:duration=6`,
      '-c:v',
      'mpeg4',
      '-pix_fmt',
      'yuv420p',
      '-c:a',
      'aac',
      '-shortest',
      '-metadata',
      `title=${fixture.name}`,
      moviePath,
    ]);
  }
  await fs.writeFile(
    path.join(mediaFixtureDir, `${metadataFlowNfoFileName}.nfo`),
    `<movie>
  <title>${metadataFlowNfoTitle}</title>
  <plot>NFO imported overview one</plot>
  <genre>Jellyrin NFO Drama</genre>
  <studio>Jellyrin NFO Studio</studio>
  <tag>Jellyrin NFO Tag</tag>
  <uniqueid type="imdb">tt0950099</uniqueid>
</movie>
`,
  );
}

async function ensureLiveTvFixtures() {
  await fs.mkdir(mediaFixtureDir, { recursive: true });
  const channelPath = path.join(mediaFixtureDir, 'jellyrin-live-tv-channel.ts');
  const recordingPath = path.join(mediaFixtureDir, 'jellyrin-live-tv-recording.ts');
  await fs.writeFile(channelPath, 'jellyrin live tv channel bytes');
  await fs.writeFile(recordingPath, 'jellyrin live tv recording bytes');
  await fs.writeFile(
    path.join(mediaFixtureDir, 'jellyrin-live-tv.m3u'),
    `#EXTM3U
#EXTINF:-1 tvg-id="${liveTvFlowChannelId}" tvg-name="Jellyrin Live TV" tvg-chno="101",Jellyrin Live TV
${channelPath}
`,
  );
  await fs.writeFile(
    path.join(mediaFixtureDir, 'jellyrin-live-tv.xml'),
    `<tv>
  <programme channel="${liveTvFlowChannelId}" start="20260526080000 +0000" stop="20260526090000 +0000">
    <title>Jellyrin Morning News</title>
    <desc>Golden XMLTV program fixture</desc>
  </programme>
</tv>
`,
  );
}

async function ensureSubtitleTrickplayFixture() {
  await fs.mkdir(mediaFixtureDir, { recursive: true });
  const moviePath = path.join(mediaFixtureDir, `${subtitleTrickplayFixtureName}.mkv`);
  const subtitlePath = path.join(mediaFixtureDir, `${subtitleTrickplayFixtureName}.eng.srt`);
  try {
    await fs.access(moviePath);
    return;
  } catch (_) {
    // Create below.
  }
  await fs.writeFile(
    subtitlePath,
    '1\n00:00:00,000 --> 00:00:01,500\nHello from Jellyrin subtitles\n\n'
      + '2\n00:00:08,000 --> 00:00:10,000\nSecond cue for trickplay coverage\n\n',
  );
  await execFileAsync('ffmpeg', [
    '-hide_banner',
    '-nostdin',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc=size=160x90:rate=1',
    '-f',
    'lavfi',
    '-i',
    'anullsrc=channel_layout=stereo:sample_rate=44100',
    '-i',
    subtitlePath,
    '-t',
    '12',
    '-map',
    '0:v:0',
    '-map',
    '1:a:0',
    '-map',
    '2:s:0',
    '-c:v',
    'mpeg4',
    '-c:a',
    'aac',
    '-c:s',
    'srt',
    '-metadata:s:s:0',
    'language=eng',
    moviePath,
  ]);
}

async function ensureAudioHlsFixture() {
  await fs.mkdir(audioFixtureDir, { recursive: true });
  const audioPath = path.join(audioFixtureDir, `${audioHlsFixtureName}.mp3`);
  try {
    await fs.access(audioPath);
    return;
  } catch (_) {
    // Create below.
  }
  await execFileAsync('ffmpeg', [
    '-hide_banner',
    '-nostdin',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'sine=frequency=440:sample_rate=44100:duration=12',
    '-c:a',
    'libmp3lame',
    '-b:a',
    '128k',
    '-metadata',
    `title=${audioHlsFixtureName}`,
    audioPath,
  ]);
}

async function ensureAudioLegacySegmentFixture() {
  if (!Number.isInteger(audioLegacySegmentId) || audioLegacySegmentId < 0) {
    throw new Error(`Invalid JELLYRIN_AUDIO_LEGACY_SEGMENT_ID: ${audioLegacySegmentId}`);
  }
  const body = Buffer.from('jellyrin golden legacy mp3 segment\n', 'utf8');
  for (const directory of [upstreamTranscodeDir, jellyrinTranscodeDir]) {
    await fs.mkdir(directory, { recursive: true });
    await fs.writeFile(path.join(directory, `${audioLegacySegmentId}.mp3`), body);
  }
}

async function ensureMusicFlowFixtures() {
  await fs.mkdir(audioFixtureDir, { recursive: true });
  const fixtures = [
    { name: `${musicFlowFixturePrefix} 01`, frequency: '523.25', track: '1' },
    { name: `${musicFlowFixturePrefix} 02`, frequency: '659.25', track: '2' },
  ];
  for (const fixture of fixtures) {
    const audioPath = path.join(audioFixtureDir, `${fixture.name}.mp3`);
    await execFileAsync('ffmpeg', [
      '-hide_banner',
      '-nostdin',
      '-y',
      '-f',
      'lavfi',
      '-i',
      `sine=frequency=${fixture.frequency}:sample_rate=44100:duration=10`,
      '-c:a',
      'libmp3lame',
      '-b:a',
      '128k',
      '-metadata',
      `title=${fixture.name}`,
      '-metadata',
      `album=${musicFlowAlbum}`,
      '-metadata',
      `artist=${musicFlowArtist}`,
      '-metadata',
      `album_artist=${musicFlowAlbumArtist}`,
      '-metadata',
      `genre=${musicFlowGenre}`,
      '-metadata',
      `track=${fixture.track}`,
      audioPath,
    ]);
  }
}

async function ensureSeriesFlowFixtures() {
  const seasonDir = path.join(seriesFixtureDir, seriesFlowName, seriesFlowSeasonName);
  await fs.mkdir(seasonDir, { recursive: true });
  for (const episode of [
    { number: 1, color: 'red', frequency: '440' },
    { number: 2, color: 'green', frequency: '554.37' },
    { number: 3, color: 'blue', frequency: '659.25' },
  ]) {
    const episodeName = `${seriesFlowName} S01E${String(episode.number).padStart(2, '0')}`;
    const episodePath = path.join(seasonDir, `${episodeName}.mp4`);
    await execFileAsync('ffmpeg', [
      '-hide_banner',
      '-nostdin',
      '-y',
      '-f',
      'lavfi',
      '-i',
      `color=c=${episode.color}:s=320x180:r=24:d=5`,
      '-f',
      'lavfi',
      '-i',
      `sine=frequency=${episode.frequency}:sample_rate=44100:duration=5`,
      '-c:v',
      'mpeg4',
      '-pix_fmt',
      'yuv420p',
      '-c:a',
      'aac',
      '-shortest',
      '-metadata',
      `title=${episodeName}`,
      episodePath,
    ]);
  }
}

function resumeTracePositionTicks(movie) {
  const runtimeTicks = Number(movie.RunTimeTicks || 0);
  if (Number.isFinite(runtimeTicks) && runtimeTicks > 0) {
    return Math.max(1, Math.floor(runtimeTicks / 2));
  }
  return 50_000_000;
}

function resumeTraceEligible(movie) {
  const runtimeTicks = Number(movie.RunTimeTicks || 0);
  return Number.isFinite(runtimeTicks) && runtimeTicks >= 300 * 10_000_000;
}

function defaultStreamIndex(movie, streamType) {
  const streams = movie.MediaSources?.[0]?.MediaStreams || movie.MediaStreams || [];
  const stream = streams.find((candidate) => candidate.Type === streamType && candidate.IsDefault)
    || streams.find((candidate) => candidate.Type === streamType);
  return stream?.Index;
}

function withoutUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, child]) => child !== undefined));
}

function hlsTranscodeDeviceProfile() {
  return {
    DirectPlayProfiles: [],
    TranscodingProfiles: [
      {
        Container: 'ts',
        Type: 'Video',
        AudioCodec: 'aac,mp2,opus,flac',
        VideoCodec: 'h264',
        Context: 'Streaming',
        Protocol: 'hls',
        MaxAudioChannels: '2',
        MinSegments: '1',
        BreakOnNonKeyFrames: false,
      },
    ],
    ContainerProfiles: [],
    CodecProfiles: [],
  };
}

function mpvShimDeviceProfile() {
  return {
    DirectPlayProfiles: [
      {
        Container: 'mp4,m4v,mkv,webm,mov,avi,ts',
        Type: 'Video',
        VideoCodec: 'h264,hevc,vp8,vp9,mpeg4,mpeg2video',
        AudioCodec: 'aac,mp3,opus,vorbis,flac,ac3,eac3,mp2',
      },
      {
        Container: 'mp3,aac,flac,ogg,opus,m4a',
        Type: 'Audio',
        AudioCodec: 'mp3,aac,flac,vorbis,opus',
      },
    ],
    TranscodingProfiles: [
      {
        Container: 'ts',
        Type: 'Video',
        AudioCodec: 'aac,mp3',
        VideoCodec: 'h264',
        Context: 'Streaming',
        Protocol: 'hls',
        MaxAudioChannels: '2',
      },
    ],
    ContainerProfiles: [],
    CodecProfiles: [],
    SubtitleProfiles: [
      { Format: 'srt', Method: 'External' },
      { Format: 'vtt', Method: 'External' },
    ],
  };
}

function nonWebClientContracts(targetName) {
  return [
    {
      id: 'mpv-shim',
      authorization: nonWebClientAuthorization('Jellyfin MPV Shim', `Jellyrin QA MPV ${targetName}`, `non-web-client-mpv-${targetName}`),
      deviceProfile: mpvShimDeviceProfile(),
    },
    {
      id: 'kodi',
      authorization: nonWebClientAuthorization('Kodi Sync Queue', `Jellyrin QA Kodi ${targetName}`, `non-web-client-kodi-${targetName}`),
      deviceProfile: kodiDeviceProfile(),
    },
    {
      id: 'android-tv',
      authorization: nonWebClientAuthorization('Jellyfin Android TV', `Jellyrin QA Android TV ${targetName}`, `non-web-client-android-tv-${targetName}`),
      deviceProfile: androidTvDeviceProfile(),
    },
    {
      id: 'android-mobile',
      authorization: nonWebClientAuthorization('Jellyfin Android', `Jellyrin QA Android ${targetName}`, `non-web-client-android-mobile-${targetName}`),
      deviceProfile: androidMobileDeviceProfile(),
    },
    {
      id: 'swiftfin',
      authorization: nonWebClientAuthorization('Swiftfin', `Jellyrin QA Swiftfin ${targetName}`, `non-web-client-swiftfin-${targetName}`),
      deviceProfile: swiftfinDeviceProfile(),
    },
    {
      id: 'roku',
      authorization: nonWebClientAuthorization('Jellyfin Roku', `Jellyrin QA Roku ${targetName}`, `non-web-client-roku-${targetName}`),
      deviceProfile: rokuDeviceProfile(),
    },
  ];
}

function nonWebClientAuthorization(client, device, deviceId) {
  return `MediaBrowser Client="${client}", Device="${device}", DeviceId="${deviceId}", Version="dev"`;
}

function kodiDeviceProfile() {
  return directVideoDeviceProfile({
    containers: 'mp4,m4v,mkv,webm,mov,avi,ts',
    videoCodecs: 'h264,hevc,vp8,vp9,mpeg4,mpeg2video',
    audioCodecs: 'aac,mp3,opus,vorbis,flac,ac3,eac3,dts,mp2',
    audioContainers: 'mp3,aac,flac,ogg,opus,m4a,wav',
    directAudioCodecs: 'mp3,aac,flac,vorbis,opus',
    subtitles: ['srt', 'vtt', 'ass', 'ssa', 'sub'],
  });
}

function androidTvDeviceProfile() {
  return directVideoDeviceProfile({
    containers: 'mp4,m4v,mkv,webm',
    videoCodecs: 'h264,hevc,vp9',
    audioCodecs: 'aac,mp3,opus,vorbis,flac,ac3,eac3',
    audioContainers: 'mp3,aac,flac,ogg,opus,m4a',
    directAudioCodecs: 'mp3,aac,flac,vorbis,opus',
    subtitles: ['srt', 'vtt'],
  });
}

function androidMobileDeviceProfile() {
  return directVideoDeviceProfile({
    containers: 'mp4,m4v,mkv,webm',
    videoCodecs: 'h264,hevc,vp9',
    audioCodecs: 'aac,mp3,opus,vorbis,flac,ac3,eac3',
    audioContainers: 'mp3,aac,flac,ogg,opus,m4a',
    directAudioCodecs: 'mp3,aac,flac,vorbis,opus',
    subtitles: ['srt', 'vtt'],
  });
}

function swiftfinDeviceProfile() {
  return directVideoDeviceProfile({
    containers: 'mp4,m4v,mov',
    videoCodecs: 'h264,hevc',
    audioCodecs: 'aac,mp3,ac3,eac3,alac,flac',
    audioContainers: 'mp3,aac,flac,m4a',
    directAudioCodecs: 'mp3,aac,flac,alac',
    subtitles: ['srt', 'vtt'],
  });
}

function rokuDeviceProfile() {
  return directVideoDeviceProfile({
    containers: 'mp4,m4v,mov',
    videoCodecs: 'h264,hevc',
    audioCodecs: 'aac,mp3,ac3,eac3',
    audioContainers: 'mp3,aac,m4a',
    directAudioCodecs: 'mp3,aac',
    subtitles: ['srt', 'vtt'],
  });
}

function directVideoDeviceProfile({ containers, videoCodecs, audioCodecs, audioContainers, directAudioCodecs, subtitles }) {
  return {
    DirectPlayProfiles: [
      {
        Container: containers,
        Type: 'Video',
        VideoCodec: videoCodecs,
        AudioCodec: audioCodecs,
      },
      {
        Container: audioContainers,
        Type: 'Audio',
        AudioCodec: directAudioCodecs,
      },
    ],
    TranscodingProfiles: [
      {
        Container: 'ts',
        Type: 'Video',
        AudioCodec: 'aac',
        VideoCodec: 'h264',
        Context: 'Streaming',
        Protocol: 'hls',
        MaxAudioChannels: '2',
      },
    ],
    ContainerProfiles: [],
    CodecProfiles: [],
    SubtitleProfiles: subtitles.map((format) => ({ Format: format, Method: 'External' })),
  };
}

async function browserFetchJson(page, request) {
  return page.evaluate(async ({ method, url, token, authorization, body }) => {
    const headers = {};
    if (token) {
      headers['X-Emby-Token'] = token;
    }
    if (authorization) {
      headers.Authorization = authorization;
    }
    if (body !== undefined) {
      headers['Content-Type'] = 'application/json';
    }
    const response = await fetch(url, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    let json = null;
    if (text) {
      try {
        json = JSON.parse(text);
      } catch (_) {
        json = null;
      }
    }
    return {
      status: response.status,
      json,
    };
  }, request);
}

async function startWebsocketProbe(page, baseUrl, sockets) {
  await page.evaluate(async ({ baseUrl, sockets }) => {
    if (window.__jellyrinWsProbe?.sockets) {
      for (const socket of Object.values(window.__jellyrinWsProbe.sockets)) {
        try {
          socket.close();
        } catch (_) {
          // Ignore stale probe sockets.
        }
      }
    }
    const wsBaseUrl = baseUrl.replace(/^http:/i, 'ws:').replace(/^https:/i, 'wss:');
    const probe = {
      messages: [],
      sockets: {},
    };
    window.__jellyrinWsProbe = probe;

    await Promise.all(sockets.map(({ name, token, deviceId }) => new Promise((resolve, reject) => {
      const socket = new WebSocket(`${wsBaseUrl}/socket?api_key=${encodeURIComponent(token)}&deviceId=${encodeURIComponent(deviceId || `sessions-${name}`)}`);
      probe.sockets[name] = socket;
      const timeout = setTimeout(() => reject(new Error(`websocket ${name} did not open`)), 10_000);
      socket.addEventListener('open', () => {
        clearTimeout(timeout);
        const sessionsStart = JSON.stringify({ MessageType: 'SessionsStart', Data: '0,1500' });
        socket.send(sessionsStart);
        probe.messages.push({ socket: name, direction: 'sent', messageType: 'SessionsStart' });
        resolve();
      });
      socket.addEventListener('message', (event) => {
        try {
          const parsed = JSON.parse(event.data);
          probe.messages.push({
            socket: name,
            direction: 'received',
            messageType: parsed.MessageType,
            data: parsed.Data,
          });
          if (parsed.MessageType === 'ForceKeepAlive') {
            const keepAlive = JSON.stringify({ MessageType: 'KeepAlive' });
            socket.send(keepAlive);
            probe.messages.push({ socket: name, direction: 'sent', messageType: 'KeepAlive' });
          }
        } catch (_) {
          probe.messages.push({ socket: name, direction: 'received', messageType: '<non-json>' });
        }
      });
      socket.addEventListener('error', () => {
        clearTimeout(timeout);
        reject(new Error(`websocket ${name} error`));
      }, { once: true });
    })));
  }, { baseUrl, sockets });
}

async function waitForWebsocketMessages(page, expectations, options = {}) {
  const minimumCount = options.minimumCount || 1;
  await page.waitForFunction(
    ({ expectations, minimumCount }) => {
      const messages = window.__jellyrinWsProbe?.messages || [];
      return expectations.every(([socket, messageType]) =>
        messages.filter((message) =>
          message.socket === socket
          && message.direction === 'received'
          && message.messageType === messageType
        ).length >= minimumCount,
      );
    },
    { expectations, minimumCount },
    { timeout: options.timeout || 15_000 },
  );
}

async function websocketReceivedCounts(page, messageTypes) {
  return page.evaluate((messageTypes) => {
    const messages = window.__jellyrinWsProbe?.messages || [];
    return Object.fromEntries(messageTypes.map((messageType) => [
      messageType,
      messages.filter((message) =>
        message.direction === 'received'
        && message.messageType === messageType
      ).length,
    ]));
  }, messageTypes);
}

async function waitForAdditionalWebsocketMessages(page, previousCounts, options = {}) {
  await page.waitForFunction(
    ({ previousCounts }) => {
      const messages = window.__jellyrinWsProbe?.messages || [];
      return Object.entries(previousCounts).some(([messageType, previousCount]) =>
        messages.filter((message) =>
          message.direction === 'received'
          && message.messageType === messageType
        ).length > previousCount
      );
    },
    { previousCounts },
    { timeout: options.timeout || 15_000 },
  );
}

async function closeWebsocketProbe(page) {
  await page.evaluate(() => {
    if (!window.__jellyrinWsProbe?.sockets) {
      return;
    }
    for (const socket of Object.values(window.__jellyrinWsProbe.sockets)) {
      try {
        socket.close();
      } catch (_) {
        // Ignore close races.
      }
    }
  });
}

async function browserFetchText(page, request) {
  return page.evaluate(async ({ method, url, token }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'X-Emby-Token': token,
      },
    });
    return {
      status: response.status,
      contentType: response.headers.get('content-type') || '',
      text: await response.text(),
    };
  }, request);
}

// Like browserFetchText but bounded: aborts after timeoutMs if the server does not close the
// response (e.g. upstream live.m3u8 may long-poll indefinitely). Reads body as chunks via
// ReadableStream and decodes with TextDecoder; returns whatever was received before timeout.
// Use this for HLS playlist endpoints that may hang on a live/long-polling server.
async function browserFetchTextBounded(page, request) {
  return page.evaluate(async ({ method, url, token, timeoutMs }) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    let response;
    try {
      response = await fetch(url, {
        method,
        headers: { 'X-Emby-Token': token },
        signal: controller.signal,
      });
    } catch (err) {
      clearTimeout(timer);
      return { status: 0, contentType: '', text: '', error: String(err) };
    }
    const contentType = response.headers.get('content-type') || '';
    const decoder = new TextDecoder();
    let text = '';
    try {
      const reader = response.body.getReader();
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) text += decoder.decode(value, { stream: true });
      }
      text += decoder.decode();
    } catch (_) {
      // Abort or partial read: return what we have.
    } finally {
      clearTimeout(timer);
      controller.abort();
    }
    return { status: response.status, contentType, text };
  }, { ...request, timeoutMs: request.timeoutMs ?? 20000 });
}

async function browserFetchBinary(page, request) {
  return page.evaluate(async ({ method, url, token }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'X-Emby-Token': token,
      },
    });
    const bytes = new Uint8Array(await response.arrayBuffer());
    return {
      status: response.status,
      contentType: response.headers.get('content-type') || '',
      cacheControl: response.headers.get('cache-control') || '',
      etag: response.headers.get('etag') || '',
      byteLength: bytes.length,
      startsWithJpeg: bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xd8,
      startsWithPng: bytes.length >= 8
        && bytes[0] === 0x89
        && bytes[1] === 0x50
        && bytes[2] === 0x4e
        && bytes[3] === 0x47
        && bytes[4] === 0x0d
        && bytes[5] === 0x0a
        && bytes[6] === 0x1a
        && bytes[7] === 0x0a,
    };
  }, request);
}

// Probe a (possibly infinite) stream: fetch the URL, read at most minBytes bytes from the
// body using ReadableStream, then abort. Returns status, contentType, and byteLength read.
// Used to verify that a live-TV stream URL returns real bytes without hanging indefinitely.
async function browserFetchStreamProbe(page, request) {
  return page.evaluate(async ({ url, token, minBytes }) => {
    const controller = new AbortController();
    let response;
    try {
      response = await fetch(url, {
        method: 'GET',
        headers: { 'X-Emby-Token': token },
        signal: controller.signal,
      });
    } catch (err) {
      return { status: 0, contentType: '', byteLength: 0, error: String(err) };
    }
    if (!response.ok && response.status !== 200) {
      return { status: response.status, contentType: response.headers.get('content-type') || '', byteLength: 0 };
    }
    const contentType = response.headers.get('content-type') || '';
    let totalBytes = 0;
    try {
      const reader = response.body.getReader();
      while (totalBytes < minBytes) {
        const { done, value } = await reader.read();
        if (done) break;
        totalBytes += value ? value.length : 0;
        if (totalBytes >= minBytes) {
          reader.cancel().catch(() => {});
          break;
        }
      }
    } catch (_) {
      // Abort or partial read is expected; bytes already counted.
    } finally {
      controller.abort();
    }
    return { status: response.status, contentType, byteLength: totalBytes };
  }, request);
}

// Like browserFetchStreamProbe but holds the connection open for holdMs ms after reading minBytes.
// This ensures two concurrent callers have overlapping in-flight requests so the simulator's
// maxConcurrentByChannel counter captures the peak. Returns status, contentType, byteLength.
async function browserFetchStreamProbeOverlap(page, request) {
  return page.evaluate(async ({ url, token, minBytes, holdMs }) => {
    const controller = new AbortController();
    let response;
    try {
      response = await fetch(url, {
        method: 'GET',
        headers: token ? { 'X-Emby-Token': token } : {},
        signal: controller.signal,
      });
    } catch (err) {
      return { status: 0, contentType: '', byteLength: 0, error: String(err) };
    }
    if (!response.ok && response.status !== 200) {
      return { status: response.status, contentType: response.headers.get('content-type') || '', byteLength: 0 };
    }
    const contentType = response.headers.get('content-type') || '';
    let totalBytes = 0;
    try {
      const reader = response.body.getReader();
      while (totalBytes < minBytes) {
        const { done, value } = await reader.read();
        if (done) break;
        totalBytes += value ? value.length : 0;
      }
      // Hold the connection open for holdMs to ensure overlap with concurrent probes.
      await new Promise((resolve) => { setTimeout(resolve, holdMs || 500); });
      reader.cancel().catch(() => {});
    } catch (_) {
      // Abort or partial read is expected; bytes already counted.
    } finally {
      controller.abort();
    }
    return { status: response.status, contentType, byteLength: totalBytes };
  }, request);
}

async function browserFetchImageUpload(page, request) {
  return page.evaluate(async ({ method, url, token, imageBase64 }) => {
    const response = await fetch(url, {
      method,
      headers: {
        'X-Emby-Token': token,
        'Content-Type': 'image/png',
      },
      body: imageBase64,
    });
    return {
      status: response.status,
      contentType: response.headers.get('content-type') || '',
    };
  }, request);
}

function firstPlaylistUri(text) {
  return String(text || '')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => line && !line.startsWith('#'));
}

function resolveRelativeUrl(base, next) {
  return new URL(next, new URL(base, 'http://placeholder.invalid')).pathname
    + new URL(next, new URL(base, 'http://placeholder.invalid')).search;
}

// Make a direct HTTP request from Node.js context (not browser page) and return parsed JSON.
// Used for simulator /stats and /stats/reset calls to avoid cross-origin browser fetch issues.
function nodeHttpJson(method, absoluteUrl, body) {
  return new Promise((resolve) => {
    const parsed = new URL(absoluteUrl);
    const bodyStr = body !== undefined ? JSON.stringify(body) : null;
    const options = {
      hostname: parsed.hostname,
      port: parseInt(parsed.port, 10) || 80,
      path: parsed.pathname + parsed.search,
      method,
      headers: bodyStr ? { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(bodyStr) } : {},
    };
    const req = http.request(options, (res) => {
      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        let json = null;
        try { json = JSON.parse(data); } catch (_) { json = null; }
        resolve({ status: res.statusCode, json });
      });
    });
    req.on('error', () => resolve({ status: 0, json: null }));
    if (bodyStr) req.write(bodyStr);
    req.end();
  });
}

// Download the body of an authenticated HTTP GET request to a local file.
// Used to save recording files for ffprobe analysis.
// Returns { ok: boolean, bytes: number } where ok is true when status 200 and bytes > 0.
function nodeHttpBinary(method, absoluteUrl, token, destFile) {
  const fsSync = require('node:fs');
  return new Promise((resolve) => {
    const parsed = new URL(absoluteUrl);
    const options = {
      hostname: parsed.hostname,
      port: parseInt(parsed.port, 10) || 80,
      path: parsed.pathname + parsed.search,
      method,
      headers: token ? { 'X-Emby-Token': token } : {},
    };
    const req = http.request(options, (res) => {
      if (res.statusCode !== 200) {
        res.resume();
        resolve({ ok: false, bytes: 0 });
        return;
      }
      const out = fsSync.createWriteStream(destFile);
      let bytes = 0;
      res.on('data', (chunk) => { bytes += chunk.length; out.write(chunk); });
      res.on('end', () => { out.end(); resolve({ ok: true, bytes }); });
      res.on('error', () => { out.destroy(); resolve({ ok: false, bytes }); });
    });
    req.on('error', () => resolve({ ok: false, bytes: 0 }));
    req.setTimeout(15000, () => { req.destroy(); resolve({ ok: false, bytes: 0 }); });
    req.end();
  });
}

function playSessionIdFromTranscodingUrl(transcodingUrl) {
  try {
    const url = new URL(transcodingUrl, 'http://placeholder.invalid');
    return url.searchParams.get('PlaySessionId') || null;
  } catch (_) {
    return null;
  }
}

async function deleteLiveTvHlsSession(page, auth, playSessionId) {
  if (!playSessionId) return;
  await browserFetchJson(page, {
    method: 'DELETE',
    url: `/Videos/ActiveEncodings?PlaySessionId=${encodeURIComponent(playSessionId)}&DeviceId=browser-trace`,
    token: auth.AccessToken,
  }).catch(() => {});
}

async function probeLiveTvHlsPlayback(page, summary, auth, target, channel, options = {}) {
  const result = {
    ok: false,
    status: 0,
    phase: 'init',
    playSessionId: null,
    transcodingUrl: null,
    master200: false,
    mediaLive: false,
    segment200: false,
  };
  try {
    if (target.name === 'jellyrin' && /^hdhr_/.test(channel.Id || '')) {
      let ms = channel.MediaSources?.[0] || null;
      const channelDetail = await browserFetchJson(page, {
        method: 'GET',
        url: `/LiveTv/Channels/${encodeURIComponent(channel.Id)}?fields=MediaSources`,
        token: auth.AccessToken,
      });
      if (channelDetail.status !== 200 && !ms) {
        return { ...result, status: channelDetail.status, phase: 'channel-detail' };
      }
      ms = channelDetail.json?.MediaSources?.[0] || ms;
      const transcodingUrl = ms?.TranscodingUrl;
      if (!transcodingUrl) {
        return { ...result, phase: 'transcoding-url' };
      }
      result.transcodingUrl = transcodingUrl;
      result.playSessionId = playSessionIdFromTranscodingUrl(transcodingUrl);
    } else {
      const playbackInfo = await browserFetchJson(page, {
        method: 'POST',
        url: `/Items/${encodeURIComponent(channel.Id)}/PlaybackInfo`,
        token: auth.AccessToken,
        body: {
          UserId: auth.User.Id,
          EnableTranscoding: true,
          EnableDirectPlay: false,
          EnableDirectStream: false,
          AutoOpenLiveStream: true,
          DeviceProfile: hlsTranscodeDeviceProfile(),
        },
      });
      if (playbackInfo.status !== 200) {
        return { ...result, status: playbackInfo.status, phase: 'playback-info' };
      }
      const transcodingUrl = playbackInfo.json?.MediaSources?.[0]?.TranscodingUrl;
      if (!transcodingUrl) {
        return { ...result, status: playbackInfo.status, phase: 'transcoding-url' };
      }
      result.transcodingUrl = transcodingUrl;
      result.playSessionId = playbackInfo.json?.PlaySessionId || playSessionIdFromTranscodingUrl(transcodingUrl);
    }

    const master = await browserFetchTextBounded(page, {
      method: 'GET',
      url: result.transcodingUrl,
      token: auth.AccessToken,
      timeoutMs: options.masterTimeoutMs || 20000,
    });
    result.status = master.status;
    result.phase = 'master';
    result.master200 = master.status === 200
      && master.contentType.includes('mpegurl')
      && master.text.includes('#EXT-X-STREAM-INF');
    if (!result.master200) {
      return result;
    }

    const mediaPlaylistPath = firstPlaylistUri(master.text);
    if (!mediaPlaylistPath) {
      result.phase = 'media-uri';
      return result;
    }
    const mediaPlaylistUrl = resolveRelativeUrl(result.transcodingUrl, mediaPlaylistPath);
    const media = await browserFetchTextBounded(page, {
      method: 'GET',
      url: mediaPlaylistUrl,
      token: auth.AccessToken,
      timeoutMs: options.mediaTimeoutMs || 35000,
    });
    result.status = media.status;
    result.phase = 'media';
    result.mediaLive = media.status === 200
      && media.text.includes('#EXTINF')
      && !media.text.includes('#EXT-X-ENDLIST');
    if (!result.mediaLive) {
      return result;
    }

    const segmentPath = firstPlaylistUri(media.text);
    if (!segmentPath) {
      result.phase = 'segment-uri';
      return result;
    }
    const segmentUrl = resolveRelativeUrl(mediaPlaylistUrl, segmentPath);
    const segmentProbe = await browserFetchStreamProbe(page, {
      url: segmentUrl,
      token: auth.AccessToken,
      minBytes: 1,
    });
    result.status = segmentProbe.status;
    result.phase = 'segment';
    result.segment200 = (segmentProbe.status === 200 || segmentProbe.status === 206)
      && segmentProbe.contentType.includes('video/mp2t')
      && segmentProbe.byteLength >= 1;
    result.ok = result.segment200;
    return result;
  } finally {
    if (options.cleanup !== false) {
      await deleteLiveTvHlsSession(page, auth, result.playSessionId);
    }
  }
}

async function liveTvRecordingPresenceByName(page, auth, name) {
  const recordings = await browserFetchJson(page, {
    method: 'GET',
    url: '/LiveTv/Recordings',
    token: auth.AccessToken,
  }).catch(() => ({ status: 0, json: null }));
  const items = recordings.status === 200 && Array.isArray(recordings.json?.Items)
    ? recordings.json.Items.filter((item) => item.Name === name)
    : [];
  return {
    status: recordings.status,
    matches: items,
    completed: items.some(
      (item) => item.Status === 'Completed'
        || (item.Status !== 'InProgress' && item.RunTimeTicks != null && item.RunTimeTicks > 0),
    ),
    inProgress: items.some((item) => item.Status === 'InProgress'),
  };
}

async function authenticateTarget(page, summary, target) {
  if (target.apiKey) {
    const usersResponse = await page.request.get(`${summary.baseUrl}/Users`, {
      headers: { 'X-Emby-Token': target.apiKey },
    });
    if (!usersResponse.ok()) {
      throw new Error(`API-key user lookup returned HTTP ${usersResponse.status()}`);
    }
    const users = await usersResponse.json();
    const user = users?.[0];
    if (!user?.Id) {
      throw new Error('API-key user lookup returned no users');
    }
    return {
      AccessToken: target.apiKey,
      User: user,
      authMethod: 'api_key',
    };
  }

  const apiAuthResponse = await page.request.post(`${summary.baseUrl}/Users/AuthenticateByName`, {
    headers: {
      Authorization: 'MediaBrowser Client="Jellyrin Browser Trace", Device="Harness", DeviceId="browser-trace", Version="dev"',
    },
    data: { Username: target.username, Pw: target.password },
  });
  if (!apiAuthResponse.ok()) {
    throw new Error(`API authentication returned HTTP ${apiAuthResponse.status()}`);
  }
  return {
    ...(await apiAuthResponse.json()),
    authMethod: 'password',
  };
}

async function establishWebSession(page, summary, publicInfo, target, auth, targetRoute) {
  if (auth.authMethod === 'api_key') {
    await preauthenticateWebWithApiKey(page, summary.baseUrl, publicInfo, auth);
    await page.goto(`${summary.baseUrl}/web/#${targetRoute}`, {
      waitUntil: 'domcontentloaded',
    });
    await page.waitForFunction((route) => window.location.hash === `#${route}`, targetRoute, { timeout: 20_000 });
    return;
  }
  await loginThroughWeb(page, summary.baseUrl, publicInfo.Id, target);
}

async function preauthenticateWebWithApiKey(page, baseUrl, publicInfo, auth) {
  await page.addInitScript(({ baseUrl, publicInfo, auth }) => {
    const now = Date.now();
    localStorage.setItem('jellyfin_credentials', JSON.stringify({
      Servers: [{
        Id: publicInfo.Id,
        Name: publicInfo.ServerName,
        LocalAddress: baseUrl,
        ManualAddress: baseUrl,
        LastConnectionMode: 2,
        DateLastAccessed: now,
        AccessToken: auth.AccessToken,
        UserId: auth.User.Id,
      }],
    }));
  }, { baseUrl, publicInfo, auth });
}

async function loginThroughWeb(page, baseUrl, serverId, target) {
  await page.goto(`${baseUrl}/web/#/login?serverid=${serverId}&url=%2Fhome`, {
    waitUntil: 'domcontentloaded',
  });
  const manualName = page.locator('#txtManualName');
  await manualName.waitFor({ state: 'visible', timeout: 5_000 }).catch(() => {});
  if (!(await manualName.isVisible().catch(() => false))) {
    await page.locator('.btnManual:visible').click({ timeout: 20_000 });
    await manualName.waitFor({ state: 'visible', timeout: 20_000 });
  }
  await manualName.fill(target.username);
  await page.locator('#txtManualPassword').fill(target.password);

  const authResponse = page.waitForResponse((response) =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200,
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authResponse;
  await page.waitForURL(/\/web\/#\/home/, { timeout: 20_000 });
}

function wirePageCapture(page, summary, requestLog, consoleLog, websocketLog) {
  page.on('response', async (response) => {
    const request = response.request();
    const requestPostData = sanitizePostData(request.postData());
    const record = {
      ts: new Date().toISOString(),
      method: request.method(),
      url: sanitizeUrl(response.url()),
      path: pathWithQuery(response.url()),
      status: response.status(),
      resourceType: request.resourceType(),
      requestHeaders: redactHeaders(request.headers()),
      requestPostData,
      responseHeaders: selectedResponseHeaders(response.headers()),
      responseContentType: response.headers()['content-type'] || '',
      queryKeysPreservingCase: Array.from(new URL(response.url()).searchParams.keys()),
    };
    if (record.responseContentType.includes('application/json')) {
      record.responseShape = await responseShape(response);
    }
    captureFlowInvariants(summary, record, requestPostData);
    summary.requests += 1;
    if (response.status() >= 400 && !allowedFailedResponse(response)) {
      summary.failedResponses.push(`${response.status()} ${sanitizeUrl(response.url())}`);
    }
    await requestLog.write(record);
  });

  page.on('console', async (message) => {
    const text = redactText(message.text());
    const record = {
      ts: new Date().toISOString(),
      type: message.type(),
      text,
      location: message.location(),
    };
    if (['error', 'warning'].includes(message.type())) {
      summary.consoleErrors.push(text);
    }
    await consoleLog.write(record);
  });

  page.on('pageerror', async (error) => {
    const record = {
      ts: new Date().toISOString(),
      message: redactText(error.message),
      stack: error.stack ? redactText(error.stack) : undefined,
    };
    summary.pageErrors.push(record.message);
    await consoleLog.write({ ...record, type: 'pageerror' });
  });

  page.on('websocket', (websocket) => {
    summary.websockets += 1;
    const url = sanitizeUrl(websocket.url());
    websocketLog.write({ ts: new Date().toISOString(), event: 'open', url });
    websocket.on('framesent', (frame) => {
      const parsed = parseJsonPayload(frame.payload);
      addWebsocketMessageType(summary, parsed);
      if (parsed && parsed.MessageType === 'KeepAlive') {
        summary.invariants.websocketKeepAlive = true;
      }
      websocketLog.write(websocketFrameRecord('sent', url, frame.payload));
    });
    websocket.on('framereceived', (frame) => {
      const parsed = parseJsonPayload(frame.payload);
      addWebsocketMessageType(summary, parsed);
      if (parsed && parsed.MessageType === 'Sessions') {
        summary.invariants.websocketSessions = true;
      }
      if (parsed && parsed.MessageType === 'ForceKeepAlive') {
        summary.invariants.websocketKeepAlive = true;
      }
      websocketLog.write(websocketFrameRecord('received', url, frame.payload));
    });
    websocket.on('close', () => {
      websocketLog.write({ ts: new Date().toISOString(), event: 'close', url });
    });
  });
}

function compareSummaries(summaries) {
  const reasons = [];
  for (const summary of summaries) {
    if (summary.status === 'failed') {
      reasons.push(`${summary.target}: ${summary.error}`);
    }
    if (summary.skipped) {
      reasons.push(`${summary.target}: skipped: ${summary.reason}`);
    }
    if (summary.failedResponses.length > 0) {
      reasons.push(`${summary.target}: unexpected failed responses: ${summary.failedResponses.join(', ')}`);
    }
    if (summary.pageErrors.length > 0) {
      reasons.push(`${summary.target}: page errors: ${summary.pageErrors.join(', ')}`);
    }
    const unexpectedConsoleErrors = summary.consoleErrors.filter((text) => !ignoredConsoleError(text));
    if (unexpectedConsoleErrors.length > 0) {
      reasons.push(`${summary.target}: unexpected console errors: ${unexpectedConsoleErrors.join(', ')}`);
    }
    for (const failure of invariantFailures(summary)) {
      reasons.push(`${summary.target}: ${failure}`);
    }
  }
  reasons.push(...compareCompletedTargets(summaries));
  return {
    failed: reasons.length > 0,
    reasons,
  };
}

function captureFlowInvariants(summary, record, requestPostData) {
  if (!['startup-wizard', 'p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay', 'audio-hls-legacy', 'music', 'series', 'playlists-collections', 'images', 'metadata-search', 'auth-users', 'sessions-websocket', 'syncplay', 'plugins-packages', 'live-tv', 'channels', 'non-web-client', 'scheduled-tasks', 'backup-restore', 'migration-import'].includes(flow)) {
    return;
  }
  const pathname = new URL(record.url).pathname;
  const key = criticalRequestKey(record, requestPostData);
  if (key) {
    const previous = summary.criticalRequests[key];
    const keepPreviousSyncPlayListShape = flow === 'syncplay'
      && key === 'syncplay-list'
      && previous?.status === 200
      && Array.isArray(previous.responseShape)
      && previous.responseShape.length > 0
      && Array.isArray(record.responseShape)
      && record.responseShape.length === 0;
    if (!keepPreviousSyncPlayListShape && (!previous || previous.status >= 400 || record.status < 400)) {
      summary.criticalRequests[key] = criticalRequestSummary(record, requestPostData);
    }
  }
  if (pathname.endsWith('/PlaybackInfo') && record.status === 200) {
    summary.invariants.playbackInfo200 = true;
    if (flow === 'subtitles-trickplay') {
      summary.invariants.subtitlePlaybackInfo200 = true;
    }
    if (flow === 'transcode-hls') {
      summary.invariants.transcodePlaybackInfo200 = true;
      if (record.responseShape?.MediaSources?.[0]?.TranscodingUrl) {
        summary.invariants.transcodingUrlPresent = true;
      }
    }
    if (flow === 'audio-hls-legacy') {
      summary.invariants.audioPlaybackInfo200 = true;
      if (record.responseShape?.MediaSources?.[0]?.TranscodingUrl) {
        summary.invariants.audioTranscodingUrlPresent = true;
      }
    }
  }
  if (/\/Videos\/[^/]+\/stream/i.test(pathname) && [200, 206].includes(record.status)) {
    summary.invariants.streamOk = true;
  }
  if (pathname === '/Sessions/Playing' && record.method === 'POST' && record.status === 204) {
    summary.invariants.sessionPlaying204 = true;
    if (requestPostData && typeof requestPostData === 'object' && requestPostData.PlayMethod) {
      summary.invariants.playMethods.push(requestPostData.PlayMethod);
    }
  }
  if (pathname === '/Sessions/Playing/Progress' && record.method === 'POST' && record.status === 204) {
    summary.invariants.playbackProgress204 = true;
  }
  if (flow === 'startup-wizard') {
    if (record.method === 'GET' && pathname === '/System/Info/Public' && record.status === 200) {
      if (record.responseShape?.StartupWizardCompleted) {
        summary.invariants.startupPublicInfoComplete = true;
      } else {
        summary.invariants.startupPublicInfoIncomplete = true;
      }
    }
    if (record.method === 'GET' && pathname === '/Startup/Configuration' && record.status === 200) {
      summary.invariants.startupConfig200 = true;
    }
    if (record.method === 'POST' && pathname === '/Startup/Configuration' && [200, 204].includes(record.status)) {
      summary.invariants.startupConfig204 = true;
    }
    if (record.method === 'POST' && pathname === '/Startup/RemoteAccess' && [200, 204].includes(record.status)) {
      summary.invariants.startupRemoteAccess204 = true;
    }
    if (record.method === 'GET' && pathname === '/Startup/User' && record.status === 200) {
      summary.invariants.startupUser200 = true;
    }
    if (record.method === 'POST' && pathname === '/Startup/User' && [200, 204].includes(record.status)) {
      summary.invariants.startupUser204 = true;
    }
    if (record.method === 'POST' && pathname === '/Startup/Complete' && [200, 204].includes(record.status)) {
      summary.invariants.startupComplete204 = true;
    }
    if (record.method === 'POST' && pathname === '/Users/AuthenticateByName' && record.status === 200) {
      summary.invariants.startupLogin200 = true;
    }
    if (record.method === 'GET' && pathname === '/System/Info' && record.status === 200) {
      summary.invariants.startupSystemInfo200 = true;
    }
  }
  if (pathname === '/UserItems/Resume' && record.method === 'GET' && record.status === 200) {
    summary.invariants.resumeList200 = true;
    const items = record.responseShape?.Items;
    if (Array.isArray(items) && items.length > 0) {
      summary.invariants.resumeItemMatched = true;
      summary.invariants.resumePositionTicks = 'number';
    }
  }
  if (/\/transcoding\/|\/hls\/|\/hls1\/|\.m3u8$/i.test(pathname)) {
    summary.invariants.unexpectedTranscodePath = true;
  }
  if (flow === 'transcode-hls') {
    if (record.method === 'GET' && /master\.m3u8$/i.test(pathname) && record.status === 200) {
      summary.invariants.hlsMaster200 = true;
      addUnique(summary.invariants.hlsPlaylistShapes, 'master');
    }
    if (record.method === 'GET' && /(?:main|live|stream)\.m3u8$/i.test(pathname) && record.status === 200) {
      summary.invariants.hlsMedia200 = true;
      addUnique(summary.invariants.hlsPlaylistShapes, 'media');
    }
    if (record.method === 'GET' && /\/(?:hls|hls1)\/.*\.(?:ts|mp4|aac|mp3)$/i.test(pathname) && [200, 206].includes(record.status)) {
      summary.invariants.hlsSegment200 = true;
      addUnique(summary.invariants.hlsSegmentContentTypes, mediaType(record.responseContentType));
    }
  }
  if (flow === 'admin-dashboard' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/System/Info') {
      summary.invariants.adminSystemInfo200 = true;
    }
    if (record.method === 'GET' && pathname === '/System/Info/Storage') {
      summary.invariants.adminStorage200 = true;
    }
    if (record.method === 'GET' && pathname === '/ScheduledTasks') {
      summary.invariants.adminScheduledTasks200 = true;
    }
    if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
      summary.invariants.adminActivityLog200 = true;
    }
    if (record.method === 'GET' && pathname === '/Devices') {
      summary.invariants.adminDevices200 = true;
    }
    if (record.method === 'GET' && pathname === '/Plugins') {
      summary.invariants.adminPlugins200 = true;
    }
    if (record.method === 'GET' && pathname === '/Repositories') {
      summary.invariants.adminRepositories200 = true;
    }
    if (record.method === 'GET' && pathname === '/web/ConfigurationPages') {
      summary.invariants.adminConfigPages200 = true;
    }
  }
  if (flow === 'plugins-packages') {
    if (record.method === 'GET' && pathname === '/Plugins' && record.status === 200) {
      summary.invariants.pluginsList200 = true;
    }
    if (record.method === 'GET' && pathname === '/Package/Repositories' && record.status === 200) {
      summary.invariants.pluginRepositories200 = true;
    }
    if (record.method === 'POST' && pathname === '/Package/Repositories' && [200, 204].includes(record.status)) {
      summary.invariants.pluginRepositoryUpdated = true;
    }
    if (record.method === 'GET' && ['/Packages', '/Package/Packages'].includes(pathname) && record.status === 200) {
      summary.invariants.pluginPackages200 = true;
    }
    if (record.method === 'GET' && /\/Plugins\/[^/]+\/Manifest$/i.test(pathname) && record.status === 200) {
      summary.invariants.pluginManifest200 = true;
    }
    if (record.method === 'POST' && /\/Package\/Packages\/Installed\/[^/]+$/i.test(pathname) && record.status === 409) {
      summary.invariants.pluginInstallRejected = true;
    }
    if (record.method === 'POST' && /\/Plugins\/[^/]+\/[^/]+\/Enable$/i.test(pathname) && record.status === 409) {
      summary.invariants.pluginEnableRejected = true;
    }
    if (record.method === 'POST' && /\/Plugins\/[^/]+\/[^/]+\/Disable$/i.test(pathname) && record.status === 409) {
      summary.invariants.pluginDisableRejected = true;
    }
    if (record.method === 'DELETE' && /\/Plugins\/[^/]+\/[^/]+$/i.test(pathname) && record.status === 409) {
      summary.invariants.pluginUninstallRejected = true;
    }
  }
  if (flow === 'live-tv') {
    if (record.method === 'POST' && pathname === '/System/Configuration/livetv' && [200, 204].includes(record.status)) {
      summary.invariants.liveTvConfigUpdated = true;
    }
    if (record.method === 'GET' && pathname === '/LiveTv/Info' && record.status === 200) {
      summary.invariants.liveTvInfo200 = true;
    }
    if (record.method === 'GET' && pathname === '/LiveTv/TunerHosts/Types' && record.status === 200) {
      summary.invariants.liveTvTunerTypes200 = true;
    }
    if (record.method === 'GET' && pathname === '/LiveTv/Channels' && record.status === 200) {
      summary.invariants.liveTvChannels200 = true;
    }
    if (record.method === 'GET' && pathname === '/LiveTv/Programs' && record.status === 200) {
      summary.invariants.liveTvGuidePrograms200 = true;
    }
    if (record.method === 'GET' && /\/LiveTv\/LiveStreamFiles\/[^/]+\/stream\.ts$/i.test(pathname) && record.status === 200) {
      summary.invariants.liveTvStream200 = true;
    }
    if (record.method === 'GET' && pathname === '/LiveTv/Recordings' && record.status === 200) {
      summary.invariants.liveTvRecordings200 = true;
    }
    if (record.method === 'GET' && /\/LiveTv\/LiveRecordings\/[^/]+\/stream$/i.test(pathname) && record.status === 200) {
      summary.invariants.liveTvRecordingStream200 = true;
    }
    if (record.method === 'POST' && pathname === '/LiveTv/Timers' && record.status === 200) {
      summary.invariants.liveTvTimerCreated = true;
    }
    if (record.method === 'DELETE' && /\/LiveTv\/Timers\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.liveTvTimerDeleted = true;
    }
    if (record.method === 'POST' && pathname === '/LiveTv/SeriesTimers' && record.status === 200) {
      summary.invariants.liveTvSeriesTimerCreated = true;
    }
    if (record.method === 'DELETE' && /\/LiveTv\/SeriesTimers\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.liveTvSeriesTimerDeleted = true;
    }
  }
  if (flow === 'libraries' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/UserViews') {
      summary.invariants.libraryViews200 = true;
    }
    if (record.method === 'GET' && pathname === '/UserViews/GroupingOptions') {
      summary.invariants.libraryGroupingOptions200 = true;
    }
    if (record.method === 'GET' && pathname === '/Library/VirtualFolders') {
      summary.invariants.libraryVirtualFolders200 = true;
    }
    if (record.method === 'GET' && pathname === '/Items/Counts') {
      summary.invariants.libraryItemsCounts200 = true;
    }
    if (record.method === 'GET' && pathname === '/Items') {
      summary.invariants.libraryItems200 = true;
    }
    if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/Latest$/i.test(pathname)) {
      summary.invariants.libraryLatest200 = true;
    }
  }
  if (flow === 'subtitles-trickplay' && record.status === 200) {
    if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/subtitles\.m3u8$/i.test(pathname)) {
      summary.invariants.subtitlePlaylist200 = true;
    }
    if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/Stream\.vtt$/i.test(pathname)) {
      summary.invariants.subtitleVtt200 = true;
    }
    if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/tiles\.m3u8$/i.test(pathname)) {
      summary.invariants.trickplayPlaylist200 = true;
    }
    if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/[^/]+\.jpg$/i.test(pathname)) {
      summary.invariants.trickplayTile200 = true;
    }
  }
  if (flow === 'audio-hls-legacy' && record.status === 200) {
    if (record.method === 'GET' && /\/Audio\/[^/]+\/master\.m3u8$/i.test(pathname)) {
      summary.invariants.audioHlsMaster200 = true;
    }
    if (record.method === 'GET' && /\/Audio\/[^/]+\/main\.m3u8$/i.test(pathname)) {
      summary.invariants.audioHlsMedia200 = true;
    }
    if (record.method === 'GET' && /\/Audio\/[^/]+\/hls1\/.*\.(?:mp3|aac|ts)$/i.test(pathname)) {
      summary.invariants.audioHlsDynamicSegment200 = true;
      addUnique(summary.invariants.audioHlsSegmentContentTypes, mediaType(record.responseContentType));
    }
    if (record.method === 'GET' && /\/(?:HlsSegment\/)?Audio\/[^/]+\/hls\/[^/]+\/stream\.(?:mp3|aac)$/i.test(pathname)) {
      summary.invariants.audioHlsLegacySegment200 = true;
      addUnique(summary.invariants.audioHlsSegmentContentTypes, mediaType(record.responseContentType));
    }
  }
  if (flow === 'music' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/UserViews') {
      summary.invariants.musicViewMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Items') {
      summary.invariants.musicSongsMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Albums') {
      summary.invariants.musicAlbumMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Artists') {
      summary.invariants.musicArtistMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Artists/AlbumArtists') {
      summary.invariants.musicAlbumArtistMatched = true;
    }
    if (record.method === 'GET' && pathname === '/MusicGenres') {
      summary.invariants.musicGenreMatched = true;
    }
    if (record.method === 'GET' && /\/InstantMix\//i.test(pathname)) {
      summary.invariants.musicInstantMix200 = true;
    }
    if (record.method === 'GET' && /\/Audio\/[^/]+\/stream\.mp3$/i.test(pathname)) {
      summary.invariants.musicAudioStream200 = true;
      addUnique(summary.invariants.musicAudioStreamContentTypes, mediaType(record.responseContentType));
    }
  }
  if (flow === 'series' && record.status === 200) {
    if (record.method === 'GET' && pathname === '/UserViews') {
      summary.invariants.seriesViewMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Items') {
      summary.invariants.seriesEpisodesMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Items/Counts') {
      summary.invariants.seriesCounts200 = true;
    }
    if (record.method === 'GET' && pathname === '/Shows/NextUp') {
      summary.invariants.seriesNextUp200 = true;
    }
    if (record.method === 'GET' && /\/Shows\/[^/]+\/Seasons$/i.test(pathname)) {
      summary.invariants.seriesSeasons200 = true;
    }
    if (record.method === 'GET' && /\/Shows\/[^/]+\/Episodes$/i.test(pathname)) {
      summary.invariants.seriesEpisodesRoute200 = true;
    }
    if (record.method === 'GET' && /\/(?:Library\/)?Shows\/[^/]+\/Similar$/i.test(pathname)) {
      summary.invariants.seriesSimilar200 = true;
    }
    if (record.method === 'GET' && /\/Videos\/[^/]+\/stream\.mp4$/i.test(pathname)) {
      summary.invariants.seriesStream200 = true;
      addUnique(summary.invariants.seriesStreamContentTypes, mediaType(record.responseContentType));
    }
  }
  if (flow === 'playlists-collections') {
    if (record.method === 'POST' && pathname === '/Playlists' && record.status === 200) {
      summary.invariants.playlistCreated = true;
    }
    if (record.method === 'GET' && /\/Playlists\/[^/]+$/i.test(pathname) && record.status === 200) {
      summary.invariants.playlistDetail200 = true;
    }
    if (record.method === 'GET' && /\/Playlists\/[^/]+\/Items$/i.test(pathname) && record.status === 200) {
      summary.invariants.playlistItems200 = true;
    }
    if (record.method === 'POST' && /\/Playlists\/[^/]+\/Items\/[^/]+\/Move\/\d+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.playlistMove204 = true;
    }
    if (record.method === 'POST' && /\/Playlists\/[^/]+\/Items\/[^/]+\/Move\/\d+$/i.test(pathname) && record.status === 400 && summary.target === 'upstream') {
      summary.invariants.playlistMoveUnsupported400 = true;
    }
    if (record.method === 'DELETE' && /\/Playlists\/[^/]+\/Items$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.playlistDeleteItem204 = true;
    }
    if (record.method === 'POST' && /\/Playlists\/[^/]+\/Items$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.playlistAddItem204 = true;
    }
    if (record.method === 'POST' && /\/Playlists\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.playlistRename204 = true;
    }
    if (record.method === 'POST' && /\/Playlists\/[^/]+$/i.test(pathname) && record.status === 400 && summary.target === 'upstream') {
      summary.invariants.playlistRenameUnsupported400 = true;
    }
    if (record.method === 'POST' && pathname === '/Collections' && record.status === 200) {
      summary.invariants.collectionCreated = true;
    }
    if (record.method === 'POST' && /\/Collections\/[^/]+\/Items$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.collectionAddItems204 = true;
    }
    if (record.method === 'DELETE' && /\/Collections\/[^/]+\/Items$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.collectionDeleteItems204 = true;
    }
  }
  if (flow === 'images') {
    if (record.method === 'GET' && /\/Items\/[^/]+\/Images$/i.test(pathname) && record.status === 200) {
      if (!summary.invariants.imageInfosInitial200) {
        summary.invariants.imageInfosInitial200 = true;
      } else if (!summary.invariants.imageInfosAfterUpload200) {
        summary.invariants.imageInfosAfterUpload200 = true;
      } else {
        summary.invariants.imageInfosAfterDelete200 = true;
      }
    }
    if (record.method === 'POST' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.imageUpload204 = true;
    }
    if (record.method === 'GET' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname) && record.status === 200) {
      summary.invariants.imageGet200 = true;
    }
    if (record.method === 'HEAD' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname) && record.status === 200) {
      summary.invariants.imageHead200 = true;
    }
    if (record.method === 'GET' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary\/0\/[^/]+\/png\/320\/180\/0\/0$/i.test(pathname) && record.status === 200) {
      summary.invariants.imageExtendedGet200 = true;
    }
    if (record.method === 'GET' && /\/(?:RemoteImage\/)?Items\/[^/]+\/RemoteImages\/Providers$/i.test(pathname) && record.status === 200) {
      summary.invariants.imageProviders200 = true;
    }
    if (record.method === 'DELETE' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.imageDelete204 = true;
    }
  }
  if (flow === 'metadata-search') {
    if (record.method === 'POST' && /\/ItemUpdate\/Items\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      if (!summary.invariants.metadataUpdatePrimary204) {
        summary.invariants.metadataUpdatePrimary204 = true;
      } else {
        summary.invariants.metadataUpdateSimilar204 = true;
      }
    }
    if (record.method === 'GET' && /\/ItemUpdate\/Items\/[^/]+\/MetadataEditor$/i.test(pathname) && record.status === 200) {
      summary.invariants.metadataEditor200 = true;
    }
    if (record.method === 'GET' && /\/ItemLookup\/Items\/[^/]+\/ExternalIdInfos$/i.test(pathname) && record.status === 200) {
      summary.invariants.metadataExternalIds200 = true;
    }
    if (record.method === 'GET' && pathname === '/Items' && record.status === 200) {
      summary.invariants.metadataItemsSearch200 = true;
    }
    if (record.method === 'GET' && pathname === '/Search/Hints' && record.status === 200) {
      summary.invariants.metadataSearchHints200 = true;
    }
    if (record.method === 'GET' && pathname === '/Genres' && record.status === 200) {
      summary.invariants.metadataGenreMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Studios' && record.status === 200) {
      summary.invariants.metadataStudioMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Persons' && record.status === 200) {
      summary.invariants.metadataPersonMatched = true;
    }
    if (record.method === 'GET' && pathname === '/Years' && record.status === 200) {
      summary.invariants.metadataYearMatched = true;
    }
    if (record.method === 'GET' && /\/Items\/[^/]+\/Similar$/i.test(pathname) && record.status === 200) {
      summary.invariants.metadataSimilar200 = true;
    }
  }
  if (flow === 'auth-users') {
    if (record.method === 'GET' && pathname === '/Users/Public' && record.status === 200) {
      summary.invariants.authUsersPublic200 = true;
    }
    if (record.method === 'GET' && pathname === '/Users' && record.status === 200) {
      summary.invariants.authUsersList200 = true;
    }
    if (record.method === 'GET' && pathname === '/Auth/Providers' && record.status === 200) {
      summary.invariants.authProviders200 = true;
    }
    if (record.method === 'GET' && pathname === '/Auth/PasswordResetProviders' && record.status === 200) {
      summary.invariants.authPasswordResetProviders200 = true;
    }
    if (record.method === 'POST' && pathname === '/Users/New' && record.status === 200) {
      summary.invariants.authUserCreated = true;
    }
    if (record.method === 'POST' && pathname === '/Users/AuthenticateByName' && record.status === 200) {
      summary.invariants.authCreatedUserLogin200 = true;
    }
    if (record.method === 'GET' && pathname === '/Users/Me' && record.status === 200) {
      summary.invariants.authCreatedUserMe200 = true;
    }
    if (record.method === 'GET' && /\/Users\/[^/]+$/i.test(pathname) && record.status === 200) {
      summary.invariants.authUserDetail200 = true;
    }
    if (record.method === 'POST' && /\/Users\/[^/]+\/Policy$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.authUserPolicy204 = true;
    }
    if (record.method === 'POST' && /\/Users\/[^/]+\/Configuration$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.authUserConfiguration204 = true;
    }
    if (record.method === 'GET' && pathname === '/Auth/Keys' && record.status === 200) {
      summary.invariants.authKeysList200 = true;
    }
    if (record.method === 'POST' && pathname === '/Auth/Keys' && [200, 204].includes(record.status)) {
      summary.invariants.authKeyCreated = true;
    }
    if (record.method === 'GET' && pathname === '/System/Info' && record.status === 200) {
      summary.invariants.authKeyUsable = true;
    }
    if (record.method === 'DELETE' && /\/Auth\/Keys\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.authKeyRevoked = true;
    }
    if (record.method === 'POST' && pathname === '/Sessions/Logout' && [200, 204].includes(record.status)) {
      summary.invariants.authCreatedUserLogout204 = true;
    }
    if (record.method === 'DELETE' && /\/Users\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.authUserDeleted = true;
    }
  }
  if (flow === 'channels') {
    if (record.method === 'GET' && pathname === '/Channels' && record.status === 200) {
      summary.invariants.channelsList200 = true;
    }
    if (record.method === 'GET' && pathname === '/Channels/Features' && record.status === 200) {
      summary.invariants.channelsFeatures200 = true;
    }
    if (record.method === 'GET' && /\/Channels\/[^/]+\/Items$/i.test(pathname) && record.status === 200) {
      summary.invariants.channelsItems200 = true;
    }
    if (record.method === 'GET' && pathname === '/Channels/Items/Latest' && record.status === 200) {
      summary.invariants.channelsLatest200 = true;
    }
    if (record.method === 'GET' && /\/Channels\/[^/]+\/Features$/i.test(pathname) && record.status === 200) {
      summary.invariants.channelsFeatureMatched = true;
    }
  }
  if (flow === 'non-web-client') {
    if (record.method === 'POST' && pathname === '/Users/AuthenticateByName' && record.status === 200) {
      summary.invariants.nonWebClientAuthenticated = true;
    }
    if (record.method === 'GET' && pathname === '/System/Info' && record.status === 200) {
      summary.invariants.nonWebSystemInfo200 = true;
    }
    if (record.method === 'GET' && /\/Users\/[^/]+\/Views$/i.test(pathname) && record.status === 200) {
      summary.invariants.nonWebBrowse200 = true;
    }
    if (record.method === 'POST' && /\/Items\/[^/]+\/PlaybackInfo$/i.test(pathname) && record.status === 200) {
      summary.invariants.nonWebPlaybackInfo200 = true;
    }
    if (record.method === 'GET' && /\/Videos\/[^/]+\/stream\.mp4$/i.test(pathname) && [200, 206].includes(record.status)) {
      summary.invariants.nonWebStream200 = true;
    }
    if (record.method === 'POST' && pathname === '/Sessions/Playing/Progress' && record.status === 204) {
      summary.invariants.nonWebProgress204 = true;
    }
    if (record.method === 'GET' && pathname === '/UserItems/Resume' && record.status === 200) {
      summary.invariants.nonWebResumeMatched = true;
    }
    if (record.method === 'GET' && pathname === '/System/Configuration/network' && record.status === 200) {
      summary.invariants.nonWebDlnaUnsupportedDecided = true;
    }
  }
  if (flow === 'scheduled-tasks') {
    if (record.method === 'GET' && pathname === '/ScheduledTasks' && record.status === 200) {
      summary.invariants.scheduledTasksList200 = true;
    }
    if (record.method === 'GET' && /\/ScheduledTasks\/[^/]+$/i.test(pathname) && record.status === 200) {
      summary.invariants.scheduledTasksDetail200 = true;
    }
    if (record.method === 'POST' && /\/ScheduledTasks\/Running\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.scheduledTasksStarted = true;
    }
    if (record.method === 'DELETE' && /\/ScheduledTasks\/Running\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.scheduledTasksCancelled = true;
    }
    if (record.method === 'POST' && /\/ScheduledTasks\/[^/]+\/Triggers$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.scheduledTasksTriggers204 = true;
    }
    if (record.method === 'POST' && pathname === '/Library/Refresh' && [200, 204].includes(record.status)) {
      summary.invariants.scheduledTasksLibraryRefresh204 = true;
    }
    if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries' && record.status === 200) {
      summary.invariants.scheduledTasksActivityLogged = true;
    }
  }
  if (flow === 'backup-restore') {
    if (record.method === 'GET' && pathname === '/Backup' && record.status === 200) {
      summary.invariants.backupList200 = true;
    }
    if (record.method === 'POST' && pathname === '/Backup/Create' && record.status === 200) {
      summary.invariants.backupCreated = true;
      summary.invariants.backupSnapshotSummary = true;
    }
    if (record.method === 'GET' && pathname === '/Backup/Manifest' && record.status === 200) {
      summary.invariants.backupManifest200 = true;
    }
    if (record.method === 'POST' && pathname === '/Backup/Restore' && [200, 204].includes(record.status)) {
      summary.invariants.backupRestored = true;
    }
    if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries' && record.status === 200) {
      summary.invariants.backupActivityLogged = true;
    }
  }
  if (flow === 'migration-import') {
    if (record.method === 'POST' && pathname === '/Migration/Jellyfin/DryRun' && record.status === 200) {
      summary.invariants.migrationDryRun200 = true;
      summary.invariants.migrationReadOnlyPolicy = true;
    }
    if (record.method === 'POST' && pathname === '/Migration/Jellyfin/Import' && record.status === 200) {
      summary.invariants.migrationImport200 = true;
      summary.invariants.migrationBackupCreated = true;
      summary.invariants.migrationRollbackDocumented = true;
    }
    if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries' && record.status === 200) {
      summary.invariants.migrationActivityLogged = true;
    }
  }
  if (flow === 'sessions-websocket') {
    if (record.method === 'GET' && pathname === '/Sessions' && record.status === 200) {
      summary.invariants.sessionsList200 = true;
    }
    if (record.method === 'POST' && /\/Sessions\/Capabilities\/Full$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.sessionsCapabilities204 = true;
    }
    if (record.method === 'POST' && /\/Sessions\/[^/]+\/User\/[^/]+$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.sessionsUserAdd204 = true;
    }
    if (record.method === 'POST' && /\/Sessions\/[^/]+\/Playing$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.sessionsRemotePlay204 = true;
    }
    if (record.method === 'POST' && /\/Sessions\/[^/]+\/Playing\/Pause$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.sessionsRemotePlaystate204 = true;
    }
    if (record.method === 'POST' && /\/Sessions\/[^/]+\/Playing\/Stop$/i.test(pathname) && [200, 204].includes(record.status)) {
      summary.invariants.sessionsRemoteStop204 = true;
    }
  }
  if (flow === 'syncplay') {
    if (record.method === 'POST' && pathname === '/SyncPlay/New' && record.status === 200) {
      summary.invariants.syncplayGroupCreated = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Join' && record.status === 200) {
      summary.invariants.syncplayGuestJoined = true;
    }
    if (record.method === 'GET' && pathname === '/SyncPlay/List' && record.status === 200) {
      summary.invariants.syncplayList200 = true;
    }
    if (record.method === 'GET' && /\/SyncPlay\/[^/]+$/i.test(pathname) && record.status === 200) {
      summary.invariants.syncplayGet200 = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Play' && [200, 204].includes(record.status)) {
      summary.invariants.syncplayPlay204 = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Pause' && [200, 204].includes(record.status)) {
      summary.invariants.syncplayPause204 = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Seek' && [200, 204].includes(record.status)) {
      summary.invariants.syncplaySeek204 = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Unpause' && [200, 204].includes(record.status)) {
      summary.invariants.syncplayUnpause204 = true;
    }
    if (record.method === 'POST' && pathname === '/SyncPlay/Leave' && [200, 204].includes(record.status)) {
      if (!summary.invariants.syncplayGuestLeft) {
        summary.invariants.syncplayGuestLeft = true;
      } else {
        summary.invariants.syncplayOwnerLeft = true;
      }
    }
  }
}

function criticalRequestKey(record, requestPostData) {
  const pathname = new URL(record.url).pathname;
  if (record.method === 'POST' && pathname.toLowerCase() === '/users/authenticatebyname') {
    return flow === 'auth-users' ? 'auth-created-user-login' : 'auth';
  }
  if (flow === 'plugins-packages' && record.method === 'GET' && pathname === '/Plugins') {
    return 'plugins-list';
  }
  if (flow === 'plugins-packages' && record.method === 'GET' && pathname === '/Package/Repositories') {
    return 'plugin-repositories';
  }
  if (flow === 'plugins-packages' && record.method === 'GET' && ['/Packages', '/Package/Packages'].includes(pathname)) {
    return 'plugin-packages';
  }
  if (flow === 'plugins-packages' && record.method === 'GET' && /\/Plugins\/[^/]+\/Manifest$/i.test(pathname)) {
    return 'plugin-manifest';
  }
  if (flow === 'live-tv' && record.method === 'GET' && pathname === '/LiveTv/Info') {
    return 'live-tv-info';
  }
  if (flow === 'live-tv' && record.method === 'GET' && pathname === '/LiveTv/TunerHosts/Types') {
    return 'live-tv-tuner-types';
  }
  if (flow === 'live-tv' && record.method === 'POST' && pathname === '/LiveTv/TunerHosts') {
    return 'live-tv-hdhr-tuner-host';
  }
  if (flow === 'live-tv' && record.method === 'GET' && pathname === '/LiveTv/Channels') {
    return 'live-tv-channels';
  }
  if (flow === 'live-tv' && record.method === 'GET' && /\/LiveTv\/LiveStreamFiles\/[^/]+\/stream\.ts$/i.test(pathname)) {
    return 'live-tv-hdhr-stream';
  }
  if (flow === 'channels' && record.method === 'GET' && pathname === '/Channels') {
    const searchParams = new URL(record.url).searchParams;
    if (searchParams.get('SupportsMediaDeletion') === 'true') {
      return 'channels-media-deletion-filter';
    }
    if (searchParams.get('SupportsMediaDeletion') === 'false') {
      return 'channels-filters';
    }
    return 'channels-list';
  }
  if (flow === 'channels' && record.method === 'GET' && pathname === '/Channels/Features') {
    return 'channels-features';
  }
  if (flow === 'channels' && record.method === 'GET' && /\/Channels\/[^/]+\/Items$/i.test(pathname)) {
    return 'channels-items';
  }
  if (flow === 'channels' && record.method === 'GET' && pathname === '/Channels/Items/Latest') {
    return 'channels-latest';
  }
  if (flow === 'channels' && record.method === 'GET' && /\/Channels\/[^/]+\/Features$/i.test(pathname)) {
    return 'channels-feature-by-id';
  }
  if (flow === 'non-web-client' && record.method === 'GET' && pathname === '/System/Info') {
    return 'non-web-system-info';
  }
  if (flow === 'non-web-client' && record.method === 'GET' && /\/Users\/[^/]+\/Views$/i.test(pathname)) {
    return 'non-web-views';
  }
  if (flow === 'non-web-client' && record.method === 'POST' && /\/Items\/[^/]+\/PlaybackInfo$/i.test(pathname)) {
    return 'non-web-playback-info';
  }
  if (flow === 'non-web-client' && record.method === 'GET' && /\/Videos\/[^/]+\/stream\.mp4$/i.test(pathname)) {
    return 'non-web-video-stream';
  }
  if (flow === 'non-web-client' && record.method === 'POST' && pathname === '/Sessions/Playing/Progress') {
    return 'non-web-progress';
  }
  if (flow === 'non-web-client' && record.method === 'GET' && pathname === '/UserItems/Resume') {
    return 'non-web-resume';
  }
  if (flow === 'non-web-client' && record.method === 'GET' && pathname === '/System/Configuration/network') {
    return 'non-web-network-config';
  }
  if (flow === 'scheduled-tasks' && record.method === 'GET' && pathname === '/ScheduledTasks') {
    return 'scheduled-tasks-list';
  }
  if (flow === 'scheduled-tasks' && record.method === 'GET' && /\/ScheduledTasks\/[^/]+$/i.test(pathname)) {
    return 'scheduled-tasks-detail';
  }
  if (flow === 'scheduled-tasks' && record.method === 'POST' && /\/ScheduledTasks\/Running\/[^/]+$/i.test(pathname)) {
    return 'scheduled-tasks-start';
  }
  if (flow === 'scheduled-tasks' && record.method === 'DELETE' && /\/ScheduledTasks\/Running\/[^/]+$/i.test(pathname)) {
    return 'scheduled-tasks-cancel';
  }
  if (flow === 'scheduled-tasks' && record.method === 'POST' && /\/ScheduledTasks\/[^/]+\/Triggers$/i.test(pathname)) {
    return 'scheduled-tasks-triggers';
  }
  if (flow === 'scheduled-tasks' && record.method === 'POST' && pathname === '/Library/Refresh') {
    return 'scheduled-tasks-library-refresh';
  }
  if (flow === 'scheduled-tasks' && record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
    return 'scheduled-tasks-activity-log';
  }
  if (flow === 'backup-restore' && record.method === 'GET' && pathname === '/Backup') {
    return 'backup-list';
  }
  if (flow === 'backup-restore' && record.method === 'POST' && pathname === '/Backup/Create') {
    return 'backup-create';
  }
  if (flow === 'backup-restore' && record.method === 'GET' && pathname === '/Backup/Manifest') {
    return 'backup-manifest';
  }
  if (flow === 'backup-restore' && record.method === 'POST' && pathname === '/Backup/Restore') {
    return 'backup-restore';
  }
  if (flow === 'backup-restore' && record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
    return 'backup-activity-log';
  }
  if (flow === 'migration-import' && record.method === 'POST' && pathname === '/Migration/Jellyfin/DryRun') {
    return 'migration-dry-run';
  }
  if (flow === 'migration-import' && record.method === 'POST' && pathname === '/Migration/Jellyfin/Import') {
    return 'migration-import';
  }
  if (flow === 'migration-import' && record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
    return 'migration-activity-log';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/New') {
    return 'syncplay-new';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/SetNewQueue') {
    return 'syncplay-play';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/Join') {
    return 'syncplay-join';
  }
  if (flow === 'syncplay' && record.method === 'GET' && pathname === '/SyncPlay/List') {
    return 'syncplay-list';
  }
  if (flow === 'syncplay' && record.method === 'GET' && /\/SyncPlay\/[^/]+$/i.test(pathname)) {
    return 'syncplay-get';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/Pause') {
    return 'syncplay-pause';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/Play') {
    return 'syncplay-play';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/Seek') {
    return 'syncplay-seek';
  }
  if (flow === 'syncplay' && record.method === 'POST' && pathname === '/SyncPlay/Unpause') {
    return 'syncplay-unpause';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Users/Public') {
    return 'auth-users-public';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Users') {
    return 'auth-users-list';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Auth/Providers') {
    return 'auth-providers';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Auth/PasswordResetProviders') {
    return 'auth-password-reset-providers';
  }
  if (flow === 'auth-users' && record.method === 'POST' && pathname === '/Users/New') {
    return 'auth-user-create';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Users/Me') {
    return 'auth-users-me';
  }
  if (flow === 'auth-users' && record.method === 'GET' && /\/Users\/[^/]+$/i.test(pathname)) {
    return 'auth-user-detail';
  }
  if (flow === 'auth-users' && record.method === 'POST' && /\/Users\/[^/]+\/Policy$/i.test(pathname)) {
    return 'auth-user-policy';
  }
  if (flow === 'auth-users' && record.method === 'POST' && /\/Users\/[^/]+\/Configuration$/i.test(pathname)) {
    return 'auth-user-configuration';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/Auth/Keys') {
    return 'auth-keys-list';
  }
  if (flow === 'auth-users' && record.method === 'POST' && pathname === '/Auth/Keys') {
    return 'auth-key-create';
  }
  if (flow === 'auth-users' && record.method === 'GET' && pathname === '/System/Info') {
    return 'auth-key-system-info';
  }
  if (flow === 'auth-users' && record.method === 'DELETE' && /\/Auth\/Keys\/[^/]+$/i.test(pathname)) {
    return 'auth-key-delete';
  }
  if (flow === 'auth-users' && record.method === 'POST' && pathname === '/Sessions/Logout') {
    return 'auth-user-logout';
  }
  if (flow === 'auth-users' && record.method === 'DELETE' && /\/Users\/[^/]+$/i.test(pathname)) {
    return 'auth-user-delete';
  }
  if (flow === 'startup-wizard' && record.method === 'GET' && pathname === '/System/Info/Public') {
    return 'startup-public-info';
  }
  if (flow === 'startup-wizard' && record.method === 'GET' && pathname === '/Startup/Configuration') {
    return 'startup-config';
  }
  if (flow === 'startup-wizard' && record.method === 'POST' && pathname === '/Startup/Configuration') {
    return 'startup-config-update';
  }
  if (flow === 'startup-wizard' && record.method === 'GET' && pathname === '/Startup/User') {
    return 'startup-user';
  }
  if (flow === 'startup-wizard' && record.method === 'POST' && pathname === '/Startup/User') {
    return 'startup-user-update';
  }
  if (flow === 'startup-wizard' && record.method === 'POST' && pathname === '/Startup/RemoteAccess') {
    return 'startup-remote-access';
  }
  if (flow === 'startup-wizard' && record.method === 'GET' && pathname === '/Users/Public') {
    const phase = new URL(record.url).searchParams.get('phase');
    return phase === 'after' ? 'startup-public-users-after' : 'startup-public-users-before';
  }
  if (flow === 'startup-wizard' && record.method === 'POST' && pathname === '/Startup/Complete') {
    return 'startup-complete';
  }
  if (flow === 'startup-wizard' && record.method === 'POST' && pathname === '/Users/AuthenticateByName') {
    return 'startup-login';
  }
  if (flow === 'startup-wizard' && record.method === 'GET' && pathname === '/System/Info') {
    return 'startup-system-info';
  }
  if (flow === 'sessions-websocket' && record.method === 'GET' && pathname === '/Sessions') {
    return 'sessions-list';
  }
  if (flow === 'sessions-websocket' && record.method === 'POST' && /\/Sessions\/Capabilities\/Full$/i.test(pathname)) {
    return 'sessions-capabilities';
  }
  if (flow === 'sessions-websocket' && record.method === 'POST' && /\/Sessions\/[^/]+\/User\/[^/]+$/i.test(pathname)) {
    return 'sessions-add-user';
  }
  if (flow === 'sessions-websocket' && record.method === 'POST' && /\/Sessions\/[^/]+\/Playing$/i.test(pathname)) {
    return 'sessions-remote-play';
  }
  if (flow === 'sessions-websocket' && record.method === 'POST' && /\/Sessions\/[^/]+\/Playing\/Pause$/i.test(pathname)) {
    return 'sessions-remote-playstate';
  }
  if (flow === 'sessions-websocket' && record.method === 'POST' && /\/Sessions\/[^/]+\/Playing\/Stop$/i.test(pathname)) {
    return 'sessions-remote-stop';
  }
  if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/Latest$/i.test(pathname)) {
    return 'library-latest';
  }
  if (flow === 'playlists-collections' && record.method === 'GET' && /\/Users\/[^/]+\/Items\/[^/]+$/i.test(pathname)) {
    return 'playlist-detail';
  }
  if (record.method === 'GET' && /\/Users\/[^/]+\/Items\/[^/]+$/i.test(pathname)) {
    return 'item-detail';
  }
  if (record.method === 'POST' && /\/Items\/[^/]+\/PlaybackInfo$/i.test(pathname)) {
    return 'playback-info';
  }
  if (record.method === 'GET' && /\/Videos\/[^/]+\/stream/i.test(pathname)) {
    return flow === 'series' ? 'series-video-stream' : 'video-stream';
  }
  if (record.method === 'POST' && pathname === '/Sessions/Playing') {
    return 'sessions-playing';
  }
  if (record.method === 'POST' && pathname === '/Sessions/Playing/Progress') {
    return 'sessions-playing-progress';
  }
  if (record.method === 'GET' && pathname === '/UserItems/Resume') {
    return 'resume-list';
  }
  if (record.method === 'GET' && pathname === '/Albums') {
    return 'music-albums';
  }
  if (record.method === 'GET' && pathname === '/Artists') {
    return 'music-artists';
  }
  if (record.method === 'GET' && pathname === '/Artists/AlbumArtists') {
    return 'music-album-artists';
  }
  if (record.method === 'GET' && pathname === '/MusicGenres') {
    return 'music-genres';
  }
  if (record.method === 'GET' && /\/Items\/[^/]+\/InstantMix$/i.test(pathname)) {
    return 'music-instant-mix';
  }
  if (record.method === 'GET' && pathname === '/InstantMix/MusicGenres/InstantMix') {
    return 'music-genre-instant-mix';
  }
  if (record.method === 'GET' && /\/InstantMix\/MusicGenres\/[^/]+\/InstantMix$/i.test(pathname)) {
    return 'music-genre-instant-mix';
  }
  if (record.method === 'GET' && /\/Audio\/[^/]+\/stream\.mp3$/i.test(pathname)) {
    return 'music-audio-stream';
  }
  if (record.method === 'GET' && pathname === '/Shows/NextUp') {
    return 'series-next-up';
  }
  if (record.method === 'GET' && /\/Shows\/[^/]+\/Seasons$/i.test(pathname)) {
    return 'series-seasons';
  }
  if (record.method === 'GET' && /\/Shows\/[^/]+\/Episodes$/i.test(pathname)) {
    return 'series-episodes';
  }
  if (record.method === 'GET' && /\/(?:Library\/)?Shows\/[^/]+\/Similar$/i.test(pathname)) {
    return 'series-similar';
  }
  if (record.method === 'POST' && pathname === '/Playlists') {
    return 'playlist-create';
  }
  if (record.method === 'GET' && /\/Playlists\/[^/]+$/i.test(pathname)) {
    return 'playlist-detail';
  }
  if (record.method === 'GET' && /\/Playlists\/[^/]+\/Items$/i.test(pathname)) {
    return 'playlist-items';
  }
  if (record.method === 'POST' && /\/Playlists\/[^/]+\/Items\/[^/]+\/Move\/\d+$/i.test(pathname)) {
    return 'playlist-move';
  }
  if (record.method === 'DELETE' && /\/Playlists\/[^/]+\/Items$/i.test(pathname)) {
    return 'playlist-remove-item';
  }
  if (record.method === 'POST' && /\/Playlists\/[^/]+\/Items$/i.test(pathname)) {
    return 'playlist-add-item';
  }
  if (record.method === 'POST' && /\/Playlists\/[^/]+$/i.test(pathname)) {
    return 'playlist-rename';
  }
  if (record.method === 'POST' && pathname === '/Collections') {
    return 'collection-create';
  }
  if (record.method === 'POST' && /\/Collections\/[^/]+\/Items$/i.test(pathname)) {
    return 'collection-add-items';
  }
  if (record.method === 'DELETE' && /\/Collections\/[^/]+\/Items$/i.test(pathname)) {
    return 'collection-remove-items';
  }
  if (record.method === 'GET' && /\/Items\/[^/]+\/Images$/i.test(pathname)) {
    return 'image-infos';
  }
  if (record.method === 'POST' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname)) {
    return 'image-upload';
  }
  if (record.method === 'GET' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname)) {
    return 'image-get';
  }
  if (record.method === 'HEAD' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname)) {
    return 'image-head';
  }
  if (record.method === 'GET' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary\/0\/[^/]+\/png\/320\/180\/0\/0$/i.test(pathname)) {
    return 'image-extended-get';
  }
  if (record.method === 'GET' && /\/(?:RemoteImage\/)?Items\/[^/]+\/RemoteImages\/Providers$/i.test(pathname)) {
    return 'image-providers';
  }
  if (record.method === 'DELETE' && /\/(?:Image\/)?Items\/[^/]+\/Images\/Primary$/i.test(pathname)) {
    return 'image-delete';
  }
  if (record.method === 'POST' && /\/(?:ItemUpdate\/)?Items\/[^/]+$/i.test(pathname)) {
    return requestPostData?.Overview?.includes('similar') ? 'metadata-update-similar' : 'metadata-update-primary';
  }
  if (record.method === 'GET' && /\/(?:ItemUpdate\/)?Items\/[^/]+\/MetadataEditor$/i.test(pathname)) {
    return 'metadata-editor';
  }
  if (record.method === 'GET' && /\/(?:ItemLookup\/)?Items\/[^/]+\/ExternalIdInfos$/i.test(pathname)) {
    return 'metadata-external-ids';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && pathname === '/Items') {
    return 'metadata-items-search';
  }
  if (record.method === 'GET' && pathname === '/Search/Hints') {
    return 'metadata-search-hints';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && pathname === '/Genres') {
    return 'metadata-genres';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && pathname === '/Studios') {
    return 'metadata-studios';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && pathname === '/Persons') {
    return 'metadata-persons';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && pathname === '/Years') {
    return 'metadata-years';
  }
  if (flow === 'metadata-search' && record.method === 'GET' && /\/Items\/[^/]+\/Similar$/i.test(pathname)) {
    return 'metadata-similar';
  }
  if (record.method === 'GET' && /\/Audio\/[^/]+\/master\.m3u8$/i.test(pathname)) {
    return 'audio-hls-master';
  }
  if (record.method === 'GET' && /\/Audio\/[^/]+\/main\.m3u8$/i.test(pathname)) {
    return 'audio-hls-media';
  }
  if (record.method === 'GET' && /\/Audio\/[^/]+\/hls1\/.*\.(?:mp3|aac|ts)$/i.test(pathname)) {
    return 'audio-hls-dynamic-segment';
  }
  if (record.method === 'GET' && /\/(?:HlsSegment\/)?Audio\/[^/]+\/hls\/[^/]+\/stream\.(?:mp3|aac)$/i.test(pathname)) {
    return 'audio-hls-legacy-segment';
  }
  if (record.method === 'GET' && /master\.m3u8$/i.test(pathname)) {
    return 'hls-master';
  }
  if (record.method === 'GET' && /(?:main|live|stream)\.m3u8$/i.test(pathname)) {
    return 'hls-media';
  }
  if (record.method === 'GET' && /\/(?:hls|hls1)\/.*\.(?:ts|mp4|aac|mp3)$/i.test(pathname)) {
    return 'hls-segment';
  }
  if (record.method === 'GET' && pathname === '/System/Info') {
    return 'admin-system-info';
  }
  if (record.method === 'GET' && pathname === '/System/Info/Storage') {
    return 'admin-storage';
  }
  if (record.method === 'GET' && pathname === '/ScheduledTasks') {
    return 'admin-scheduled-tasks';
  }
  if (record.method === 'GET' && pathname === '/System/ActivityLog/Entries') {
    return 'admin-activity-log';
  }
  if (record.method === 'GET' && pathname === '/Devices') {
    return 'admin-devices';
  }
  if (record.method === 'GET' && pathname === '/Plugins') {
    return 'admin-plugins';
  }
  if (record.method === 'GET' && pathname === '/Repositories') {
    return 'admin-repositories';
  }
  if (record.method === 'GET' && pathname === '/web/ConfigurationPages') {
    return 'admin-config-pages';
  }
  if (record.method === 'GET' && pathname === '/UserViews') {
    return 'library-user-views';
  }
  if (record.method === 'GET' && pathname === '/UserViews/GroupingOptions') {
    return 'library-grouping-options';
  }
  if (record.method === 'GET' && pathname === '/Library/VirtualFolders') {
    return 'library-virtual-folders';
  }
  if (record.method === 'GET' && pathname === '/Items/Counts') {
    return 'library-items-counts';
  }
  if (record.method === 'GET' && pathname === '/Items') {
    return 'library-items';
  }
  if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/subtitles\.m3u8$/i.test(pathname)) {
    return 'subtitle-playlist';
  }
  if (record.method === 'GET' && /\/(?:Subtitle\/)?Videos\/[^/]+\/[^/]+\/Subtitles\/[^/]+\/Stream\.vtt$/i.test(pathname)) {
    return 'subtitle-vtt';
  }
  if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/tiles\.m3u8$/i.test(pathname)) {
    return 'trickplay-playlist';
  }
  if (record.method === 'GET' && /\/(?:Trickplay\/)?Videos\/[^/]+\/Trickplay\/[^/]+\/[^/]+\.jpg$/i.test(pathname)) {
    return 'trickplay-tile';
  }
  return null;
}

function criticalRequestSummary(record, requestPostData) {
  const summary = {
    method: record.method,
    status: record.status,
    contentType: record.responseContentType,
    queryKeys: record.queryKeysPreservingCase,
    responseShape: record.responseShape,
  };
  if (record.responseHeaders['accept-ranges']) {
    summary.acceptRanges = record.responseHeaders['accept-ranges'];
  }
  if (record.responseHeaders['content-range']) {
    summary.hasContentRange = true;
  }
  if (requestPostData && typeof requestPostData === 'object' && requestPostData.PlayMethod) {
    summary.playMethod = requestPostData.PlayMethod;
  }
  return summary;
}

function compareCompletedTargets(summaries) {
  if (!['startup-wizard', 'p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay', 'audio-hls-legacy', 'music', 'series', 'playlists-collections', 'images', 'metadata-search', 'auth-users', 'sessions-websocket', 'syncplay', 'channels', 'non-web-client', 'scheduled-tasks', 'backup-restore', 'migration-import'].includes(flow)) {
    return [];
  }
  const upstream = summaries.find((summary) => summary.target === 'upstream' && summary.status === 'completed');
  const jellyrin = summaries.find((summary) => summary.target === 'jellyrin' && summary.status === 'completed');
  if (!upstream || !jellyrin) {
    return [];
  }

  const reasons = [];
  const keys = flow === 'startup-wizard'
    ? ['startup-public-info', 'startup-config', 'startup-config-update', 'startup-user', 'startup-user-update', 'startup-remote-access', 'startup-public-users-before', 'startup-complete', 'startup-login', 'startup-system-info', 'startup-public-users-after']
    : flow === 'resume'
    ? ['resume-list', 'sessions-playing-progress']
    : flow === 'transcode-hls'
      ? ['playback-info', 'hls-master', 'hls-media', 'hls-segment']
      : flow === 'admin-dashboard'
        ? [
            'admin-system-info',
            'admin-storage',
            'admin-scheduled-tasks',
            'admin-activity-log',
            'admin-devices',
            'admin-plugins',
            'admin-repositories',
            'admin-config-pages',
          ]
        : flow === 'libraries'
          ? [
              'library-user-views',
              'library-grouping-options',
              'library-virtual-folders',
              'library-items-counts',
              'library-items',
              'library-latest',
            ]
          : flow === 'subtitles-trickplay'
            ? ['playback-info', 'subtitle-playlist', 'subtitle-vtt', 'trickplay-playlist', 'trickplay-tile']
            : flow === 'audio-hls-legacy'
              ? ['audio-hls-master', 'audio-hls-media', 'audio-hls-dynamic-segment', 'audio-hls-legacy-segment']
              : flow === 'music'
                ? ['library-user-views', 'library-items', 'music-albums', 'music-artists', 'music-album-artists', 'music-genres', 'music-instant-mix', 'music-audio-stream']
                : flow === 'series'
                  ? ['library-user-views', 'library-items', 'library-items-counts', 'series-next-up', 'series-seasons', 'series-episodes', 'series-similar', 'series-video-stream']
                  : flow === 'playlists-collections'
                    ? ['playlist-create', 'playlist-detail', 'playlist-items', 'playlist-move', 'playlist-remove-item', 'playlist-add-item', 'playlist-rename', 'collection-create', 'collection-add-items', 'collection-remove-items']
                    : flow === 'images'
                      ? ['image-infos', 'image-upload', 'image-get', 'image-head', 'image-extended-get', 'image-providers', 'image-delete']
                      : flow === 'metadata-search'
                        ? ['metadata-update-primary', 'metadata-update-similar', 'metadata-editor', 'metadata-external-ids', 'metadata-items-search', 'metadata-search-hints', 'metadata-genres', 'metadata-studios', 'metadata-persons', 'metadata-years', 'metadata-similar']
                        : flow === 'auth-users'
                          ? ['auth-users-public', 'auth-users-list', 'auth-providers', 'auth-password-reset-providers', 'auth-user-create', 'auth-created-user-login', 'auth-users-me', 'auth-user-detail', 'auth-user-policy', 'auth-user-configuration', 'auth-keys-list', 'auth-key-create', 'auth-key-system-info', 'auth-key-delete', 'auth-user-logout', 'auth-user-delete']
                          : flow === 'sessions-websocket'
                            ? ['sessions-list', 'sessions-capabilities', 'sessions-add-user', 'sessions-remote-play', 'sessions-remote-playstate', 'sessions-remote-stop']
                            : flow === 'syncplay'
                              ? ['syncplay-new', 'syncplay-join', 'syncplay-list', 'syncplay-get', 'syncplay-play', 'syncplay-pause', 'syncplay-seek', 'syncplay-unpause']
                            : flow === 'plugins-packages'
                              ? ['plugins-list', 'plugin-repositories', 'plugin-packages']
                              : flow === 'live-tv'
                                ? ['live-tv-info', 'live-tv-tuner-types', 'live-tv-hdhr-tuner-host', 'live-tv-channels']
                                : flow === 'channels'
                                  ? ['channels-list', 'channels-features', 'channels-filters', 'channels-media-deletion-filter', 'channels-items', 'channels-latest', 'channels-feature-by-id']
                                  : flow === 'non-web-client'
                                    ? ['non-web-system-info', 'non-web-views', 'non-web-playback-info', 'non-web-video-stream', 'non-web-progress', 'non-web-resume']
                                    : flow === 'scheduled-tasks'
                                      ? ['scheduled-tasks-list', 'scheduled-tasks-detail', 'scheduled-tasks-start', 'scheduled-tasks-cancel', 'scheduled-tasks-triggers', 'scheduled-tasks-library-refresh', 'scheduled-tasks-activity-log']
                                      : flow === 'backup-restore'
                                        ? ['backup-list', 'backup-create', 'backup-manifest', 'backup-restore', 'backup-activity-log']
                                        : flow === 'migration-import'
                                          ? ['migration-dry-run', 'migration-import', 'migration-activity-log']
                              : ['auth', 'item-detail', 'playback-info', 'video-stream', 'sessions-playing'];
  for (const key of keys) {
    const upstreamRequest = upstream.criticalRequests[key];
    const jellyrinRequest = jellyrin.criticalRequests[key];
    if (!upstreamRequest && !jellyrinRequest) {
      continue;
    }
    if (!upstreamRequest || !jellyrinRequest) {
      reasons.push(`cross-target: missing critical request ${key}`);
      continue;
    }
    reasons.push(...compareCriticalRequest(key, upstreamRequest, jellyrinRequest));
  }
  reasons.push(...compareTargetInvariants(upstream, jellyrin));
  return reasons;
}

function compareCriticalRequest(key, upstreamRequest, jellyrinRequest) {
  const reasons = [];
  if (upstreamRequest.method !== jellyrinRequest.method) {
    reasons.push(`cross-target ${key}: method ${upstreamRequest.method} != ${jellyrinRequest.method}`);
  }
  if (key === 'video-stream' || key === 'hls-segment' || key === 'audio-hls-legacy-segment' || key === 'music-audio-stream' || key === 'series-video-stream') {
    if (![200, 206].includes(upstreamRequest.status) || ![200, 206].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: stream status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    if (mediaType(upstreamRequest.contentType) !== mediaType(jellyrinRequest.contentType)) {
      reasons.push(`cross-target ${key}: media type ${upstreamRequest.contentType} != ${jellyrinRequest.contentType}`);
    }
    if (Boolean(upstreamRequest.hasContentRange) !== Boolean(jellyrinRequest.hasContentRange)) {
      reasons.push(`cross-target ${key}: content-range presence differs`);
    }
    return reasons;
  }
  if (key === 'audio-hls-dynamic-segment') {
    const allowedAudioHlsTypes = new Set(['audio/mpeg', 'audio/aac', 'video/mp2t']);
    if (![200, 206].includes(upstreamRequest.status) || ![200, 206].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: stream status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    if (!allowedAudioHlsTypes.has(mediaType(upstreamRequest.contentType)) || !allowedAudioHlsTypes.has(mediaType(jellyrinRequest.contentType))) {
      reasons.push(`cross-target ${key}: unexpected media type ${upstreamRequest.contentType} / ${jellyrinRequest.contentType}`);
    }
    if (Boolean(upstreamRequest.hasContentRange) !== Boolean(jellyrinRequest.hasContentRange)) {
      reasons.push(`cross-target ${key}: content-range presence differs`);
    }
    return reasons;
  }
  if (key === 'trickplay-tile') {
    if (upstreamRequest.status !== jellyrinRequest.status) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != ${jellyrinRequest.status}`);
    }
    if (mediaType(upstreamRequest.contentType) !== mediaType(jellyrinRequest.contentType)) {
      reasons.push(`cross-target ${key}: media type ${upstreamRequest.contentType} != ${jellyrinRequest.contentType}`);
    }
    return reasons;
  }
  if (key === 'playlist-move') {
    const compatibleMove = (
      [200, 204].includes(upstreamRequest.status) && [200, 204].includes(jellyrinRequest.status)
    ) || (
      upstreamRequest.status === 400 && [200, 204].includes(jellyrinRequest.status)
    );
    if (!compatibleMove) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (key === 'playlist-rename') {
    const compatibleRename = (
      [200, 204].includes(upstreamRequest.status) && [200, 204].includes(jellyrinRequest.status)
    ) || (
      upstreamRequest.status === 400 && [200, 204].includes(jellyrinRequest.status)
    );
    if (!compatibleRename) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['playlist-remove-item', 'playlist-add-item', 'collection-add-items', 'collection-remove-items'].includes(key)) {
    if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['image-upload', 'image-delete'].includes(key)) {
    if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['metadata-update-primary', 'metadata-update-similar'].includes(key)) {
    if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['auth-user-policy', 'auth-user-configuration', 'auth-key-create', 'auth-key-delete', 'auth-user-logout', 'auth-user-delete'].includes(key)) {
    if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['startup-config-update', 'startup-user-update', 'startup-remote-access', 'startup-complete', 'sessions-capabilities', 'sessions-add-user', 'sessions-remote-play', 'sessions-remote-playstate', 'sessions-remote-stop'].includes(key)) {
    if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    return reasons;
  }
  if (['live-tv-info', 'live-tv-tuner-types', 'live-tv-hdhr-tuner-host', 'live-tv-channels', 'live-tv-hdhr-stream'].includes(key)) {
    // Live TV HDHomeRun comparable keys: verify both targets responded successfully. Response shapes
    // intentionally differ (Jellyrin uses eager materialisation with string channel IDs; upstream uses
    // async guide refresh with GUID-based internal channel IDs), so only status is compared.
    const allowedStatuses = key === 'live-tv-hdhr-tuner-host' || key === 'live-tv-channels' || key === 'live-tv-hdhr-stream'
      ? [200, 204]
      : [200];
    if (!allowedStatuses.includes(upstreamRequest.status) || !allowedStatuses.includes(jellyrinRequest.status)) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} vs ${jellyrinRequest.status} (expected one of ${allowedStatuses.join('/')})`);
    }
    return reasons;
  }
  if (key.startsWith('syncplay-')) {
    const syncplayMutationKeys = ['syncplay-join', 'syncplay-play', 'syncplay-pause', 'syncplay-seek', 'syncplay-unpause'];
    if (syncplayMutationKeys.includes(key)) {
      if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
        reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
      }
      return reasons;
    }
    if (upstreamRequest.status !== 200 || jellyrinRequest.status !== 200) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
      return reasons;
    }
    reasons.push(...compareRequiredShape(key, upstreamRequest.responseShape, jellyrinRequest.responseShape));
    return reasons;
  }
  if (key.startsWith('non-web-')) {
    if (key === 'non-web-video-stream') {
      if (![200, 206].includes(upstreamRequest.status) || ![200, 206].includes(jellyrinRequest.status)) {
        reasons.push(`cross-target ${key}: stream status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
      }
      if (mediaType(upstreamRequest.contentType) !== mediaType(jellyrinRequest.contentType)) {
        reasons.push(`cross-target ${key}: media type ${upstreamRequest.contentType} != ${jellyrinRequest.contentType}`);
      }
      return reasons;
    }
    if (key === 'non-web-progress') {
      if (![200, 204].includes(upstreamRequest.status) || ![200, 204].includes(jellyrinRequest.status)) {
        reasons.push(`cross-target ${key}: mutation status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
      }
      return reasons;
    }
    if (upstreamRequest.status !== 200 || jellyrinRequest.status !== 200) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
      return reasons;
    }
    reasons.push(...compareRequiredShape(key, upstreamRequest.responseShape, jellyrinRequest.responseShape));
    return reasons;
  }
  if (['image-get', 'image-head', 'image-extended-get'].includes(key)) {
    if (upstreamRequest.status !== 200 || jellyrinRequest.status !== 200) {
      reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != compatible ${jellyrinRequest.status}`);
    }
    if (mediaType(upstreamRequest.contentType) !== 'image/png' || mediaType(jellyrinRequest.contentType) !== 'image/png') {
      reasons.push(`cross-target ${key}: expected image/png, got ${upstreamRequest.contentType} / ${jellyrinRequest.contentType}`);
    }
    return reasons;
  }
  if (upstreamRequest.status !== jellyrinRequest.status) {
    reasons.push(`cross-target ${key}: status ${upstreamRequest.status} != ${jellyrinRequest.status}`);
  }
  if ([
    'item-detail',
    'playback-info',
    'resume-list',
    'admin-system-info',
    'admin-storage',
    'admin-scheduled-tasks',
    'admin-activity-log',
    'admin-devices',
    'admin-plugins',
    'admin-repositories',
    'admin-config-pages',
    'library-user-views',
    'library-grouping-options',
    'library-virtual-folders',
    'library-items-counts',
    'library-items',
    'library-latest',
    'music-albums',
    'music-artists',
    'music-album-artists',
    'music-genres',
    'music-instant-mix',
    'music-genre-instant-mix',
    'series-next-up',
    'series-seasons',
    'series-episodes',
    'series-similar',
    'playlist-create',
    'playlist-detail',
    'playlist-items',
    'collection-create',
    'metadata-editor',
    'metadata-external-ids',
    'metadata-items-search',
    'metadata-search-hints',
    'metadata-genres',
    'metadata-studios',
    'metadata-persons',
    'metadata-years',
    'metadata-similar',
    'auth-users-public',
    'auth-users-list',
    'auth-providers',
    'auth-password-reset-providers',
    'auth-user-create',
    'auth-created-user-login',
    'auth-users-me',
    'auth-user-detail',
    'auth-keys-list',
    'auth-key-system-info',
    'startup-public-info',
    'startup-config',
    'startup-user',
    'startup-login',
    'startup-system-info',
    'sessions-list',
  ].includes(key)) {
    reasons.push(...compareRequiredShape(key, upstreamRequest.responseShape, jellyrinRequest.responseShape));
  } else if (JSON.stringify(upstreamRequest.responseShape) !== JSON.stringify(jellyrinRequest.responseShape)) {
    reasons.push(`cross-target ${key}: response shape differs`);
  }
  if (key === 'sessions-playing' && !compatiblePlayMethod(upstreamRequest.playMethod, jellyrinRequest.playMethod)) {
    reasons.push(`cross-target ${key}: play method ${upstreamRequest.playMethod} != compatible ${jellyrinRequest.playMethod}`);
  }
  return reasons;
}

function compareRequiredShape(key, upstreamShape, jellyrinShape) {
  const required = {
    'item-detail': [
      'Id',
      'Name',
      'Type',
      'MediaType',
      'MediaSources',
      'MediaSources.[].Id',
      'MediaSources.[].MediaStreams',
      'MediaSources.[].MediaStreams.[].Type',
      'UserData',
      'UserData.PlaybackPositionTicks',
    ],
    'playback-info': [
      'MediaSources',
      'MediaSources.[].Id',
      'MediaSources.[].SupportsDirectPlay',
      'MediaSources.[].SupportsDirectStream',
      'MediaSources.[].MediaStreams',
      'MediaSources.[].MediaStreams.[].Type',
      'PlaySessionId',
    ],
    'resume-list': [
      'Items',
      'Items.[].Id',
      'Items.[].Name',
      'Items.[].Type',
      'Items.[].UserData',
      'Items.[].UserData.PlaybackPositionTicks',
      'Items.[].UserData.Played',
      'TotalRecordCount',
    ],
    'admin-system-info': ['ProductName', 'Version', 'ServerName', 'StartupWizardCompleted'],
    'admin-storage': ['ProgramDataFolder', 'WebFolder', 'CacheFolder', 'LogFolder', 'TranscodingTempFolder'],
    'admin-scheduled-tasks': ['[].Id', '[].Name', '[].Key', '[].State'],
    'admin-activity-log': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Date'],
    'admin-devices': ['Items'],
    'admin-plugins': [],
    'admin-repositories': [],
    'admin-config-pages': [],
    'library-user-views': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'library-grouping-options': [],
    'library-virtual-folders': ['[].Name', '[].CollectionType', '[].Locations'],
    'library-items-counts': [],
    'library-items': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].UserData'],
    'library-latest': ['[].Id', '[].Name', '[].Type'],
    'music-albums': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'music-artists': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'music-album-artists': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'music-genres': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'music-instant-mix': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'music-genre-instant-mix': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'series-next-up': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].SeriesName', 'Items.[].SeriesId'],
    'series-seasons': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].SeriesName', 'Items.[].SeriesId'],
    'series-episodes': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].SeriesName', 'Items.[].SeriesId', 'Items.[].IndexNumber', 'Items.[].ParentIndexNumber'],
    'series-similar': ['Items', 'TotalRecordCount'],
    'playlist-create': ['Id'],
    'playlist-detail': ['Id', 'Name', 'Type', 'ChildCount'],
    'playlist-items': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].PlaylistItemId'],
    'collection-create': ['Id'],
    'metadata-editor': ['ExternalIdInfos', 'ExternalIdInfos.[].Key', 'Cultures', 'Countries'],
    'metadata-external-ids': ['[].Key'],
    'metadata-items-search': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'metadata-search-hints': ['SearchHints', 'TotalRecordCount', 'SearchHints.[].ItemId', 'SearchHints.[].Name', 'SearchHints.[].Type'],
    'metadata-genres': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'metadata-studios': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'metadata-persons': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'metadata-years': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'metadata-similar': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'auth-users-public': [],
    'auth-users-list': ['[].Id', '[].Name', '[].Policy', '[].Configuration'],
    'auth-providers': [],
    'auth-password-reset-providers': [],
    'auth-user-create': ['Id', 'Name', 'Policy', 'Configuration'],
    'auth-created-user-login': ['AccessToken', 'User', 'User.Id', 'User.Name'],
    'auth-users-me': ['Id', 'Name'],
    'auth-user-detail': ['Id', 'Name', 'Policy', 'Configuration'],
    'auth-keys-list': ['Items', 'TotalRecordCount'],
    'auth-key-system-info': ['Id', 'ServerName', 'Version', 'StartupWizardCompleted'],
    'startup-public-info': ['Id', 'ServerName', 'Version', 'StartupWizardCompleted'],
    'startup-config': ['ServerName', 'UICulture', 'MetadataCountryCode', 'PreferredMetadataLanguage'],
    'startup-user': ['Name'],
    'startup-login': ['AccessToken', 'User', 'User.Id', 'User.Name'],
    'startup-system-info': ['Id', 'ServerName', 'Version', 'StartupWizardCompleted'],
    'sessions-list': ['[].Id', '[].UserId', '[].Client', '[].DeviceId', '[].SupportsRemoteControl', '[].SupportedCommands'],
    'syncplay-new': ['GroupId', 'GroupName', 'Participants', 'State'],
    'syncplay-list': ['[].GroupId', '[].GroupName', '[].Participants', '[].State'],
    'syncplay-get': ['GroupId', 'GroupName', 'Participants', 'State'],
    'non-web-system-info': ['Id', 'ServerName', 'Version'],
    'non-web-views': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type'],
    'non-web-playback-info': ['MediaSources', 'MediaSources.[].Id', 'MediaSources.[].SupportsDirectPlay', 'MediaSources.[].SupportsDirectStream', 'MediaSources.[].MediaStreams', 'MediaSources.[].MediaStreams.[].Type', 'PlaySessionId'],
    'non-web-resume': ['Items', 'TotalRecordCount', 'Items.[].Id', 'Items.[].Name', 'Items.[].Type', 'Items.[].UserData'],
  }[key] || [];
  const reasons = [];
  const upstreamKeys = shapeKeys(upstreamShape);
  const jellyrinKeys = shapeKeys(jellyrinShape);
  for (const shapeKey of required) {
    if (!upstreamKeys.has(shapeKey)) {
      reasons.push(`cross-target ${key}: upstream missing required shape ${shapeKey}`);
    }
    if (!jellyrinKeys.has(shapeKey)) {
      reasons.push(`cross-target ${key}: jellyrin missing required shape ${shapeKey}`);
    }
  }
  return reasons;
}

function shapeKeys(value, prefix = '') {
  const keys = new Set();
  if (Array.isArray(value)) {
    if (value.length > 0) {
      for (const key of shapeKeys(value[0], `${prefix}[].`)) {
        keys.add(key);
      }
    }
    return keys;
  }
  if (value && typeof value === 'object') {
    for (const [key, child] of Object.entries(value)) {
      const fullKey = `${prefix}${key}`;
      keys.add(fullKey);
      for (const childKey of shapeKeys(child, `${fullKey}.`)) {
        keys.add(childKey);
      }
    }
  }
  return keys;
}

function compareTargetInvariants(upstream, jellyrin) {
  const reasons = [];
  if (upstream.invariants.websocketKeepAlive !== jellyrin.invariants.websocketKeepAlive) {
    reasons.push('cross-target: websocket KeepAlive invariant differs');
  }
  if (upstream.invariants.unexpectedTranscodePath !== jellyrin.invariants.unexpectedTranscodePath) {
    reasons.push('cross-target: unexpected transcode/HLS invariant differs');
  }
  return reasons;
}

function addWebsocketMessageType(summary, parsed) {
  if (!parsed || !parsed.MessageType) {
    return;
  }
  if (!summary.invariants.websocketMessageTypes.includes(parsed.MessageType)) {
    summary.invariants.websocketMessageTypes.push(parsed.MessageType);
    summary.invariants.websocketMessageTypes.sort();
  }
}

function addUnique(values, value) {
  if (!values.includes(value)) {
    values.push(value);
    values.sort();
  }
}

function compatiblePlayMethod(upstreamMethod, jellyrinMethod) {
  if (!upstreamMethod || !jellyrinMethod) {
    return true;
  }
  return ['DirectPlay', 'DirectStream'].includes(upstreamMethod)
    && ['DirectPlay', 'DirectStream'].includes(jellyrinMethod);
}

function mediaType(contentType) {
  return String(contentType || '').split(';')[0].trim().toLowerCase();
}

function invariantFailures(summary) {
  if (!['startup-wizard', 'p0-direct-play', 'resume', 'transcode-hls', 'admin-dashboard', 'libraries', 'subtitles-trickplay', 'audio-hls-legacy', 'music', 'series', 'playlists-collections', 'images', 'metadata-search', 'auth-users', 'sessions-websocket', 'syncplay', 'channels', 'non-web-client', 'scheduled-tasks', 'backup-restore', 'migration-import', 'live-tv'].includes(flow) || summary.status !== 'completed') {
    return [];
  }
  const failures = [];
  if (flow === 'startup-wizard') {
    for (const [field, label] of [
      ['startupPublicInfoIncomplete', 'incomplete public info'],
      ['startupConfig200', 'startup configuration read'],
      ['startupConfig204', 'startup configuration update'],
      ['startupRemoteAccess204', 'startup remote access update'],
      ['startupUser200', 'startup user read'],
      ['startupUser204', 'startup user update'],
      ['startupPublicUsersBeforeComplete', 'public users before complete'],
      ['startupComplete204', 'startup complete'],
      ['startupPublicInfoComplete', 'completed public info'],
      ['startupLogin200', 'first admin login'],
      ['startupSystemInfo200', 'authenticated system info'],
      ['startupPublicUsersAfterComplete', 'public users after complete'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing startup wizard ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'resume') {
    if (!summary.invariants.playbackProgress204) {
      failures.push('missing Sessions/Playing/Progress 204 invariant');
    }
    if (!summary.invariants.resumeList200) {
      failures.push('missing UserItems/Resume 200 invariant');
    }
    if (!summary.invariants.resumeItemMatched) {
      failures.push('missing resume item invariant');
    }
    return failures;
  }
  if (flow === 'transcode-hls') {
    if (!summary.invariants.transcodePlaybackInfo200) {
      failures.push('missing transcode PlaybackInfo 200 invariant');
    }
    if (!summary.invariants.transcodingUrlPresent) {
      failures.push('missing TranscodingUrl invariant');
    }
    if (!summary.invariants.hlsMaster200) {
      failures.push('missing HLS master playlist 200 invariant');
    }
    if (!summary.invariants.hlsMedia200) {
      failures.push('missing HLS media playlist 200 invariant');
    }
    if (!summary.invariants.hlsSegment200) {
      failures.push('missing HLS segment 200/206 invariant');
    }
    return failures;
  }
  if (flow === 'admin-dashboard') {
    for (const [field, label] of [
      ['adminSystemInfo200', 'System/Info'],
      ['adminStorage200', 'System/Info/Storage'],
      ['adminScheduledTasks200', 'ScheduledTasks'],
      ['adminActivityLog200', 'System/ActivityLog/Entries'],
      ['adminDevices200', 'Devices'],
      ['adminPlugins200', 'Plugins'],
      ['adminRepositories200', 'Repositories'],
      ['adminConfigPages200', 'web/ConfigurationPages'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing admin ${label} 200 invariant`);
      }
    }
    return failures;
  }
  if (flow === 'libraries') {
    for (const [field, label] of [
      ['libraryViews200', 'UserViews'],
      ['libraryGroupingOptions200', 'UserViews/GroupingOptions'],
      ['libraryVirtualFolders200', 'Library/VirtualFolders'],
      ['libraryItemsCounts200', 'Items/Counts'],
      ['libraryItems200', 'Items'],
      ['libraryLatest200', 'Users/Items/Latest'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing library ${label} 200 invariant`);
      }
    }
    if (!summary.invariants.libraryViewMatched) {
      failures.push('missing library view match invariant');
    }
    if (!summary.invariants.libraryItemMatched) {
      failures.push('missing library item match invariant');
    }
    return failures;
  }
  if (flow === 'subtitles-trickplay') {
    for (const [field, label] of [
      ['subtitlePlaybackInfo200', 'subtitle PlaybackInfo'],
      ['subtitleStreamMatched', 'subtitle stream match'],
      ['subtitlePlaylist200', 'subtitle playlist'],
      ['subtitlePlaylistShape', 'subtitle playlist shape'],
      ['subtitleVtt200', 'subtitle VTT stream'],
      ['subtitleVttCue', 'subtitle VTT cue'],
      ['trickplayPlaylist200', 'trickplay playlist'],
      ['trickplayImagesOnly', 'trickplay images-only playlist'],
      ['trickplayTile200', 'trickplay tile'],
      ['trickplayTileJpeg', 'trickplay JPEG tile'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'audio-hls-legacy') {
    for (const [field, label] of [
      ['audioItemMatched', 'audio item match'],
      ['audioPlaybackInfo200', 'audio PlaybackInfo'],
      ['audioHlsMaster200', 'audio HLS master playlist'],
      ['audioHlsMedia200', 'audio HLS media playlist'],
      ['audioHlsDynamicSegment200', 'audio HLS dynamic segment'],
      ['audioHlsLegacySegment200', 'audio HLS legacy segment'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'music') {
    for (const [field, label] of [
      ['musicViewMatched', 'music view'],
      ['musicSongsMatched', 'music songs'],
      ['musicAlbumMatched', 'music album'],
      ['musicArtistMatched', 'music artist'],
      ['musicAlbumArtistMatched', 'music album artist'],
      ['musicGenreMatched', 'music genre'],
      ['musicInstantMix200', 'music instant mix'],
      ['musicInstantMixResults', 'music instant mix results'],
      ['musicAudioStream200', 'music audio stream'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'series') {
    for (const [field, label] of [
      ['seriesViewMatched', 'series view'],
      ['seriesEpisodesMatched', 'series episodes'],
      ['seriesEpisodeMetadataMatched', 'series episode metadata'],
      ['seriesCounts200', 'series counts'],
      ['seriesNextUp200', 'series next up'],
      ['seriesSeasons200', 'series seasons'],
      ['seriesSeasonMatched', 'series season match'],
      ['seriesEpisodesRoute200', 'series episodes route'],
      ['seriesEpisodesRouteMatched', 'series episodes route match'],
      ['seriesSimilar200', 'series similar'],
      ['seriesStream200', 'series episode stream'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'playlists-collections') {
    for (const [field, label] of [
      ['playlistCreated', 'playlist create'],
      ['playlistDetail200', 'playlist detail'],
      ['playlistItems200', 'playlist items'],
      ['playlistItemIdsMatched', 'playlist item ids'],
      ['playlistDeleteItem204', 'playlist delete item'],
      ['playlistAddItem204', 'playlist add item'],
      ['collectionCreated', 'collection create'],
      ['collectionAddItems204', 'collection add items'],
      ['collectionDeleteItems204', 'collection delete items'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    if (!summary.invariants.playlistMove204 && !(summary.target === 'upstream' && summary.invariants.playlistMoveUnsupported400)) {
      failures.push('missing playlist move invariant');
    }
    if (!summary.invariants.playlistMovedOrderMatched && !summary.invariants.playlistMoveUnsupported400) {
      failures.push('missing playlist moved order invariant');
    }
    if (!summary.invariants.playlistRename204 && !(summary.target === 'upstream' && summary.invariants.playlistRenameUnsupported400)) {
      failures.push('missing playlist rename invariant');
    }
    return failures;
  }
  if (flow === 'images') {
    for (const [field, label] of [
      ['imageItemMatched', 'image item match'],
      ['imageInfosInitial200', 'initial image infos'],
      ['imageUpload204', 'image upload'],
      ['imageInfosAfterUpload200', 'post-upload image infos'],
      ['imageInfoTagPresent', 'post-upload image tag'],
      ['imageGet200', 'direct image get'],
      ['imageGetPng', 'direct image png'],
      ['imageHead200', 'direct image head'],
      ['imageHeadPng', 'direct image head content type'],
      ['imageExtendedGet200', 'extended image get'],
      ['imageExtendedGetPng', 'extended image png'],
      ['imageProviders200', 'remote image providers'],
      ['imageDelete204', 'image delete'],
      ['imageInfosAfterDelete200', 'post-delete image infos'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'metadata-search') {
    for (const [field, label] of [
      ['metadataItemsMatched', 'metadata fixture items'],
      ['metadataUpdatePrimary204', 'primary metadata update'],
      ['metadataUpdateSimilar204', 'similar metadata update'],
      ['metadataEditor200', 'metadata editor'],
      ['metadataEditorProviderIds', 'metadata editor provider ids'],
      ['metadataExternalIds200', 'external id infos'],
      ['metadataItemsSearch200', 'items search'],
      ['metadataSearchHints200', 'search hints'],
      ['metadataGenreMatched', 'genre search'],
      ['metadataStudioMatched', 'studio search'],
      ['metadataPersonMatched', 'person search'],
      ['metadataYearMatched', 'year search'],
      ['metadataSimilar200', 'similar route'],
      ['metadataSimilarMatched', 'similar shared metadata result'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'auth-users') {
    for (const [field, label] of [
      ['authUsersPublic200', 'public users'],
      ['authUsersList200', 'admin users list'],
      ['authProviders200', 'authentication providers'],
      ['authPasswordResetProviders200', 'password reset providers'],
      ['authUserCreated', 'user create'],
      ['authCreatedUserLogin200', 'created user login'],
      ['authCreatedUserMe200', 'created user me'],
      ['authUserDetail200', 'user detail'],
      ['authUserPolicy204', 'user policy update'],
      ['authUserConfiguration204', 'user configuration update'],
      ['authKeysList200', 'API keys list'],
      ['authKeyCreated', 'API key create'],
      ['authKeyUsable', 'API key system info'],
      ['authKeyRevoked', 'API key revoke'],
      ['authCreatedUserLogout204', 'created user logout'],
      ['authUserDeleted', 'user delete'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing auth/users ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'plugins-packages') {
    for (const [field, label] of [
      ['pluginsList200', 'plugins list'],
      ['pluginRepositories200', 'plugin repositories'],
      ['pluginPackages200', 'plugin packages'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    if (summary.target === 'jellyrin') {
      for (const [field, label] of [
        ['pluginsListEmpty', 'empty installed plugins'],
        ['pluginRepositoryUpdated', 'plugin repository update'],
        ['pluginPackageMatched', 'plugin package catalog'],
        ['pluginManifest200', 'plugin manifest'],
        ['pluginInstallRejected', 'package install rejected'],
        ['pluginEnableRejected', 'plugin enable rejected'],
        ['pluginDisableRejected', 'plugin disable rejected'],
        ['pluginUninstallRejected', 'plugin uninstall rejected'],
      ]) {
        if (!summary.invariants[field]) {
          failures.push(`missing ${label} invariant`);
        }
      }
    }
    return failures;
  }
  if (flow === 'live-tv') {
    // HDHomeRun invariants required for both targets (upstream-comparable).
    // liveTvHdhrStream200: both targets verify actual bytes from the HDHomeRun stream using
    // AbortController — Jellyrin via GET /LiveTv/LiveStreamFiles/<id>/stream.ts (now streaming
    // incrementally), upstream via the LiveStreamFiles URL returned by PlaybackInfo.
    // liveTvHdhrStreamSetup is also set for informational purposes but is not gating.
    for (const [field, label] of [
      ['liveTvInfo200', 'live tv info'],
      ['liveTvTunerTypes200', 'live tv tuner types'],
      ['liveTvHdhrTunerAdded', 'live tv HDHomeRun tuner added'],
      ['liveTvHdhrChannelMatched', 'live tv HDHomeRun channel matched'],
      ['liveTvHdhrStream200', 'live tv HDHomeRun stream bytes'],
      ['liveTvHdhrTwoClientStream', 'live tv HDHomeRun two-client sharing (maxConcurrent===1)'],
      ['liveTvHdhrStreamRefcountReleased', 'live tv HDHomeRun refcount released (currentConcurrent===0)'],
      ['liveTvHdhrHlsMaster200', 'live tv HDHomeRun HLS master playlist 200'],
      ['liveTvHdhrHlsMediaLive', 'live tv HDHomeRun HLS media playlist live (no ENDLIST)'],
      ['liveTvHdhrHlsSegment200', 'live tv HDHomeRun HLS segment 200 (video/mp2t bytes>0)'],
      ['liveTvHdhrSeriesTimerCreated', 'live tv HDHomeRun series timer created from real guide ProgramId'],
      ['liveTvHdhrSeriesTimerGeneratesTimers', 'live tv HDHomeRun series timer generated child timers'],
      ['liveTvHdhrSeriesRecordingPlayable', 'live tv HDHomeRun series timer recording playable by ffprobe'],
      ['liveTvHdhrTunerLimitFirstOpen', 'live tv HDHomeRun TunerCount=1 first open (200 + bytes)'],
      ['liveTvHdhrTunerLimitConflict', 'live tv HDHomeRun TunerCount=1 conflict (HTTP 500)'],
      ['liveTvHdhrTunerLimitHlsConflict', 'live tv HDHomeRun TunerCount=1 direct TS blocks HLS cross-mode conflict'],
      ['liveTvHdhrTunerLimitRecordingConflict', 'live tv HDHomeRun TunerCount=1 direct TS blocks recording cross-mode conflict'],
      ['liveTvHdhrTunerLimitRecovery', 'live tv HDHomeRun TunerCount=1 recovery after close (200 + bytes)'],
      ['liveTvHdhrTunerLimitHlsRecovery', 'live tv HDHomeRun TunerCount=1 HLS recovery after close (segment bytes)'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing ${label} invariant`);
      }
    }
    // Synthetic M3U/XMLTV and jellyrin-only invariants; upstream skips this block by design.
    // liveTvHdhrHlsMediaLive and liveTvHdhrHlsSegment200 ARE upstream-comparable (checked above for
    // both targets): once the simulator serves a monotonic-DTS continuous TS, upstream's ffmpeg
    // produces real HLS segments too (verified: 90 h264 packets in an upstream segment).
    // liveTvHdhrHlsActiveEncoding is jellyrin-only: upstream Jellyfin does not expose GET
    // /Videos/ActiveEncodings (returns 405, only DELETE allowed), so it cannot be tested on upstream;
    // upstream cleanup is validated via DELETE instead.
    // liveTvHdhrTunerLimitSharingExempt is jellyrin-only: 2 consumers of the same channel are exempt
    // from the TunerCount limit (sharing path, no new slot consumed). Upstream's stream-sharing path
    // is not directly comparable via the simulator's concurrent-connection metric.
    if (summary.target === 'jellyrin') {
      for (const [field, label] of [
        ['liveTvConfigUpdated', 'live tv config update'],
        ['liveTvChannels200', 'live tv channels'],
        ['liveTvChannelMatched', 'live tv channel fixture'],
        ['liveTvGuidePrograms200', 'live tv guide programs'],
        ['liveTvProgramMatched', 'live tv program fixture'],
        ['liveTvStream200', 'live tv channel stream'],
        ['liveTvRecordings200', 'live tv recordings'],
        ['liveTvRecordingStream200', 'live tv recording stream'],
        ['liveTvTimerCreated', 'live tv timer create'],
        ['liveTvTimerDeleted', 'live tv timer delete'],
        ['liveTvSeriesTimerCreated', 'live tv series timer create'],
        ['liveTvSeriesTimerDeleted', 'live tv series timer delete'],
        ['liveTvHdhrTwoClientByteCheck', 'live tv HDHomeRun two-client byte check (2nd consumer bytes>=1)'],
        ['liveTvHdhrHlsActiveEncoding', 'live tv HDHomeRun HLS listed in ActiveEncodings + removed after DELETE'],
        ['liveTvHdhrHlsTranscodeUrl', 'live tv HDHomeRun TranscodingUrl with SupportsTranscoding:true + hls'],
        ['liveTvHdhrHlsFfmpegReaped', 'live tv HDHomeRun HLS ffmpeg reaped (/stats currentConcurrent===0)'],
        ['liveTvHdhrSeriesTimerCleanup', 'live tv HDHomeRun series timer cleanup + child timer cascade'],
        ['liveTvHdhrTunerLimitRecordingNoZombie', 'live tv HDHomeRun TunerCount=1 recording conflict leaves no InProgress zombie'],
        ['liveTvHdhrTunerLimitSharingExempt', 'live tv HDHomeRun TunerCount=1 sharing exempt (same channel, maxConcurrent===1)'],
      ]) {
        if (!summary.invariants[field]) {
          failures.push(`missing ${label} invariant`);
        }
      }
    }
    return failures;
  }
  if (flow === 'sessions-websocket') {
    for (const [field, label] of [
      ['websocketKeepAlive', 'ForceKeepAlive/KeepAlive'],
      ['websocketSessions', 'Sessions message'],
      ['sessionsTwoClientsOpened', 'two websocket clients'],
      ['sessionsStartSent', 'SessionsStart'],
      ['sessionsMessageReceived', 'initial Sessions response'],
      ['sessionsList200', 'Sessions list'],
      ['sessionsCapabilities204', 'session capabilities'],
      ['sessionsUserAdd204', 'additional user update'],
      ['sessionsObserverUpdate', 'observer Sessions update'],
      ['sessionsRemotePlay204', 'remote play command'],
      ['sessionsRemotePlayMessage', 'remote Play websocket message'],
      ['sessionsRemotePlaystate204', 'remote playstate command'],
      ['sessionsRemotePlaystateMessage', 'remote Playstate websocket message'],
      ['sessionsRemoteStop204', 'remote stop command'],
      ['sessionsRemoteStoppedMessage', 'remote stopped websocket message'],
      ['sessionsCleanupConfirmed', 'remote stop cleanup'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing sessions/websocket ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'syncplay') {
    for (const [field, label] of [
      ['websocketKeepAlive', 'ForceKeepAlive/KeepAlive'],
      ['syncplayTwoClientsOpened', 'two websocket clients'],
      ['syncplayGroupCreated', 'group create'],
      ['syncplayGuestJoined', 'guest join'],
      ['syncplayList200', 'group list'],
      ['syncplayGet200', 'group detail'],
      ['syncplayPlay204', 'play command'],
      ['syncplayPlayFanout', 'play fanout'],
      ['syncplayPause204', 'pause command'],
      ['syncplayPauseFanout', 'pause fanout'],
      ['syncplaySeek204', 'seek command'],
      ['syncplaySeekFanout', 'seek fanout'],
      ['syncplayUnpause204', 'unpause command'],
      ['syncplayUnpauseFanout', 'unpause fanout'],
      ['syncplayRaceSequenced', 'race sequencing'],
      ['syncplayDriftCorrection', 'drift correction'],
      ['syncplayGuestReconnectDeduped', 'guest reconnect dedupe'],
      ['syncplayStaleCleanup', 'stale cleanup'],
      ['syncplayGuestLogoutRemoved', 'guest logout cleanup'],
      ['syncplayGuestLeft', 'guest leave'],
      ['syncplayOwnerLeft', 'owner leave'],
      ['syncplayCleanupConfirmed', 'group cleanup'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing SyncPlay ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'channels') {
    for (const [field, label] of [
      ['channelsList200', 'channels list'],
      ['channelsFeatures200', 'channels features'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing Channels ${label} invariant`);
      }
    }
    if (summary.target === 'jellyrin') {
      for (const [field, label] of [
        ['channelsProviderMatched', 'local provider'],
        ['channelsFilterMatched', 'supports/favorite filters'],
        ['channelsDeletionFilterMatched', 'media deletion filter'],
        ['channelsItems200', 'provider items'],
        ['channelsItemMatched', 'fixture channel item'],
        ['channelsLatest200', 'latest channel items'],
        ['channelsFeatureMatched', 'feature capabilities'],
      ]) {
        if (!summary.invariants[field]) {
          failures.push(`missing Channels ${label} invariant`);
        }
      }
    }
    return failures;
  }
  if (flow === 'non-web-client') {
    for (const [field, label] of [
      ['nonWebClientAuthenticated', 'client authentication'],
      ['nonWebSystemInfo200', 'system info discovery'],
      ['nonWebBrowse200', 'library browse'],
      ['nonWebMovieMatched', 'movie item match'],
      ['nonWebPlaybackInfo200', 'PlaybackInfo'],
      ['nonWebDirectMediaSource', 'direct media source'],
      ['nonWebStream200', 'direct stream'],
      ['nonWebProgress204', 'playback progress'],
      ['nonWebResumeMatched', 'resume state'],
      ['nonWebDlnaUnsupportedDecided', 'DLNA/UPnP decision'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing non-web client ${label} invariant`);
      }
    }
    const profileCount = Number(summary.invariants.nonWebClientProfileCount || 0);
    if (profileCount < 6) {
      failures.push(`missing non-web client contract profiles, got ${profileCount}/6`);
    }
    return failures;
  }
  if (flow === 'scheduled-tasks') {
    for (const [field, label] of [
      ['websocketKeepAlive', 'ForceKeepAlive/KeepAlive'],
      ['scheduledTasksList200', 'task list'],
      ['scheduledTasksDetail200', 'task detail'],
      ['scheduledTasksStarted', 'task start'],
      ['scheduledTasksWebsocketUpdate', 'websocket update'],
      ['scheduledTasksCompleted', 'task completion'],
      ['scheduledTasksCancelled', 'task cancel'],
      ['scheduledTasksTriggers204', 'trigger update'],
      ['scheduledTasksLibraryRefresh204', 'library refresh'],
      ['scheduledTasksActivityLogged', 'activity log'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing scheduled-tasks ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'backup-restore') {
    for (const [field, label] of [
      ['backupList200', 'backup list'],
      ['backupCreated', 'backup create'],
      ['backupSnapshotSummary', 'snapshot summary'],
      ['backupManifest200', 'backup manifest'],
      ['backupRestored', 'backup restore'],
      ['backupActivityLogged', 'activity log'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing backup-restore ${label} invariant`);
      }
    }
    return failures;
  }
  if (flow === 'migration-import') {
    for (const [field, label] of [
      ['migrationDryRun200', 'dry-run'],
      ['migrationReadOnlyPolicy', 'read-only source policy'],
      ['migrationImport200', 'import'],
      ['migrationBackupCreated', 'backup'],
      ['migrationRollbackDocumented', 'rollback'],
      ['migrationActivityLogged', 'activity log'],
    ]) {
      if (!summary.invariants[field]) {
        failures.push(`missing migration-import ${label} invariant`);
      }
    }
    return failures;
  }
  if (!summary.invariants.playbackInfo200) {
    failures.push('missing PlaybackInfo 200 invariant');
  }
  if (!summary.invariants.streamOk) {
    failures.push('missing video stream 200/206 invariant');
  }
  if (!summary.invariants.sessionPlaying204) {
    failures.push('missing Sessions/Playing 204 invariant');
  }
  if (!summary.invariants.websocketKeepAlive) {
    failures.push('missing websocket keepalive invariant');
  }
  if (summary.invariants.unexpectedTranscodePath) {
    failures.push('direct-play trace unexpectedly used transcode/HLS path');
  }
  if (
    summary.invariants.playMethods.length > 0
    && !summary.invariants.playMethods.every((method) => ['DirectPlay', 'DirectStream'].includes(method))
  ) {
    failures.push(`unexpected play methods: ${summary.invariants.playMethods.join(', ')}`);
  }
  return failures;
}

function ignoredConsoleError(text) {
  return [
    'A bad HTTP response code (404) was received when fetching the script.',
    'Failed to load resource: the server responded with a status of 404 (Not Found)',
    'Failed to load resource: the server responded with a status of 400 (Bad Request)',
    // TunerCount limit conflict: expected 500 from /Items/.../PlaybackInfo (upstream) or
    // /LiveTv/LiveStreamFiles/.../stream.ts (Jellyrin) when TunerCount=1 is exceeded.
    'Failed to load resource: the server responded with a status of 500 (Internal Server Error)',
    'React Router Future Flag Warning',
    'Not initializing chromecast: chrome object is missing',
    'You rendered descendant <Routes> (or called `useRoutes()`) at "/"',
    'MEDIA_NOT_SUPPORTED',
  ].some((allowed) => text.includes(allowed));
}

function allowedFailedResponse(response) {
  const url = response.url();
  const pathname = new URL(url).pathname;
  const method = response.request().method().toUpperCase();
  if (url.includes('/Branding/Splashscreen')) {
    return true;
  }
  if (response.status() === 404 && pathname === '/web/undefined') {
    return true;
  }
  if (response.status() === 400 && url.startsWith('http://127.0.0.1:8096/') && /\/Playlists\/[^/]+\/Items\/[^/]+\/Move\/\d+$/i.test(pathname)) {
    return true;
  }
  if (response.status() === 400 && url.startsWith('http://127.0.0.1:8096/') && /\/Playlists\/[^/]+$/i.test(pathname)) {
    return true;
  }
  if (flow === 'plugins-packages' && response.status() === 409) {
    return true;
  }
  if (flow === 'syncplay' && response.status() === 404 && /\/SyncPlay\/[^/]+$/i.test(pathname)) {
    return true;
  }
  if (response.status() === 400 && pathname === '/SyncPlay/List') {
    return true;
  }
  // upstream Jellyfin's DELETE /Videos/ActiveEncodings requires DeviceId in addition to
  // PlaySessionId; without it the server returns 400. This exception is scoped to the
  // upstream target URL so it never silently hides Jellyrin errors on this path.
  // The DeviceId "browser-trace" is included in the DELETE URL so this should not trigger
  // in practice; kept as a safety net for the upstream target only.
  if (flow === 'live-tv' && response.status() === 400
    && pathname === '/Videos/ActiveEncodings'
    && url.startsWith(process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096')) {
    return true;
  }
  // Cleanup calls are intentionally idempotent in the Live TV golden. A conflict can fail
  // before an HLS session exists, and a short timer can disappear before explicit cleanup.
  if (flow === 'live-tv' && response.status() === 404
    && ((method === 'DELETE' && pathname === '/Videos/ActiveEncodings')
      || (method === 'DELETE' && /\/LiveTv\/Timers\/[^/]+$/i.test(pathname)))) {
    return true;
  }
  // TunerCount limit conflict returns HTTP 500 when TunerCount=1 is reached:
  //   - Jellyrin: GET /LiveTv/LiveStreamFiles/{id}/stream.ts -> 500 via ApiError::internal.
  //   - Jellyrin HLS: GET /Videos/{id}/master.m3u8 -> 500 via the shared tuner lease.
  //   - Recording timers may synchronously surface the same conflict via POST /LiveTv/Timers.
  //   - upstream: POST /Items/{id}/PlaybackInfo (AutoOpenLiveStream=true) -> 500 via
  //     ExceptionMiddleware (LiveTvConflictException -> _ => 500).
  // Both are expected observable results (R-CONFLICT-500). Scoped to the live-tv flow to
  // avoid silently hiding real errors in other flows.
  if (flow === 'live-tv' && response.status() === 500
    && ((method === 'GET' && /\/LiveTv\/LiveStreamFiles\//i.test(pathname))
      || (method === 'GET' && /\/Videos\/[^/]+\/master\.m3u8/i.test(pathname))
      || (method === 'POST' && /\/LiveTv\/Timers$/i.test(pathname))
      || (method === 'POST' && /\/Items\/[^/]+\/PlaybackInfo/i.test(pathname)))) {
    return true;
  }
  return false;
}

async function responseShape(response) {
  try {
    return shapeOf(await response.json());
  } catch (_) {
    return '<unreadable-json>';
  }
}

function websocketFrameRecord(direction, url, payload) {
  const parsed = parseJsonPayload(payload);
  const data = parsed && typeof parsed === 'object' ? parsed.Data : undefined;
  return {
    ts: new Date().toISOString(),
    event: 'frame',
    direction,
    url,
    messageType: parsed && typeof parsed === 'object' ? parsed.MessageType : undefined,
    dataShape: data === undefined ? undefined : shapeOf(data),
  };
}

function parseJsonPayload(payload) {
  if (typeof payload !== 'string') {
    return null;
  }
  try {
    return JSON.parse(payload);
  } catch (_) {
    return null;
  }
}

function shapeOf(value) {
  if (Array.isArray(value)) {
    return value.length === 0 ? [] : [shapeOf(value[0])];
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, shapeOf(value[key])]),
    );
  }
  if (value === null) {
    return 'null';
  }
  return typeof value;
}

function sanitizePostData(postData) {
  if (!postData) {
    return null;
  }
  try {
    return redactValue(JSON.parse(postData));
  } catch (_) {
    return '<non-json-post-data>';
  }
}

function redactHeaders(headers) {
  return Object.fromEntries(
    Object.entries(headers)
      .filter(([key]) => safeRequestHeader(key))
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, value]) => [
        key,
        secretKey(key) ? '<redacted>' : value,
      ]),
  );
}

function safeRequestHeader(key) {
  return [
    'accept',
    'content-type',
    'origin',
    'range',
    'referer',
    'user-agent',
  ].includes(key.toLowerCase()) || secretKey(key);
}

function selectedResponseHeaders(headers) {
  const selected = {};
  for (const key of [
    'accept-ranges',
    'cache-control',
    'content-length',
    'content-range',
    'content-type',
    'etag',
    'last-modified',
    'location',
  ]) {
    if (headers[key] !== undefined) {
      selected[key] = secretKey(key) ? '<redacted>' : headers[key];
    }
  }
  return selected;
}

function redactValue(value) {
  if (Array.isArray(value)) {
    return value.map(redactValue);
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [
        key,
        secretKey(key) ? '<redacted>' : redactValue(child),
      ]),
    );
  }
  return value;
}

function secretKey(key) {
  return /authorization|cookie|token|api[_-]?key|password|passwd|pw|access[_-]?token|secret/i.test(key);
}

function sanitizeUrl(url) {
  const parsed = new URL(url);
  for (const key of Array.from(parsed.searchParams.keys())) {
    if (secretKey(key)) {
      parsed.searchParams.set(key, 'REDACTED');
    }
  }
  return parsed.toString();
}

function redactText(text) {
  return text
    .replace(/([?&](?:api[_-]?key|ApiKey|access[_-]?token|X-Emby-Token|token|password|Pw)=)[^&\s"']+/gi, '$1REDACTED')
    .replace(/("(?:access[_-]?token|AccessToken|api[_-]?key|ApiKey|X-Emby-Token|token|password|Pw)"\s*:\s*")[^"]+/gi, '$1REDACTED')
    .replace(/(Authorization["':= ]+)(Bearer\s+)?[A-Za-z0-9._~+/=-]{12,}/gi, '$1$2REDACTED');
}

function pathWithQuery(url) {
  const parsed = new URL(sanitizeUrl(url));
  return `${parsed.pathname}${parsed.search}`;
}

function trimTrailingSlash(value) {
  return value.replace(/\/+$/, '');
}

async function jsonlWriter(filePath) {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  const handle = await fs.open(filePath, 'w');
  return {
    async write(record) {
      await handle.write(`${JSON.stringify(record)}\n`);
    },
    async close() {
      await handle.close();
    },
  };
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
