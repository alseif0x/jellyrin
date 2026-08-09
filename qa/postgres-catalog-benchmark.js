#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const repoRoot = path.resolve(__dirname, '..');
const plansDir = process.env.JELLYRIN_PLANS_DIR
  || path.resolve(repoRoot, '..', '..', 'plans');
const outputPath = path.join(plansDir, 'generated', 'postgres-catalog-benchmark.json');
const databaseUrl = process.env.JELLYRIN_TEST_POSTGRES_URL;
const allowWrite = process.env.JELLYRIN_BENCHMARK_ALLOW_WRITE === '1';
const repetitions = boundedInteger(process.env.JELLYRIN_CATALOG_BENCHMARK_REPETITIONS, 12, 3, 50);
const sizes = parseSizes(process.env.JELLYRIN_CATALOG_BENCHMARK_SIZES || '10000,100000,500000');
const schema = `jellyrin_catalog_bench_${process.pid}_${Date.now()}`;

async function main() {
  if (!databaseUrl) {
    throw new Error('JELLYRIN_TEST_POSTGRES_URL is required');
  }
  if (!allowWrite) {
    throw new Error('set JELLYRIN_BENCHMARK_ALLOW_WRITE=1 to create an isolated benchmark schema');
  }
  const connection = parseConnection(databaseUrl);
  const result = {
    generatedAt: new Date().toISOString(),
    postgresVersion: psql(connection, 'SHOW server_version;').trim(),
    repetitions,
    sizes,
    schemaLifecycle: 'isolated-created-and-dropped',
    datasets: [],
  };

  try {
    psql(connection, setupSql());
    for (const size of sizes) {
      psql(connection, seedSql(size));
      psql(connection, sampleSql(size, repetitions, false));
      const currentPlan = explain(connection, moviePageSql(size));
      const currentGenrePlans = genrePlans(connection);
      psql(connection, candidateIndexSql());
      psql(connection, sampleSql(size, repetitions, true));
      const candidatePlan = explain(connection, moviePageSql(size));
      const metrics = JSON.parse(psql(connection, metricsSql(size)));
      const byScenario = Object.fromEntries(metrics.map((metric) => [metric.scenario, metric]));
      result.datasets.push({
        size,
        metrics,
        collectionTypeIndex: {
          currentP95Ms: byScenario.movie_page_current?.p95Ms ?? null,
          candidateP95Ms: byScenario.movie_page_candidate?.p95Ms ?? null,
          p95Speedup: ratio(
            byScenario.movie_page_current?.p95Ms,
            byScenario.movie_page_candidate?.p95Ms,
          ),
          currentPlan: summarizePlan(currentPlan),
          candidatePlan: summarizePlan(candidatePlan),
        },
        genreSelectorProjection: {
          selectiveVisibleRows: Math.floor((size - 1) / 1000) + 1,
          commonVisibleRows: Math.floor(size / 5) - Math.floor(size / 100),
          order: 'alternating-exists-in-and-in-exists',
          p95: genreMetricSummary(byScenario, 'current'),
          plans: currentGenrePlans,
        },
      });
      psql(connection, `DROP INDEX IF EXISTS ${ident(schema)}.media_items_visible_collection_name_page_idx;`);
    }
    await fs.mkdir(path.dirname(outputPath), { recursive: true });
    await fs.writeFile(outputPath, `${JSON.stringify(result, null, 2)}\n`);
    console.log(`wrote ${outputPath}`);
  } finally {
    psql(connection, `DROP SCHEMA IF EXISTS ${ident(schema)} CASCADE;`);
  }
}

