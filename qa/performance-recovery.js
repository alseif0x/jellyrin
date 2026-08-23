#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const api = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/lib.rs'), 'utf8');
  const catalogCache = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/catalog_cache.rs'), 'utf8');
  const auxiliaryFfmpegTelemetry = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/auxiliary_ffmpeg_telemetry.rs'), 'utf8');
  const transcode = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-transcode/src/lib.rs'), 'utf8');
  const dlna = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/dlna.rs'), 'utf8');
  const db = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-db/src/postgres.rs'), 'utf8');
  const dbLib = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-db/src/lib.rs'), 'utf8');
  const postgresCatalog = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-db/src/postgres_catalog.rs'), 'utf8');
  const server = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-server/src/main.rs'), 'utf8');
  const catalogIndex = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-db/migrations-postgres/202608080107_catalog_collection_type_index.sql'), 'utf8');
  const catalogBenchmark = await fs.readFile(path.join(repoRoot, 'qa/postgres-catalog-benchmark.js'), 'utf8');
  const rootCargo = await fs.readFile(path.join(repoRoot, 'Cargo.toml'), 'utf8');
  const infrastructure = await fs.readFile(path.join(repoRoot, 'docker-compose.infrastructure.yml'), 'utf8');
  const redisOverlay = await fs.readFile(path.join(repoRoot, 'docker-compose.redis-cache.yml'), 'utf8');
  const minimumStack = await fs.readFile(path.join(repoRoot, 'docs/minimum-stack.md'), 'utf8');
  const playbackBatchFunctions = [
    'async fn items_to_json',
    'async fn filtered_media_items',
    'async fn media_list_items_to_json',
    'async fn parent_virtual_items',
    'async fn special_user_view_items_result',
    'async fn apply_latest_user_configuration',
    'async fn series_seasons',
    'async fn tv_series_items_result',
    'async fn tv_series_summary_by_id',
    'async fn tv_season_summary_by_id',
  ];

  const checks = [
    check('streaming-reader-stream', api.includes('ReaderStream::new(file)') && api.includes('ReaderStream::new(file.take(content_length))')),
    check('streaming-range-headers', api.includes('ACCEPT_RANGES') && api.includes('CONTENT_RANGE') && api.includes('RANGE_NOT_SATISFIABLE')),
    check('bounded-transcode-dedupe', api.includes('TRANSCODE_DEDUPE_LOCKS') && api.includes('claim_transcode_session') && api.includes('hls_transcode_dedupe_lock_is_shared_per_key')),
    check('startup-transcode-recovery', server.includes('cleanup_stale_hls_transcodes(&db)') && server.includes('reconcile_transcode_sessions_on_startup(&db)')),
    check('periodic-transcode-cleanup', server.includes('spawn_periodic_transcode_cleanup') && api.includes('cleanup_orphan_hls_transcode_dirs')),
    check('hls-cleanup-is-confined-to-real-direct-child', extractFunction(api, 'async fn cleanup_hls_transcode_dir_in_root').includes('validate_hls_cleanup_dir(root, session_dir)') && extractFunction(api, 'async fn cleanup_hls_transcode_dir_in_root').includes('remove_dir_all(&session_dir)') && extractFunction(api, 'async fn validate_hls_cleanup_dir').includes('Component::ParentDir') && extractFunction(api, 'async fn validate_hls_cleanup_dir').includes('symlink_metadata(root)') && extractFunction(api, 'async fn validate_hls_cleanup_dir').includes('symlink_metadata(candidate)') && api.includes('hls_cleanup_accepts_only_real_direct_children_of_a_real_root')),
    check('library-scan-recovery', api.includes('recover_stale_library_scan_runs') && api.includes('LibraryScanFanoutConcurrency')),
    check('postgres-bounded-api-pool', db.includes('DEFAULT_MAX_CONNECTIONS') && db.includes('max_connections')),
    check('postgres-separated-worker-pool', db.includes('worker_pool: PgPool') && db.includes('DEFAULT_WORKER_MAX_CONNECTIONS')),
    check('postgres-pool-isolation-and-load-runner', db.includes('postgres_api_and_worker_pools_remain_isolated_when_saturated') && db.includes('postgres_pool_local_load') && db.includes('wait_until_backend_is_sleeping') && db.includes('POOL_RESPONSIVENESS_LIMIT')),
    check('postgres-query-timeouts', db.includes('statement_timeout') && db.includes('lock_timeout')),
    check('direct-http-compression-without-required-proxy', rootCargo.includes('"compression-gzip"') && api.includes('.layer(CompressionLayer::new())') && api.includes('direct_http_json_responses_are_gzip_compressed') && minimumStack.includes('nginx u otro proxy inverso | Opcional')),
    check('shared-folder-counts-use-bounded-fail-open-cache', extractFunction(api, 'async fn user_views_result').includes('cached_media_item_counts_by_virtual_folder') && extractFunction(api, 'async fn user_views_result_legacy').includes('cached_media_item_counts_by_virtual_folder') && extractFunction(catalogCache, 'pub(crate) async fn cached_media_item_counts_by_virtual_folder').includes('media_item_counts_by_virtual_folder') && catalogCache.includes('MAX_CACHE_VALUE_BYTES') && catalogCache.includes('CacheLookup::Unavailable')),
    check('compose-cache-overlay-mounts-credential-and-orders-health', redisOverlay.includes('JELLYRIN_REDIS_URL_FILE: /run/secrets/jellyrin-redis-url') && redisOverlay.includes('JELLYRIN_REDIS_URL_HOST_FILE') && redisOverlay.includes('condition: service_healthy') && !redisOverlay.includes('JELLYRIN_REDIS_URL:')),
    check('series-page-skips-unrequested-exact-total', extractFunction(api, 'async fn tv_series_items_result').includes('query_flag(&query._enable_total_record_count)') && extractFunction(postgresCatalog, 'pub async fn tv_series_catalog_search_page').includes('if include_total_record_count') && extractFunction(postgresCatalog, 'pub async fn tv_series_catalog_search_page').includes('"0::bigint"') && postgresCatalog.includes('assert_eq!(without_total.total_record_count, 0)')),
    check('postgres-resource-profile-is-parameterized-and-documented', infrastructure.includes('${POSTGRES_MEMORY_LIMIT:-512m}') && infrastructure.includes('${POSTGRES_SHARED_BUFFERS:-128MB}') && infrastructure.includes('${POSTGRES_EFFECTIVE_IO_CONCURRENCY:-1}') && minimumStack.includes('POSTGRES_MEMORY_LIMIT') && minimumStack.includes('POSTGRES_EFFECTIVE_IO_CONCURRENCY')),
    check('postgres-readiness-and-schema-health', db.includes('pub async fn health') && db.includes('pub async fn schema_health')),
    check('large-browse-100k-smoke', api.includes('large_browse_paging_handles_100k_items_without_expanding_response') && api.includes('0..100_000')),
    check('catalog-legacy-playback-batched', playbackBatchFunctions.every((signature) => {
      const body = extractFunction(api, signature);
      return body.includes('MediaCatalogStore::playback_states_for_items')
        && !body.includes('playback_state_for_item(');
    })),
    check('catalog-list-browse-batched', extractFunction(api, 'async fn special_collection_items').includes('media_list_item_counts') && !extractFunction(api, 'async fn special_collection_items').includes('media_list_items(') && extractFunction(api, 'async fn special_playlist_items').includes('media_list_item_counts') && extractFunction(api, 'async fn special_playlist_items').includes('media_list_ids_with_user_permission') && !extractFunction(api, 'async fn special_playlist_items').includes('media_list_items(')),
    check('catalog-domain-scoped-fallbacks', ['async fn parent_virtual_items', 'async fn movie_recommendations', 'async fn shows_next_up', 'async fn shows_upcoming', 'async fn series_seasons'].every((signature) => !extractFunction(api, signature).includes('.media_items()')) && !extractFunction(api, 'async fn shows_upcoming').includes('media_item_metadata()')),
    check('dlna-and-sidecars-avoid-global-catalog-scans', extractFunction(dlna, 'async fn media_items_for_folder').includes('media_items_for_virtual_folders') && !extractFunction(dlna, 'async fn media_items_for_folder').includes('.media_items()') && extractFunction(dlna, 'async fn media_item_metadata_map').includes('media_item_metadata_by_item_ids') && !extractFunction(dlna, 'async fn media_item_metadata_map').includes('media_item_metadata()') && extractFunction(api, 'async fn virtual_folder_detail_json').includes('media_item_counts_by_virtual_folder') && !extractFunction(api, 'async fn virtual_folder_detail_json').includes('.media_items()') && extractFunction(api, 'async fn local_sidecar_items').includes('media_items_for_virtual_folders') && !extractFunction(api, 'async fn local_sidecar_items').includes('.media_items()')),
    check('dlna-root-browse-batches-folders', extractFunction(dlna, 'async fn didl_root_children').includes('media_items_for_virtual_folders(&folder_ids)') && !extractFunction(dlna, 'async fn didl_root_children').includes('media_items_for_folder(')),
    check('catalog-scoped-metadata-and-instant-mix', extractFunction(api, 'async fn audio_instant_mix_items').includes('media_items_for_virtual_folders') && !extractFunction(api, 'async fn audio_instant_mix_items').includes('.media_items()') && ['async fn media_segments', 'async fn search_hint_metadata_values', 'async fn remote_trailers', 'async fn item_count_metadata_values'].every((signature) => extractFunction(api, signature).includes('media_item_metadata_by_item_ids') && !extractFunction(api, signature).includes('media_item_metadata()'))),
    check('search-hints-use-bounded-native-catalog-with-safe-fallback', extractFunction(api, 'async fn search_hints_catalog_result').includes('resolved_media_catalog_query_for_items') && extractFunction(api, 'async fn search_hints_catalog_result').includes('MediaItemCatalogSearchScope::SearchHintFields') && !extractFunction(api, 'async fn search_hints_catalog_result').includes('.media_items()') && dbLib.includes('fn push_sqlite_search_hint_metadata_filter') && postgresCatalog.includes('fn push_postgres_search_hint_filter') && ['Album', 'AlbumName', 'AlbumArtist', 'AlbumArtists', 'SeriesName', 'Series', 'Artists'].every((key) => dbLib.includes(`'${key}'`) && postgresCatalog.includes(`'${key}'`))),
    check('series-id-lookup-is-tv-scoped-with-inline-metadata', extractFunction(api, 'async fn series_name_for_id').includes('tv_episode_catalog_snapshot_for_series') && !extractFunction(api, 'async fn series_name_for_id').includes('.media_items()') && extractFunction(api, 'async fn tv_episode_catalog_snapshot_for_series').includes('MediaCatalogStore::tv_series_lookup_candidates_for_series') && extractFunction(api, 'async fn tv_episode_catalog_snapshot_without_canonical_series_id').includes('MediaCatalogStore::tv_series_lookup_candidates_without_canonical_series_id') && extractFunction(dbLib, 'pub async fn tv_series_lookup_candidates_for_series').includes('item.metadata_json') && extractFunction(postgresCatalog, 'pub async fn tv_series_lookup_candidates_for_series').includes('item.metadata')),
    check('tv-fallbacks-use-scoped-inline-metadata-snapshot', extractFunction(api, 'async fn tv_episode_catalog_snapshot').includes('MediaCatalogStore::tv_series_lookup_candidates') && ['async fn series_episodes', 'async fn series_seasons', 'async fn authenticated_similar_show_items', 'async fn apply_manual_series_metadata_update', 'async fn apply_remote_series_search_result', 'async fn refresh_tv_metadata_for_folder', 'async fn refresh_tv_metadata_for_series'].every((signature) => {
      const body = extractFunction(api, signature);
      return body.includes('tv_episode_catalog_snapshot')
        && !body.includes('.media_items()')
        && !body.includes('media_items_by_collection_type')
        && !body.includes('media_metadata_by_item_id(')
        && !body.includes('metadata_payload_for_item(');
    }) && !extractFunction(api, 'async fn refresh_tv_metadata_for_folder').includes('refresh_tv_metadata_for_series(') && extractFunction(api, 'async fn refresh_tv_metadata_for_folder').includes('refresh_tv_metadata_for_series_episodes') && api.includes('tv_refresh_grouping_preserves_cross_folder_series_scope') && api.includes('tv_refresh_grouping_preserves_episode_input_order')),
    check('image-owner-and-ancestors-avoid-global-catalog-scans', extractFunction(api, 'async fn media_item_or_folder_by_id').includes('MediaCatalogStore::media_item_exists') && !extractFunction(api, 'async fn media_item_or_folder_by_id').includes('.media_items()') && extractFunction(api, 'async fn item_ancestors').includes('MediaCatalogStore::media_item_by_id_visible') && extractFunction(api, 'async fn item_ancestors').includes('tv_episode_catalog_snapshot') && !extractFunction(api, 'async fn item_ancestors').includes('.media_items()')),
    check('instant-mix-and-remote-search-use-effective-type-candidates', extractFunction(api, 'async fn media_items_and_metadata_by_effective_types').includes('MediaCatalogStore::media_items_with_metadata_by_effective_types') && ['async fn remote_search_local_results', 'async fn instant_mix_from_metadata_entity', 'async fn music_genre_name_by_entity_id', 'async fn music_genre_instant_mix_response'].every((signature) => {
      const body = extractFunction(api, signature);
      return body.includes('media_items_and_metadata_by_effective_types')
        && !body.includes('.media_items()')
        && !body.includes('.media_item_metadata()');
    }) && extractFunction(api, 'async fn remote_search_local_results').includes('"BoxSet" => {') && api.includes('music_genre_id_resolution_preserves_non_audio_catalog_scope')),
    check('theme-and-metadata-similar-avoid-global-catalog-scans', extractFunction(api, 'async fn theme_items_result').includes('media_items_for_virtual_folders') && !extractFunction(api, 'async fn theme_items_result').includes('.media_items()') && extractFunction(api, 'async fn similar_items_from_metadata_entity').includes('MediaCatalogStore::media_items_with_metadata_by_effective_types') && extractFunction(api, 'async fn similar_items_from_metadata_entity').includes('metadata_values_for_keys_from_payloads') && extractFunction(api, 'async fn similar_items_from_metadata_entity').includes('metadata_entity_audio_items_from_payloads') && !extractFunction(api, 'async fn similar_items_from_metadata_entity').includes('.media_items()') && !extractFunction(api, 'async fn similar_items_from_metadata_entity').includes('.media_item_metadata()') && api.includes('theme_and_metadata_similar_scans_preserve_scope_with_large_unrelated_catalog')),
    check('item-counts-use-native-bounded-catalog-contract', extractFunction(api, 'async fn media_catalog_counts_result').includes('MediaCatalogStore::media_item_catalog_counts') && !extractFunction(api, 'async fn media_catalog_counts_result').includes('.media_items()') && extractFunction(api, 'async fn media_catalog_counts_result').includes('return Ok(None)') && extractFunction(api, 'async fn item_counts').includes('media_catalog_counts_result') && extractFunction(api, 'async fn user_item_counts').includes('media_catalog_counts_result') && dbLib.includes('DatabaseOperation::CatalogCounts') && extractFunction(dbLib, 'async fn media_item_catalog_counts_unobserved').includes("json_type(item.metadata_json, '$.Album') IS NOT NULL") && extractFunction(postgresCatalog, 'async fn media_item_catalog_counts_unobserved').includes("item.metadata ?| ARRAY['Album'") && api.includes('item_counts_fast_path_handles_more_than_one_page_and_complex_shapes_fallback_exactly')),
    check('catalog-collection-index-is-measured', catalogIndex.includes('collection_type, lower(name), id') && catalogIndex.includes('missing_since IS NULL') && catalogBenchmark.includes('EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)') && catalogBenchmark.includes('10000,100000,500000')),
    check('postgres-copy-is-measured-not-enabled', postgresCatalog.includes('postgres_snapshot_stage_copy_benchmark') && postgresCatalog.includes('postgres_copy_text_encoder_escapes_delimiters_controls_and_null') && !extractFunction(postgresCatalog, 'async fn replace_remote_media_library_snapshot_in_transaction').includes('copy_in_raw')),
    check('live-hls-copy-first-has-bounded-fallback', api.includes('apply_live_hls_ffmpeg_policy(&mut request, configured_ffmpeg_mode())') && extractFunction(api, 'fn apply_live_hls_ffmpeg_policy').includes('request.video_mode = HlsStreamMode::Copy') && extractFunction(api, 'fn apply_live_hls_ffmpeg_policy').includes('request.audio_mode = HlsStreamMode::Copy') && api.includes('live_hls_enabled_mode_builds_one_encode_fallback') && api.includes('let mut fallback = plan.fallback') && api.includes('attempt_count = 2')),
    check('hls-seek-generation-has-derived-deadline-and-terminal-timeout', extractFunction(api, 'async fn generate_missing_hls_segment').includes('hls_seek_generation_timeout(segment_duration_ticks)') && extractFunction(api, 'async fn generate_missing_hls_segment').includes('HlsSeekProcessOutcome::TimedOut') && extractFunction(api, 'async fn generate_missing_hls_segment').includes('seek-generation-timeout') && extractFunction(api, 'async fn wait_for_hls_seek_process').includes('tokio::time::sleep(timeout)') && extractFunction(api, 'async fn wait_for_hls_seek_process').includes('process.stop().await') && api.includes('hls_seek_deadline_timeout_stops_and_reaps_process')),
    check('fallback-observability-is-bounded-and-secret-free', api.includes('TRANSCODE_EXECUTIONS') && api.includes('LIVE_HLS_SESSION_REGISTRY_MAX_ENTRIES') && api.includes('"EffectiveExecutionMode"') && api.includes('"FallbackReasonCode"') && api.includes('"AttemptCount"') && extractFunction(api, 'async fn update_transcode_execution(').includes("Option<&'static str>") && !extractFunction(api, 'async fn update_transcode_execution(').includes('stderr') && !extractFunction(api, 'async fn update_transcode_execution(').includes('command.args')),
    check('ffmpeg-process-telemetry-is-numeric-bounded-and-in-memory', api.includes('spawn_transcode_observation_task') && api.includes('"ProcessCpuPercent"') && api.includes('"ProcessRssBytes"') && api.includes('"TranscodingSpeed"') && api.includes('process_resource_sampling_supported()') && !extractFunction(api, 'async fn update_transcode_execution_resources').includes('.update_transcode_session')),
    check('auxiliary-ffmpeg-telemetry-is-wired-aggregated-and-payload-free', auxiliaryFfmpegTelemetry.includes('enum AuxiliaryFfmpegOutcome') && auxiliaryFfmpegTelemetry.includes('impl Drop for AuxiliaryFfmpegTelemetryAttempt') && auxiliaryFfmpegTelemetry.includes('DURATION_BUCKET_UPPER_MILLIS') && extractFunction(api, 'async fn run_auxiliary_ffmpeg_output').includes('auxiliary_ffmpeg_telemetry().start()') && extractFunction(api, 'async fn run_auxiliary_ffmpeg_output').includes('AuxiliaryFfmpegOutcome::CapacityUnavailable') && extractFunction(api, 'async fn run_auxiliary_ffmpeg_output').includes('AuxiliaryFfmpegOutcome::NonZeroExit') && !extractFunction(api, 'async fn run_auxiliary_ffmpeg_output').includes('command.args') && extractFunction(api, 'async fn transcode_observability_summary').includes('"AuxiliaryExecutions"')),
    check('ffprobe-outcomes-and-duration-are-fixed-cardinality', api.includes('ffprobe_telemetry_snapshot()') && api.includes('"LocalAndRemote"') && api.includes('"OutputLimited"') && dbLib.includes('FfprobeOutcome::InvalidJson') && dbLib.includes('FfprobeOutcome::TimedOut')),
    check('ffmpeg-and-ffprobe-share-one-process-wide-multimedia-coordinator', transcode.includes('static MULTIMEDIA_PROCESS_COORDINATOR: OnceLock<TranscodeCoordinator>') && transcode.includes('TranscodeJobKind::Probe') && transcode.includes('JELLYRIN_MAX_PROBE_JOBS') && transcode.includes('acquire_multimedia_probe') && extractFunction(api, 'fn transcode_coordinator').includes('multimedia_process_coordinator()') && !api.includes('static TRANSCODE_COORDINATOR') && extractFunction(dbLib, 'async fn probe_media_info_input').includes('acquire_multimedia_probe().await') && extractFunction(api, 'async fn ensure_xtream_remote_media_info').includes('probe_remote_media_info_admitted') && dbLib.includes('FfprobeOutcome::CapacityUnavailable') && api.includes('"CapacityUnavailable": executions.capacity_unavailable') && transcode.includes('multimedia_process_probe_and_encode_share_the_aggregate_limit') && transcode.includes('multimedia_process_probe_queue_is_bounded_timeout_and_cancel_safe')),
    check('legacy-sqlite-wal-is-fail-closed-until-embedded-fix', dbLib.includes('persistent_legacy_sqlite_uses_rollback_journal_until_wal_fix_is_pinned') && !dbLib.includes('SqliteJournalMode::Wal')),
    check('resource-admission-and-database-diagnostics', api.includes('"Database": database_observability_summary') && api.includes('"CatalogSync": catalog_sync_observability_summary') && api.includes('admission_metrics_json(ffmpeg_admission_metrics().snapshot())') && api.includes('admission_metrics_json(remote_probe_admission_metrics().snapshot())') && api.includes('"WaitMilliseconds"') && dbLib.includes('pub struct DatabaseRuntimeDiagnostics') && dbLib.includes('pub struct CatalogSyncDiagnostics') && postgresCatalog.includes('pub async fn catalog_sync_diagnostics')),
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
  };

  await fs.mkdir(generatedDir, { recursive: true });
  await fs.writeFile(
    path.join(generatedDir, 'performance-recovery.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  await fs.writeFile(path.join(generatedDir, 'performance-recovery.md'), renderMarkdown(result));
  console.log(`wrote ${path.join(generatedDir, 'performance-recovery.md')}`);

  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

function extractFunction(source, signature) {
  const start = source.indexOf(signature);
  if (start < 0) return '';
  const nextFunction = source.indexOf('\nfn ', start + signature.length);
  const nextAsyncFunction = source.indexOf('\nasync fn ', start + signature.length);
  const candidates = [nextFunction, nextAsyncFunction].filter((index) => index >= 0);
  const end = candidates.length > 0 ? Math.min(...candidates) : source.length;
  return source.slice(start, end);
}

function check(id, passed) {
  return {
    id,
    status: passed ? 'passed' : 'failed',
  };
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Performance Recovery Matrix');
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
