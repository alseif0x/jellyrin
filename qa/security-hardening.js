#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const api = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-api/src/lib.rs'), 'utf8');
  const core = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-core/src/lib.rs'), 'utf8');
  const transcode = await fs.readFile(path.join(repoRoot, 'crates/jellyrin-transcode/src/lib.rs'), 'utf8');

  const checks = [
    check('admin-auth-backup', api.includes('async fn backups') && api.includes('require_admin(&state.db, &headers, query.api_key.as_deref()).await?')),
    check('admin-auth-migration', api.includes('async fn jellyfin_migration_import') && api.includes('let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?')),
    check('admin-auth-logs', api.includes('async fn system_logs') && api.includes('async fn system_log_file') && occurrences(api, 'require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?') >= 3),
    check('startup-auth-exception-scoped', api.includes('require_user_or_startup_incomplete') && api.includes('require_admin_or_startup_incomplete')),
    check('traversal-log-file-name', api.includes('fn safe_log_file_path') && api.includes("name.contains('/')") && api.includes("name.contains('\\\\')")),
    check('symlink-log-policy', api.includes('tokio::fs::symlink_metadata') && api.includes('metadata.file_type().is_symlink()')),
    check('configuration-page-name-policy', api.includes('fn is_dashboard_configuration_page_name') && api.includes('dashboard_configuration_page')),
    check('backup-restore-path-policy', api.includes('fn backup_restore_path_is_safe') && api.includes('Jellyfin migration library location is not safe to import')),
    check('client-log-upload-limit', api.includes('CLIENT_LOG_DOCUMENT_LIMIT_BYTES') && api.includes('to_bytes(body, CLIENT_LOG_DOCUMENT_LIMIT_BYTES)')),
    check('image-upload-limit', api.includes('IMAGE_UPLOAD_LIMIT_BYTES') && api.includes('to_bytes(body, IMAGE_UPLOAD_LIMIT_BYTES)')),
    check('auth-lockout', api.includes('AUTH_LOCKOUT_FAILURE_LIMIT') && api.includes('StatusCode::TOO_MANY_REQUESTS')),
    check('token-redaction', api.includes('fn redact_sensitive_log_text') && api.includes('[REDACTED]')),
    check('ffmpeg-no-shell', core.includes('FfmpegCommandSpec::new("ffmpeg", args)') && transcode.includes('Command::new(&command.program)') && transcode.includes('.args(&command.args)')),
    check('ffmpeg-no-stdin', core.includes('"-nostdin"') && transcode.includes('.stdin(Stdio::null())')),
    check('transcode-temp-sanitized', transcode.includes('fn sanitize_hls_path_component') && transcode.includes('sanitize_hls_path_component(play_session_id)')),
    check('cors-not-permissive', !api.includes('CorsLayer::permissive') && !api.includes('allow_origin(Any)')),
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
    path.join(generatedDir, 'security-hardening.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  await fs.writeFile(path.join(generatedDir, 'security-hardening.md'), renderMarkdown(result));
  console.log(`wrote ${path.join(generatedDir, 'security-hardening.md')}`);

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

function occurrences(value, needle) {
  return value.split(needle).length - 1;
}

function renderMarkdown(result) {
  const lines = [];
  lines.push('# Security Hardening Matrix');
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