function parseConnection(value) {
  let parsed;
  try {
    parsed = new URL(value);
  } catch {
    throw new Error('JELLYRIN_TEST_POSTGRES_URL must be a valid PostgreSQL URL');
  }
  if (!['postgres:', 'postgresql:'].includes(parsed.protocol)) {
    throw new Error('JELLYRIN_TEST_POSTGRES_URL must use postgres:// or postgresql://');
  }
  const database = decodeURIComponent(parsed.pathname.replace(/^\//, ''));
  if (!parsed.hostname || !parsed.username || !database) {
    throw new Error('PostgreSQL URL must include host, user, and database');
  }
  return {
    host: parsed.hostname,
    port: parsed.port || '5432',
    user: decodeURIComponent(parsed.username),
    password: decodeURIComponent(parsed.password),
    database,
  };
}

function psql(connection, sql) {
  const run = spawnSync('psql', [
    '-X', '-v', 'ON_ERROR_STOP=1', '-Atq',
    '-h', connection.host,
    '-p', connection.port,
    '-U', connection.user,
    '-d', connection.database,
  ], {
    input: `SET statement_timeout = '120s';\n${sql}\n`,
    encoding: 'utf8',
    env: {
      ...process.env,
      PGPASSWORD: connection.password,
      PGAPPNAME: 'jellyrin-catalog-benchmark',
    },
    maxBuffer: 16 * 1024 * 1024,
  });
  if (run.error) throw run.error;
  if (run.status !== 0) {
    throw new Error(`psql benchmark command failed: ${redact(run.stderr)}`);
  }
  return run.stdout.trim();
}

function setupSql() {
  return `
    CREATE SCHEMA ${ident(schema)};
    CREATE TABLE ${ident(schema)}.media_items (
      id uuid PRIMARY KEY,
      virtual_folder_id uuid NOT NULL,
      name text NOT NULL,
      collection_type text NOT NULL,
      created_at timestamptz NOT NULL,
      updated_at timestamptz NOT NULL,
      missing_since timestamptz
    );
    CREATE TABLE ${ident(schema)}.playback_states (
      user_id uuid NOT NULL,
      item_id uuid NOT NULL,
      played boolean NOT NULL,
      is_favorite boolean NOT NULL,
      position_ticks bigint NOT NULL,
      PRIMARY KEY (user_id, item_id)
    );
    CREATE TABLE ${ident(schema)}.media_item_genre_selectors (
      item_id uuid NOT NULL REFERENCES ${ident(schema)}.media_items(id) ON DELETE CASCADE,
      selector text NOT NULL,
      PRIMARY KEY (item_id, selector)
    );
    CREATE TABLE ${ident(schema)}.samples (
      dataset_size integer NOT NULL,
      scenario text NOT NULL,
      elapsed_ms double precision NOT NULL
    );
    CREATE INDEX media_items_visible_name_page_idx
      ON ${ident(schema)}.media_items (lower(name), id)
      WHERE missing_since IS NULL;
    CREATE INDEX media_items_visible_folder_name_page_idx
      ON ${ident(schema)}.media_items (virtual_folder_id, lower(name), id)
      WHERE missing_since IS NULL;
    CREATE INDEX media_items_visible_created_page_idx
      ON ${ident(schema)}.media_items (created_at, id)
      WHERE missing_since IS NULL;
    CREATE INDEX media_items_visible_updated_page_idx
      ON ${ident(schema)}.media_items (updated_at, id)
      WHERE missing_since IS NULL;
    CREATE INDEX media_item_genre_selectors_lookup_idx
      ON ${ident(schema)}.media_item_genre_selectors (selector, item_id);
  `;
}

function seedSql(size) {
  return `
    TRUNCATE ${ident(schema)}.media_items, ${ident(schema)}.playback_states,
      ${ident(schema)}.media_item_genre_selectors;
    DELETE FROM ${ident(schema)}.samples WHERE dataset_size = ${size};
    BEGIN;
    SET LOCAL synchronous_commit = off;
    INSERT INTO ${ident(schema)}.media_items (
      id, virtual_folder_id, name, collection_type, created_at, updated_at, missing_since
    )
    SELECT
      md5('item-' || value)::uuid,
      CASE value % 8
        WHEN 0 THEN '00000000-0000-0000-0000-000000000001'::uuid
        WHEN 1 THEN '00000000-0000-0000-0000-000000000002'::uuid
        WHEN 2 THEN '00000000-0000-0000-0000-000000000003'::uuid
        WHEN 3 THEN '00000000-0000-0000-0000-000000000004'::uuid
        WHEN 4 THEN '00000000-0000-0000-0000-000000000005'::uuid
        WHEN 5 THEN '00000000-0000-0000-0000-000000000006'::uuid
        WHEN 6 THEN '00000000-0000-0000-0000-000000000007'::uuid
        ELSE '00000000-0000-0000-0000-000000000008'::uuid
      END,
      'Catalog Item ' || lpad(value::text, 9, '0'),
      CASE value % 3 WHEN 0 THEN 'movies' WHEN 1 THEN 'tvshows' ELSE 'music' END,
      now() - make_interval(secs => value % 31536000),
      now() - make_interval(secs => value % 2592000),
      CASE WHEN value % 100 = 0 THEN now() ELSE NULL END
    FROM generate_series(1, ${size}) AS value;
    INSERT INTO ${ident(schema)}.playback_states (
      user_id, item_id, played, is_favorite, position_ticks
    )
    SELECT
      '10000000-0000-0000-0000-000000000001'::uuid,
      id,
      (row_number() OVER ()) % 4 = 0,
      (row_number() OVER ()) % 17 = 0,
      CASE WHEN (row_number() OVER ()) % 5 = 0 THEN 900000000 ELSE 0 END
    FROM ${ident(schema)}.media_items
    WHERE missing_since IS NULL AND collection_type = 'movies';
    INSERT INTO ${ident(schema)}.media_item_genre_selectors (item_id, selector)
    SELECT md5('item-' || value)::uuid, 'genre-common'
    FROM generate_series(1, ${size}) AS value
    WHERE value % 5 = 0
    UNION ALL
    SELECT md5('item-' || value)::uuid, 'genre-rare'
    FROM generate_series(1, ${size}) AS value
    WHERE value % 1000 = 1;
    COMMIT;
    ANALYZE ${ident(schema)}.media_items;
    ANALYZE ${ident(schema)}.playback_states;
    ANALYZE ${ident(schema)}.media_item_genre_selectors;
  `;
}

function sampleSql(size, sampleRepetitions, candidate) {
  const suffix = candidate ? 'candidate' : 'current';
  const movieScenario = `movie_page_${suffix}`;
  const offset = Math.min(Math.floor(size / 10), 10000);
  return `
    DO $benchmark$
    DECLARE started timestamptz; iteration integer;
    BEGIN
      FOR iteration IN 1..${sampleRepetitions} LOOP
        started := clock_timestamp();
        PERFORM id FROM ${ident(schema)}.media_items
          WHERE missing_since IS NULL
          ORDER BY lower(name), id LIMIT 100 OFFSET ${offset};
        INSERT INTO ${ident(schema)}.samples VALUES
          (${size}, 'visible_page_${suffix}', extract(epoch FROM clock_timestamp() - started) * 1000);

        started := clock_timestamp();
        PERFORM id FROM ${ident(schema)}.media_items
          WHERE missing_since IS NULL AND collection_type = 'movies'
          ORDER BY lower(name), id LIMIT 100 OFFSET ${offset};
        INSERT INTO ${ident(schema)}.samples VALUES
          (${size}, '${movieScenario}', extract(epoch FROM clock_timestamp() - started) * 1000);

        started := clock_timestamp();
        PERFORM item.id FROM ${ident(schema)}.media_items AS item
          LEFT JOIN ${ident(schema)}.playback_states AS playback
            ON playback.item_id = item.id
           AND playback.user_id = '10000000-0000-0000-0000-000000000001'::uuid
          WHERE item.missing_since IS NULL
            AND item.collection_type = 'movies'
            AND coalesce(playback.played, false) = false
          ORDER BY lower(item.name), item.id LIMIT 100 OFFSET ${offset};
        INSERT INTO ${ident(schema)}.samples VALUES
          (${size}, 'movie_playback_page_${suffix}', extract(epoch FROM clock_timestamp() - started) * 1000);

        started := clock_timestamp();
        PERFORM id FROM ${ident(schema)}.media_items
          WHERE missing_since IS NULL
            AND virtual_folder_id = '00000000-0000-0000-0000-000000000001'::uuid
          ORDER BY lower(name), id LIMIT 100 OFFSET ${offset};
        INSERT INTO ${ident(schema)}.samples VALUES
          (${size}, 'folder_page_${suffix}', extract(epoch FROM clock_timestamp() - started) * 1000);

        ${candidate ? '' : [
          genreBenchmarkPairSql(size, suffix, 'rare', 'page'),
          genreBenchmarkPairSql(size, suffix, 'common', 'page'),
          genreBenchmarkPairSql(size, suffix, 'rare', 'count'),
          genreBenchmarkPairSql(size, suffix, 'common', 'count'),
        ].join('\n')}
      END LOOP;
    END $benchmark$;
  `;
}

function genreBenchmarkPairSql(size, suffix, selector, operation) {
  const selectorValue = `genre-${selector}`;
  const count = operation === 'count';
  const existsQuery = genreQuerySql(selectorValue, 'exists', count).replace(/^SELECT /, 'PERFORM ');
  const inQuery = genreQuerySql(selectorValue, 'in', count).replace(/^SELECT /, 'PERFORM ');
  const timed = (query, shape) => `
    started := clock_timestamp();
    ${query};
    INSERT INTO ${ident(schema)}.samples VALUES
      (${size}, 'genre_${selector}_${shape}_${operation}_${suffix}',
       extract(epoch FROM clock_timestamp() - started) * 1000);`;
  return `
    IF iteration % 2 = 1 THEN
      ${timed(existsQuery, 'exists')}
      ${timed(inQuery, 'in')}
    ELSE
      ${timed(inQuery, 'in')}
      ${timed(existsQuery, 'exists')}
    END IF;`;
}

function candidateIndexSql() {
  return `
    CREATE INDEX media_items_visible_collection_name_page_idx
      ON ${ident(schema)}.media_items (collection_type, lower(name), id)
      WHERE missing_since IS NULL;
    ANALYZE ${ident(schema)}.media_items;
  `;
}

function moviePageSql(size) {
  const offset = Math.min(Math.floor(size / 10), 10000);
  return `SELECT id FROM ${ident(schema)}.media_items
    WHERE missing_since IS NULL AND collection_type = 'movies'
    ORDER BY lower(name), id LIMIT 100 OFFSET ${offset}`;
}

function genreQuerySql(selector, shape, count) {
  const predicate = shape === 'exists'
    ? `EXISTS (
        SELECT 1 FROM ${ident(schema)}.media_item_genre_selectors AS genre
        WHERE genre.item_id = item.id AND genre.selector = ANY(ARRAY['${selector}']::text[])
      )`
    : `item.id IN (
        SELECT genre.item_id FROM ${ident(schema)}.media_item_genre_selectors AS genre
        WHERE genre.selector = ANY(ARRAY['${selector}']::text[])
      )`;
  const select = count ? 'count(*)' : 'item.id';
  const page = count ? '' : ' ORDER BY lower(item.name), item.id LIMIT 100';
  return `SELECT ${select} FROM ${ident(schema)}.media_items AS item
    WHERE item.missing_since IS NULL AND ${predicate}${page}`;
}

function genrePlans(connection) {
  return Object.fromEntries(
    ['genre-rare', 'genre-common'].flatMap((selector) =>
      ['exists', 'in'].flatMap((shape) =>
        [false, true].map((count) => [
          `${selector.replace('genre-', '')}_${shape}_${count ? 'count' : 'page'}`,
          summarizePlan(explain(connection, genreQuerySql(selector, shape, count))),
        ]),
      ),
    ),
  );
}

function genreMetricSummary(metrics, suffix) {
  const result = {};
  for (const selector of ['rare', 'common']) {
    for (const operation of ['page', 'count']) {
      const exists = metrics[`genre_${selector}_exists_${operation}_${suffix}`]?.p95Ms ?? null;
      const inSubquery = metrics[`genre_${selector}_in_${operation}_${suffix}`]?.p95Ms ?? null;
      result[`${selector}_${operation}`] = {
        existsMs: exists,
        inMs: inSubquery,
        inSpeedup: ratio(exists, inSubquery),
      };
    }
  }
  return result;
}

function explain(connection, query) {
  return JSON.parse(psql(connection, `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) ${query};`))[0];
}

function metricsSql(size) {
  return `
    SELECT coalesce(json_agg(row_to_json(metric) ORDER BY scenario), '[]'::json)::text
    FROM (
      SELECT scenario,
        round(percentile_cont(0.5) WITHIN GROUP (ORDER BY elapsed_ms)::numeric, 3)::float8 AS "p50Ms",
        round(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms)::numeric, 3)::float8 AS "p95Ms",
        round(max(elapsed_ms)::numeric, 3)::float8 AS "maxMs"
      FROM ${ident(schema)}.samples
      WHERE dataset_size = ${size}
      GROUP BY scenario
    ) AS metric;
  `;
}

function summarizePlan(plan) {
  const nodes = [];
  walkPlan(plan.Plan, nodes);
  return {
    planningTimeMs: plan['Planning Time'],
    executionTimeMs: plan['Execution Time'],
    nodes,
  };
}

function walkPlan(node, output) {
  output.push({
    nodeType: node['Node Type'],
    relation: node['Relation Name'] || null,
    index: node['Index Name'] || null,
    actualRows: node['Actual Rows'],
    sharedHitBlocks: node['Shared Hit Blocks'],
    sharedReadBlocks: node['Shared Read Blocks'],
  });
  for (const child of node.Plans || []) walkPlan(child, output);
}

function parseSizes(raw) {
  const values = raw.split(',').map((value) => boundedInteger(value, NaN, 1000, 500000));
  if (values.some((value) => !Number.isInteger(value)) || values.length === 0) {
    throw new Error('JELLYRIN_CATALOG_BENCHMARK_SIZES must be comma-separated integers from 1000 to 500000');
  }
  return [...new Set(values)];
}

function boundedInteger(raw, fallback, minimum, maximum) {
  const parsed = raw === undefined ? fallback : Number.parseInt(raw, 10);
  return Number.isInteger(parsed) && parsed >= minimum && parsed <= maximum ? parsed : fallback;
}

function ident(value) {
  return `"${value.replaceAll('"', '""')}"`;
}

function ratio(left, right) {
  if (!Number.isFinite(left) || !Number.isFinite(right) || right <= 0) return null;
  return Math.round((left / right) * 1000) / 1000;
}

function redact(value) {
  return String(value || '').replaceAll(databaseUrl || '', '[REDACTED]').trim();
}

main().catch((error) => {
  console.error(redact(error instanceof Error ? error.message : error));
  process.exitCode = 1;
});
