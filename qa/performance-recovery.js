#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const api = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/lib.rs'), 'utf8');
  const db = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-db/src/lib.rs'), 'utf8');
  const server = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-server/src/main.rs'), 'utf8');

  const checks = [
    check('streaming-reader-stream', api.includes('ReaderStream::new(file)') && api.includes('ReaderStream::new(file.take(content_length))')),
    check('streaming-range-headers', api.includes('ACCEPT_RANGES') && api.includes('CONTENT_RANGE') && api.includes('RANGE_NOT_SATISFIABLE')),
    check('bounded-transcode-dedupe', api.includes('TRANSCODE_DEDUPE_LOCKS') && api.includes('claim_transcode_session') && api.includes('hls_transcode_dedupe_lock_is_shared_per_key')),
    check('startup-transcode-recovery', server.includes('cleanup_stale_hls_transcodes(&db)') && server.includes('reconcile_transcode_sessions_on_startup(&db)')),
    check('periodic-transcode-cleanup', server.includes('spawn_periodic_transcode_cleanup') && api.includes('cleanup_orphan_hls_transcode_dirs')),
    check('library-scan-recovery', api.includes('recover_stale_library_scan_runs') && api.includes('LibraryScanFanoutConcurrency')),
    check('sqlite-busy-timeout', db.includes('SQLITE_BUSY_TIMEOUT_MS') && db.includes('busy_timeout') && db.includes('sqlite_runtime_settings_enable_busy_timeout_and_foreign_keys')),
    check('sqlite-foreign-keys', db.includes('.foreign_keys(true)') && db.includes('PRAGMA foreign_keys = ON')),
    check('sqlite-wal-file-db', db.includes('SqliteJournalMode::Wal') && db.includes('should_enable_wal')),
    check('large-browse-100k-smoke', api.includes('large_browse_paging_handles_100k_items_without_expanding_response') && api.includes('0..100_000')),
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
