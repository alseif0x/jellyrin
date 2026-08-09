const crypto = require('node:crypto');
const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');

function configuredPostgresUrl() {
  const candidates = [
    process.env.JELLYRIN_QA_POSTGRES_URL,
    process.env.JELLYRIN_TEST_POSTGRES_URL,
    process.env.DATABASE_URL,
  ];
  const databaseUrl = candidates.find((candidate) => isPostgresUrl(candidate));
  if (!databaseUrl) {
    throw new Error(
      'PostgreSQL QA requires JELLYRIN_QA_POSTGRES_URL or '
        + 'JELLYRIN_TEST_POSTGRES_URL (a DDL-capable disposable test database)',
    );
  }
  return databaseUrl;
}

function isPostgresUrl(value) {
  return typeof value === 'string' && /^postgres(?:ql)?:\/\//i.test(value);
}

async function prepareIsolatedPostgres(prefix) {
  const baseUrl = configuredPostgresUrl();
  const schema = schemaName(prefix);
  const quotedSchema = quoteIdentifier(schema);
  await requireCommand('psql', ['--version']);

  const create = await runCommand('psql', [
    baseUrl,
    '--no-psqlrc',
    '--set',
    'ON_ERROR_STOP=1',
    '--command',
    [
      'BEGIN',
      "SELECT pg_advisory_xact_lock(hashtextextended('jellyrin:schema:migration', 0))",
      'CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public',
      'COMMIT',
    ].join('; '),
    '--command',
    `CREATE SCHEMA ${quotedSchema}`,
  ]);
  if (create.code !== 0) {
    throw new Error(`failed to create isolated PostgreSQL QA schema: ${tail(create.stderr)}`);
  }

  const databaseUrl = withSearchPath(baseUrl, schema);
  try {
    const migrator = await ensureMigratorBinary();
    const migrate = await runCommand(migrator, ['schema'], {
      env: { ...process.env, DATABASE_URL: databaseUrl },
    });
    if (migrate.code !== 0) {
      throw new Error(`PostgreSQL schema migration failed: ${tail(migrate.stderr)}`);
    }
    const migrationCount = Number(await queryScalar(databaseUrl, 'SELECT COUNT(*) FROM _sqlx_migrations WHERE success'));
    if (!Number.isSafeInteger(migrationCount) || migrationCount < 1) {
      throw new Error('PostgreSQL migration history is empty after schema migration');
    }
    await assertRuntimeMigrationHistoryPrivileges(databaseUrl);
    return {
      baseUrl,
      databaseUrl,
      schema,
      migrationCount,
      async backup(filePath) {
        await requireCommand('pg_dump', ['--version']);
        const dump = await runCommand('pg_dump', [
          '--dbname',
          baseUrl,
          '--format',
          'custom',
          '--schema',
          schema,
          '--no-owner',
          '--file',
          filePath,
        ]);
        if (dump.code !== 0) {
          throw new Error(`PostgreSQL backup failed: ${tail(dump.stderr)}`);
        }
      },
      async restore(filePath) {
        await requireCommand('pg_restore', ['--version']);
        await dropSchema(baseUrl, schema);
        const restore = await runCommand('pg_restore', [
          '--dbname',
          baseUrl,
          '--exit-on-error',
          '--no-owner',
          filePath,
        ]);
        if (restore.code !== 0) {
          throw new Error(`PostgreSQL restore failed: ${tail(restore.stderr)}`);
        }
        await assertRuntimeMigrationHistoryPrivileges(databaseUrl);
      },
      async cleanup() {
        await dropSchema(baseUrl, schema);
      },
    };
  } catch (error) {
    await dropSchema(baseUrl, schema).catch(() => {});
    throw error;
  }
}

function withSearchPath(databaseUrl, schema) {
  const parsed = new URL(databaseUrl);
  const existingOptions = parsed.searchParams.get('options');
  const searchPathOption = `-csearch_path=${schema},public`;
  parsed.searchParams.set(
    'options',
    existingOptions ? `${existingOptions} ${searchPathOption}` : searchPathOption,
  );
  return parsed.toString();
}

async function queryScalar(databaseUrl, sql) {
  const query = await runCommand('psql', [
    databaseUrl,
    '--no-psqlrc',
    '--tuples-only',
    '--no-align',
    '--set',
    'ON_ERROR_STOP=1',
    '--command',
    sql,
  ]);
  if (query.code !== 0) {
    throw new Error(`PostgreSQL QA query failed: ${tail(query.stderr)}`);
  }
  return query.stdout.trim();
}

async function assertRuntimeMigrationHistoryPrivileges(databaseUrl) {
  const runtimeRoleExists = await queryScalar(
    databaseUrl,
    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime')",
  );
  if (runtimeRoleExists !== 't') {
    return;
  }
  const selectOnly = await queryScalar(
    databaseUrl,
    `
      SELECT
        has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'SELECT')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'INSERT')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'UPDATE')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'DELETE')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'TRUNCATE')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'REFERENCES')
        AND NOT has_table_privilege('jellyrin_runtime', '_sqlx_migrations', 'TRIGGER')
    `,
  );
  if (selectOnly !== 't') {
    throw new Error('jellyrin_runtime must have SELECT-only access to _sqlx_migrations');
  }
}

async function dropSchema(databaseUrl, schema) {
  const result = await runCommand('psql', [
    databaseUrl,
    '--no-psqlrc',
    '--set',
    'ON_ERROR_STOP=1',
    '--command',
    `DROP SCHEMA IF EXISTS ${quoteIdentifier(schema)} CASCADE`,
  ]);
  if (result.code !== 0) {
    throw new Error(`failed to remove PostgreSQL QA schema: ${tail(result.stderr)}`);
  }
}

async function ensureMigratorBinary() {
  const configured = process.env.JELLYRIN_MIGRATE_BIN;
  if (configured && await exists(configured)) {
    return configured;
  }
  const binary = path.join(repoRoot, 'target', 'debug', 'jellyrin-migrate');
  const build = await runCommand('cargo', ['build', '-p', 'jellyrin-migrate']);
  if (build.code !== 0) {
    throw new Error(`cargo build -p jellyrin-migrate failed: ${tail(build.stderr)}`);
  }
  if (!await exists(binary)) {
    throw new Error(`missing built migration binary: ${binary}`);
  }
  return binary;
}

function schemaName(prefix) {
  const normalized = String(prefix).toLowerCase().replace(/[^a-z0-9]+/g, '_').slice(0, 20);
  const suffix = crypto.randomBytes(6).toString('hex');
  return `jellyrin_qa_${normalized}_${process.pid}_${suffix}`.slice(0, 63);
}

function quoteIdentifier(identifier) {
  if (!/^[a-z][a-z0-9_]{0,62}$/.test(identifier)) {
    throw new Error('invalid generated PostgreSQL schema identifier');
  }
  return `"${identifier}"`;
}

async function requireCommand(command, versionArgs) {
  const result = await runCommand(command, versionArgs);
  if (result.code !== 0) {
    throw new Error(`${command} is required for the PostgreSQL release smoke`);
  }
}

function runCommand(command, args, options = {}) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
      ...options,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    child.on('error', (error) => resolve({ code: 1, stdout, stderr: error.message }));
    child.on('close', (code, signal) => resolve({
      code: code ?? 1,
      signal,
      stdout,
      stderr,
    }));
  });
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

function tail(value) {
  return value.split('\n').filter(Boolean).slice(-20).join('\n');
}

module.exports = {
  isPostgresUrl,
  prepareIsolatedPostgres,
  queryScalar,
};
