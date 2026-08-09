#!/usr/bin/env node

const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');

const root = path.resolve(__dirname, '..');

async function read(relativePath) {
  return fs.readFile(path.join(root, relativePath), 'utf8');
}

test('PostgreSQL operation scripts are valid Bash and fail closed around credentials', async () => {
  for (const relativePath of ['ops/postgres/backup.sh', 'ops/postgres/restore-drill.sh']) {
    const syntax = spawnSync('bash', ['-n', relativePath], { cwd: root, encoding: 'utf8' });
    assert.equal(syntax.status, 0, syntax.stderr);
  }

  const backup = await read('ops/postgres/backup.sh');
  assert.match(backup, /JELLYRIN_POSTGRES_BACKUP_AGE_RECIPIENTS:\?/);
  assert.match(backup, /PGSERVICE=/);
  assert.doesNotMatch(backup, /DATABASE_URL/);
  assert.match(backup, /pg_restore --list/);
  assert.match(backup, /sha256sum --check --strict/);
  assert.match(backup, /mv -- .*STAGING_DIR.*SNAPSHOT_DIR/);
  assert.match(backup, /prune_tier daily/);
  assert.match(backup, /prune_tier weekly/);
  assert.match(backup, /prune_tier monthly/);

  const restore = await read('ops/postgres/restore-drill.sh');
  assert.match(restore, /sha256sum --check --strict SHA256SUMS/);
  assert.match(restore, /createdb/);
  assert.match(restore, /--template=template0/);
  assert.match(restore, /pg_restore/);
  assert.match(restore, /--exit-on-error/);
  assert.match(restore, /dropdb --if-exists --force/);
  assert.match(restore, /CREATE EXTENSION IF NOT EXISTS pg_stat_statements/);
  assert.doesNotMatch(restore, /PGDATABASE=/);
  assert.match(restore, /--dbname="\$\{RESTORE_DATABASE\}"/);
  assert.match(restore, /_sqlx_migrations WHERE NOT success/);
  assert.match(restore, /NOT convalidated/);
});

test('backup timer is persistent and service receives credentials through systemd', async () => {
  const service = await read('ops/jellyrin-postgres-backup.service');
  const timer = await read('ops/jellyrin-postgres-backup.timer');
  const localPeer = await read('ops/postgres/jellyrin-postgres-backup-local-peer.conf.example');
  const backupService = await read('ops/postgres/backup-pg-service.conf.example');
  const restoreService = await read('ops/postgres/restore-pg-service.conf.example');
  assert.match(service, /LoadCredential=pg_service\.conf:/);
  assert.match(service, /LoadCredential=pgpass:/);
  assert.match(service, /LoadCredential=age-recipients\.txt:/);
  assert.match(service, /ProtectSystem=strict/);
  assert.match(service, /ReadWritePaths=\/var\/backups\/jellyrin-postgres/);
  assert.match(timer, /OnCalendar=/);
  assert.match(timer, /Persistent=true/);
  assert.match(timer, /RandomizedDelaySec=/);
  assert.match(localPeer, /Environment=PGPASSFILE=/);
  assert.match(localPeer, /^LoadCredential=$/m);
  assert.doesNotMatch(localPeer, /LoadCredential=pgpass:/);
  assert.match(backupService, /^user=jellyrin-backup$/m);
  assert.match(restoreService, /^user=postgres$/m);
  assert.match(backupService, /^host=\/var\/run\/postgresql$/m);
  assert.match(restoreService, /^host=\/var\/run\/postgresql$/m);
});

test('Compose PostgreSQL records normalized statement telemetry', async () => {
  const compose = await read('docker-compose.infrastructure.yml');
  const bootstrap = await read('ops/postgres/init/001-bootstrap.sh');
  assert.match(compose, /shared_preload_libraries=pg_stat_statements/);
  assert.match(compose, /pg_stat_statements\.track=all/);
  assert.match(compose, /pg_stat_statements\.max=5000/);
  assert.match(bootstrap, /CREATE EXTENSION IF NOT EXISTS pg_stat_statements/);
});
