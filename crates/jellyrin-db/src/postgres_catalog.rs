use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::OsStr,
    path::Path,
};

use anyhow::Context;
use futures_util::TryStreamExt;
use jellyrin_core::{MediaItem, PlaybackState, VirtualFolder, tv_episode_path_info};
use serde_json::Value;
use sqlx::{Acquire, PgConnection, Postgres, QueryBuilder, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    CatalogSyncCountsRow, CatalogSyncDiagnostics, CatalogSyncRunDiagnostics, DatabasePoolRole,
    EffectiveTypeCandidateScope, MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE,
    MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION, MediaItemCatalogCounts, MediaItemCatalogEntry,
    MediaItemCatalogPage, MediaItemCatalogQuery, MediaItemCatalogSearchScope,
    MediaItemCatalogSortField, MediaItemFacetCandidateQuery, MediaItemFacetKind,
    MediaItemFacetValue, MediaItemFavoriteFilter, MediaItemFilterSummary, MediaItemForImageTag,
    MediaItemMetadata, MediaItemQueryFilterProjection, MediaItemQueryFilterProjectionSource,
    MediaItemQueryFilterSelection, MediaItemQueryFilterValues, PostgresDatabase,
    REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS, RemoteMediaCatalogStage, RemoteMediaItemUpsert,
    RemoteMediaLibrarySnapshot, RemoteMediaLibraryStageSpec, SortDirection,
    TV_SERIES_CATALOG_PROJECTION_VERSION, TvSeriesCatalogKey, TvSeriesCatalogPage,
    catalog_sync_duration_millis, encode_media_item_query_filter_position,
    extract_media_item_facets, extract_media_item_filter_selectors,
    extract_media_item_genre_selectors, extract_media_item_query_filter_projection,
    is_upcoming_media_item_entry, nonnegative_count, normalized_facet_query_values,
    prepare_remote_media_library_stage_specs, retain_entries_with_effective_types,
    telemetry::DatabaseOperation, upcoming_media_item_premiere_parts,
    validate_remote_media_catalog_stage_append,
};

const REMOTE_SNAPSHOT_INSERT_CHUNK_SIZE: usize = 1_000;
const FACET_STAGE_INSERT_CHUNK_SIZE: usize = 2_000;
const FACET_REBUILD_BATCH_SIZE: i64 = 500;
pub const MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME: &str = "media_item_query_filter_values";

pub const MEDIA_ITEM_FACET_PROJECTION_NAME: &str = "media_item_facets";
pub const MEDIA_ITEM_FACET_PROJECTION_VERSION: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaItemFacetProjectionMode {
    EnsureCurrent,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaItemFacetProjectionReport {
    pub rebuilt: bool,
    pub source_item_count: u64,
    pub projected_facet_count: u64,
    pub projected_alias_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaItemQueryFilterProjectionReport {
    pub rebuilt: bool,
    pub source_item_count: u64,
    pub projected_value_count: u64,
}

pub async fn ensure_media_item_query_filter_projection(
    tx: &mut Transaction<'_, Postgres>,
    mode: MediaItemFacetProjectionMode,
) -> anyhow::Result<MediaItemQueryFilterProjectionReport> {
    let marker = sqlx::query_as::<_, (i32, i64, i64)>(
        "SELECT extractor_version, source_item_count, projected_facet_count \
         FROM jellyrin_derived_projection_versions WHERE projection_name = $1",
    )
    .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((version, source_count, value_count)) = marker {
        anyhow::ensure!(
            version <= MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
            "media item query-filter projection version {version} is newer than supported version {MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION}"
        );
        if mode == MediaItemFacetProjectionMode::EnsureCurrent
            && version == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION
        {
            let actual = sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "WITH value_counts AS (\
                     SELECT item_id, count(*) AS value_count \
                     FROM media_item_query_filter_values GROUP BY item_id\
                 ) \
                 SELECT (SELECT count(*) FROM media_items), \
                        count(*) FILTER (WHERE source.extractor_version = $1), \
                        coalesce(sum(value_counts.value_count), 0)::bigint, \
                        count(*) FILTER (WHERE source.projected_value_count \
                            <> coalesce(value_counts.value_count, 0)) \
                 FROM media_item_query_filter_sources AS source \
                 LEFT JOIN value_counts ON value_counts.item_id = source.item_id",
            )
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
            .fetch_one(&mut **tx)
            .await?;
            if actual.0 == actual.1 && actual.3 == 0 {
                if source_count != actual.0 || value_count != actual.2 {
                    sqlx::query(
                        "UPDATE jellyrin_derived_projection_versions \
                         SET source_item_count = $2, projected_facet_count = $3, \
                             completed_at = CURRENT_TIMESTAMP \
                         WHERE projection_name = $1 \
                           AND (source_item_count, projected_facet_count) \
                               IS DISTINCT FROM ($2, $3)",
                    )
                    .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
                    .bind(actual.0)
                    .bind(actual.2)
                    .execute(&mut **tx)
                    .await?;
                }
                return Ok(MediaItemQueryFilterProjectionReport {
                    rebuilt: false,
                    source_item_count: u64::try_from(actual.0)
                        .context("negative query-filter projection source count")?,
                    projected_value_count: u64::try_from(actual.2)
                        .context("negative query-filter projection value count")?,
                });
            }
        }
    }

    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "LOCK TABLE media_items, media_item_query_filter_sources, \
         media_item_query_filter_values IN SHARE ROW EXCLUSIVE MODE",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM media_item_query_filter_sources")
        .execute(&mut **tx)
        .await?;

    let mut last_item_id = None::<Uuid>;
    let mut source_item_count = 0_u64;
    let mut projected_value_count = 0_u64;
    loop {
        let rows = if let Some(last_item_id) = last_item_id {
            sqlx::query_as::<_, (Uuid, Uuid, String, String, Value, Value)>(
                "SELECT id, virtual_folder_id, path, media_type, media_streams, metadata FROM media_items \
                 WHERE id > $1 ORDER BY id LIMIT $2",
            )
            .bind(last_item_id)
            .bind(FACET_REBUILD_BATCH_SIZE)
            .fetch_all(&mut **tx)
            .await?
        } else {
            sqlx::query_as::<_, (Uuid, Uuid, String, String, Value, Value)>(
                "SELECT id, virtual_folder_id, path, media_type, media_streams, metadata FROM media_items \
                 ORDER BY id LIMIT $1",
            )
            .bind(FACET_REBUILD_BATCH_SIZE)
            .fetch_all(&mut **tx)
            .await?
        };
        if rows.is_empty() {
            break;
        }
        let projections = rows
            .iter()
            .map(
                |(item_id, folder_id, path, media_type, media_streams, metadata)| {
                    let streams = media_streams.as_array().map(Vec::as_slice).unwrap_or(&[]);
                    let projection = (
                        *item_id,
                        *folder_id,
                        extract_media_item_query_filter_projection(
                            MediaItemQueryFilterProjectionSource {
                                path,
                                media_type,
                                media_streams: streams,
                                metadata,
                            },
                        ),
                    );
                    let projected_value_count = i32::try_from(projection.2.values.len())
                        .context("query-filter value count overflow")?;
                    Ok((
                        projection.0,
                        projection.1,
                        projection.2,
                        projected_value_count,
                    ))
                },
            )
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut source_insert = QueryBuilder::<Postgres>::new(
            "INSERT INTO media_item_query_filter_sources (item_id, virtual_folder_id, extractor_version, \
             container_present, container_value, media_type, is_video, has_subtitles, \
             has_trailer, projected_value_count, completed_at) ",
        );
        source_insert.push_values(
            &projections,
            |mut values, (item_id, folder_id, projection, projected_value_count)| {
                values
                    .push_bind(*item_id)
                    .push_bind(*folder_id)
                    .push_bind(projection.extractor_version)
                    .push_bind(projection.features.container_present)
                    .push_bind(&projection.features.container)
                    .push_bind(&projection.features.media_type)
                    .push_bind(projection.features.is_video)
                    .push_bind(projection.features.has_subtitles)
                    .push_bind(projection.features.has_trailer)
                    .push_bind(*projected_value_count)
                    .push("CURRENT_TIMESTAMP");
            },
        );
        source_insert.build().execute(&mut **tx).await?;
        let projected_values = projections
            .iter()
            .flat_map(|(item_id, folder_id, projection, _)| {
                projection
                    .values
                    .iter()
                    .map(move |value| (*item_id, *folder_id, value))
            })
            .collect::<Vec<_>>();
        for chunk in projected_values.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut insert = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_query_filter_values (item_id, virtual_folder_id, value_kind, \
                 display_value, source_key, source_priority, source_position) ",
            );
            insert.push_values(chunk, |mut values, (item_id, folder_id, value)| {
                values
                    .push_bind(*item_id)
                    .push_bind(*folder_id)
                    .push_bind(value.kind.as_str())
                    .push_bind(&value.display_value)
                    .push_bind(&value.source_key)
                    .push_bind(i32::from(value.source_priority))
                    .push_bind(encode_media_item_query_filter_position(&value.position));
            });
            insert.build().execute(&mut **tx).await?;
        }
        source_item_count = source_item_count
            .checked_add(u64::try_from(projections.len()).context("source count overflow")?)
            .context("source count overflow")?;
        projected_value_count = projected_value_count
            .checked_add(u64::try_from(projected_values.len()).context("value count overflow")?)
            .context("value count overflow")?;
        last_item_id = rows.last().map(|row| row.0);
    }
    sqlx::query(
        r#"
        INSERT INTO jellyrin_derived_projection_versions (
            projection_name, extractor_version, completed_at, source_item_count,
            projected_facet_count, projected_alias_count
        ) VALUES ($1, $2, CURRENT_TIMESTAMP, $3, $4, 0)
        ON CONFLICT (projection_name) DO UPDATE SET
            extractor_version = excluded.extractor_version,
            completed_at = excluded.completed_at,
            source_item_count = excluded.source_item_count,
            projected_facet_count = excluded.projected_facet_count,
            projected_alias_count = 0
        "#,
    )
    .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
    .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
    .bind(i64::try_from(source_item_count).context("query-filter source count overflow")?)
    .bind(i64::try_from(projected_value_count).context("query-filter value count overflow")?)
    .execute(&mut **tx)
    .await?;
    Ok(MediaItemQueryFilterProjectionReport {
        rebuilt: true,
        source_item_count,
        projected_value_count,
    })
}

pub async fn ensure_media_item_facet_projection(
    tx: &mut Transaction<'_, Postgres>,
    mode: MediaItemFacetProjectionMode,
) -> anyhow::Result<MediaItemFacetProjectionReport> {
    let marker = sqlx::query_as::<_, (i32, i64, i64, i64)>(
        r#"
        SELECT extractor_version, source_item_count,
               projected_facet_count, projected_alias_count
        FROM jellyrin_derived_projection_versions
        WHERE projection_name = $1
        "#,
    )
    .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((version, source_items, facets, aliases)) = marker {
        anyhow::ensure!(
            version <= MEDIA_ITEM_FACET_PROJECTION_VERSION,
            "media item facet projection version {version} is newer than supported version {MEDIA_ITEM_FACET_PROJECTION_VERSION}"
        );
        if mode == MediaItemFacetProjectionMode::EnsureCurrent
            && version == MEDIA_ITEM_FACET_PROJECTION_VERSION
        {
            return Ok(MediaItemFacetProjectionReport {
                rebuilt: false,
                source_item_count: u64::try_from(source_items)
                    .context("negative media item facet source count")?,
                projected_facet_count: u64::try_from(facets)
                    .context("negative media item facet count")?,
                projected_alias_count: u64::try_from(aliases)
                    .context("negative media item facet alias count")?,
            });
        }
    }

    // Keep the source metadata and both derived tables stable while rebuilding. AccessShare
    // readers continue to see the previous committed projection until this transaction commits.
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut **tx)
        .await
        .context("failed to configure media item facet projection lock timeout")?;
    sqlx::query(
        "LOCK TABLE media_items, media_item_facets, media_item_facet_aliases, \
         media_item_genre_selectors, media_item_filter_selectors, media_item_upcoming_dates \
         IN SHARE ROW EXCLUSIVE MODE",
    )
    .execute(&mut **tx)
    .await
    .context("failed to lock media item facet projection tables")?;
    sqlx::query("DELETE FROM media_item_facets")
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_genre_selectors")
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_upcoming_dates")
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_filter_selectors")
        .execute(&mut **tx)
        .await?;

    let mut last_item_id = None::<Uuid>;
    let mut source_item_count = 0_u64;
    let mut projected_facet_count = 0_u64;
    let mut projected_alias_count = 0_u64;
    loop {
        let rows = if let Some(last_item_id) = last_item_id {
            sqlx::query_as::<_, (Uuid, Value)>(
                "SELECT id, metadata FROM media_items WHERE id > $1 ORDER BY id LIMIT $2",
            )
            .bind(last_item_id)
            .bind(FACET_REBUILD_BATCH_SIZE)
            .fetch_all(&mut **tx)
            .await?
        } else {
            sqlx::query_as::<_, (Uuid, Value)>(
                "SELECT id, metadata FROM media_items ORDER BY id LIMIT $1",
            )
            .bind(FACET_REBUILD_BATCH_SIZE)
            .fetch_all(&mut **tx)
            .await?
        };
        if rows.is_empty() {
            break;
        }
        source_item_count = source_item_count.saturating_add(rows.len() as u64);
        let mut facets = Vec::new();
        for (item_id, metadata) in &rows {
            for facet in extract_media_item_facets(metadata) {
                let position =
                    i32::try_from(facet.position).context("media item facet position overflow")?;
                facets.push((*item_id, facet, position));
            }
        }
        projected_facet_count = projected_facet_count.saturating_add(facets.len() as u64);
        for facet_chunk in facets.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_facets (\
                 item_id, facet_kind, normalized_value, display_value, stable_id, \
                 position, payload) ",
            );
            query.push_values(facet_chunk, |mut values, (item_id, facet, position)| {
                values
                    .push_bind(*item_id)
                    .push_bind(facet.kind.as_str())
                    .push_bind(&facet.normalized_value)
                    .push_bind(&facet.display_value)
                    .push_bind(&facet.stable_id)
                    .push_bind(*position)
                    .push_bind(&facet.payload);
            });
            query.build().execute(&mut **tx).await?;
        }
        let aliases = facets
            .iter()
            .flat_map(|(item_id, facet, _)| {
                facet.aliases.iter().map(move |alias| {
                    (
                        *item_id,
                        facet.kind,
                        facet.normalized_value.as_str(),
                        alias.as_str(),
                    )
                })
            })
            .collect::<Vec<_>>();
        projected_alias_count = projected_alias_count.saturating_add(aliases.len() as u64);
        for alias_chunk in aliases.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_facet_aliases (\
                 item_id, facet_kind, normalized_value, entity_id) ",
            );
            query.push_values(
                alias_chunk,
                |mut values, (item_id, kind, normalized_value, entity_id)| {
                    values
                        .push_bind(*item_id)
                        .push_bind(kind.as_str())
                        .push_bind(*normalized_value)
                        .push_bind(*entity_id);
                },
            );
            query.build().execute(&mut **tx).await?;
        }
        let genre_selectors = rows
            .iter()
            .flat_map(|(item_id, metadata)| {
                extract_media_item_genre_selectors(metadata)
                    .into_iter()
                    .map(move |selector| (*item_id, selector))
            })
            .collect::<Vec<_>>();
        for selector_chunk in genre_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_genre_selectors (item_id, selector) ",
            );
            query.push_values(selector_chunk, |mut values, (item_id, selector)| {
                values.push_bind(*item_id).push_bind(selector);
            });
            query.build().execute(&mut **tx).await?;
        }
        let filter_selectors = rows
            .iter()
            .flat_map(|(item_id, metadata)| {
                extract_media_item_filter_selectors(metadata)
                    .into_iter()
                    .map(move |(kind, selector)| (*item_id, kind, selector))
            })
            .collect::<Vec<_>>();
        for selector_chunk in filter_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_filter_selectors \
                 (item_id, selector_kind, selector) ",
            );
            query.push_values(selector_chunk, |mut values, (item_id, kind, selector)| {
                values
                    .push_bind(*item_id)
                    .push_bind(kind.as_str())
                    .push_bind(selector);
            });
            query.build().execute(&mut **tx).await?;
        }
        let upcoming_dates = rows
            .iter()
            .filter_map(|(item_id, metadata)| {
                upcoming_media_item_premiere_parts(metadata)
                    .map(|(unix_seconds, nanosecond)| (*item_id, unix_seconds, nanosecond))
            })
            .collect::<Vec<_>>();
        for date_chunk in upcoming_dates.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_upcoming_dates \
                 (item_id, unix_seconds, nanosecond) ",
            );
            query.push_values(
                date_chunk,
                |mut values, (item_id, unix_seconds, nanosecond)| {
                    values
                        .push_bind(*item_id)
                        .push_bind(*unix_seconds)
                        .push_bind(*nanosecond);
                },
            );
            query.build().execute(&mut **tx).await?;
        }
        last_item_id = rows.last().map(|(item_id, _)| *item_id);
    }

    sqlx::query(
        r#"
        INSERT INTO jellyrin_derived_projection_versions (
            projection_name, extractor_version, completed_at, source_item_count,
            projected_facet_count, projected_alias_count
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (projection_name) DO UPDATE SET
            extractor_version = excluded.extractor_version,
            completed_at = excluded.completed_at,
            source_item_count = excluded.source_item_count,
            projected_facet_count = excluded.projected_facet_count,
            projected_alias_count = excluded.projected_alias_count
        "#,
    )
    .bind(MEDIA_ITEM_FACET_PROJECTION_NAME)
    .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
    .bind(OffsetDateTime::now_utc())
    .bind(i64::try_from(source_item_count).context("media item facet source count overflow")?)
    .bind(i64::try_from(projected_facet_count).context("media item facet count overflow")?)
    .bind(i64::try_from(projected_alias_count).context("media item facet alias count overflow")?)
    .execute(&mut **tx)
    .await?;

    Ok(MediaItemFacetProjectionReport {
        rebuilt: true,
        source_item_count,
        projected_facet_count,
        projected_alias_count,
    })
}

impl PostgresDatabase {
    pub async fn catalog_sync_diagnostics(&self) -> anyhow::Result<CatalogSyncDiagnostics> {
        let counts = sqlx::query_as::<_, CatalogSyncCountsRow>(
            r#"
            SELECT COUNT(*) AS total,
                   COUNT(*) FILTER (WHERE status = 'running') AS running,
                   COUNT(*) FILTER (WHERE status = 'completed') AS completed,
                   COUNT(*) FILTER (WHERE status = 'failed') AS failed
            FROM catalog_sync_runs
            "#,
        )
        .fetch_one(&self.pool)
        .await?;
        let last_run = sqlx::query_as::<_, (String, i64, OffsetDateTime, Option<OffsetDateTime>)>(
            r#"
            SELECT status, item_count, started_at, completed_at
            FROM catalog_sync_runs
            ORDER BY started_at DESC, id DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(
            |(status, item_count, started_at, completed_at)| CatalogSyncRunDiagnostics {
                status,
                item_count: nonnegative_count(item_count),
                started_at,
                completed_at,
                duration_millis: catalog_sync_duration_millis(started_at, completed_at),
            },
        );
        Ok(CatalogSyncDiagnostics {
            total: nonnegative_count(counts.total),
            running: nonnegative_count(counts.running),
            completed: nonnegative_count(counts.completed),
            failed: nonnegative_count(counts.failed),
            last_run,
        })
    }

    pub async fn virtual_folders(&self) -> anyhow::Result<Vec<VirtualFolder>> {
        let rows = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations, created_at, updated_at
            FROM virtual_folders
            ORDER BY lower(name), name
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_virtual_folder(
        &self,
        name: &str,
        collection_type: Option<&str>,
        locations: Vec<String>,
    ) -> anyhow::Result<VirtualFolder> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );

        let row = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $5)
            ON CONFLICT ((lower(name))) DO UPDATE SET
                collection_type = excluded.collection_type,
                locations = excluded.locations,
                updated_at = excluded.updated_at
            RETURNING id, name, collection_type, locations, created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(trimmed_name)
        .bind(trimmed_optional_str(collection_type))
        .bind(serde_json::to_value(normalized_locations(locations))?)
        .bind(OffsetDateTime::now_utc())
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn add_virtual_folder_path(&self, name: &str, path: &str) -> anyhow::Result<()> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );
        let trimmed_path = path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );

        // Serialize read/modify/write updates for the same folder. Without the row lock, two
        // concurrent path additions can both read the same JSON array and silently lose one.
        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Api);
        let transaction_result = self.pool.begin().await;
        acquire.finish_result(&transaction_result);
        let mut transaction = transaction_result?;
        let folder = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations, created_at, updated_at
            FROM virtual_folders
            WHERE lower(name) = lower($1)
            FOR UPDATE
            "#,
        )
        .bind(trimmed_name)
        .fetch_optional(&mut *transaction)
        .await?
        .context("virtual folder not found")?;

        let mut locations = parse_locations(folder.locations)?;
        if !locations.iter().any(|location| location == trimmed_path) {
            locations.push(trimmed_path.to_owned());
            sqlx::query(
                r#"
                UPDATE virtual_folders
                SET locations = $1, updated_at = $2
                WHERE id = $3
                "#,
            )
            .bind(serde_json::to_value(normalized_locations(locations))?)
            .bind(OffsetDateTime::now_utc())
            .bind(folder.id)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn rename_virtual_folder(&self, name: &str, new_name: &str) -> anyhow::Result<bool> {
        let trimmed_name = name.trim();
        let trimmed_new_name = new_name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );
        anyhow::ensure!(
            !trimmed_new_name.is_empty(),
            "virtual folder new name must not be empty"
        );

        let result = sqlx::query(
            r#"
            UPDATE virtual_folders
            SET name = $1, updated_at = $2
            WHERE lower(name) = lower($3)
            "#,
        )
        .bind(trimmed_new_name)
        .bind(OffsetDateTime::now_utc())
        .bind(trimmed_name)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_virtual_folder_path(
        &self,
        name: &str,
        path: &str,
        new_path: &str,
    ) -> anyhow::Result<bool> {
        let trimmed_path = path.trim();
        let trimmed_new_path = new_path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );
        anyhow::ensure!(
            !trimmed_new_path.is_empty(),
            "virtual folder new path must not be empty"
        );

        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations, created_at, updated_at
            FROM virtual_folders
            WHERE lower(name) = lower($1)
            FOR UPDATE
            "#,
        )
        .bind(name.trim())
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(false);
        };

        let mut locations = parse_locations(row.locations)?;
        let Some(index) = locations
            .iter()
            .position(|location| location == trimmed_path)
        else {
            return Ok(false);
        };
        locations[index] = trimmed_new_path.to_owned();
        sqlx::query(
            r#"
            UPDATE virtual_folders
            SET locations = $1, updated_at = $2
            WHERE id = $3
            "#,
        )
        .bind(serde_json::to_value(normalized_locations(locations))?)
        .bind(OffsetDateTime::now_utc())
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn remove_virtual_folder_path(&self, name: &str, path: &str) -> anyhow::Result<bool> {
        let trimmed_path = path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );

        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations, created_at, updated_at
            FROM virtual_folders
            WHERE lower(name) = lower($1)
            FOR UPDATE
            "#,
        )
        .bind(name.trim())
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(false);
        };

        let mut locations = parse_locations(row.locations)?;
        let original_len = locations.len();
        locations.retain(|location| location != trimmed_path);
        if locations.len() == original_len {
            return Ok(false);
        }

        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            UPDATE virtual_folders
            SET locations = $1, updated_at = $2
            WHERE id = $3
            "#,
        )
        .bind(serde_json::to_value(normalized_locations(locations))?)
        .bind(now)
        .bind(row.id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM media_items
            WHERE virtual_folder_id = $1
              AND (path = $2 OR path LIKE $3 ESCAPE '\')
            "#,
        )
        .bind(row.id)
        .bind(trimmed_path)
        .bind(escaped_path_prefix(trimmed_path))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(true)
    }

    pub async fn delete_virtual_folder(&self, name: &str) -> anyhow::Result<bool> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "virtual folder name must not be empty"
        );
        let result = sqlx::query("DELETE FROM virtual_folders WHERE lower(name) = lower($1)")
            .bind(trimmed_name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn media_items(&self) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL
            ORDER BY lower(name), name
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_items_by_collection_type(
        &self,
        collection_type: &str,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL AND collection_type = $1
            ORDER BY lower(name), name
            "#,
        )
        .bind(collection_type)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_items_by_name_search(
        &self,
        search_term: &str,
        collection_types: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogNameSearch, DatabasePoolRole::Api);
        let result = self
            .media_items_by_name_search_unobserved(search_term, collection_types, limit)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_by_name_search_unobserved(
        &self,
        search_term: &str,
        collection_types: &[&str],
        limit: usize,
    ) -> anyhow::Result<Vec<MediaItem>> {
        let search_term = search_term.trim();
        if search_term.is_empty() || collection_types.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let collection_types = collection_types
            .iter()
            .map(|value| value.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL
              AND lower(name) LIKE lower($1)
              AND lower(collection_type) = ANY($2)
            ORDER BY lower(name), name
            LIMIT $3
            "#,
        )
        .bind(format!("%{search_term}%"))
        .bind(collection_types)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Returns a bounded catalog page and an exact total from one repeatable-read snapshot.
    pub async fn media_item_catalog_page(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogPage> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api);
        let result = self.media_item_catalog_page_unobserved(query).await;
        observation.finish_result(&result, |page| {
            u64::try_from(page.items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_catalog_page_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogPage> {
        super::validate_media_item_catalog_query(query)?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;

        let mut count = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint ");
        push_postgres_catalog_from(&mut count, query);
        push_postgres_catalog_filters(&mut count, query);
        let total_record_count = count
            .build_query_scalar::<i64>()
            .fetch_one(&mut *transaction)
            .await?;
        let total_record_count =
            usize::try_from(total_record_count).context("media catalog count exceeded usize")?;

        let effective_limit = query.limit.min(MEDIA_ITEM_CATALOG_MAX_PAGE_SIZE);
        if effective_limit == 0 {
            transaction.commit().await?;
            return Ok(MediaItemCatalogPage {
                items: Vec::new(),
                total_record_count,
                start_index: query.start_index,
            });
        }

        let mut page = QueryBuilder::<Postgres>::new(
            r#"SELECT item.id, item.virtual_folder_id, item.name, item.path,
                      item.media_type, item.collection_type, item.file_size,
                      item.runtime_ticks, item.bitrate, item.width, item.height,
                      item.media_streams, item.metadata, item.created_at, item.updated_at,
                      playback.user_id AS playback_user_id,
                      playback.item_id AS playback_item_id,
                      playback.media_source_id AS playback_media_source_id,
                      playback.audio_stream_index AS playback_audio_stream_index,
                      playback.subtitle_stream_index AS playback_subtitle_stream_index,
                      playback.position_ticks AS playback_position_ticks,
                      playback.is_paused AS playback_is_paused,
                      playback.played AS playback_played,
                      playback.is_favorite AS playback_is_favorite,
                      playback.rating AS playback_rating,
                      playback.updated_at AS playback_updated_at "#,
        );
        push_postgres_catalog_from(&mut page, query);
        push_postgres_catalog_filters(&mut page, query);
        push_postgres_catalog_order(&mut page, query);
        page.push(" LIMIT ")
            .push_bind(i64::try_from(effective_limit)?);
        page.push(" OFFSET ")
            .push_bind(i64::try_from(query.start_index)?);

        let rows = page
            .build_query_as::<PostgresMediaItemCatalogRow>()
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;

        Ok(MediaItemCatalogPage {
            items: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total_record_count,
            start_index: query.start_index,
        })
    }

    pub async fn media_item_catalog_counts(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogCounts> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogCounts, DatabasePoolRole::Api);
        let result = self.media_item_catalog_counts_unobserved(query).await;
        observation.finish_result(&result, |counts| counts.item_count);
        result
    }

    async fn media_item_catalog_counts_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
    ) -> anyhow::Result<MediaItemCatalogCounts> {
        super::validate_media_item_catalog_query(query)?;
        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Api);
        let transaction_result = self.pool.begin().await;
        acquire.finish_result(&transaction_result);
        let mut transaction = transaction_result?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;

        let mut aggregate =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint AS item_count, ");
        aggregate
            .push("COUNT(*) FILTER (WHERE (")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'movie')::bigint AS movie_count, COUNT(*) FILTER (WHERE (")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'episode')::bigint AS episode_count, COUNT(*) FILTER (WHERE (")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'audio')::bigint AS song_count, COUNT(*) FILTER (WHERE (")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'musicvideo')::bigint AS music_video_count, COUNT(*) FILTER (WHERE (")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = 'book')::bigint AS book_count ");
        push_postgres_catalog_from(&mut aggregate, query);
        push_postgres_catalog_filters(&mut aggregate, query);
        let row = aggregate
            .build_query_as::<PostgresCatalogAggregateRow>()
            .fetch_one(&mut *transaction)
            .await?;

        let mut projection = QueryBuilder::<Postgres>::new("SELECT item.name, item.path, ");
        projection.push(POSTGRES_MEDIA_ITEM_TYPE_SQL).push(
            " AS item_type, CAST(item.metadata -> 'Album' AS text) AS album, \
                   CAST(item.metadata -> 'AlbumName' AS text) AS album_name, \
                   CAST(item.metadata -> 'Artists' AS text) AS artists, \
                   CAST(item.metadata -> 'AlbumArtists' AS text) AS album_artists, \
                   CAST(item.metadata -> 'RemoteTrailers' AS text) AS remote_trailers, \
                   CAST(item.metadata -> 'Trailers' AS text) AS trailers ",
        );
        push_postgres_catalog_from(&mut projection, query);
        push_postgres_catalog_filters(&mut projection, query);
        projection
            .push(" AND ((")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(
                ") = 'episode' OR item.metadata ?| ARRAY['Album', 'AlbumName', 'Artists', \
                   'AlbumArtists', 'RemoteTrailers', 'Trailers']::text[])",
            );
        let mut series_names = BTreeSet::new();
        let mut metadata_counts = super::CatalogMetadataCountAccumulator::default();
        {
            let mut rows = projection
                .build_query_as::<PostgresCatalogCountProjectionRow>()
                .fetch(&mut *transaction);
            while let Some(projected) = rows.try_next().await? {
                if projected.item_type == "episode" {
                    series_names.insert(
                        tv_episode_path_info(&projected.name, &projected.path)
                            .series_name
                            .to_ascii_lowercase(),
                    );
                }
                metadata_counts.add_album(projected.album.as_deref())?;
                metadata_counts.add_album(projected.album_name.as_deref())?;
                metadata_counts.add_artist(projected.artists.as_deref())?;
                metadata_counts.add_artist(projected.album_artists.as_deref())?;
                metadata_counts.add_trailers(projected.remote_trailers.as_deref())?;
                metadata_counts.add_trailers(projected.trailers.as_deref())?;
            }
        }
        transaction.commit().await?;

        Ok(MediaItemCatalogCounts {
            movie_count: nonnegative_catalog_count(row.movie_count, "movie")?,
            series_count: u64::try_from(series_names.len()).context("series count exceeded u64")?,
            episode_count: nonnegative_catalog_count(row.episode_count, "episode")?,
            artist_count: u64::try_from(metadata_counts.artists.len())
                .context("artist count exceeded u64")?,
            trailer_count: metadata_counts.trailers,
            song_count: nonnegative_catalog_count(row.song_count, "song")?,
            album_count: u64::try_from(metadata_counts.albums.len())
                .context("album count exceeded u64")?,
            music_video_count: nonnegative_catalog_count(row.music_video_count, "music video")?,
            book_count: nonnegative_catalog_count(row.book_count, "book")?,
            item_count: nonnegative_catalog_count(row.item_count, "item")?,
        })
    }

    /// Returns the complete set of values exposed by `/Items/Filters` for the catalog selection.
    ///
    /// Pagination and ordering are deliberately absent: the selected CTE applies the same catalog
    /// joins and predicates as [`Self::media_item_catalog_page`], then PostgreSQL expands and
    /// de-duplicates metadata and stream values as a set.
    pub async fn media_item_query_filter_values(
        &self,
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogFilterSummary,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_item_query_filter_values_unobserved(query, selection)
            .await;
        observation.finish_result(&result, postgres_query_filter_value_count);
        result
    }

    async fn media_item_query_filter_values_unobserved(
        &self,
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        super::validate_media_item_catalog_query(query)?;
        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Api);
        let connection_result = self.pool.acquire().await;
        acquire.finish_result(&connection_result);
        let mut connection = connection_result?;
        let mut transaction = connection.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;

        let mut coverage = QueryBuilder::<Postgres>::new(
            "WITH selected AS MATERIALIZED (SELECT item.id, item.virtual_folder_id ",
        );
        push_postgres_catalog_from(&mut coverage, query);
        push_postgres_catalog_filters(&mut coverage, query);
        if query.virtual_folder_ids.len() == 1 {
            coverage.push(
                "), value_counts AS (\
                 SELECT projected.item_id, projected.virtual_folder_id, count(*) AS value_count \
                 FROM media_item_query_filter_values AS projected \
                 WHERE projected.virtual_folder_id = ANY(",
            );
            coverage.push_bind(&query.virtual_folder_ids);
            coverage.push(
                ") GROUP BY projected.item_id, projected.virtual_folder_id\
                 ) SELECT (SELECT count(*) FROM selected) = count(source.item_id) \
                    AND coalesce(bool_and(source.projected_value_count \
                        = coalesce(value_counts.value_count, 0)), TRUE) \
                 FROM selected \
                 JOIN media_item_query_filter_sources AS source \
                   ON source.item_id = selected.id \
                  AND source.virtual_folder_id = selected.virtual_folder_id \
                  AND source.extractor_version = ",
            );
            coverage.push_bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION);
            coverage.push(
                " LEFT JOIN value_counts \
                    ON value_counts.item_id = selected.id \
                   AND value_counts.virtual_folder_id = selected.virtual_folder_id",
            );
        } else {
            coverage.push(
                ") SELECT NOT EXISTS (\
             SELECT 1 FROM selected \
             LEFT JOIN media_item_query_filter_sources AS source \
               ON source.item_id = selected.id \
              AND source.virtual_folder_id = selected.virtual_folder_id \
              AND source.extractor_version = ",
            );
            coverage.push_bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION);
            coverage.push(
                " WHERE source.item_id IS NULL OR source.projected_value_count <> (\
                SELECT count(*) FROM media_item_query_filter_values AS projected \
                WHERE projected.item_id = selected.id \
                  AND projected.virtual_folder_id = selected.virtual_folder_id))",
            );
        }
        let covered = coverage
            .build_query_scalar::<bool>()
            .fetch_one(&mut *transaction)
            .await?;
        let result = if covered {
            sqlx::query("SET LOCAL work_mem = '32MB'")
                .execute(&mut *transaction)
                .await?;
            sqlx::query("SET LOCAL jit = off")
                .execute(&mut *transaction)
                .await?;
            Self::media_item_query_filter_values_projected(query, selection, &mut transaction).await
        } else {
            let mut values =
                Self::media_item_query_filter_values_legacy(query, &mut transaction).await?;
            values.retain_selection(selection);
            Ok(values)
        };
        transaction.commit().await?;
        result
    }

    async fn media_item_query_filter_values_projected(
        query: &MediaItemCatalogQuery,
        selection: MediaItemQueryFilterSelection,
        connection: &mut PgConnection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let mut values = QueryBuilder::<Postgres>::new(
            "WITH selected AS MATERIALIZED (SELECT item.id, item.virtual_folder_id, lower(item.name) AS item_sort ",
        );
        push_postgres_catalog_from(&mut values, query);
        push_postgres_catalog_filters(&mut values, query);
        values.push(
            r#"), candidates(kind, normalized_value, display_value, item_sort, item_id,
                             key_priority, position) AS (
                SELECT projected.value_kind, lower(btrim(projected.display_value)),
                       projected.display_value, selected.item_sort, selected.id,
                       projected.source_priority, projected.source_position
            "#,
        );
        if query.virtual_folder_ids.len() == 1 {
            values.push(
                " FROM media_item_query_filter_values AS projected \
                  JOIN selected ON selected.id = projected.item_id \
                   AND selected.virtual_folder_id = projected.virtual_folder_id \
                  WHERE projected.virtual_folder_id = ANY(",
            );
            values.push_bind(&query.virtual_folder_ids);
            values.push(") AND projected.value_kind = ANY(");
            values.push_bind(selection.projected_fields());
            values.push(")");
        } else {
            values.push(
                r#" FROM selected
                    CROSS JOIN LATERAL (
                        SELECT value.value_kind, value.display_value, value.source_priority,
                               value.source_position
                        FROM media_item_query_filter_values AS value
                        WHERE value.item_id = selected.id
                          AND value.virtual_folder_id = selected.virtual_folder_id
                          AND value.value_kind = ANY("#,
            );
            values.push_bind(selection.projected_fields());
            values.push(
                r#")
                        OFFSET 0
                    ) AS projected"#,
            );
        }
        values.push(
            r#"), ranked AS (
                SELECT kind, normalized_value, display_value,
                       row_number() OVER (
                           PARTITION BY kind, normalized_value
                           ORDER BY item_sort COLLATE "C", item_id, key_priority, position
                       ) AS spelling_rank
                FROM candidates
            "#,
        );
        if selection.includes_scalars() {
            values.push(
                r#"), scalar_summary AS (
                    SELECT COALESCE(
                               array_agg(DISTINCT lower(source.container_value))
                                   FILTER (WHERE source.container_present),
                               ARRAY[]::text[]
                           ) AS containers,
                           COALESCE(
                               array_agg(DISTINCT source.media_type),
                               ARRAY[]::text[]
                           ) AS media_types,
                           COALESCE(bool_or(source.is_video), FALSE) AS has_video,
                           COALESCE(bool_or(source.has_subtitles), FALSE) AS has_subtitles,
                           COALESCE(bool_or(source.has_trailer), FALSE) AS has_trailer
                    FROM selected
                    JOIN media_item_query_filter_sources AS source
                      ON source.item_id = selected.id
                     AND source.virtual_folder_id = selected.virtual_folder_id
                )
                "#,
            );
        } else {
            values.push(")");
        }
        values.push(
            r#", result AS (
                SELECT kind, normalized_value, display_value
                FROM ranked WHERE spelling_rank = 1
            "#,
        );
        if selection.includes_scalars() {
            values.push(
                r#" UNION ALL
                    SELECT 'containers', container.value, container.value
                    FROM scalar_summary
                    CROSS JOIN LATERAL unnest(scalar_summary.containers) AS container(value)
                    UNION ALL
                    SELECT 'media_types', media_type.value, media_type.value
                    FROM scalar_summary
                    CROSS JOIN LATERAL unnest(scalar_summary.media_types) AS media_type(value)
                    UNION ALL
                    SELECT 'video_types', 'videofile', 'VideoFile'
                    FROM scalar_summary WHERE scalar_summary.has_video
                    UNION ALL
                    SELECT 'has_subtitles', 'true', 'true'
                    FROM scalar_summary WHERE scalar_summary.has_subtitles
                    UNION ALL
                    SELECT 'has_trailer', 'true', 'true'
                    FROM scalar_summary WHERE scalar_summary.has_trailer
                "#,
            );
        }
        values.push(
            r#")
                SELECT kind, display_value FROM result
                ORDER BY kind COLLATE "C", normalized_value COLLATE "C"
            "#,
        );
        let rows = values
            .build_query_as::<PostgresQueryFilterValueRow>()
            .fetch_all(connection)
            .await?;
        postgres_query_filter_values_from_rows(rows)
    }

    async fn media_item_query_filter_values_legacy(
        query: &MediaItemCatalogQuery,
        connection: &mut PgConnection,
    ) -> anyhow::Result<MediaItemQueryFilterValues> {
        let mut values = QueryBuilder::<Postgres>::new(
            "WITH RECURSIVE selected AS MATERIALIZED (\
             SELECT item.id, lower(item.name) AS item_sort, item.path, item.media_type, \
                    item.media_streams, item.metadata ",
        );
        push_postgres_catalog_from(&mut values, query);
        push_postgres_catalog_filters(&mut values, query);
        values.push(
            r#"),
            raw_metadata(
                kind, value, item_sort, item_id, key_priority, position
            ) AS (
                SELECT mapping.kind, selected.metadata -> mapping.key,
                       selected.item_sort, selected.id, mapping.key_priority,
                       ARRAY[]::bigint[]
                FROM selected
                CROSS JOIN (VALUES
                    ('albums', 'Album', 0),
                    ('albums', 'AlbumName', 1),
                    ('genres', 'Genres', 0),
                    ('official_ratings', 'OfficialRating', 0),
                    ('official_ratings', 'OfficialRatings', 1),
                    ('series_statuses', 'SeriesStatus', 0),
                    ('staff_names', 'People', 0),
                    ('staff_names', 'SeriesPeople', 1),
                    ('studios', 'Studios', 0),
                    ('years', 'Years', 1)
                ) AS mapping(kind, key, key_priority)
                WHERE selected.metadata ? mapping.key
                UNION ALL
                SELECT 'trailer_source', field.value, selected.item_sort, selected.id,
                       CASE lower(field.key)
                           WHEN 'remotetrailers' THEN 0 ELSE 1
                       END,
                       ARRAY[]::bigint[]
                FROM selected
                CROSS JOIN LATERAL jsonb_each(selected.metadata) AS field(key, value)
                WHERE lower(field.key) IN ('remotetrailers', 'trailers')
            ),
            expanded_metadata(
                kind, value, item_sort, item_id, key_priority, position
            ) AS (
                SELECT kind, value, item_sort, item_id, key_priority, position
                FROM raw_metadata
                UNION ALL
                SELECT expanded_metadata.kind, element.value,
                       expanded_metadata.item_sort, expanded_metadata.item_id,
                       expanded_metadata.key_priority,
                       expanded_metadata.position || element.ordinality::bigint
                FROM expanded_metadata
                CROSS JOIN LATERAL jsonb_array_elements(
                    CASE WHEN jsonb_typeof(expanded_metadata.value) = 'array'
                         THEN expanded_metadata.value ELSE '[]'::jsonb END
                ) WITH ORDINALITY AS element(value, ordinality)
            ),
            metadata_values(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT kind,
                       btrim(CASE jsonb_typeof(value)
                           WHEN 'string' THEN value #>> '{}'
                           WHEN 'number' THEN value #>> '{}'
                           WHEN 'object' THEN CASE
                               WHEN jsonb_typeof(value -> 'Name') = 'string'
                               THEN value ->> 'Name'
                           END
                           ELSE NULL
                       END),
                       item_sort, item_id, key_priority, position, FALSE
                FROM expanded_metadata
                WHERE kind <> 'trailer_source'
            ),
            projected_metadata_values(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT CASE facet.facet_kind
                           WHEN 'music_artist' THEN 'artists'
                           WHEN 'music_album_artist' THEN 'artists'
                           WHEN 'tag' THEN 'tags'
                           WHEN 'year' THEN 'years'
                       END,
                       facet.display_value, selected.item_sort, selected.id,
                       CASE facet.facet_kind
                           WHEN 'music_album_artist' THEN 1 ELSE 0
                       END,
                       ARRAY[facet.position::bigint], FALSE
                FROM selected
                JOIN media_item_facets AS facet ON facet.item_id = selected.id
                WHERE (
                    (facet.facet_kind = 'music_artist' AND selected.metadata ? 'Artists')
                    OR (facet.facet_kind = 'music_album_artist'
                        AND selected.metadata ? 'AlbumArtists')
                    OR (facet.facet_kind = 'tag' AND selected.metadata ? 'Tags')
                    OR (facet.facet_kind = 'year'
                        AND selected.metadata ? 'ProductionYear')
                )
            ),
            stream_values(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT CASE lower(stream.value ->> 'Type')
                           WHEN 'audio' THEN 'audio_languages'
                           WHEN 'subtitle' THEN 'subtitle_languages'
                       END,
                       CASE lower(btrim(stream.value ->> 'Language'))
                           WHEN 'fre' THEN 'fra'
                           WHEN 'ger' THEN 'deu'
                           ELSE btrim(stream.value ->> 'Language')
                       END,
                       selected.item_sort, selected.id, 0,
                       ARRAY[stream.ordinality::bigint], FALSE
                FROM selected
                CROSS JOIN LATERAL jsonb_array_elements(selected.media_streams)
                    WITH ORDINALITY AS stream(value, ordinality)
                WHERE jsonb_typeof(stream.value -> 'Type') = 'string'
                  AND jsonb_typeof(stream.value -> 'Language') = 'string'
                  AND lower(stream.value ->> 'Type') IN ('audio', 'subtitle')
                  AND btrim(stream.value ->> 'Language') <> ''
                  AND lower(btrim(stream.value ->> 'Language')) <> 'und'
            ),
            item_values(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT 'containers', lower((regexp_match(
                           selected.path, '\.([^./]*)$'))[1]),
                       selected.item_sort, selected.id, 0, ARRAY[]::bigint[], TRUE
                FROM selected
                WHERE selected.path ~ '\.[^./]*$'
                  AND selected.path !~ '(^|/)\.[^./]*$'
                  AND selected.path !~ '(^|/)\.\.?$'
                UNION ALL
                SELECT 'media_types', selected.media_type, selected.item_sort, selected.id,
                       0, ARRAY[]::bigint[], TRUE
                FROM selected
                UNION ALL
                SELECT 'video_types', 'VideoFile', selected.item_sort, selected.id,
                       0, ARRAY[]::bigint[], FALSE
                FROM selected
                WHERE lower(selected.media_type) = 'video'
                UNION ALL
                SELECT 'has_subtitles', 'true', selected.item_sort, selected.id,
                       0, ARRAY[]::bigint[], FALSE
                FROM selected
                WHERE EXISTS (
                    SELECT 1
                    FROM jsonb_array_elements(selected.media_streams) AS stream(value)
                    WHERE jsonb_typeof(stream.value -> 'Type') = 'string'
                      AND lower(stream.value ->> 'Type') = 'subtitle'
                )
            ),
            trailer_value(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT 'has_trailer', 'true', item_sort, item_id, key_priority, position,
                       FALSE
                FROM expanded_metadata
                WHERE kind = 'trailer_source'
                  AND (
                    (jsonb_typeof(value) = 'string' AND btrim(value #>> '{}') <> '')
                    OR (jsonb_typeof(value) = 'object' AND CASE
                        WHEN value ? 'Url' THEN jsonb_typeof(value -> 'Url') = 'string'
                                                 AND btrim(value ->> 'Url') <> ''
                        WHEN value ? 'url' THEN jsonb_typeof(value -> 'url') = 'string'
                                                 AND btrim(value ->> 'url') <> ''
                        WHEN value ? 'Path' THEN jsonb_typeof(value -> 'Path') = 'string'
                                                  AND btrim(value ->> 'Path') <> ''
                        WHEN value ? 'path' THEN jsonb_typeof(value -> 'path') = 'string'
                                                  AND btrim(value ->> 'path') <> ''
                        ELSE FALSE
                    END)
                  )
            ),
            candidate_values(
                kind, display_value, item_sort, item_id, key_priority, position,
                preserve_empty
            ) AS (
                SELECT * FROM metadata_values
                UNION ALL
                SELECT * FROM projected_metadata_values
                UNION ALL
                SELECT * FROM stream_values
                UNION ALL
                SELECT * FROM item_values
                UNION ALL
                SELECT * FROM trailer_value
            ),
            ranked_values AS (
                SELECT kind, display_value,
                       ROW_NUMBER() OVER (
                           PARTITION BY kind, CASE WHEN kind = 'media_types'
                               THEN display_value ELSE lower(btrim(display_value)) END
                           ORDER BY item_sort COLLATE "C", item_id, key_priority, position
                       ) AS spelling_rank
                FROM candidate_values
                WHERE display_value IS NOT NULL
                  AND (preserve_empty OR display_value <> '')
            )
            SELECT kind, display_value
            FROM ranked_values
            WHERE spelling_rank = 1
            ORDER BY kind COLLATE "C", (CASE WHEN kind = 'media_types'
                THEN display_value ELSE lower(btrim(display_value)) END) COLLATE "C"
            "#,
        );

        let rows = values
            .build_query_as::<PostgresQueryFilterValueRow>()
            .fetch_all(connection)
            .await?;
        postgres_query_filter_values_from_rows(rows)
    }

    pub async fn tv_series_lookup_candidates(&self) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemCatalogRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams, item.metadata, item.created_at, item.updated_at,
                   NULL::uuid AS playback_user_id,
                   NULL::uuid AS playback_item_id,
                   NULL::text AS playback_media_source_id,
                   NULL::bigint AS playback_audio_stream_index,
                   NULL::bigint AS playback_subtitle_stream_index,
                   NULL::bigint AS playback_position_ticks,
                   NULL::boolean AS playback_is_paused,
                   NULL::boolean AS playback_played,
                   NULL::boolean AS playback_is_favorite,
                   NULL::double precision AS playback_rating,
                   NULL::timestamptz AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) = ANY(ARRAY['tvshows', 'tvshow', 'series']::text[])
            ORDER BY lower(item.name), item.name
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn tv_series_catalog_page(
        &self,
        virtual_folder_id: Option<Uuid>,
        start_index: usize,
        limit: usize,
    ) -> anyhow::Result<Option<TvSeriesCatalogPage>> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let projection_covered: bool = sqlx::query_scalar(
            r#"
            SELECT CASE
                WHEN $1::uuid IS NOT NULL THEN EXISTS (
                    SELECT 1
                    FROM media_item_tv_series_coverage AS coverage
                    WHERE coverage.virtual_folder_id = $1
                      AND coverage.projection_version = $2
                )
                ELSE NOT EXISTS (
                    SELECT 1
                    FROM virtual_folders AS folder
                    LEFT JOIN media_item_tv_series_coverage AS coverage
                      ON coverage.virtual_folder_id = folder.id
                     AND coverage.projection_version = $2
                    WHERE lower(coalesce(folder.collection_type, '')) = ANY(
                          ARRAY['tvshows', 'tvshow', 'series']::text[]
                      )
                      AND coverage.virtual_folder_id IS NULL
                ) AND NOT EXISTS (
                    SELECT 1
                    FROM virtual_folders AS folder
                    JOIN media_items AS item ON item.virtual_folder_id = folder.id
                    WHERE lower(coalesce(folder.collection_type, '')) <> ALL(
                              ARRAY['tvshows', 'tvshow', 'series']::text[]
                          )
                      AND item.missing_since IS NULL
                      AND item.media_type = 'Video'
                      AND lower(item.collection_type) = ANY(
                          ARRAY['tvshows', 'tvshow', 'series']::text[]
                      )
                ) AND NOT EXISTS (
                    SELECT 1
                    FROM media_item_tv_series AS series
                    JOIN media_item_tv_series_coverage AS coverage
                      ON coverage.virtual_folder_id = series.virtual_folder_id
                     AND coverage.projection_version = $2
                    GROUP BY series.series_id
                    HAVING count(*) > 1
                )
            END
            "#,
        )
        .bind(virtual_folder_id)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .fetch_one(&mut *transaction)
        .await?;
        if !projection_covered {
            transaction.commit().await?;
            return Ok(None);
        }
        let requested_limit = limit;
        let mut series = sqlx::query_as::<_, (String, String, i64)>(
            r#"
            SELECT series_id,
                   min(series_name) AS series_name,
                   COUNT(*) OVER () AS total_series
            FROM media_item_tv_series AS series
            JOIN media_item_tv_series_coverage AS coverage
              ON coverage.virtual_folder_id = series.virtual_folder_id
             AND coverage.projection_version = $4
            WHERE true
              AND ($1::uuid IS NULL OR series.virtual_folder_id = $1)
            GROUP BY series_id
            ORDER BY lower(min(series_name)), min(series_name), series_id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(virtual_folder_id)
        .bind(i64::try_from(limit.max(1))?)
        .bind(i64::try_from(start_index)?)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .fetch_all(&mut *transaction)
        .await?;
        let total = if let Some((_, _, total)) = series.first() {
            *total
        } else if start_index == 0 {
            0
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(DISTINCT series.series_id) FROM media_item_tv_series AS series JOIN media_item_tv_series_coverage AS coverage ON coverage.virtual_folder_id = series.virtual_folder_id AND coverage.projection_version = $2 WHERE ($1::uuid IS NULL OR series.virtual_folder_id = $1)",
            )
            .bind(virtual_folder_id)
            .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
            .fetch_one(&mut *transaction)
            .await?
        };
        if requested_limit == 0 {
            series.clear();
        }
        let rows = if series.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, PostgresMediaItemCatalogRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.media_streams, item.metadata, item.created_at, item.updated_at,
                       NULL::uuid AS playback_user_id, NULL::uuid AS playback_item_id,
                       NULL::text AS playback_media_source_id,
                       NULL::bigint AS playback_audio_stream_index,
                       NULL::bigint AS playback_subtitle_stream_index,
                       NULL::bigint AS playback_position_ticks,
                       NULL::boolean AS playback_is_paused, NULL::boolean AS playback_played,
                       NULL::boolean AS playback_is_favorite,
                       NULL::double precision AS playback_rating,
                       NULL::timestamptz AS playback_updated_at
                FROM media_item_tv_series_members AS member
                JOIN media_items AS item ON item.id = member.item_id
                JOIN media_item_tv_series_coverage AS coverage
                  ON coverage.virtual_folder_id = member.virtual_folder_id
                 AND coverage.projection_version = $3
                WHERE item.missing_since IS NULL
                  AND ($1::uuid IS NULL OR member.virtual_folder_id = $1)
                  AND member.series_id = ANY($2::text[])
                ORDER BY lower(item.name), item.name, item.id
                "#,
            )
            .bind(virtual_folder_id)
            .bind(
                series
                    .iter()
                    .map(|(id, _, _)| id.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
            .fetch_all(&mut *transaction)
            .await?
        };
        transaction.commit().await?;
        Ok(Some(TvSeriesCatalogPage {
            series: series
                .into_iter()
                .map(|(id, name, _)| TvSeriesCatalogKey { id, name })
                .collect(),
            episodes: rows
                .into_iter()
                .map(TryInto::try_into)
                .collect::<anyhow::Result<Vec<_>>>()?,
            total_record_count: usize::try_from(total)?,
            start_index,
        }))
    }

    async fn rebuild_tv_series_catalog_projection_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        virtual_folder_id: Uuid,
    ) -> anyhow::Result<bool> {
        sqlx::query("DELETE FROM media_item_tv_series_coverage WHERE virtual_folder_id = $1")
            .bind(virtual_folder_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM media_item_tv_series WHERE virtual_folder_id = $1")
            .bind(virtual_folder_id)
            .execute(&mut **tx)
            .await?;

        let eligible: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM virtual_folders AS folder
                WHERE folder.id = $1
                  AND lower(coalesce(folder.collection_type, '')) = ANY(
                      ARRAY['tvshows', 'tvshow', 'series']::text[]
                  )
            ) AND NOT EXISTS (
                SELECT 1
                FROM media_items AS invalid
                WHERE invalid.virtual_folder_id = $1
                  AND invalid.missing_since IS NULL
                  AND invalid.media_type = 'Video'
                  AND lower(invalid.collection_type) = ANY(
                      ARRAY['tvshows', 'tvshow', 'series']::text[]
                  )
                  AND (
                      NULLIF(btrim(invalid.metadata->>'SeriesId'), '') IS NULL
                      OR btrim(invalid.metadata->>'SeriesId') !~ '^[0-9a-f]{32}$'
                      OR NULLIF(btrim(invalid.metadata->>'SeriesName'), '') IS NULL
                  )
            ) AND NOT EXISTS (
                SELECT 1
                FROM media_items AS conflicting
                WHERE conflicting.virtual_folder_id = $1
                  AND conflicting.missing_since IS NULL
                  AND conflicting.media_type = 'Video'
                  AND lower(conflicting.collection_type) = ANY(
                      ARRAY['tvshows', 'tvshow', 'series']::text[]
                  )
                GROUP BY btrim(conflicting.metadata->>'SeriesId')
                HAVING count(DISTINCT btrim(conflicting.metadata->>'SeriesName')) > 1
            )
            "#,
        )
        .bind(virtual_folder_id)
        .fetch_one(&mut **tx)
        .await?;
        if !eligible {
            return Ok(false);
        }

        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series (
                virtual_folder_id, series_id, series_name, episode_count
            )
            SELECT item.virtual_folder_id,
                   btrim(item.metadata->>'SeriesId'),
                   min(btrim(item.metadata->>'SeriesName')),
                   count(*)
            FROM media_items AS item
            WHERE item.virtual_folder_id = $1
              AND item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
              )
            GROUP BY item.virtual_folder_id, btrim(item.metadata->>'SeriesId')
            "#,
        )
        .bind(virtual_folder_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series_members (item_id, virtual_folder_id, series_id)
            SELECT item.id, item.virtual_folder_id, btrim(item.metadata->>'SeriesId')
            FROM media_items AS item
            WHERE item.virtual_folder_id = $1
              AND item.missing_since IS NULL
              AND item.media_type = 'Video'
              AND lower(item.collection_type) = ANY(
                  ARRAY['tvshows', 'tvshow', 'series']::text[]
              )
            "#,
        )
        .bind(virtual_folder_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_tv_series_coverage (
                virtual_folder_id, projection_version, episode_count, series_count
            )
            SELECT $1, $2,
                   (SELECT count(*) FROM media_item_tv_series_members
                    WHERE virtual_folder_id = $1),
                   (SELECT count(*) FROM media_item_tv_series
                    WHERE virtual_folder_id = $1)
            "#,
        )
        .bind(virtual_folder_id)
        .bind(TV_SERIES_CATALOG_PROJECTION_VERSION)
        .execute(&mut **tx)
        .await?;
        Ok(true)
    }

    pub async fn tv_next_up_candidates(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogNextUpCandidates,
            DatabasePoolRole::Api,
        );
        let result: anyhow::Result<Vec<MediaItemCatalogEntry>> = async {
            let rows = sqlx::query_as::<_, PostgresMediaItemCatalogRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.media_streams, item.metadata, item.created_at, item.updated_at,
                       playback.user_id AS playback_user_id,
                       playback.item_id AS playback_item_id,
                       playback.media_source_id AS playback_media_source_id,
                       playback.audio_stream_index AS playback_audio_stream_index,
                       playback.subtitle_stream_index AS playback_subtitle_stream_index,
                       playback.position_ticks AS playback_position_ticks,
                       playback.is_paused AS playback_is_paused,
                       playback.played AS playback_played,
                       playback.is_favorite AS playback_is_favorite,
                       playback.rating AS playback_rating,
                       playback.updated_at AS playback_updated_at
                FROM media_items AS item
                LEFT JOIN playback_states AS playback
                  ON playback.item_id = item.id AND playback.user_id = $1
                WHERE item.missing_since IS NULL
                  AND item.media_type = 'Video'
                  AND item.collection_type = 'tvshows'
                  AND NOT COALESCE(playback.played, false)
                "#,
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter().map(TryInto::try_into).collect()
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    pub async fn tv_upcoming_candidates(
        &self,
        now: OffsetDateTime,
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogUpcomingCandidates,
            DatabasePoolRole::Api,
        );
        let result: anyhow::Result<Vec<MediaItemCatalogEntry>> = async {
            // OFFSET 0 intentionally keeps the lateral lookup as an optimizer barrier. Without
            // it PostgreSQL flattens the join and chooses a hash join that scans every visible TV
            // row; the range index must drive bounded primary-key lookups instead.
            let mut rows = sqlx::query_as::<_, PostgresMediaItemCatalogRow>(
                r#"
                SELECT item.id, item.virtual_folder_id, item.name, item.path,
                       item.media_type, item.collection_type, item.file_size,
                       item.runtime_ticks, item.bitrate, item.width, item.height,
                       item.media_streams, item.metadata, item.created_at, item.updated_at,
                       NULL::uuid AS playback_user_id,
                       NULL::uuid AS playback_item_id,
                       NULL::text AS playback_media_source_id,
                       NULL::bigint AS playback_audio_stream_index,
                       NULL::bigint AS playback_subtitle_stream_index,
                       NULL::bigint AS playback_position_ticks,
                       NULL::boolean AS playback_is_paused,
                       NULL::boolean AS playback_played,
                       NULL::boolean AS playback_is_favorite,
                       NULL::double precision AS playback_rating,
                       NULL::timestamptz AS playback_updated_at
                FROM media_item_upcoming_dates AS upcoming
                CROSS JOIN LATERAL (
                    SELECT candidate.*
                    FROM media_items AS candidate
                    WHERE candidate.id = upcoming.item_id
                      AND candidate.missing_since IS NULL
                      AND candidate.media_type = 'Video'
                      AND candidate.collection_type = 'tvshows'
                    OFFSET 0
                ) AS item
                WHERE (upcoming.unix_seconds, upcoming.nanosecond) > ($1, $2)
                "#,
            )
            .bind(now.unix_timestamp())
            .bind(i32::try_from(now.nanosecond()).context("current nanosecond overflow")?)
            .fetch(&self.pool);
            let mut candidates = Vec::new();
            while let Some(row) = rows.try_next().await? {
                let entry = MediaItemCatalogEntry::try_from(row)?;
                if is_upcoming_media_item_entry(&entry, now) {
                    candidates.push(entry);
                }
            }
            Ok(candidates)
        }
        .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    pub async fn media_items_with_metadata_by_effective_types(
        &self,
        item_types: &[String],
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogEffectiveTypeCandidates,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_items_with_metadata_by_effective_types_unobserved(item_types)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_with_metadata_by_effective_types_unobserved(
        &self,
        item_types: &[String],
    ) -> anyhow::Result<Vec<MediaItemCatalogEntry>> {
        let scope = EffectiveTypeCandidateScope::from_effective_types(item_types);
        if scope.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams, item.metadata, item.created_at, item.updated_at,
                   NULL::uuid AS playback_user_id,
                   NULL::uuid AS playback_item_id,
                   NULL::text AS playback_media_source_id,
                   NULL::bigint AS playback_audio_stream_index,
                   NULL::bigint AS playback_subtitle_stream_index,
                   NULL::bigint AS playback_position_ticks,
                   NULL::boolean AS playback_is_paused,
                   NULL::boolean AS playback_played,
                   NULL::boolean AS playback_is_favorite,
                   NULL::double precision AS playback_rating,
                   NULL::timestamptz AS playback_updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
            "#,
        );

        if !scope.all_raw_media_types {
            query.push(" AND (");
            let mut has_predicate = false;
            if scope.all_video {
                query.push("item.media_type = 'Video'");
                has_predicate = true;
            } else if !scope.video_collection_types.is_empty() {
                query.push("(item.media_type = 'Video' AND item.collection_type IN (");
                {
                    let mut separated = query.separated(", ");
                    for collection_type in &scope.video_collection_types {
                        separated.push_bind(*collection_type);
                    }
                }
                query.push("))");
                has_predicate = true;
            }
            for media_type in &scope.raw_media_types {
                if has_predicate {
                    query.push(" OR ");
                }
                query.push("item.media_type = ").push_bind(*media_type);
                has_predicate = true;
            }
            debug_assert!(has_predicate);
            query.push(")");
        }

        let entries = query
            .build_query_as::<PostgresMediaItemCatalogRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(retain_entries_with_effective_types(entries, item_types))
    }

    pub async fn media_items_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogFolderItems, DatabasePoolRole::Api);
        let result = self
            .media_items_for_virtual_folders_unobserved(folder_ids)
            .await;
        observation.finish_result(&result, |items| {
            u64::try_from(items.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_items_for_virtual_folders_unobserved(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItem>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL AND virtual_folder_id = ANY($1)
            ORDER BY lower(name), name
            "#,
        )
        .bind(folder_ids.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_item_counts_by_virtual_folder(
        &self,
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogFolderCounts,
            DatabasePoolRole::Api,
        );
        let result = self.media_item_counts_by_virtual_folder_unobserved().await;
        observation.finish_result(&result, |counts| {
            u64::try_from(counts.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_counts_by_virtual_folder_unobserved(
        &self,
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemCountRow>(
            r#"
            SELECT virtual_folder_id, COUNT(*) AS count
            FROM media_items
            WHERE missing_since IS NULL
            GROUP BY virtual_folder_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.virtual_folder_id, row.count.max(0) as usize))
            .collect())
    }

    pub async fn media_item_filter_summary_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
    ) -> anyhow::Result<MediaItemFilterSummary> {
        if folder_ids.is_empty() {
            return Ok(MediaItemFilterSummary::default());
        }

        let genres = self
            .distinct_media_item_metadata_values_for_virtual_folders(folder_ids, "Genres")
            .await?;
        let tags = self
            .distinct_media_item_metadata_values_for_virtual_folders(folder_ids, "Tags")
            .await?;
        let rows = sqlx::query_as::<_, PostgresMediaItemFilterRow>(
            r#"
            SELECT path, media_type
            FROM media_items
            WHERE missing_since IS NULL AND virtual_folder_id = ANY($1)
            "#,
        )
        .bind(folder_ids.to_vec())
        .fetch_all(&self.pool)
        .await?;

        let mut containers = BTreeSet::new();
        let mut media_types = BTreeSet::new();
        for row in rows {
            if !row.media_type.trim().is_empty() {
                media_types.insert(row.media_type);
            }
            if let Some(extension) = Path::new(&row.path).extension().and_then(OsStr::to_str) {
                let extension = extension.trim().to_ascii_lowercase();
                if !extension.is_empty() {
                    containers.insert(extension);
                }
            }
        }

        Ok(MediaItemFilterSummary {
            genres,
            tags,
            containers: containers.into_iter().collect(),
            media_types: media_types.into_iter().collect(),
        })
    }

    pub async fn distinct_media_item_metadata_values_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
        key: &str,
    ) -> anyhow::Result<Vec<String>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }

        let values = sqlx::query_scalar::<_, String>(
            r#"
            SELECT value
            FROM (
                SELECT DISTINCT expanded.value
                FROM media_items
                CROSS JOIN LATERAL jsonb_array_elements_text(
                    CASE
                        WHEN jsonb_typeof(metadata -> $1) = 'array' THEN metadata -> $1
                        WHEN metadata ? $1 THEN jsonb_build_array(metadata -> $1)
                        ELSE '[]'::jsonb
                    END
                ) AS expanded(value)
                WHERE missing_since IS NULL
                  AND virtual_folder_id = ANY($2)
                  AND NULLIF(btrim(expanded.value), '') IS NOT NULL
            ) AS unique_values
            ORDER BY lower(value), value
            "#,
        )
        .bind(key)
        .bind(folder_ids.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(values)
    }

    pub async fn media_item_facet_values(
        &self,
        kind: MediaItemFacetKind,
        virtual_folder_ids: &[Uuid],
    ) -> anyhow::Result<Vec<MediaItemFacetValue>> {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT normalized_value, display_value, stable_id, payload
            FROM (
                SELECT facet.normalized_value, facet.display_value, facet.stable_id,
                       facet.payload,
                       ROW_NUMBER() OVER (
                           PARTITION BY facet.normalized_value
                           ORDER BY item.created_at, facet.position, facet.item_id
                       ) AS facet_rank
                FROM media_item_facets AS facet
                JOIN media_items AS item ON item.id = facet.item_id
                WHERE item.missing_since IS NULL AND facet.facet_kind =
            "#,
        );
        query.push_bind(kind.as_str());
        if !virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in virtual_folder_ids {
                separated.push_bind(*folder_id);
            }
            separated.push_unseparated(")");
        }
        query.push(
            ") AS ranked WHERE facet_rank = 1 ORDER BY normalized_value, display_value, stable_id",
        );
        let rows = query
            .build_query_as::<(String, String, String, Value)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(normalized_value, display_value, stable_id, payload)| MediaItemFacetValue {
                    normalized_value,
                    display_value,
                    stable_id,
                    payload,
                },
            )
            .collect())
    }

    pub async fn media_item_facet_by_entity_id(
        &self,
        kind: MediaItemFacetKind,
        entity_id: &str,
    ) -> anyhow::Result<Option<MediaItemFacetValue>> {
        let row = sqlx::query_as::<_, (String, String, String, Value)>(
            r#"
            SELECT facet.normalized_value, facet.display_value, facet.stable_id, facet.payload
            FROM media_item_facets AS facet
            JOIN media_item_facet_aliases AS alias
              ON alias.item_id = facet.item_id
             AND alias.facet_kind = facet.facet_kind
             AND alias.normalized_value = facet.normalized_value
            JOIN media_items AS item ON item.id = facet.item_id
            WHERE item.missing_since IS NULL
              AND facet.facet_kind = $1
              AND alias.entity_id = $2
            ORDER BY item.created_at, facet.position, facet.item_id
            LIMIT 1
            "#,
        )
        .bind(kind.as_str())
        .bind(entity_id.trim().to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(normalized_value, display_value, stable_id, payload)| MediaItemFacetValue {
                normalized_value,
                display_value,
                stable_id,
                payload,
            },
        ))
    }

    pub async fn media_item_facet_by_normalized_value(
        &self,
        kind: MediaItemFacetKind,
        value: &str,
        virtual_folder_ids: &[Uuid],
    ) -> anyhow::Result<Option<MediaItemFacetValue>> {
        let normalized_value = value.trim().to_ascii_lowercase();
        if normalized_value.is_empty() {
            return Ok(None);
        }
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT facet.normalized_value, facet.display_value, facet.stable_id, facet.payload
            FROM media_item_facets AS facet
            JOIN media_items AS item ON item.id = facet.item_id
            WHERE item.missing_since IS NULL
              AND facet.facet_kind =
            "#,
        );
        query.push_bind(kind.as_str());
        query
            .push(" AND facet.normalized_value = ")
            .push_bind(normalized_value);
        if !virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in virtual_folder_ids {
                separated.push_bind(*folder_id);
            }
            separated.push_unseparated(")");
        }
        query.push(" ORDER BY item.created_at, facet.position, facet.item_id LIMIT 1");
        Ok(query
            .build_query_as::<(String, String, String, Value)>()
            .fetch_optional(&self.pool)
            .await?
            .map(
                |(normalized_value, display_value, stable_id, payload)| MediaItemFacetValue {
                    normalized_value,
                    display_value,
                    stable_id,
                    payload,
                },
            ))
    }

    pub async fn media_item_ids_for_facets(
        &self,
        query_spec: &MediaItemFacetCandidateQuery,
    ) -> anyhow::Result<Vec<Uuid>> {
        let normalized_values = normalized_facet_query_values(&query_spec.normalized_values);
        let entity_ids = normalized_facet_query_values(&query_spec.entity_ids);
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT facet.item_id
            FROM media_item_facets AS facet
            JOIN media_items AS item ON item.id = facet.item_id
            LEFT JOIN media_item_facet_aliases AS alias
              ON alias.item_id = facet.item_id
             AND alias.facet_kind = facet.facet_kind
             AND alias.normalized_value = facet.normalized_value
            WHERE item.missing_since IS NULL
            "#,
        );
        if let Some(kind) = query_spec.kind {
            query
                .push(" AND facet.facet_kind = ")
                .push_bind(kind.as_str());
        }
        if !query_spec.virtual_folder_ids.is_empty() {
            query.push(" AND item.virtual_folder_id IN (");
            let mut separated = query.separated(", ");
            for folder_id in &query_spec.virtual_folder_ids {
                separated.push_bind(*folder_id);
            }
            separated.push_unseparated(")");
        }
        if !normalized_values.is_empty() || !entity_ids.is_empty() {
            query.push(" AND (");
            if !normalized_values.is_empty() {
                query.push("facet.normalized_value IN (");
                let mut separated = query.separated(", ");
                for value in &normalized_values {
                    separated.push_bind(value);
                }
                separated.push_unseparated(")");
            }
            if !normalized_values.is_empty() && !entity_ids.is_empty() {
                query.push(" OR ");
            }
            if !entity_ids.is_empty() {
                query.push("alias.entity_id IN (");
                let mut separated = query.separated(", ");
                for entity_id in &entity_ids {
                    separated.push_bind(entity_id);
                }
                separated.push_unseparated(")");
            }
            query.push(")");
        }
        query.push(" ORDER BY facet.item_id");
        Ok(query
            .build_query_scalar::<Uuid>()
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn rebuild_media_item_facets(&self) -> anyhow::Result<()> {
        let mut tx = self.worker_pool.begin().await?;
        ensure_media_item_facet_projection(&mut tx, MediaItemFacetProjectionMode::Force).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn begin_remote_media_catalog_stage(
        &self,
        libraries: Vec<RemoteMediaLibraryStageSpec>,
    ) -> anyhow::Result<RemoteMediaCatalogStage> {
        let libraries = prepare_remote_media_library_stage_specs(libraries)?;
        let stage = RemoteMediaCatalogStage::new(Uuid::new_v4());
        let stage_id = stage.parsed_id()?;
        let mut tx = self.worker_pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO remote_media_catalog_stages (
                id, status, extractor_version, query_filter_extractor_version,
                created_at, updated_at
            )
            VALUES ($1, 'open', $2, $3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            "#,
        )
        .bind(stage_id)
        .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .execute(&mut *tx)
        .await?;
        for library in libraries {
            sqlx::query(
                r#"
                INSERT INTO remote_media_catalog_stage_libraries (
                    stage_id, library_key, position, library_name,
                    collection_type, source_location
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                "#,
            )
            .bind(stage_id)
            .bind(library.key)
            .bind(library.position)
            .bind(library.library_name)
            .bind(library.collection_type)
            .bind(library.source_location)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(stage)
    }

    pub async fn append_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
        library_key: &str,
        items: Vec<RemoteMediaItemUpsert>,
    ) -> anyhow::Result<()> {
        validate_remote_media_catalog_stage_append(&items)?;
        anyhow::ensure!(
            matches!(library_key, "movies" | "series"),
            "remote media catalogue stage library key must be movies or series"
        );
        let stage_id = stage.parsed_id()?;
        let prepared = items
            .into_iter()
            .map(PreparedRemoteMediaItem::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut tx = self.worker_pool.begin().await?;
        let (status, extractor_version, query_filter_version) =
            sqlx::query_as::<_, (String, i32, i32)>(
                "SELECT status, extractor_version, query_filter_extractor_version \
             FROM remote_media_catalog_stages WHERE id = $1 FOR UPDATE",
            )
            .bind(stage_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("remote media catalogue stage not found")?;
        anyhow::ensure!(status == "open", "remote media catalogue stage is not open");
        anyhow::ensure!(
            extractor_version == MEDIA_ITEM_FACET_PROJECTION_VERSION,
            "remote media catalogue stage extractor version is stale"
        );
        anyhow::ensure!(
            query_filter_version == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
            "remote media catalogue stage query-filter extractor version is stale"
        );
        let appended_count = i64::try_from(prepared.len())
            .context("remote media catalogue stage append count overflow")?;
        let staged_item_count = sqlx::query_scalar::<_, i64>(
            r#"
            UPDATE remote_media_catalog_stage_libraries
            SET item_count = item_count + $3
            WHERE stage_id = $1 AND library_key = $2
              AND item_count + $3 <= $4
            RETURNING item_count
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .bind(appended_count)
        .bind(
            i64::try_from(REMOTE_MEDIA_CATALOG_STAGE_MAX_LIBRARY_ITEMS)
                .context("remote media catalogue stage library limit overflow")?,
        )
        .fetch_optional(&mut *tx)
        .await?;
        anyhow::ensure!(
            staged_item_count.is_some(),
            "remote media catalogue stage library was not found or exceeded its item limit"
        );

        let mut item_query = QueryBuilder::<Postgres>::new(
            "INSERT INTO remote_media_catalog_stage_items (\
             stage_id, library_key, id, name, path, media_type, collection_type, \
             runtime_ticks, bitrate, width, height, media_streams, metadata) ",
        );
        item_query.push_values(&prepared, |mut values, item| {
            values
                .push_bind(stage_id)
                .push_bind(library_key)
                .push_bind(item.id)
                .push_bind(&item.name)
                .push_bind(&item.path)
                .push_bind(&item.media_type)
                .push_bind(&item.collection_type)
                .push_bind(item.runtime_ticks)
                .push_bind(item.bitrate)
                .push_bind(item.width)
                .push_bind(item.height)
                .push_bind(&item.media_streams)
                .push_bind(&item.metadata);
        });
        item_query.build().execute(&mut *tx).await?;

        let query_filter_projections = prepared
            .iter()
            .map(|item| {
                let streams = item
                    .media_streams
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let projection = extract_media_item_query_filter_projection(
                    MediaItemQueryFilterProjectionSource {
                        path: &item.path,
                        media_type: &item.media_type,
                        media_streams: streams,
                        metadata: &item.metadata,
                    },
                );
                let value_count = i32::try_from(projection.values.len())
                    .context("staged query-filter value count overflow")?;
                Ok((item.id, projection, value_count))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if !query_filter_projections.is_empty() {
            let mut sources = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_query_filter_sources (stage_id, item_id, \
                 container_present, container_value, media_type, is_video, has_subtitles, \
                 has_trailer, projected_value_count) ",
            );
            sources.push_values(
                &query_filter_projections,
                |mut row, (item_id, projection, value_count)| {
                    row.push_bind(stage_id)
                        .push_bind(*item_id)
                        .push_bind(projection.features.container_present)
                        .push_bind(&projection.features.container)
                        .push_bind(&projection.features.media_type)
                        .push_bind(projection.features.is_video)
                        .push_bind(projection.features.has_subtitles)
                        .push_bind(projection.features.has_trailer)
                        .push_bind(*value_count);
                },
            );
            sources.build().execute(&mut *tx).await?;
        }
        let query_filter_values = query_filter_projections
            .iter()
            .flat_map(|(item_id, projection, _)| {
                projection.values.iter().map(move |value| (*item_id, value))
            })
            .collect::<Vec<_>>();
        for chunk in query_filter_values.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut values = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_query_filter_values (stage_id, item_id, \
                 value_kind, display_value, source_key, source_priority, source_position) ",
            );
            values.push_values(chunk, |mut row, (item_id, value)| {
                row.push_bind(stage_id)
                    .push_bind(*item_id)
                    .push_bind(value.kind.as_str())
                    .push_bind(&value.display_value)
                    .push_bind(&value.source_key)
                    .push_bind(i32::from(value.source_priority))
                    .push_bind(encode_media_item_query_filter_position(&value.position));
            });
            values.build().execute(&mut *tx).await?;
        }

        let mut facets = Vec::new();
        for item in &prepared {
            for facet in extract_media_item_facets(&item.metadata) {
                let position =
                    i32::try_from(facet.position).context("media item facet position overflow")?;
                facets.push((item.id, facet, position));
            }
        }
        for chunk in facets.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_facets (\
                 stage_id, item_id, facet_kind, normalized_value, display_value, stable_id, \
                 position, payload) ",
            );
            query.push_values(chunk, |mut values, (item_id, facet, position)| {
                values
                    .push_bind(stage_id)
                    .push_bind(*item_id)
                    .push_bind(facet.kind.as_str())
                    .push_bind(&facet.normalized_value)
                    .push_bind(&facet.display_value)
                    .push_bind(&facet.stable_id)
                    .push_bind(*position)
                    .push_bind(&facet.payload);
            });
            query.build().execute(&mut *tx).await?;
        }
        let aliases = facets
            .iter()
            .flat_map(|(item_id, facet, _)| {
                facet.aliases.iter().map(move |alias| {
                    (
                        *item_id,
                        facet.kind,
                        facet.normalized_value.as_str(),
                        alias.as_str(),
                    )
                })
            })
            .collect::<Vec<_>>();
        for chunk in aliases.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_facet_aliases (\
                 stage_id, item_id, facet_kind, normalized_value, entity_id) ",
            );
            query.push_values(
                chunk,
                |mut values, (item_id, kind, normalized_value, entity_id)| {
                    values
                        .push_bind(stage_id)
                        .push_bind(*item_id)
                        .push_bind(kind.as_str())
                        .push_bind(*normalized_value)
                        .push_bind(*entity_id);
                },
            );
            query.build().execute(&mut *tx).await?;
        }

        let genre_selectors = prepared
            .iter()
            .flat_map(|item| {
                extract_media_item_genre_selectors(&item.metadata)
                    .into_iter()
                    .map(move |selector| (item.id, selector))
            })
            .collect::<Vec<_>>();
        for chunk in genre_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_genre_selectors \
                 (stage_id, item_id, selector) ",
            );
            query.push_values(chunk, |mut values, (item_id, selector)| {
                values
                    .push_bind(stage_id)
                    .push_bind(*item_id)
                    .push_bind(selector);
            });
            query.build().execute(&mut *tx).await?;
        }

        let filter_selectors = prepared
            .iter()
            .flat_map(|item| {
                extract_media_item_filter_selectors(&item.metadata)
                    .into_iter()
                    .map(move |(kind, selector)| (item.id, kind, selector))
            })
            .collect::<Vec<_>>();
        for chunk in filter_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_filter_selectors \
                 (stage_id, item_id, selector_kind, selector) ",
            );
            query.push_values(chunk, |mut values, (item_id, kind, selector)| {
                values
                    .push_bind(stage_id)
                    .push_bind(*item_id)
                    .push_bind(kind.as_str())
                    .push_bind(selector);
            });
            query.build().execute(&mut *tx).await?;
        }

        let upcoming_dates = prepared
            .iter()
            .filter_map(|item| {
                upcoming_media_item_premiere_parts(&item.metadata)
                    .map(|(unix_seconds, nanosecond)| (item.id, unix_seconds, nanosecond))
            })
            .collect::<Vec<_>>();
        for chunk in upcoming_dates.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO remote_media_catalog_stage_upcoming_dates \
                 (stage_id, item_id, unix_seconds, nanosecond) ",
            );
            query.push_values(chunk, |mut values, (item_id, unix_seconds, nanosecond)| {
                values
                    .push_bind(stage_id)
                    .push_bind(*item_id)
                    .push_bind(*unix_seconds)
                    .push_bind(*nanosecond);
            });
            query.build().execute(&mut *tx).await?;
        }
        sqlx::query(
            "UPDATE remote_media_catalog_stages SET updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(stage_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn abort_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<()> {
        let stage_id = stage.parsed_id()?;
        let mut tx = self.worker_pool.begin().await?;
        let stage_state = sqlx::query_as::<_, (String, i32)>(
            "SELECT status, extractor_version \
             FROM remote_media_catalog_stages WHERE id = $1 FOR UPDATE",
        )
        .bind(stage_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((status, _extractor_version)) = stage_state {
            anyhow::ensure!(
                status != "publishing",
                "remote media catalogue stage is publishing"
            );
            sqlx::query(
                "UPDATE remote_media_catalog_stages \
                 SET status = 'aborted', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
            )
            .bind(stage_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM remote_media_catalog_stages WHERE id = $1")
                .bind(stage_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn cleanup_abandoned_remote_media_catalog_stages(
        &self,
        older_than: OffsetDateTime,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM remote_media_catalog_stages
            WHERE status IN ('open', 'aborted') AND updated_at < $1
            "#,
        )
        .bind(older_than)
        .execute(&self.worker_pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn publish_remote_media_catalog_stage(
        &self,
        stage: &RemoteMediaCatalogStage,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        let stage_id = stage.parsed_id()?;
        let mut tx = self.worker_pool.begin().await?;
        let (status, extractor_version, query_filter_version) =
            sqlx::query_as::<_, (String, i32, i32)>(
                "SELECT status, extractor_version, query_filter_extractor_version \
             FROM remote_media_catalog_stages WHERE id = $1 FOR UPDATE",
            )
            .bind(stage_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("remote media catalogue stage not found")?;
        anyhow::ensure!(status == "open", "remote media catalogue stage is not open");
        anyhow::ensure!(
            extractor_version == MEDIA_ITEM_FACET_PROJECTION_VERSION,
            "remote media catalogue stage extractor version is stale"
        );
        anyhow::ensure!(
            query_filter_version == MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION,
            "remote media catalogue stage query-filter extractor version is stale"
        );
        let rows = sqlx::query_as::<_, (String, i16, String, String, String, i64)>(
            r#"
            SELECT library_key, position, library_name, collection_type, source_location,
                   item_count
            FROM remote_media_catalog_stage_libraries
            WHERE stage_id = $1
            ORDER BY position
            "#,
        )
        .bind(stage_id)
        .fetch_all(&mut *tx)
        .await?;
        let expected_counts = rows
            .iter()
            .map(|(key, _, _, _, _, item_count)| (key.clone(), *item_count))
            .collect::<HashMap<_, _>>();
        let libraries = prepare_remote_media_library_stage_specs(
            rows.into_iter()
                .map(
                    |(key, _position, library_name, collection_type, source_location, _count)| {
                        RemoteMediaLibraryStageSpec {
                            key,
                            library_name,
                            collection_type,
                            source_location,
                        }
                    },
                )
                .collect(),
        )?;
        sqlx::query(
            "UPDATE remote_media_catalog_stages \
             SET status = 'publishing', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(stage_id)
        .execute(&mut *tx)
        .await?;

        let mut lock_names = libraries
            .iter()
            .map(|library| library.library_name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        lock_names.sort_unstable();
        for lock_name in lock_names {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(lock_name)
                .execute(&mut *tx)
                .await?;
        }

        let mut folders = Vec::with_capacity(2);
        for library in libraries {
            let item_count = Self::load_durable_remote_media_stage_library_in_transaction(
                &mut tx,
                stage_id,
                &library.key,
            )
            .await?;
            anyhow::ensure!(
                i64::try_from(item_count).context("remote snapshot item count overflow")?
                    == *expected_counts
                        .get(&library.key)
                        .context("remote media catalogue stage library count is missing")?,
                "remote media catalogue stage item count mismatch"
            );
            let folder_row = sqlx::query_as::<_, PostgresVirtualFolderRow>(
                r#"
                INSERT INTO virtual_folders (
                    id, name, collection_type, locations, created_at, updated_at
                )
                VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                ON CONFLICT ((lower(name))) DO UPDATE SET
                    collection_type = excluded.collection_type,
                    locations = excluded.locations,
                    updated_at = excluded.updated_at
                RETURNING id, name, collection_type, locations, created_at, updated_at
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(&library.library_name)
            .bind(trimmed_optional_str(Some(&library.collection_type)))
            .bind(serde_json::to_value(normalized_locations(vec![
                library.source_location,
            ]))?)
            .fetch_one(&mut *tx)
            .await?;
            let sync_run_id = Uuid::new_v4();
            sqlx::query(
                r#"
                INSERT INTO catalog_sync_runs (
                    id, virtual_folder_id, generation_id, status, item_count, started_at
                )
                VALUES ($1, $2, $3, 'running', $4, CURRENT_TIMESTAMP)
                "#,
            )
            .bind(sync_run_id)
            .bind(folder_row.id)
            .bind(Uuid::new_v4())
            .bind(i64::try_from(item_count).context("remote snapshot item count overflow")?)
            .execute(&mut *tx)
            .await?;
            folders.push(
                self.publish_prepared_remote_media_library_stage_in_transaction(
                    &mut tx,
                    folder_row,
                    sync_run_id,
                )
                .await?,
            );
        }
        sqlx::query("DELETE FROM remote_media_catalog_stages WHERE id = $1")
            .bind(stage_id)
            .execute(&mut *tx)
            .await?;
        let commit_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncCommit,
            DatabasePoolRole::Worker,
        );
        let commit_result = tx.commit().await.map_err(anyhow::Error::from);
        commit_observation.finish_result(&commit_result, |_| 0);
        commit_result?;
        Ok(folders)
    }

    async fn load_durable_remote_media_stage_library_in_transaction(
        tx: &mut sqlx::Transaction<'_, Postgres>,
        stage_id: Uuid,
        library_key: &str,
    ) -> anyhow::Result<u64> {
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_remote_snapshot_stage (
                id uuid PRIMARY KEY,
                name text NOT NULL,
                path text NOT NULL UNIQUE,
                media_type text NOT NULL,
                collection_type text,
                runtime_ticks bigint,
                bitrate bigint,
                width integer,
                height integer,
                media_streams jsonb NOT NULL,
                metadata jsonb NOT NULL
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_facet_stage (
                item_id uuid NOT NULL,
                facet_kind text NOT NULL,
                normalized_value text NOT NULL,
                display_value text NOT NULL,
                stable_id text NOT NULL,
                position integer NOT NULL,
                payload jsonb NOT NULL,
                PRIMARY KEY (item_id, facet_kind, normalized_value)
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_facet_alias_stage (
                item_id uuid NOT NULL,
                facet_kind text NOT NULL,
                normalized_value text NOT NULL,
                entity_id text NOT NULL,
                PRIMARY KEY (item_id, facet_kind, normalized_value, entity_id)
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_genre_selector_stage (
                item_id uuid NOT NULL,
                selector text NOT NULL,
                PRIMARY KEY (item_id, selector)
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_filter_selector_stage (
                item_id uuid NOT NULL,
                selector_kind text NOT NULL,
                selector text NOT NULL,
                PRIMARY KEY (item_id, selector_kind, selector)
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_upcoming_date_stage (
                item_id uuid PRIMARY KEY,
                unix_seconds bigint NOT NULL,
                nanosecond integer NOT NULL
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_query_filter_source_stage (
                item_id uuid PRIMARY KEY,
                container_present boolean NOT NULL,
                container_value text,
                media_type text NOT NULL,
                is_video boolean NOT NULL,
                has_subtitles boolean NOT NULL,
                has_trailer boolean NOT NULL,
                projected_value_count integer NOT NULL
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_query_filter_value_stage (
                item_id uuid NOT NULL,
                value_kind text NOT NULL,
                display_value text NOT NULL,
                source_key text NOT NULL,
                source_priority integer NOT NULL,
                source_position text NOT NULL,
                PRIMARY KEY (item_id, value_kind, source_key, source_position)
            ) ON COMMIT DROP
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "TRUNCATE jellyrin_remote_snapshot_stage, \
             jellyrin_media_item_facet_alias_stage, jellyrin_media_item_facet_stage, \
             jellyrin_media_item_genre_selector_stage, \
             jellyrin_media_item_filter_selector_stage, \
             jellyrin_media_item_upcoming_date_stage, jellyrin_query_filter_value_stage, \
             jellyrin_query_filter_source_stage",
        )
        .execute(&mut **tx)
        .await?;
        let inserted = sqlx::query(
            r#"
            INSERT INTO jellyrin_remote_snapshot_stage (
                id, name, path, media_type, collection_type, runtime_ticks, bitrate,
                width, height, media_streams, metadata
            )
            SELECT id, name, path, media_type, collection_type, runtime_ticks, bitrate,
                   width, height, media_streams, metadata
            FROM remote_media_catalog_stage_items
            WHERE stage_id = $1 AND library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_media_item_facet_stage (
                item_id, facet_kind, normalized_value, display_value, stable_id, position, payload
            )
            SELECT facet.item_id, facet.facet_kind, facet.normalized_value,
                   facet.display_value, facet.stable_id, facet.position, facet.payload
            FROM remote_media_catalog_stage_facets AS facet
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = facet.stage_id AND item.id = facet.item_id
            WHERE facet.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_media_item_facet_alias_stage (
                item_id, facet_kind, normalized_value, entity_id
            )
            SELECT alias.item_id, alias.facet_kind, alias.normalized_value, alias.entity_id
            FROM remote_media_catalog_stage_facet_aliases AS alias
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = alias.stage_id AND item.id = alias.item_id
            WHERE alias.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_media_item_genre_selector_stage (item_id, selector)
            SELECT selector.item_id, selector.selector
            FROM remote_media_catalog_stage_genre_selectors AS selector
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = selector.stage_id AND item.id = selector.item_id
            WHERE selector.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_media_item_filter_selector_stage (
                item_id, selector_kind, selector
            )
            SELECT selector.item_id, selector.selector_kind, selector.selector
            FROM remote_media_catalog_stage_filter_selectors AS selector
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = selector.stage_id AND item.id = selector.item_id
            WHERE selector.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_media_item_upcoming_date_stage (
                item_id, unix_seconds, nanosecond
            )
            SELECT upcoming.item_id, upcoming.unix_seconds, upcoming.nanosecond
            FROM remote_media_catalog_stage_upcoming_dates AS upcoming
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = upcoming.stage_id AND item.id = upcoming.item_id
            WHERE upcoming.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        let projected_sources = sqlx::query(
            r#"
            INSERT INTO jellyrin_query_filter_source_stage (
                item_id, container_present, container_value, media_type, is_video,
                has_subtitles, has_trailer, projected_value_count
            )
            SELECT source.item_id, source.container_present, source.container_value,
                   source.media_type, source.is_video, source.has_subtitles,
                   source.has_trailer, source.projected_value_count
            FROM remote_media_catalog_stage_query_filter_sources AS source
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = source.stage_id AND item.id = source.item_id
            WHERE source.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO jellyrin_query_filter_value_stage (
                item_id, value_kind, display_value, source_key, source_priority, source_position
            )
            SELECT value.item_id, value.value_kind, value.display_value, value.source_key,
                   value.source_priority, value.source_position
            FROM remote_media_catalog_stage_query_filter_values AS value
            JOIN remote_media_catalog_stage_items AS item
              ON item.stage_id = value.stage_id AND item.id = value.item_id
            WHERE value.stage_id = $1 AND item.library_key = $2
            "#,
        )
        .bind(stage_id)
        .bind(library_key)
        .execute(&mut **tx)
        .await?;
        anyhow::ensure!(
            projected_sources.rows_affected() == inserted.rows_affected(),
            "remote media catalogue stage query-filter source coverage is incomplete"
        );
        let projection_counts = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT coalesce(sum(projected_value_count), 0), \
                    (SELECT count(*) FROM jellyrin_query_filter_value_stage), \
                    (SELECT count(*) FROM jellyrin_query_filter_source_stage AS source \
                     WHERE source.projected_value_count <> (SELECT count(*) \
                       FROM jellyrin_query_filter_value_stage AS value \
                       WHERE value.item_id = source.item_id)) \
             FROM jellyrin_query_filter_source_stage",
        )
        .fetch_one(&mut **tx)
        .await?;
        anyhow::ensure!(
            projection_counts.0 == projection_counts.1 && projection_counts.2 == 0,
            "remote media catalogue stage query-filter value coverage is incomplete"
        );
        Ok(inserted.rows_affected())
    }

    /// Atomically publishes a complete remote library snapshot.
    ///
    /// A transaction-local staging table lets PostgreSQL retain unchanged rows (and therefore
    /// their dependent state) while deleting only stale or identity-conflicting entries. The
    /// live folder never exposes a partially imported snapshot.
    pub async fn replace_remote_media_library_snapshot(
        &self,
        library_name: &str,
        collection_type: &str,
        source_location: &str,
        items: Vec<RemoteMediaItemUpsert>,
    ) -> anyhow::Result<VirtualFolder> {
        self.replace_remote_media_library_snapshots(vec![RemoteMediaLibrarySnapshot {
            library_name: library_name.to_owned(),
            collection_type: collection_type.to_owned(),
            source_location: source_location.to_owned(),
            items,
        }])
        .await?
        .pop()
        .context("remote media snapshot did not return its virtual folder")
    }

    /// Atomically publishes all related remote libraries as one catalogue generation batch.
    ///
    /// Validation happens before the transaction. Advisory locks are acquired in canonical name
    /// order, preventing reversed movie/series batches from deadlocking one another. Any failure
    /// while applying a later library rolls back folders, tombstones, items, and sync generations
    /// already written for earlier libraries in the same batch.
    pub async fn replace_remote_media_library_snapshots(
        &self,
        snapshots: Vec<RemoteMediaLibrarySnapshot>,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        let received_rows = snapshots.iter().fold(0u64, |total, snapshot| {
            total.saturating_add(u64::try_from(snapshot.items.len()).unwrap_or(u64::MAX))
        });
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncPublish,
            DatabasePoolRole::Worker,
        );
        let result = self
            .replace_remote_media_library_snapshots_unobserved(snapshots)
            .await;
        observation.finish_result(&result, |_| received_rows);
        result
    }

    async fn replace_remote_media_library_snapshots_unobserved(
        &self,
        snapshots: Vec<RemoteMediaLibrarySnapshot>,
    ) -> anyhow::Result<Vec<VirtualFolder>> {
        if snapshots.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = snapshots
            .into_iter()
            .map(PreparedRemoteMediaLibrarySnapshot::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut lock_names = prepared
            .iter()
            .map(|snapshot| snapshot.library_name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        lock_names.sort_unstable();
        anyhow::ensure!(
            !lock_names.windows(2).any(|names| names[0] == names[1]),
            "remote snapshot batch contains duplicate virtual folder names"
        );

        let acquire = self.telemetry.start_acquire(DatabasePoolRole::Worker);
        let transaction_result = self.worker_pool.begin().await;
        acquire.finish_result(&transaction_result);
        let mut tx = transaction_result?;
        for lock_name in lock_names {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(lock_name)
                .execute(&mut *tx)
                .await?;
        }
        let mut folders = Vec::with_capacity(prepared.len());
        for snapshot in prepared {
            folders.push(
                self.replace_remote_media_library_snapshot_in_transaction(&mut tx, snapshot)
                    .await?,
            );
        }
        let commit_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncCommit,
            DatabasePoolRole::Worker,
        );
        let commit_result = tx.commit().await.map_err(anyhow::Error::from);
        commit_observation.finish_result(&commit_result, |_| 0);
        commit_result?;
        Ok(folders)
    }

    async fn replace_remote_media_library_snapshot_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        snapshot: PreparedRemoteMediaLibrarySnapshot,
    ) -> anyhow::Result<VirtualFolder> {
        let PreparedRemoteMediaLibrarySnapshot {
            library_name,
            collection_type,
            source_location,
            items: prepared,
        } = snapshot;
        let folder_row = sqlx::query_as::<_, PostgresVirtualFolderRow>(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT ((lower(name))) DO UPDATE SET
                collection_type = excluded.collection_type,
                locations = excluded.locations,
                updated_at = excluded.updated_at
            RETURNING id, name, collection_type, locations, created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&library_name)
        .bind(trimmed_optional_str(Some(&collection_type)))
        .bind(serde_json::to_value(normalized_locations(vec![
            source_location,
        ]))?)
        .fetch_one(&mut **tx)
        .await?;
        let folder_id = folder_row.id;
        let sync_run_id = Uuid::new_v4();
        let generation_id = Uuid::new_v4();
        let sync_started_at = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO catalog_sync_runs (
                id, virtual_folder_id, generation_id, status, item_count, started_at
            )
            VALUES ($1, $2, $3, 'running', $4, $5)
            "#,
        )
        .bind(sync_run_id)
        .bind(folder_id)
        .bind(generation_id)
        .bind(i64::try_from(prepared.len()).context("remote snapshot item count overflow")?)
        .bind(sync_started_at)
        .execute(&mut **tx)
        .await?;

        let stage_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncStage,
            DatabasePoolRole::Worker,
        );
        let stage_result: anyhow::Result<()> = async {
            sqlx::query(
                r#"
            CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_remote_snapshot_stage (
                id uuid PRIMARY KEY,
                name text NOT NULL,
                path text NOT NULL UNIQUE,
                media_type text NOT NULL,
                collection_type text,
                runtime_ticks bigint,
                bitrate bigint,
                width integer,
                height integer,
                media_streams jsonb NOT NULL,
                metadata jsonb NOT NULL
            ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query("TRUNCATE jellyrin_remote_snapshot_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query(
                r#"
                CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_facet_stage (
                    item_id uuid NOT NULL,
                    facet_kind text NOT NULL,
                    normalized_value text NOT NULL,
                    display_value text NOT NULL,
                    stable_id text NOT NULL,
                    position integer NOT NULL,
                    payload jsonb NOT NULL,
                    PRIMARY KEY (item_id, facet_kind, normalized_value)
                ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                r#"
                CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_facet_alias_stage (
                    item_id uuid NOT NULL,
                    facet_kind text NOT NULL,
                    normalized_value text NOT NULL,
                    entity_id text NOT NULL,
                    PRIMARY KEY (item_id, facet_kind, normalized_value, entity_id)
                ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                r#"
                CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_genre_selector_stage (
                    item_id uuid NOT NULL,
                    selector text NOT NULL,
                    PRIMARY KEY (item_id, selector)
                ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                r#"
                CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_upcoming_date_stage (
                    item_id uuid PRIMARY KEY,
                    unix_seconds bigint NOT NULL,
                    nanosecond integer NOT NULL
                ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                r#"
                CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_media_item_filter_selector_stage (
                    item_id uuid NOT NULL,
                    selector_kind text NOT NULL,
                    selector text NOT NULL,
                    PRIMARY KEY (item_id, selector_kind, selector)
                ) ON COMMIT DROP
                "#,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_query_filter_source_stage (\
                 item_id uuid PRIMARY KEY, container_present boolean NOT NULL, \
                 container_value text, media_type text NOT NULL, is_video boolean NOT NULL, \
                 has_subtitles boolean NOT NULL, has_trailer boolean NOT NULL, \
                 projected_value_count integer NOT NULL) ON COMMIT DROP",
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "CREATE TEMPORARY TABLE IF NOT EXISTS jellyrin_query_filter_value_stage (\
                 item_id uuid NOT NULL, value_kind text NOT NULL, display_value text NOT NULL, \
                 source_key text NOT NULL, source_priority integer NOT NULL, \
                 source_position text NOT NULL, \
                 PRIMARY KEY (item_id, value_kind, source_key, source_position)) ON COMMIT DROP",
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query("TRUNCATE jellyrin_media_item_facet_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query("TRUNCATE jellyrin_media_item_facet_alias_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query("TRUNCATE jellyrin_media_item_genre_selector_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query("TRUNCATE jellyrin_media_item_upcoming_date_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query("TRUNCATE jellyrin_media_item_filter_selector_stage")
                .execute(&mut **tx)
                .await?;
            sqlx::query(
                "TRUNCATE jellyrin_query_filter_value_stage, jellyrin_query_filter_source_stage",
            )
            .execute(&mut **tx)
            .await?;

            for chunk in prepared.chunks(REMOTE_SNAPSHOT_INSERT_CHUNK_SIZE) {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO jellyrin_remote_snapshot_stage (\
                 id, name, path, media_type, collection_type, runtime_ticks, bitrate, \
                 width, height, media_streams, metadata) ",
                );
                query.push_values(chunk, |mut values, item| {
                    values
                        .push_bind(item.id)
                        .push_bind(&item.name)
                        .push_bind(&item.path)
                        .push_bind(&item.media_type)
                        .push_bind(&item.collection_type)
                        .push_bind(item.runtime_ticks)
                        .push_bind(item.bitrate)
                        .push_bind(item.width)
                        .push_bind(item.height)
                        .push_bind(&item.media_streams)
                        .push_bind(&item.metadata);
                });
                query.build().execute(&mut **tx).await?;

                let projections = chunk
                    .iter()
                    .map(|item| {
                        let streams = item
                            .media_streams
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or(&[]);
                        let projection = extract_media_item_query_filter_projection(
                            MediaItemQueryFilterProjectionSource {
                                path: &item.path,
                                media_type: &item.media_type,
                                media_streams: streams,
                                metadata: &item.metadata,
                            },
                        );
                        let value_count = i32::try_from(projection.values.len())
                            .context("prepared query-filter value count overflow")?;
                        Ok((item.id, projection, value_count))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                let mut sources = QueryBuilder::<Postgres>::new(
                    "INSERT INTO jellyrin_query_filter_source_stage (item_id, \
                     container_present, container_value, media_type, is_video, has_subtitles, \
                     has_trailer, projected_value_count) ",
                );
                sources.push_values(
                    &projections,
                    |mut row, (item_id, projection, value_count)| {
                        row.push_bind(*item_id)
                            .push_bind(projection.features.container_present)
                            .push_bind(&projection.features.container)
                            .push_bind(&projection.features.media_type)
                            .push_bind(projection.features.is_video)
                            .push_bind(projection.features.has_subtitles)
                            .push_bind(projection.features.has_trailer)
                            .push_bind(*value_count);
                    },
                );
                sources.build().execute(&mut **tx).await?;
                let values = projections
                    .iter()
                    .flat_map(|(item_id, projection, _)| {
                        projection.values.iter().map(move |value| (*item_id, value))
                    })
                    .collect::<Vec<_>>();
                for value_chunk in values.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut insert = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_query_filter_value_stage (item_id, value_kind, \
                         display_value, source_key, source_priority, source_position) ",
                    );
                    insert.push_values(value_chunk, |mut row, (item_id, value)| {
                        row.push_bind(*item_id)
                            .push_bind(value.kind.as_str())
                            .push_bind(&value.display_value)
                            .push_bind(&value.source_key)
                            .push_bind(i32::from(value.source_priority))
                            .push_bind(encode_media_item_query_filter_position(&value.position));
                    });
                    insert.build().execute(&mut **tx).await?;
                }

                let mut facets = Vec::new();
                for item in chunk {
                    for facet in extract_media_item_facets(&item.metadata) {
                        let position = i32::try_from(facet.position)
                            .context("media item facet position overflow")?;
                        facets.push((item.id, facet, position));
                    }
                }
                for facet_chunk in facets.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut query = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_media_item_facet_stage (\
                         item_id, facet_kind, normalized_value, display_value, stable_id, \
                         position, payload) ",
                    );
                    query.push_values(facet_chunk, |mut values, (item_id, facet, position)| {
                        values
                            .push_bind(*item_id)
                            .push_bind(facet.kind.as_str())
                            .push_bind(&facet.normalized_value)
                            .push_bind(&facet.display_value)
                            .push_bind(&facet.stable_id)
                            .push_bind(*position)
                            .push_bind(&facet.payload);
                    });
                    query.build().execute(&mut **tx).await?;
                }
                let aliases = facets
                    .iter()
                    .flat_map(|(item_id, facet, _)| {
                        facet.aliases.iter().map(move |alias| {
                            (
                                *item_id,
                                facet.kind,
                                facet.normalized_value.as_str(),
                                alias.as_str(),
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                for alias_chunk in aliases.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut query = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_media_item_facet_alias_stage (\
                         item_id, facet_kind, normalized_value, entity_id) ",
                    );
                    query.push_values(
                        alias_chunk,
                        |mut values, (item_id, kind, normalized_value, entity_id)| {
                            values
                                .push_bind(*item_id)
                                .push_bind(kind.as_str())
                                .push_bind(*normalized_value)
                                .push_bind(*entity_id);
                        },
                    );
                    query.build().execute(&mut **tx).await?;
                }
                let genre_selectors = chunk
                    .iter()
                    .flat_map(|item| {
                        extract_media_item_genre_selectors(&item.metadata)
                            .into_iter()
                            .map(move |selector| (item.id, selector))
                    })
                    .collect::<Vec<_>>();
                for selector_chunk in genre_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut query = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_media_item_genre_selector_stage \
                         (item_id, selector) ",
                    );
                    query.push_values(selector_chunk, |mut values, (item_id, selector)| {
                        values.push_bind(*item_id).push_bind(selector);
                    });
                    query.build().execute(&mut **tx).await?;
                }
                let filter_selectors = chunk
                    .iter()
                    .flat_map(|item| {
                        extract_media_item_filter_selectors(&item.metadata)
                            .into_iter()
                            .map(move |(kind, selector)| (item.id, kind, selector))
                    })
                    .collect::<Vec<_>>();
                for selector_chunk in filter_selectors.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut query = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_media_item_filter_selector_stage \
                         (item_id, selector_kind, selector) ",
                    );
                    query.push_values(selector_chunk, |mut values, (item_id, kind, selector)| {
                        values
                            .push_bind(*item_id)
                            .push_bind(kind.as_str())
                            .push_bind(selector);
                    });
                    query.build().execute(&mut **tx).await?;
                }
                let upcoming_dates = chunk
                    .iter()
                    .filter_map(|item| {
                        upcoming_media_item_premiere_parts(&item.metadata)
                            .map(|(unix_seconds, nanosecond)| (item.id, unix_seconds, nanosecond))
                    })
                    .collect::<Vec<_>>();
                for date_chunk in upcoming_dates.chunks(FACET_STAGE_INSERT_CHUNK_SIZE) {
                    let mut query = QueryBuilder::<Postgres>::new(
                        "INSERT INTO jellyrin_media_item_upcoming_date_stage \
                         (item_id, unix_seconds, nanosecond) ",
                    );
                    query.push_values(
                        date_chunk,
                        |mut values, (item_id, unix_seconds, nanosecond)| {
                            values
                                .push_bind(*item_id)
                                .push_bind(*unix_seconds)
                                .push_bind(*nanosecond);
                        },
                    );
                    query.build().execute(&mut **tx).await?;
                }
            }
            let projection_counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "SELECT count(*), coalesce(sum(projected_value_count), 0), \
                        (SELECT count(*) FROM jellyrin_query_filter_value_stage), \
                        (SELECT count(*) FROM jellyrin_query_filter_source_stage AS source \
                         WHERE source.projected_value_count <> (SELECT count(*) \
                           FROM jellyrin_query_filter_value_stage AS value \
                           WHERE value.item_id = source.item_id)) \
                 FROM jellyrin_query_filter_source_stage",
            )
            .fetch_one(&mut **tx)
            .await?;
            anyhow::ensure!(
                projection_counts.0
                    == i64::try_from(prepared.len()).context("projection source count overflow")?
                    && projection_counts.1 == projection_counts.2
                    && projection_counts.3 == 0,
                "prepared query-filter projection coverage mismatch"
            );
            Ok(())
        }
        .await;
        stage_observation.finish_result(&stage_result, |_| {
            u64::try_from(prepared.len()).unwrap_or(u64::MAX)
        });
        stage_result?;

        self.publish_prepared_remote_media_library_stage_in_transaction(tx, folder_row, sync_run_id)
            .await
    }

    async fn publish_prepared_remote_media_library_stage_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        folder_row: PostgresVirtualFolderRow,
        sync_run_id: Uuid,
    ) -> anyhow::Result<VirtualFolder> {
        let folder_id = folder_row.id;

        let external_conflicts: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM media_items AS current
            JOIN jellyrin_remote_snapshot_stage AS staged
              ON current.id = staged.id OR current.path = staged.path
            WHERE current.virtual_folder_id <> $1
            "#,
        )
        .bind(folder_id)
        .fetch_one(&mut **tx)
        .await?;
        anyhow::ensure!(
            external_conflicts == 0,
            "remote snapshot contains ids or paths owned by another virtual folder"
        );

        // Tombstone entries no longer present in the provider snapshot. This intentionally does
        // not cascade-delete playback/list state; a later generation may make the item visible
        // again using the same stable id.
        let tombstone_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncTombstone,
            DatabasePoolRole::Worker,
        );
        let tombstone_result = sqlx::query(
            r#"
            UPDATE media_items AS current
            SET missing_since = COALESCE(current.missing_since, CURRENT_TIMESTAMP),
                updated_at = CASE
                    WHEN current.missing_since IS NULL THEN CURRENT_TIMESTAMP
                    ELSE current.updated_at
                END
            WHERE current.virtual_folder_id = $1
              AND (
                    NOT EXISTS (
                        SELECT 1
                        FROM jellyrin_remote_snapshot_stage AS staged
                        WHERE staged.id = current.id
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM jellyrin_remote_snapshot_stage AS staged
                        WHERE staged.path = current.path AND staged.id <> current.id
                    )
              )
            "#,
        )
        .bind(folder_id)
        .execute(&mut **tx)
        .await
        .map_err(anyhow::Error::from);
        tombstone_observation.finish_result(&tombstone_result, |result| result.rows_affected());
        tombstone_result?;

        let merge_observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogSyncMerge,
            DatabasePoolRole::Worker,
        );
        let merge_result = sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                last_seen_at, missing_since, file_size, modified_at,
                runtime_ticks, bitrate, width, height, media_streams, metadata,
                created_at, updated_at
            )
            SELECT staged.id, $1, staged.name, staged.path, staged.media_type,
                   staged.collection_type, CURRENT_TIMESTAMP, NULL, NULL, NULL,
                   staged.runtime_ticks, staged.bitrate, staged.width, staged.height,
                   staged.media_streams, staged.metadata, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            FROM jellyrin_remote_snapshot_stage AS staged
            ON CONFLICT (id) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                name = excluded.name,
                path = excluded.path,
                media_type = excluded.media_type,
                collection_type = excluded.collection_type,
                last_seen_at = excluded.last_seen_at,
                missing_since = NULL,
                file_size = NULL,
                modified_at = NULL,
                runtime_ticks = excluded.runtime_ticks,
                bitrate = excluded.bitrate,
                width = excluded.width,
                height = excluded.height,
                media_streams = excluded.media_streams,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at
            WHERE media_items.missing_since IS NOT NULL
               OR ROW(
                    media_items.virtual_folder_id,
                    media_items.name,
                    media_items.path,
                    media_items.media_type,
                    media_items.collection_type,
                    media_items.runtime_ticks,
                    media_items.bitrate,
                    media_items.width,
                    media_items.height,
                    media_items.media_streams,
                    media_items.metadata
               ) IS DISTINCT FROM ROW(
                    excluded.virtual_folder_id,
                    excluded.name,
                    excluded.path,
                    excluded.media_type,
                    excluded.collection_type,
                    excluded.runtime_ticks,
                    excluded.bitrate,
                    excluded.width,
                    excluded.height,
                    excluded.media_streams,
                    excluded.metadata
               )
            "#,
        )
        .bind(folder_id)
        .execute(&mut **tx)
        .await
        .map_err(anyhow::Error::from);
        merge_observation.finish_result(&merge_result, |result| result.rows_affected());
        merge_result?;

        sqlx::query(
            r#"
            DELETE FROM media_item_facet_aliases AS alias
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE alias.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_media_item_facet_alias_stage AS staged
                  WHERE staged.item_id = alias.item_id
                    AND staged.facet_kind = alias.facet_kind
                    AND staged.normalized_value = alias.normalized_value
                    AND staged.entity_id = alias.entity_id
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            DELETE FROM media_item_facets AS facet
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE facet.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_media_item_facet_stage AS staged
                  WHERE staged.item_id = facet.item_id
                    AND staged.facet_kind = facet.facet_kind
                    AND staged.normalized_value = facet.normalized_value
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_facets (
                item_id, facet_kind, normalized_value, display_value,
                stable_id, position, payload
            )
            SELECT item_id, facet_kind, normalized_value, display_value,
                   stable_id, position, payload
            FROM jellyrin_media_item_facet_stage
            ON CONFLICT (item_id, facet_kind, normalized_value) DO UPDATE SET
                display_value = excluded.display_value,
                stable_id = excluded.stable_id,
                position = excluded.position,
                payload = excluded.payload
            WHERE ROW(
                media_item_facets.display_value,
                media_item_facets.stable_id,
                media_item_facets.position,
                media_item_facets.payload
            ) IS DISTINCT FROM ROW(
                excluded.display_value,
                excluded.stable_id,
                excluded.position,
                excluded.payload
            )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_facet_aliases (
                item_id, facet_kind, normalized_value, entity_id
            )
            SELECT item_id, facet_kind, normalized_value, entity_id
            FROM jellyrin_media_item_facet_alias_stage
            ON CONFLICT (item_id, facet_kind, normalized_value, entity_id) DO NOTHING
            "#,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM media_item_genre_selectors AS current
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE current.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_media_item_genre_selector_stage AS staged
                  WHERE staged.item_id = current.item_id
                    AND staged.selector = current.selector
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_genre_selectors (item_id, selector)
            SELECT item_id, selector
            FROM jellyrin_media_item_genre_selector_stage
            ON CONFLICT (item_id, selector) DO NOTHING
            "#,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM media_item_upcoming_dates AS current
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE current.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_media_item_upcoming_date_stage AS staged
                  WHERE staged.item_id = current.item_id
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_upcoming_dates (item_id, unix_seconds, nanosecond)
            SELECT item_id, unix_seconds, nanosecond
            FROM jellyrin_media_item_upcoming_date_stage
            ON CONFLICT (item_id) DO UPDATE SET
                unix_seconds = excluded.unix_seconds,
                nanosecond = excluded.nanosecond
            WHERE ROW(
                media_item_upcoming_dates.unix_seconds,
                media_item_upcoming_dates.nanosecond
            ) IS DISTINCT FROM ROW(excluded.unix_seconds, excluded.nanosecond)
            "#,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM media_item_filter_selectors AS current
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE current.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_media_item_filter_selector_stage AS staged
                  WHERE staged.item_id = current.item_id
                    AND staged.selector_kind = current.selector_kind
                    AND staged.selector = current.selector
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_filter_selectors (item_id, selector_kind, selector)
            SELECT item_id, selector_kind, selector
            FROM jellyrin_media_item_filter_selector_stage
            ON CONFLICT (item_id, selector_kind, selector) DO NOTHING
            "#,
        )
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO media_item_query_filter_sources (
                item_id, virtual_folder_id, extractor_version, container_present, container_value, media_type,
                is_video, has_subtitles, has_trailer, projected_value_count, completed_at
            )
            SELECT item_id, $2, $1, container_present, container_value, media_type, is_video,
                   has_subtitles, has_trailer, projected_value_count, CURRENT_TIMESTAMP
            FROM jellyrin_query_filter_source_stage
            WHERE TRUE
            ON CONFLICT (item_id) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                extractor_version = excluded.extractor_version,
                container_present = excluded.container_present,
                container_value = excluded.container_value,
                media_type = excluded.media_type,
                is_video = excluded.is_video,
                has_subtitles = excluded.has_subtitles,
                has_trailer = excluded.has_trailer,
                projected_value_count = excluded.projected_value_count,
                completed_at = excluded.completed_at
            WHERE (media_item_query_filter_sources.virtual_folder_id,
                   media_item_query_filter_sources.extractor_version,
                   media_item_query_filter_sources.container_present,
                   media_item_query_filter_sources.container_value,
                   media_item_query_filter_sources.media_type,
                   media_item_query_filter_sources.is_video,
                   media_item_query_filter_sources.has_subtitles,
                   media_item_query_filter_sources.has_trailer,
                   media_item_query_filter_sources.projected_value_count)
                IS DISTINCT FROM
                  (excluded.virtual_folder_id, excluded.extractor_version,
                   excluded.container_present, excluded.container_value,
                   excluded.media_type, excluded.is_video, excluded.has_subtitles,
                   excluded.has_trailer, excluded.projected_value_count)
            "#,
        )
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .bind(folder_id)
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM media_item_query_filter_values AS current
            USING jellyrin_remote_snapshot_stage AS item_stage
            WHERE current.item_id = item_stage.id
              AND NOT EXISTS (
                  SELECT 1
                  FROM jellyrin_query_filter_value_stage AS staged
                  WHERE staged.item_id = current.item_id
                    AND staged.value_kind = current.value_kind
                    AND staged.source_key = current.source_key
                    AND staged.source_position = current.source_position
              )
            "#,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO media_item_query_filter_values (
                item_id, virtual_folder_id, value_kind, display_value, source_key,
                source_priority, source_position
            )
            SELECT item_id, $1, value_kind, display_value, source_key,
                   source_priority, source_position
            FROM jellyrin_query_filter_value_stage
            WHERE TRUE
            ON CONFLICT (item_id, value_kind, source_key, source_position) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                display_value = excluded.display_value,
                source_priority = excluded.source_priority
            WHERE (media_item_query_filter_values.virtual_folder_id,
                   media_item_query_filter_values.display_value,
                   media_item_query_filter_values.source_priority)
                IS DISTINCT FROM
                  (excluded.virtual_folder_id, excluded.display_value,
                   excluded.source_priority)
            "#,
        )
        .bind(folder_id)
        .execute(&mut **tx)
        .await?;
        let incomplete = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM jellyrin_query_filter_source_stage AS staged
                LEFT JOIN media_item_query_filter_sources AS source
                  ON source.item_id = staged.item_id
                 AND source.virtual_folder_id = $1
                 AND source.extractor_version = $2
                 AND source.projected_value_count = staged.projected_value_count
                LEFT JOIN LATERAL (
                    SELECT count(*)::integer AS value_count
                    FROM media_item_query_filter_values AS value
                    WHERE value.item_id = staged.item_id
                      AND value.virtual_folder_id = $1
                ) AS value_count ON TRUE
                WHERE source.item_id IS NULL
                   OR value_count.value_count <> staged.projected_value_count
            )
            "#,
        )
        .bind(folder_id)
        .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_VERSION)
        .fetch_one(&mut **tx)
        .await?;
        anyhow::ensure!(
            !incomplete,
            "query-filter projection publication coverage mismatch"
        );
        Self::rebuild_tv_series_catalog_projection_in_transaction(tx, folder_id).await?;

        sqlx::query(
            r#"
            UPDATE catalog_sync_runs
            SET status = 'completed', completed_at = CURRENT_TIMESTAMP
            WHERE id = $1
            "#,
        )
        .bind(sync_run_id)
        .execute(&mut **tx)
        .await?;

        folder_row.try_into()
    }

    pub async fn media_item_by_id(&self, item_id: Uuid) -> anyhow::Result<MediaItem> {
        let row = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE id = $1 AND missing_since IS NULL
            "#,
        )
        .bind(item_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn media_item_exists(&self, item_id: Uuid) -> anyhow::Result<bool> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogItemExists, DatabasePoolRole::Api);
        let result = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM media_items
                WHERE id = $1 AND missing_since IS NULL
            )
            "#,
        )
        .bind(item_id)
        .fetch_one(&self.pool)
        .await
        .map_err(anyhow::Error::from);
        observation.finish_result(&result, |exists| u64::from(*exists));
        result
    }

    pub async fn media_item_by_id_visible(
        &self,
        item_id: Uuid,
    ) -> anyhow::Result<Option<MediaItem>> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::CatalogItemById, DatabasePoolRole::Api);
        let result = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE id = $1 AND missing_since IS NULL
            "#,
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(Into::into))
        .map_err(anyhow::Error::from);
        observation.finish_result(&result, |item| u64::from(item.is_some()));
        result
    }

    pub async fn delete_media_items(
        &self,
        item_ids: Vec<Uuid>,
        deleted_by_user_id: Option<Uuid>,
    ) -> anyhow::Result<u64> {
        let mut item_ids = item_ids;
        item_ids.sort_unstable();
        item_ids.dedup();
        if item_ids.is_empty() {
            return Ok(0);
        }

        let mut tx = self.pool.begin().await?;
        let visible = sqlx::query_as::<_, PostgresMediaItemPathRow>(
            r#"
            SELECT id, path
            FROM media_items
            WHERE missing_since IS NULL AND id = ANY($1)
            FOR UPDATE
            "#,
        )
        .bind(item_ids)
        .fetch_all(&mut *tx)
        .await?;
        if visible.is_empty() {
            return Ok(0);
        }
        let visible_ids = visible.iter().map(|item| item.id).collect::<Vec<_>>();
        let now = OffsetDateTime::now_utc();

        let mut audit = QueryBuilder::<Postgres>::new(
            "INSERT INTO media_item_deletions \
             (path, item_id, deleted_by_user_id, deleted_at) ",
        );
        audit.push_values(&visible, |mut values, item| {
            values
                .push_bind(&item.path)
                .push_bind(item.id)
                .push_bind(deleted_by_user_id)
                .push_bind(now);
        });
        audit.push(
            " ON CONFLICT (path) DO UPDATE SET \
             item_id = excluded.item_id, \
             deleted_by_user_id = excluded.deleted_by_user_id, \
             deleted_at = excluded.deleted_at",
        );
        audit.build().execute(&mut *tx).await?;

        for statement in [
            "DELETE FROM active_playback_sessions WHERE item_id = ANY($1)",
            "DELETE FROM active_viewing_sessions WHERE item_id = ANY($1)",
            "DELETE FROM transcode_sessions WHERE item_id = ANY($1)",
            "DELETE FROM playback_states WHERE item_id = ANY($1)",
            "DELETE FROM media_list_items WHERE item_id = ANY($1)",
            "DELETE FROM media_item_lyrics WHERE item_id = ANY($1)",
            "DELETE FROM trickplay_infos WHERE item_id = ANY($1)",
        ] {
            sqlx::query(statement)
                .bind(&visible_ids)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            r#"
            DELETE FROM media_item_versions
            WHERE primary_item_id = ANY($1) OR alternate_item_id = ANY($1)
            "#,
        )
        .bind(&visible_ids)
        .execute(&mut *tx)
        .await?;

        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET missing_since = $1, updated_at = $1
            WHERE missing_since IS NULL AND id = ANY($2)
            "#,
        )
        .bind(now)
        .bind(visible_ids)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn media_item_versions(&self, item_id: Uuid) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT item.id, item.virtual_folder_id, item.name, item.path, item.media_type,
                   item.collection_type, item.file_size, item.runtime_ticks, item.bitrate,
                   item.width, item.height, item.media_streams, item.created_at, item.updated_at
            FROM media_items AS item
            WHERE item.missing_since IS NULL
              AND item.id IN (
                    SELECT alternate_item_id
                    FROM media_item_versions
                    WHERE primary_item_id = $1
                    UNION
                    SELECT primary_item_id
                    FROM media_item_versions
                    WHERE alternate_item_id = $1
              )
            ORDER BY lower(item.name), item.name
            "#,
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn merge_media_item_versions(
        &self,
        primary_item_id: Uuid,
        alternate_item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        let mut ids = alternate_item_ids;
        ids.push(primary_item_id);
        ids.sort_unstable();
        ids.dedup();

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM media_item_versions
            WHERE primary_item_id = ANY($1) OR alternate_item_id = ANY($1)
            "#,
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await?;

        let alternate_ids = ids
            .into_iter()
            .filter(|id| *id != primary_item_id)
            .collect::<Vec<_>>();
        if !alternate_ids.is_empty() {
            let now = OffsetDateTime::now_utc();
            let mut insert = QueryBuilder::<Postgres>::new(
                "INSERT INTO media_item_versions \
                 (primary_item_id, alternate_item_id, created_at) ",
            );
            insert.push_values(alternate_ids, |mut values, alternate_id| {
                values
                    .push_bind(primary_item_id)
                    .push_bind(alternate_id)
                    .push_bind(now);
            });
            insert.push(" ON CONFLICT (primary_item_id, alternate_item_id) DO NOTHING");
            insert.build().execute(&mut *tx).await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn clear_media_item_versions(&self, item_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM media_item_versions
            WHERE primary_item_id = $1 OR alternate_item_id = $1
            "#,
        )
        .bind(item_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn latest_media_items(&self, limit: i64) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL
            ORDER BY created_at DESC, lower(name), name
            LIMIT $1
            "#,
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn latest_media_items_for_virtual_folders(
        &self,
        folder_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<MediaItem>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, PostgresMediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type,
                   file_size, runtime_ticks, bitrate, width, height, media_streams,
                   created_at, updated_at
            FROM media_items
            WHERE missing_since IS NULL AND virtual_folder_id = ANY($1)
            ORDER BY updated_at DESC, lower(name), name
            LIMIT $2
            "#,
        )
        .bind(folder_ids.to_vec())
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn update_media_item_media_info(
        &self,
        item_id: Uuid,
        runtime_ticks: Option<i64>,
        bitrate: Option<i64>,
        width: Option<i32>,
        height: Option<i32>,
        media_streams: Vec<Value>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET runtime_ticks = $2, bitrate = $3, width = $4, height = $5,
                media_streams = $6
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(runtime_ticks)
        .bind(bitrate)
        .bind(width)
        .bind(height)
        .bind(serde_json::to_value(media_streams)?)
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        replace_postgres_media_item_query_filter_projection_from_live(&mut tx, item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_media_item_metadata(
        &self,
        item_id: Uuid,
        metadata: Value,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET metadata = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(&metadata)
        .bind(OffsetDateTime::now_utc())
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        replace_postgres_media_item_facets(&mut tx, item_id, &metadata).await?;
        replace_postgres_media_item_query_filter_projection_from_live(&mut tx, item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_media_item_metadata_json(
        &self,
        item_id: &str,
        metadata: &Value,
    ) -> anyhow::Result<()> {
        let item_id = Uuid::parse_str(item_id).context("invalid media item id")?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET metadata = $2, updated_at = $3
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(metadata)
        .bind(OffsetDateTime::now_utc())
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "media item not found");
        replace_postgres_media_item_facets(&mut tx, item_id, metadata).await?;
        replace_postgres_media_item_query_filter_projection_from_live(&mut tx, item_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn media_items_without_primary_image_tag(
        &self,
    ) -> anyhow::Result<Vec<MediaItemForImageTag>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemForImageTagRow>(
            r#"
            SELECT id, path, metadata
            FROM media_items
            WHERE missing_since IS NULL
              AND media_type IN ('Video', 'Audio', 'Photo', 'Book')
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| MediaItemForImageTag {
                id: row.id.to_string(),
                path: row.path,
                metadata_json: row.metadata.to_string(),
            })
            .collect())
    }

    pub async fn media_item_metadata(&self) -> anyhow::Result<Vec<MediaItemMetadata>> {
        let rows = sqlx::query_as::<_, PostgresMediaItemMetadataRow>(
            r#"
            SELECT id, metadata
            FROM media_items
            WHERE missing_since IS NULL
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_item_metadata_by_item_ids(
        &self,
        item_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<Vec<MediaItemMetadata>> {
        let observation = self.telemetry.start_operation(
            DatabaseOperation::CatalogMetadataByIds,
            DatabasePoolRole::Api,
        );
        let result = self
            .media_item_metadata_by_item_ids_unobserved(item_ids)
            .await;
        observation.finish_result(&result, |metadata| {
            u64::try_from(metadata.len()).unwrap_or(u64::MAX)
        });
        result
    }

    async fn media_item_metadata_by_item_ids_unobserved(
        &self,
        item_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<Vec<MediaItemMetadata>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, PostgresMediaItemMetadataRow>(
            r#"
            SELECT id, metadata
            FROM media_items
            WHERE missing_since IS NULL AND id = ANY($1)
            "#,
        )
        .bind(item_ids.iter().copied().collect::<Vec<_>>())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}

const POSTGRES_MEDIA_ITEM_TYPE_SQL: &str = r#"CASE
    WHEN item.media_type = 'Video' AND item.collection_type = 'movies' THEN 'movie'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('musicvideos', 'musicvideo') THEN 'musicvideo'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('tvshows', 'tvshow', 'series')
         AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)'
        THEN 'video'
    WHEN item.media_type = 'Video'
         AND item.collection_type IN ('tvshows', 'tvshow', 'series') THEN 'episode'
    WHEN item.media_type = 'Video' THEN 'video'
    WHEN item.media_type = 'Audio' THEN 'audio'
    WHEN item.media_type = 'Photo' THEN 'photo'
    WHEN item.media_type = 'Book' THEN 'book'
    ELSE 'baseitem'
END"#;

fn push_postgres_catalog_from(builder: &mut QueryBuilder<Postgres>, query: &MediaItemCatalogQuery) {
    let selector_groups = normalized_postgres_filter_selector_groups(query);
    if selector_groups.is_empty() {
        builder.push(" FROM media_items AS item ");
    } else {
        builder.push(" FROM (");
        for (index, (kind, selectors)) in selector_groups.into_iter().enumerate() {
            if index > 0 {
                builder.push(" INTERSECT ");
            }
            builder
                .push(
                    "SELECT DISTINCT filter_selector.item_id \
                     FROM media_item_filter_selectors AS filter_selector \
                     WHERE filter_selector.selector_kind = ",
                )
                .push_bind(kind)
                .push(" AND filter_selector.selector = ANY(")
                .push_bind(selectors)
                .push(")");
        }
        // OFFSET 0 is an intentional optimizer barrier: the small selector intersection must
        // drive indexed PK lookups instead of PostgreSQL flattening the LATERAL and scanning the
        // whole media_items table. This is the same measured shape used by Upcoming candidates.
        builder.push(
            ") AS matching_filter_item \
             CROSS JOIN LATERAL (\
               SELECT candidate.* FROM media_items AS candidate \
               WHERE candidate.id = matching_filter_item.item_id OFFSET 0\
             ) AS item ",
        );
    }
    builder.push(
        "LEFT JOIN playback_states AS playback \
           ON playback.item_id = item.id AND ",
    );
    if let Some(user_id) = query.user_id {
        builder.push("playback.user_id = ").push_bind(user_id);
    } else {
        builder.push("FALSE");
    }
}

fn normalized_postgres_filter_selector_groups(
    query: &MediaItemCatalogQuery,
) -> Vec<(&'static str, Vec<String>)> {
    let mut groups = Vec::new();
    for (kind, values) in [
        ("person", &query.person_ids),
        ("studio", &query.studio_ids),
        ("tag", &query.tags),
    ] {
        let mut selectors = normalized_catalog_values(values);
        selectors.sort_unstable();
        selectors.dedup();
        if !selectors.is_empty() {
            groups.push((kind, selectors));
        }
    }
    groups
}

fn push_postgres_catalog_filters(
    builder: &mut QueryBuilder<Postgres>,
    query: &MediaItemCatalogQuery,
) {
    builder.push(" WHERE item.missing_since IS NULL");

    if !query.ids.is_empty() {
        builder
            .push(" AND item.id = ANY(")
            .push_bind(query.ids.clone())
            .push(")");
    }
    if !query.virtual_folder_ids.is_empty() {
        builder
            .push(" AND item.virtual_folder_id = ANY(")
            .push_bind(query.virtual_folder_ids.clone())
            .push(")");
    }

    let include_item_types = normalized_catalog_values(&query.include_item_types);
    if !include_item_types.is_empty() {
        push_postgres_include_item_types_filter(builder, &include_item_types);
    }
    let exclude_item_types = normalized_catalog_values(&query.exclude_item_types);
    if !exclude_item_types.is_empty() {
        builder
            .push(" AND NOT ((")
            .push(POSTGRES_MEDIA_ITEM_TYPE_SQL)
            .push(") = ANY(")
            .push_bind(exclude_item_types)
            .push("))");
    }

    push_postgres_ci_any_filter(
        builder,
        "item.collection_type",
        &query.collection_types,
        false,
    );
    push_postgres_ci_any_filter(builder, "item.media_type", &query.media_types, false);

    let mut genre_selectors = normalized_catalog_values(&query.genre_ids);
    genre_selectors.sort_unstable();
    genre_selectors.dedup();
    if !genre_selectors.is_empty() {
        builder
            .push(
                " AND EXISTS (\
                 SELECT 1 FROM media_item_genre_selectors AS genre \
                 WHERE genre.item_id = item.id AND genre.selector = ANY(",
            )
            .push_bind(genre_selectors)
            .push("))");
    }

    let containers = normalized_catalog_values(&query.containers)
        .into_iter()
        .map(|container| format!("%.{}", escape_catalog_like_value(&container)))
        .collect::<Vec<_>>();
    if !containers.is_empty() {
        builder
            .push(" AND EXISTS (SELECT 1 FROM unnest(")
            .push_bind(containers)
            .push(
                "::text[]) AS container_suffix(value) \
                 WHERE lower(item.path) LIKE container_suffix.value ESCAPE '\\')",
            );
    }

    let video_types = normalized_catalog_values(&query.video_types);
    if !video_types.is_empty() {
        builder
            .push(
                " AND (CASE WHEN item.media_type = 'Video' \
                       THEN 'videofile' ELSE 'unknown' END) = ANY(",
            )
            .push_bind(video_types)
            .push(")");
    }

    push_postgres_stream_language_filter(
        builder,
        "Audio",
        &normalized_catalog_values(&query.audio_languages),
    );
    push_postgres_stream_language_filter(
        builder,
        "Subtitle",
        &normalized_catalog_values(&query.subtitle_languages),
    );
    if let Some(has_subtitles) = query.has_subtitles {
        builder.push(
            " AND EXISTS (SELECT 1 FROM jsonb_array_elements(item.media_streams) AS stream(value) \
               WHERE lower(stream.value ->> 'Type') = 'subtitle') = ",
        );
        builder.push_bind(has_subtitles);
    }

    if catalog_static_filters_are_impossible(query) {
        builder.push(" AND FALSE");
    }

    if let Some(search_term) = normalized_catalog_scalar(query.search_term.as_deref()) {
        let pattern = format!("%{}%", escape_catalog_like_value(&search_term));
        builder
            .push(" AND (lower(item.name) LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\'");
        match query.search_scope {
            MediaItemCatalogSearchScope::Name => {}
            MediaItemCatalogSearchScope::AllMetadataScalars => {
                builder
                    .push(
                        " OR EXISTS (SELECT 1 \
                           FROM jsonb_path_query(item.metadata, '$.**') AS metadata_scalar(value) \
                           WHERE jsonb_typeof(metadata_scalar.value) IN ('string', 'number') \
                             AND lower(metadata_scalar.value #>> '{}') LIKE ",
                    )
                    .push_bind(pattern)
                    .push(" ESCAPE '\\')");
            }
            MediaItemCatalogSearchScope::SearchHintFields => {
                push_postgres_search_hint_metadata_filter(builder, &pattern);
            }
        }
        builder.push(")");
    }

    if let Some(is_hd) = query.is_hd {
        builder
            .push(" AND COALESCE(item.height >= 720, FALSE) = ")
            .push_bind(is_hd);
    }
    if let Some(is_4k) = query.is_4k {
        builder
            .push(" AND COALESCE(item.width >= 3840 OR item.height >= 2160, FALSE) = ")
            .push_bind(is_4k);
    }
    push_postgres_optional_i64_bound(builder, "item.width", query.min_width, ">=");
    push_postgres_optional_i64_bound(builder, "item.width", query.max_width, "<=");
    push_postgres_optional_i64_bound(builder, "item.height", query.min_height, ">=");
    push_postgres_optional_i64_bound(builder, "item.height", query.max_height, "<=");

    push_postgres_optional_time_bound(builder, "item.created_at", query.min_date_created, ">=");
    push_postgres_optional_time_bound(builder, "item.created_at", query.max_date_created, "<=");
    push_postgres_optional_time_bound(builder, "item.updated_at", query.min_date_last_saved, ">=");
    push_postgres_optional_time_bound(builder, "item.updated_at", query.max_date_last_saved, "<=");

    if let Some(prefix) = normalized_catalog_scalar(query.name_starts_with.as_deref()) {
        builder
            .push(" AND lower(item.name) LIKE ")
            .push_bind(format!("{}%", escape_catalog_like_value(&prefix)))
            .push(" ESCAPE '\\'");
    }
    if let Some(lower_bound) =
        normalized_catalog_scalar(query.name_starts_with_or_greater.as_deref())
    {
        builder
            .push(" AND lower(item.name) >= ")
            .push_bind(lower_bound);
    }
    if let Some(upper_bound) = normalized_catalog_scalar(query.name_less_than.as_deref()) {
        builder
            .push(" AND lower(item.name) < ")
            .push_bind(upper_bound);
    }

    if query.is_played.is_some() || query.favorite.is_some() || query.is_resumable {
        if query.user_id.is_none() {
            builder.push(" AND FALSE");
        } else {
            if let Some(is_played) = query.is_played {
                builder
                    .push(" AND COALESCE(playback.played, FALSE) = ")
                    .push_bind(is_played);
            }
            if let Some(favorite) = query.favorite {
                match favorite {
                    MediaItemFavoriteFilter::Favorite(expected) => {
                        builder
                            .push(" AND COALESCE(playback.is_favorite, FALSE) = ")
                            .push_bind(expected);
                    }
                    MediaItemFavoriteFilter::FavoriteOrLiked => {
                        builder.push(
                            " AND (COALESCE(playback.is_favorite, FALSE) \
                               OR COALESCE(playback.rating > 0, FALSE))",
                        );
                    }
                }
            }
            if query.is_resumable {
                builder.push(" AND playback.position_ticks > 0 AND playback.played = FALSE");
            }
        }
    }
}

fn push_postgres_include_item_types_filter(
    builder: &mut QueryBuilder<Postgres>,
    item_types: &[String],
) {
    let mut item_types = item_types
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    builder.push(" AND (");
    let mut pushed = false;
    for item_type in [
        "movie",
        "musicvideo",
        "episode",
        "video",
        "audio",
        "photo",
        "book",
        "baseitem",
    ] {
        if !item_types.remove(item_type) {
            continue;
        }
        if pushed {
            builder.push(" OR ");
        }
        builder.push(match item_type {
            "movie" => "(item.collection_type = 'movies' AND item.media_type = 'Video')",
            "musicvideo" => {
                "(item.collection_type IN ('musicvideos', 'musicvideo') AND item.media_type = 'Video')"
            }
            "episode" => {
                "(item.collection_type IN ('tvshows', 'tvshow', 'series') AND item.media_type = 'Video' AND lower(item.path) !~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)')"
            }
            "video" => {
                "(item.media_type = 'Video' AND ((item.collection_type IN ('tvshows', 'tvshow', 'series') AND lower(item.path) ~ '(^|/)(extras|featurettes|special features|behind the scenes|deleted scenes|interviews|trailers)(/|$)') OR item.collection_type IS NULL OR item.collection_type NOT IN ('movies', 'musicvideos', 'musicvideo', 'tvshows', 'tvshow', 'series')))"
            }
            "audio" => "item.media_type = 'Audio'",
            "photo" => "item.media_type = 'Photo'",
            "book" => "item.media_type = 'Book'",
            "baseitem" => "item.media_type NOT IN ('Video', 'Audio', 'Photo', 'Book')",
            _ => unreachable!("fixed effective item type"),
        });
        pushed = true;
    }
    if !pushed {
        builder.push("FALSE");
    }
    builder.push(")");
}

fn push_postgres_search_hint_metadata_filter(builder: &mut QueryBuilder<Postgres>, pattern: &str) {
    builder
        .push(
            " OR EXISTS (\
           WITH RECURSIVE hint_values(value) AS (\
             SELECT hint_field.value \
             FROM jsonb_each(item.metadata) AS hint_field(key, value) \
             WHERE hint_field.key = ANY(ARRAY[\
               'Album', 'AlbumName', 'AlbumArtist', 'AlbumArtists', \
               'SeriesName', 'Series', 'Artists'\
             ]::text[]) \
             UNION ALL \
             SELECT array_value.value \
             FROM hint_values \
             CROSS JOIN LATERAL jsonb_array_elements(\
               CASE WHEN jsonb_typeof(hint_values.value) = 'array' \
                    THEN hint_values.value ELSE '[]'::jsonb END\
             ) AS array_value(value)\
           ) \
           SELECT 1 FROM hint_values \
           WHERE (jsonb_typeof(hint_values.value) IN ('string', 'number') \
                  AND lower(hint_values.value #>> '{}') LIKE ",
        )
        .push_bind(pattern.to_owned())
        .push(
            " ESCAPE '\\') \
              OR (jsonb_typeof(hint_values.value) = 'object' \
                  AND jsonb_typeof(hint_values.value -> 'Name') = 'string' \
                  AND lower(hint_values.value ->> 'Name') LIKE ",
        )
        .push_bind(pattern.to_owned())
        .push(
            " ESCAPE '\\')\
         )",
        );
}

fn push_postgres_catalog_order(
    builder: &mut QueryBuilder<Postgres>,
    query: &MediaItemCatalogQuery,
) {
    builder.push(" ORDER BY ");
    let sort = if query.sort.is_empty() {
        &[(
            MediaItemCatalogSortField::SortName,
            SortDirection::Ascending,
        )][..]
    } else {
        query.sort.as_slice()
    };
    for (index, (field, direction)) in sort.iter().take(3).enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push(match field {
            MediaItemCatalogSortField::SortName => "lower(item.name)",
            MediaItemCatalogSortField::DateCreated => "item.created_at",
            MediaItemCatalogSortField::DateLastMediaAdded => "item.updated_at",
        });
        builder.push(match direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
    }
    builder.push(match sort.last().map(|(_, direction)| direction) {
        Some(SortDirection::Descending) => ", item.id DESC",
        Some(SortDirection::Ascending) | None => ", item.id ASC",
    });
}

fn push_postgres_ci_any_filter(
    builder: &mut QueryBuilder<Postgres>,
    column: &str,
    values: &[String],
    negate: bool,
) {
    let values = normalized_catalog_values(values);
    if values.is_empty() {
        return;
    }
    builder.push(if negate {
        " AND NOT (lower("
    } else {
        " AND lower("
    });
    builder
        .push(column)
        .push(") = ANY(")
        .push_bind(values)
        .push(if negate { "))" } else { ")" });
}

fn push_postgres_stream_language_filter(
    builder: &mut QueryBuilder<Postgres>,
    stream_type: &str,
    languages: &[String],
) {
    if languages.is_empty() {
        return;
    }
    builder
        .push(
            " AND EXISTS (SELECT 1 FROM jsonb_array_elements(item.media_streams) AS stream(value) \
               WHERE lower(stream.value ->> 'Type') = lower(",
        )
        .push_bind(stream_type.to_owned())
        .push(
            ") AND CASE lower(btrim(stream.value ->> 'Language')) \
                 WHEN 'fre' THEN 'fra' WHEN 'ger' THEN 'deu' \
                 ELSE lower(btrim(stream.value ->> 'Language')) END = ANY(",
        )
        .push_bind(languages.to_vec())
        .push(") AND lower(btrim(stream.value ->> 'Language')) <> 'und')");
}

fn push_postgres_optional_i64_bound(
    builder: &mut QueryBuilder<Postgres>,
    column: &str,
    value: Option<i64>,
    operator: &str,
) {
    if let Some(value) = value {
        builder
            .push(" AND ")
            .push(column)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(value);
    }
}

fn push_postgres_optional_time_bound(
    builder: &mut QueryBuilder<Postgres>,
    column: &str,
    value: Option<OffsetDateTime>,
    operator: &str,
) {
    if let Some(value) = value {
        builder
            .push(" AND ")
            .push(column)
            .push(" ")
            .push(operator)
            .push(" ")
            .push_bind(value);
    }
}

fn normalized_catalog_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn normalized_catalog_scalar(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn escape_catalog_like_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn catalog_static_filters_are_impossible(query: &MediaItemCatalogQuery) -> bool {
    let location_types = normalized_catalog_values(&query.location_types);
    let exclude_location_types = normalized_catalog_values(&query.exclude_location_types);
    (!location_types.is_empty() && !location_types.iter().any(|value| value == "filesystem"))
        || exclude_location_types
            .iter()
            .any(|value| value == "filesystem")
        || query.is_missing == Some(true)
        || query.is_unaired == Some(true)
        || query.is_folder == Some(true)
}

fn postgres_query_filter_value_count(values: &MediaItemQueryFilterValues) -> u64 {
    let count = values.genres.len()
        + values.tags.len()
        + values.official_ratings.len()
        + values.years.len()
        + values.containers.len()
        + values.media_types.len()
        + values.video_types.len()
        + values.series_statuses.len()
        + values.staff_names.len()
        + values.artists.len()
        + values.albums.len()
        + values.studios.len()
        + values.audio_languages.len()
        + values.subtitle_languages.len();
    u64::try_from(count).unwrap_or(u64::MAX)
}

fn postgres_query_filter_values_from_rows(
    rows: Vec<PostgresQueryFilterValueRow>,
) -> anyhow::Result<MediaItemQueryFilterValues> {
    let mut result = MediaItemQueryFilterValues::default();
    for row in rows {
        match row.kind.as_str() {
            "albums" => result.albums.push(row.display_value),
            "artists" => result.artists.push(row.display_value),
            "audio_languages" => result.audio_languages.push(row.display_value),
            "containers" => result.containers.push(row.display_value),
            "genres" => result.genres.push(row.display_value),
            "has_subtitles" => result.has_subtitles = true,
            "has_trailer" => result.has_trailer = true,
            "media_types" => result.media_types.push(row.display_value),
            "official_ratings" => result.official_ratings.push(row.display_value),
            "series_statuses" => result.series_statuses.push(row.display_value),
            "staff_names" => result.staff_names.push(row.display_value),
            "studios" => result.studios.push(row.display_value),
            "subtitle_languages" => result.subtitle_languages.push(row.display_value),
            "tags" => result.tags.push(row.display_value),
            "video_types" => result.video_types.push(row.display_value),
            "years" => result.years.push(row.display_value),
            unexpected => anyhow::bail!("unexpected PostgreSQL item-filter kind {unexpected}"),
        }
    }
    Ok(result)
}

#[derive(sqlx::FromRow)]
struct PostgresVirtualFolderRow {
    id: Uuid,
    name: String,
    collection_type: Option<String>,
    locations: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<PostgresVirtualFolderRow> for VirtualFolder {
    type Error = anyhow::Error;

    fn try_from(row: PostgresVirtualFolderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            collection_type: row.collection_type,
            locations: parse_locations(row.locations)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemRow {
    id: Uuid,
    virtual_folder_id: Uuid,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresMediaItemRow> for MediaItem {
    fn from(row: PostgresMediaItemRow) -> Self {
        let media_streams = row.media_streams.as_array().cloned().unwrap_or_default();
        Self {
            id: row.id,
            virtual_folder_id: row.virtual_folder_id,
            name: row.name,
            path: row.path,
            media_type: row.media_type,
            collection_type: row.collection_type,
            file_size: row.file_size,
            runtime_ticks: row.runtime_ticks,
            bitrate: row.bitrate,
            width: row.width,
            height: row.height,
            media_streams,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemCatalogRow {
    id: Uuid,
    virtual_folder_id: Uuid,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    file_size: Option<i64>,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams: Value,
    metadata: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    playback_user_id: Option<Uuid>,
    playback_item_id: Option<Uuid>,
    playback_media_source_id: Option<String>,
    playback_audio_stream_index: Option<i64>,
    playback_subtitle_stream_index: Option<i64>,
    playback_position_ticks: Option<i64>,
    playback_is_paused: Option<bool>,
    playback_played: Option<bool>,
    playback_is_favorite: Option<bool>,
    playback_rating: Option<f64>,
    playback_updated_at: Option<OffsetDateTime>,
}

#[derive(sqlx::FromRow)]
struct PostgresQueryFilterValueRow {
    kind: String,
    display_value: String,
}

impl TryFrom<PostgresMediaItemCatalogRow> for MediaItemCatalogEntry {
    type Error = anyhow::Error;

    fn try_from(row: PostgresMediaItemCatalogRow) -> Result<Self, Self::Error> {
        let playback_state = if let Some(user_id) = row.playback_user_id {
            Some(PlaybackState {
                user_id,
                item_id: row
                    .playback_item_id
                    .context("catalog playback row is missing item id")?,
                media_source_id: row.playback_media_source_id,
                audio_stream_index: row.playback_audio_stream_index,
                subtitle_stream_index: row.playback_subtitle_stream_index,
                position_ticks: row
                    .playback_position_ticks
                    .context("catalog playback row is missing position ticks")?,
                is_paused: row
                    .playback_is_paused
                    .context("catalog playback row is missing paused flag")?,
                played: row
                    .playback_played
                    .context("catalog playback row is missing played flag")?,
                is_favorite: row
                    .playback_is_favorite
                    .context("catalog playback row is missing favorite flag")?,
                rating: row.playback_rating,
                updated_at: row
                    .playback_updated_at
                    .context("catalog playback row is missing updated timestamp")?,
            })
        } else {
            None
        };
        let media_streams = row.media_streams.as_array().cloned().unwrap_or_default();
        Ok(Self {
            item: MediaItem {
                id: row.id,
                virtual_folder_id: row.virtual_folder_id,
                name: row.name,
                path: row.path,
                media_type: row.media_type,
                collection_type: row.collection_type,
                file_size: row.file_size,
                runtime_ticks: row.runtime_ticks,
                bitrate: row.bitrate,
                width: row.width,
                height: row.height,
                media_streams,
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
            metadata: row.metadata,
            playback_state,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemCountRow {
    virtual_folder_id: Uuid,
    count: i64,
}

#[derive(sqlx::FromRow)]
struct PostgresCatalogAggregateRow {
    item_count: i64,
    movie_count: i64,
    episode_count: i64,
    song_count: i64,
    music_video_count: i64,
    book_count: i64,
}

#[derive(sqlx::FromRow)]
struct PostgresCatalogCountProjectionRow {
    name: String,
    path: String,
    item_type: String,
    album: Option<String>,
    album_name: Option<String>,
    artists: Option<String>,
    album_artists: Option<String>,
    remote_trailers: Option<String>,
    trailers: Option<String>,
}

fn nonnegative_catalog_count(value: i64, label: &str) -> anyhow::Result<u64> {
    u64::try_from(value).with_context(|| format!("{label} catalog count was negative"))
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemFilterRow {
    path: String,
    media_type: String,
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemPathRow {
    id: Uuid,
    path: String,
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemMetadataRow {
    id: Uuid,
    metadata: Value,
}

impl From<PostgresMediaItemMetadataRow> for MediaItemMetadata {
    fn from(row: PostgresMediaItemMetadataRow) -> Self {
        Self {
            item_id: row.id,
            payload: row.metadata,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaItemForImageTagRow {
    id: Uuid,
    path: String,
    metadata: Value,
}

struct PreparedRemoteMediaItem {
    id: Uuid,
    name: String,
    path: String,
    media_type: String,
    collection_type: String,
    runtime_ticks: Option<i64>,
    bitrate: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    media_streams: Value,
    metadata: Value,
}

struct PreparedRemoteMediaLibrarySnapshot {
    library_name: String,
    collection_type: String,
    source_location: String,
    items: Vec<PreparedRemoteMediaItem>,
}

impl TryFrom<RemoteMediaLibrarySnapshot> for PreparedRemoteMediaLibrarySnapshot {
    type Error = anyhow::Error;

    fn try_from(snapshot: RemoteMediaLibrarySnapshot) -> Result<Self, Self::Error> {
        let library_name = snapshot.library_name.trim().to_owned();
        anyhow::ensure!(
            !library_name.is_empty(),
            "virtual folder name must not be empty"
        );
        Ok(Self {
            library_name,
            collection_type: snapshot.collection_type.trim().to_owned(),
            source_location: snapshot.source_location.trim().to_owned(),
            items: snapshot
                .items
                .into_iter()
                .map(PreparedRemoteMediaItem::try_from)
                .collect::<anyhow::Result<Vec<_>>>()?,
        })
    }
}

impl TryFrom<RemoteMediaItemUpsert> for PreparedRemoteMediaItem {
    type Error = anyhow::Error;

    fn try_from(item: RemoteMediaItemUpsert) -> Result<Self, Self::Error> {
        let raw_id = item.id.trim();
        Ok(Self {
            id: Uuid::parse_str(raw_id)
                .with_context(|| format!("invalid remote media item id: {raw_id}"))?,
            name: item.name.trim().to_owned(),
            path: item.path.trim().to_owned(),
            media_type: item.media_type.trim().to_owned(),
            collection_type: item.collection_type.trim().to_owned(),
            runtime_ticks: item.runtime_ticks,
            bitrate: item.bitrate,
            width: item.width,
            height: item.height,
            media_streams: serde_json::to_value(item.media_streams)?,
            metadata: item.metadata,
        })
    }
}

fn parse_locations(locations: Value) -> anyhow::Result<Vec<String>> {
    serde_json::from_value(locations).context("invalid virtual folder locations in database")
}

pub(super) async fn replace_postgres_media_item_facets(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    item_id: Uuid,
    metadata: &Value,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM media_item_filter_selectors WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_upcoming_dates WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_genre_selectors WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM media_item_facets WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    for facet in extract_media_item_facets(metadata) {
        sqlx::query(
            r#"
            INSERT INTO media_item_facets (
                item_id, facet_kind, normalized_value, display_value,
                stable_id, position, payload
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(item_id)
        .bind(facet.kind.as_str())
        .bind(&facet.normalized_value)
        .bind(&facet.display_value)
        .bind(&facet.stable_id)
        .bind(i32::try_from(facet.position).context("media item facet position overflow")?)
        .bind(&facet.payload)
        .execute(&mut **tx)
        .await?;
        for alias in facet.aliases {
            sqlx::query(
                r#"
                INSERT INTO media_item_facet_aliases (
                    item_id, facet_kind, normalized_value, entity_id
                ) VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(item_id)
            .bind(facet.kind.as_str())
            .bind(&facet.normalized_value)
            .bind(alias)
            .execute(&mut **tx)
            .await?;
        }
    }
    for selector in extract_media_item_genre_selectors(metadata) {
        sqlx::query("INSERT INTO media_item_genre_selectors (item_id, selector) VALUES ($1, $2)")
            .bind(item_id)
            .bind(selector)
            .execute(&mut **tx)
            .await?;
    }
    for (kind, selector) in extract_media_item_filter_selectors(metadata) {
        sqlx::query(
            "INSERT INTO media_item_filter_selectors \
             (item_id, selector_kind, selector) VALUES ($1, $2, $3)",
        )
        .bind(item_id)
        .bind(kind.as_str())
        .bind(selector)
        .execute(&mut **tx)
        .await?;
    }
    if let Some((unix_seconds, nanosecond)) = upcoming_media_item_premiere_parts(metadata) {
        sqlx::query(
            "INSERT INTO media_item_upcoming_dates \
             (item_id, unix_seconds, nanosecond) VALUES ($1, $2, $3)",
        )
        .bind(item_id)
        .bind(unix_seconds)
        .bind(nanosecond)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub(super) async fn replace_postgres_media_item_query_filter_projection(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    item_id: Uuid,
    folder_id: Uuid,
    projection: &MediaItemQueryFilterProjection,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM media_item_query_filter_sources WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        r#"
        INSERT INTO media_item_query_filter_sources (
            item_id, virtual_folder_id, extractor_version, container_present, container_value, media_type,
            is_video, has_subtitles, has_trailer, projected_value_count, completed_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, CURRENT_TIMESTAMP)
        "#,
    )
    .bind(item_id)
    .bind(folder_id)
    .bind(projection.extractor_version)
    .bind(projection.features.container_present)
    .bind(&projection.features.container)
    .bind(&projection.features.media_type)
    .bind(projection.features.is_video)
    .bind(projection.features.has_subtitles)
    .bind(projection.features.has_trailer)
    .bind(i32::try_from(projection.values.len()).context("query-filter value count overflow")?)
    .execute(&mut **tx)
    .await?;
    for value in &projection.values {
        sqlx::query(
            r#"
            INSERT INTO media_item_query_filter_values (
                item_id, virtual_folder_id, value_kind, display_value, source_key,
                source_priority, source_position
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(item_id)
        .bind(folder_id)
        .bind(value.kind.as_str())
        .bind(&value.display_value)
        .bind(&value.source_key)
        .bind(i32::from(value.source_priority))
        .bind(encode_media_item_query_filter_position(&value.position))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn replace_postgres_media_item_query_filter_projection_from_live(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    item_id: Uuid,
) -> anyhow::Result<()> {
    let (folder_id, path, media_type, media_streams, metadata) =
        sqlx::query_as::<_, (Uuid, String, String, Value, Value)>(
            "SELECT virtual_folder_id, path, media_type, media_streams, metadata FROM media_items WHERE id = $1",
        )
        .bind(item_id)
        .fetch_one(&mut **tx)
        .await?;
    let streams = media_streams.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let projection =
        extract_media_item_query_filter_projection(MediaItemQueryFilterProjectionSource {
            path: &path,
            media_type: &media_type,
            media_streams: streams,
            metadata: &metadata,
        });
    replace_postgres_media_item_query_filter_projection(tx, item_id, folder_id, &projection).await
}

fn normalized_locations(locations: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for location in locations {
        let location = location.trim();
        if !location.is_empty() && !normalized.iter().any(|value| value == location) {
            normalized.push(location.to_owned());
        }
    }
    normalized
}

fn trimmed_optional_str(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn escaped_path_prefix(path: &str) -> String {
    let escaped = path
        .trim_end_matches('/')
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("{escaped}/%")
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, time::Instant};

    use serde_json::json;
    use sqlx::{
        PgConnection, PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    };

    use super::*;

    struct IsolatedPostgres {
        database: PostgresDatabase,
        admin_pool: PgPool,
        schema: String,
    }

    impl IsolatedPostgres {
        async fn create() -> Option<Self> {
            let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
                return None;
            };
            let base_options = PgConnectOptions::from_str(&database_url)
                .expect("JELLYRIN_TEST_POSTGRES_URL must be a valid PostgreSQL URL");
            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(base_options.clone())
                .await
                .expect("failed to connect PostgreSQL catalog-test admin pool");
            let schema = format!("jellyrin_catalog_test_{}", Uuid::new_v4().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&admin_pool)
                .await
                .expect("failed to create isolated PostgreSQL test schema");

            let search_path = format!("{schema},public");
            let scoped_options = base_options.options([("search_path", &search_path)]);
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .connect_with(scoped_options.clone())
                .await
                .expect("failed to connect isolated PostgreSQL API pool");
            let worker_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(scoped_options)
                .await
                .expect("failed to connect isolated PostgreSQL worker pool");
            let database = PostgresDatabase {
                pool,
                worker_pool,
                provider_secret_vault: None,
                telemetry: std::sync::Arc::new(crate::telemetry::DatabaseTelemetry::default()),
            };
            if let Err(error) = database.migrate().await {
                database.close().await;
                sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
                    .execute(&admin_pool)
                    .await
                    .expect("failed to clean schema after migration failure");
                panic!("failed to migrate isolated PostgreSQL schema: {error:#}");
            }

            Some(Self {
                database,
                admin_pool,
                schema,
            })
        }

        async fn cleanup(self) {
            self.database.close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&self.admin_pool)
            .await
            .expect("failed to drop isolated PostgreSQL test schema");
            self.admin_pool.close().await;
        }
    }

    fn percentile_from_sorted_samples(samples: &[u128], percent: usize) -> u128 {
        let index = ((samples.len() - 1) * percent).div_ceil(100);
        samples[index]
    }

    const SNAPSHOT_STAGE_BENCHMARK_CHUNK_SIZE: usize = 1_000;
    const SNAPSHOT_STAGE_COLUMNS: &str = "(id, name, path, media_type, collection_type, \
        runtime_ticks, bitrate, width, height, media_streams, metadata)";
    const SNAPSHOT_STAGE_TABLE_DEFINITION: &str = r#"(
        id uuid PRIMARY KEY,
        name text NOT NULL,
        path text NOT NULL UNIQUE,
        media_type text NOT NULL,
        collection_type text,
        runtime_ticks bigint,
        bitrate bigint,
        width integer,
        height integer,
        media_streams jsonb NOT NULL,
        metadata jsonb NOT NULL
    )"#;

    fn benchmark_remote_item(index: usize) -> PreparedRemoteMediaItem {
        PreparedRemoteMediaItem {
            id: Uuid::from_u128(index as u128 + 1),
            name: format!("Benchmark item {index}"),
            path: format!("provider://benchmark/library/{index}.mkv"),
            media_type: "Video".to_owned(),
            collection_type: "movies".to_owned(),
            runtime_ticks: Some(54_000_000_000),
            bitrate: Some(4_000_000),
            width: Some(1920),
            height: Some(1080),
            media_streams: json!([{"Type": "Video", "Codec": "h264"}]),
            metadata: json!({"Provider": "benchmark", "Index": index}),
        }
    }

    fn push_postgres_copy_text_field(output: &mut Vec<u8>, value: Option<&str>) {
        let Some(value) = value else {
            output.extend_from_slice(br"\N");
            return;
        };
        for character in value.chars() {
            match character {
                '\\' => output.extend_from_slice(br"\\"),
                '\u{8}' => output.extend_from_slice(br"\b"),
                '\u{c}' => output.extend_from_slice(br"\f"),
                '\n' => output.extend_from_slice(br"\n"),
                '\r' => output.extend_from_slice(br"\r"),
                '\t' => output.extend_from_slice(br"\t"),
                '\u{b}' => output.extend_from_slice(br"\v"),
                _ => {
                    let mut encoded = [0; 4];
                    output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
            }
        }
    }

    fn push_postgres_copy_text_row(output: &mut Vec<u8>, item: &PreparedRemoteMediaItem) {
        let fields = [
            Some(item.id.to_string()),
            Some(item.name.clone()),
            Some(item.path.clone()),
            Some(item.media_type.clone()),
            Some(item.collection_type.clone()),
            item.runtime_ticks.map(|value| value.to_string()),
            item.bitrate.map(|value| value.to_string()),
            item.width.map(|value| value.to_string()),
            item.height.map(|value| value.to_string()),
            Some(item.media_streams.to_string()),
            Some(item.metadata.to_string()),
        ];
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push(b'\t');
            }
            push_postgres_copy_text_field(output, field.as_deref());
        }
        output.push(b'\n');
    }

    async fn benchmark_query_builder_stage(
        connection: &mut PgConnection,
        row_count: usize,
    ) -> anyhow::Result<(u128, u64)> {
        let started = Instant::now();
        for chunk_start in (0..row_count).step_by(SNAPSHOT_STAGE_BENCHMARK_CHUNK_SIZE) {
            let chunk_end = (chunk_start + SNAPSHOT_STAGE_BENCHMARK_CHUNK_SIZE).min(row_count);
            let items = (chunk_start..chunk_end)
                .map(benchmark_remote_item)
                .collect::<Vec<_>>();
            let mut query = QueryBuilder::<Postgres>::new(format!(
                "INSERT INTO jellyrin_snapshot_stage_query_builder {SNAPSHOT_STAGE_COLUMNS} "
            ));
            query.push_values(&items, |mut values, item| {
                values
                    .push_bind(item.id)
                    .push_bind(&item.name)
                    .push_bind(&item.path)
                    .push_bind(&item.media_type)
                    .push_bind(&item.collection_type)
                    .push_bind(item.runtime_ticks)
                    .push_bind(item.bitrate)
                    .push_bind(item.width)
                    .push_bind(item.height)
                    .push_bind(&item.media_streams)
                    .push_bind(&item.metadata);
            });
            query.build().execute(&mut *connection).await?;
        }
        let elapsed_millis = started.elapsed().as_millis();
        let inserted = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM jellyrin_snapshot_stage_query_builder",
        )
        .fetch_one(&mut *connection)
        .await?;
        Ok((elapsed_millis, u64::try_from(inserted)?))
    }

    async fn benchmark_copy_text_stage(
        connection: &mut PgConnection,
        row_count: usize,
    ) -> anyhow::Result<(u128, u64)> {
        let started = Instant::now();
        let statement = format!(
            "COPY jellyrin_snapshot_stage_copy {SNAPSHOT_STAGE_COLUMNS} FROM STDIN WITH (FORMAT text)"
        );
        let mut copy = connection.copy_in_raw(&statement).await?;
        for chunk_start in (0..row_count).step_by(SNAPSHOT_STAGE_BENCHMARK_CHUNK_SIZE) {
            let chunk_end = (chunk_start + SNAPSHOT_STAGE_BENCHMARK_CHUNK_SIZE).min(row_count);
            let mut payload = Vec::with_capacity((chunk_end - chunk_start) * 256);
            for index in chunk_start..chunk_end {
                push_postgres_copy_text_row(&mut payload, &benchmark_remote_item(index));
            }
            copy.send(payload).await?;
        }
        let inserted = copy.finish().await?;
        Ok((started.elapsed().as_millis(), inserted))
    }

    async fn benchmark_snapshot_stage_once(
        connection: &mut PgConnection,
        row_count: usize,
    ) -> anyhow::Result<serde_json::Value> {
        sqlx::query(
            r#"
            TRUNCATE jellyrin_snapshot_stage_query_builder, jellyrin_snapshot_stage_copy
            "#,
        )
        .execute(&mut *connection)
        .await?;
        let (query_builder_millis, query_builder_rows) =
            benchmark_query_builder_stage(connection, row_count).await?;
        let (copy_millis, copy_rows) = benchmark_copy_text_stage(connection, row_count).await?;
        anyhow::ensure!(query_builder_rows == row_count as u64);
        anyhow::ensure!(copy_rows == row_count as u64);
        let speedup_ratio = if copy_millis == 0 {
            None
        } else {
            Some(query_builder_millis as f64 / copy_millis as f64)
        };
        Ok(json!({
            "benchmark": "postgres_snapshot_stage_copy",
            "rows": row_count,
            "query_builder": {
                "wall_millis": query_builder_millis,
                "inserted_rows": query_builder_rows,
            },
            "copy_text": {
                "wall_millis": copy_millis,
                "inserted_rows": copy_rows,
            },
            "query_builder_over_copy_ratio": speedup_ratio,
        }))
    }

    #[test]
    fn postgres_copy_text_encoder_escapes_delimiters_controls_and_null() {
        let mut encoded = Vec::new();
        push_postgres_copy_text_field(
            &mut encoded,
            Some("slash\\ tab\t line\n return\r back\u{8} form\u{c} vertical\u{b} café"),
        );
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            r"slash\\ tab\t line\n return\r back\b form\f vertical\v café"
        );

        let mut encoded_null = Vec::new();
        push_postgres_copy_text_field(&mut encoded_null, None);
        assert_eq!(encoded_null, br"\N");
    }

    #[test]
    fn postgres_include_item_types_use_sargable_native_predicates() {
        let mut query = MediaItemCatalogQuery {
            include_item_types: vec!["Movie".to_string()],
            ..MediaItemCatalogQuery::default()
        };
        let mut builder = QueryBuilder::<Postgres>::new("SELECT item.id ");
        push_postgres_catalog_from(&mut builder, &query);
        push_postgres_catalog_filters(&mut builder, &query);
        let movie_sql = builder.sql();
        let movie_sql = movie_sql.as_str();
        assert!(movie_sql.contains("item.collection_type = 'movies'"));
        assert!(movie_sql.contains("item.media_type = 'Video'"));
        assert!(!movie_sql.contains(POSTGRES_MEDIA_ITEM_TYPE_SQL));

        query.include_item_types = vec![
            "Audio,Photo".to_string(),
            "Book".to_string(),
            "BaseItem".to_string(),
        ];
        let mut builder = QueryBuilder::<Postgres>::new("SELECT item.id ");
        push_postgres_catalog_from(&mut builder, &query);
        push_postgres_catalog_filters(&mut builder, &query);
        let simple_sql = builder.sql();
        let simple_sql = simple_sql.as_str();
        for predicate in [
            "item.media_type = 'Audio'",
            "item.media_type = 'Photo'",
            "item.media_type = 'Book'",
            "item.media_type NOT IN ('Video', 'Audio', 'Photo', 'Book')",
        ] {
            assert!(simple_sql.contains(predicate), "missing {predicate}");
        }
        assert!(!simple_sql.contains(POSTGRES_MEDIA_ITEM_TYPE_SQL));

        query.include_item_types = vec!["Unknown".to_string()];
        let mut builder = QueryBuilder::<Postgres>::new("SELECT item.id ");
        push_postgres_catalog_from(&mut builder, &query);
        push_postgres_catalog_filters(&mut builder, &query);
        assert!(builder.sql().as_str().contains("AND (FALSE)"));
    }

    #[tokio::test]
    #[ignore = "local PostgreSQL benchmark; requires JELLYRIN_TEST_POSTGRES_URL"]
    async fn postgres_snapshot_stage_copy_benchmark() {
        let Some(test) = IsolatedPostgres::create().await else {
            println!(
                "{}",
                json!({
                    "benchmark": "postgres_snapshot_stage_copy",
                    "skipped": true,
                    "reason": "JELLYRIN_TEST_POSTGRES_URL is not set",
                })
            );
            return;
        };
        let result = async {
            let mut transaction = test.database.worker_pool.begin().await?;
            for table in [
                "jellyrin_snapshot_stage_query_builder",
                "jellyrin_snapshot_stage_copy",
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "CREATE TEMPORARY TABLE {table} \
                     {SNAPSHOT_STAGE_TABLE_DEFINITION} ON COMMIT DROP"
                )))
                .execute(&mut *transaction)
                .await?;
            }

            for row_count in [100_000] {
                println!(
                    "{}",
                    benchmark_snapshot_stage_once(&mut transaction, row_count).await?
                );
            }
            if std::env::var("JELLYRIN_BENCHMARK_INCLUDE_500K").as_deref() == Ok("1") {
                println!(
                    "{}",
                    benchmark_snapshot_stage_once(&mut transaction, 500_000).await?
                );
            } else {
                println!(
                    "{}",
                    json!({
                        "benchmark": "postgres_snapshot_stage_copy",
                        "rows": 500_000,
                        "skipped": true,
                        "reason": "set JELLYRIN_BENCHMARK_INCLUDE_500K=1 to include this load",
                    })
                );
            }
            transaction.rollback().await?;
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "local PostgreSQL benchmark; requires JELLYRIN_TEST_POSTGRES_URL"]
    async fn postgres_tv_series_projection_benchmark() {
        let Some(test) = IsolatedPostgres::create().await else {
            println!(
                "{}",
                json!({
                    "benchmark": "postgres_tv_series_projection",
                    "skipped": true,
                    "reason": "JELLYRIN_TEST_POSTGRES_URL is not set",
                })
            );
            return;
        };
        let result = async {
            let episode_count = std::env::var("JELLYRIN_SERIES_BENCHMARK_EPISODES")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(100_000);
            let series_count = std::env::var("JELLYRIN_SERIES_BENCHMARK_SERIES")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(5_000);
            anyhow::ensure!(episode_count > 0 && series_count > 0);
            anyhow::ensure!(series_count <= episode_count);
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Series Projection Benchmark",
                    "tvshows",
                    "provider://series-benchmark",
                    Vec::new(),
                )
                .await?;
            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    last_seen_at, media_streams, metadata, created_at, updated_at
                )
                SELECT md5('series-projection-episode-' || row_number)::uuid,
                       $1,
                       format('Series %s Episode %s', series_number, row_number),
                       'provider://series-benchmark/' || row_number || '.mp4',
                       'Video', 'tvshows', CURRENT_TIMESTAMP, '[]'::jsonb,
                       jsonb_build_object(
                           'SeriesId', md5('series-projection-series-' || series_number),
                           'SeriesName', format('Series %s', series_number)
                       ),
                       CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
                FROM (
                    SELECT row_number,
                           ((row_number - 1) % $3) + 1 AS series_number
                    FROM generate_series(1, $2) AS generated(row_number)
                ) AS fixture
                "#,
            )
            .bind(folder.id)
            .bind(episode_count)
            .bind(series_count)
            .execute(&test.database.worker_pool)
            .await?;

            let rebuild_started = Instant::now();
            let mut transaction = test.database.worker_pool.begin().await?;
            PostgresDatabase::rebuild_tv_series_catalog_projection_in_transaction(
                &mut transaction,
                folder.id,
            )
            .await?;
            transaction.commit().await?;
            let rebuild_millis = rebuild_started.elapsed().as_millis();

            let page_started = Instant::now();
            for page in 0..50 {
                let result = test
                    .database
                    .tv_series_catalog_page(Some(folder.id), page * 50, 50)
                    .await?
                    .context("benchmark projection unexpectedly uncovered")?;
                anyhow::ensure!(result.total_record_count == usize::try_from(series_count)?);
            }
            let page_millis = page_started.elapsed().as_millis();
            println!(
                "{}",
                json!({
                    "benchmark": "postgres_tv_series_projection",
                    "episodes": episode_count,
                    "series": series_count,
                    "rebuild_millis": rebuild_millis,
                    "page_requests": 50,
                    "page_total_millis": page_millis,
                    "page_average_millis": page_millis / 50,
                })
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "local PostgreSQL benchmark; requires JELLYRIN_TEST_POSTGRES_URL"]
    async fn postgres_movie_filter_values_benchmark() {
        let Some(test) = IsolatedPostgres::create().await else {
            println!(
                "{}",
                json!({
                    "benchmark": "postgres_movie_filter_values",
                    "skipped": true,
                    "reason": "JELLYRIN_TEST_POSTGRES_URL is not set",
                })
            );
            return;
        };
        let result = async {
            let episode_count = std::env::var("JELLYRIN_FILTER_BENCHMARK_EPISODES")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(455_520);
            let movie_count = std::env::var("JELLYRIN_FILTER_BENCHMARK_MOVIES")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(39_093);
            let repetitions = std::env::var("JELLYRIN_FILTER_BENCHMARK_REPETITIONS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(12);
            let (selection_name, selection) =
                match std::env::var("JELLYRIN_FILTER_BENCHMARK_SELECTION")
                    .as_deref()
                    .unwrap_or("items")
                {
                    "items" => ("items", MediaItemQueryFilterSelection::ITEMS_FILTERS),
                    "filters2" => ("filters2", MediaItemQueryFilterSelection::FILTERS2),
                    "all" => ("all", MediaItemQueryFilterSelection::ALL),
                    value => anyhow::bail!(
                        "unsupported JELLYRIN_FILTER_BENCHMARK_SELECTION value: {value}"
                    ),
                };
            anyhow::ensure!(episode_count >= 0 && movie_count > 0 && repetitions > 0);

            let movie_folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Filter Benchmark Movies",
                    "movies",
                    "provider://filter-benchmark/movies",
                    Vec::new(),
                )
                .await?;
            let series_folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Filter Benchmark Series",
                    "tvshows",
                    "provider://filter-benchmark/series",
                    Vec::new(),
                )
                .await?;

            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    last_seen_at, media_streams, metadata, created_at, updated_at
                )
                SELECT md5('filter-benchmark-episode-' || value)::uuid, $1,
                       format('Episode %s', value),
                       'provider://filter-benchmark/series/' || value || '.ts',
                       'Video', 'tvshows', CURRENT_TIMESTAMP, '[]'::jsonb,
                       '{}'::jsonb, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
                FROM generate_series(1, $2) AS generated(value)
                "#,
            )
            .bind(series_folder.id)
            .bind(episode_count)
            .execute(&test.database.worker_pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    last_seen_at, media_streams, metadata, created_at, updated_at
                )
                SELECT md5('filter-benchmark-movie-' || value)::uuid, $1,
                       format('Movie %s', value),
                       'provider://filter-benchmark/movies/' || value || '.mkv',
                       'Video', 'movies', CURRENT_TIMESTAMP, '[]'::jsonb,
                       jsonb_build_object(
                           'Genres', jsonb_build_array(format('Genre %s', value % 60)),
                           'Tags', jsonb_build_array(format('Tag %s', value % 100))
                       ),
                       CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
                FROM generate_series(1, $2) AS generated(value)
                "#,
            )
            .bind(movie_folder.id)
            .bind(movie_count)
            .execute(&test.database.worker_pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO media_item_facets (
                    item_id, facet_kind, normalized_value, display_value,
                    stable_id, position, payload
                )
                SELECT item.id, 'tag', lower(item.metadata->'Tags'->>0),
                       item.metadata->'Tags'->>0,
                       md5('filter-benchmark-tag-' || (item.metadata->'Tags'->>0)),
                       0, item.metadata->'Tags'->0
                FROM media_items AS item
                WHERE item.virtual_folder_id = $1
                "#,
            )
            .bind(movie_folder.id)
            .execute(&test.database.worker_pool)
            .await?;
            sqlx::query("ANALYZE media_items")
                .execute(&test.database.worker_pool)
                .await?;
            sqlx::query("ANALYZE media_item_facets")
                .execute(&test.database.worker_pool)
                .await?;
            let mut projection_tx = test.database.worker_pool.begin().await?;
            ensure_media_item_query_filter_projection(
                &mut projection_tx,
                MediaItemFacetProjectionMode::Force,
            )
            .await?;
            projection_tx.commit().await?;
            sqlx::query("ANALYZE media_item_query_filter_sources")
                .execute(&test.database.worker_pool)
                .await?;
            sqlx::query("ANALYZE media_item_query_filter_values")
                .execute(&test.database.worker_pool)
                .await?;

            let query = MediaItemCatalogQuery {
                limit: 0,
                include_item_types: vec!["Movie".to_string()],
                ..MediaItemCatalogQuery::default()
            };
            for _ in 0..2 {
                let values = test
                    .database
                    .media_item_query_filter_values(&query, selection)
                    .await?;
                anyhow::ensure!(values.genres.len() == 60 && values.tags.len() == 100);
            }
            let temp_bytes_before: i64 = sqlx::query_scalar(
                "SELECT temp_bytes FROM pg_stat_database WHERE datname = current_database()",
            )
            .fetch_one(&test.database.pool)
            .await?;
            let concurrent_started = Instant::now();
            let (first, second, third, fourth) = tokio::try_join!(
                test.database
                    .media_item_query_filter_values(&query, selection),
                test.database
                    .media_item_query_filter_values(&query, selection),
                test.database
                    .media_item_query_filter_values(&query, selection),
                test.database
                    .media_item_query_filter_values(&query, selection),
            )?;
            for values in [first, second, third, fourth] {
                anyhow::ensure!(values.genres.len() == 60 && values.tags.len() == 100);
            }
            let concurrent_micros = concurrent_started.elapsed().as_micros();
            let mut samples = Vec::with_capacity(repetitions);
            for _ in 0..repetitions {
                let started = Instant::now();
                let values = test
                    .database
                    .media_item_query_filter_values(&query, selection)
                    .await?;
                samples.push(started.elapsed().as_micros());
                anyhow::ensure!(values.genres.len() == 60 && values.tags.len() == 100);
            }
            let temp_bytes_after: i64 = sqlx::query_scalar(
                "SELECT temp_bytes FROM pg_stat_database WHERE datname = current_database()",
            )
            .fetch_one(&test.database.pool)
            .await?;
            samples.sort_unstable();
            let p95_micros = percentile_from_sorted_samples(&samples, 95);
            let temp_bytes_delta = temp_bytes_after
                .checked_sub(temp_bytes_before)
                .context("PostgreSQL temp byte counter moved backwards")?;
            let p95_limit = std::env::var("JELLYRIN_FILTER_BENCHMARK_P95_MICROS")
                .ok()
                .and_then(|value| value.parse::<u128>().ok())
                .unwrap_or(1_500_000);
            let temp_limit = std::env::var("JELLYRIN_FILTER_BENCHMARK_TEMP_BYTES")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(64 * 1024 * 1024);
            anyhow::ensure!(
                p95_micros < p95_limit,
                "filter projection p95 threshold exceeded"
            );
            anyhow::ensure!(
                concurrent_micros < p95_limit.saturating_mul(4),
                "concurrent filter projection threshold exceeded"
            );
            anyhow::ensure!(
                temp_bytes_delta <= temp_limit,
                "filter projection temporary byte threshold exceeded"
            );
            println!(
                "{}",
                json!({
                    "benchmark": "postgres_movie_filter_values",
                    "episodes": episode_count,
                    "movies": movie_count,
                    "selection": selection_name,
                    "repetitions": repetitions,
                    "p50_micros": percentile_from_sorted_samples(&samples, 50),
                    "p95_micros": p95_micros,
                    "max_micros": samples[samples.len() - 1],
                    "four_concurrent_micros": concurrent_micros,
                    "temp_bytes_delta": temp_bytes_delta,
                })
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_diagnostics_report_separate_pools_and_safe_sync_summary() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let worker_connection = test.database.worker_pool.acquire().await?;
            let runtime = test.database.runtime_diagnostics();
            assert_eq!(runtime.driver, crate::DatabaseDriver::PostgreSql);
            assert_eq!(runtime.api_pool.max_connections, 2);
            let worker = runtime.worker_pool.unwrap();
            assert_eq!(worker.max_connections, 1);
            assert_eq!(worker.in_use, 1);
            assert_eq!(
                runtime.api_pool.idle + runtime.api_pool.in_use,
                runtime.api_pool.size
            );
            drop(worker_connection);

            let empty = test.database.catalog_sync_diagnostics().await?;
            assert_eq!(empty.total, 0);
            assert_eq!(empty.last_run, None);

            let folder = test
                .database
                .upsert_virtual_folder(
                    "Diagnostics",
                    Some("movies"),
                    vec!["/diagnostics".to_owned()],
                )
                .await?;
            let base = OffsetDateTime::now_utc() - time::Duration::minutes(10);
            for (offset, status, count, completed, error) in [
                (0_i64, "running", 10_i64, None, None),
                (
                    1_i64,
                    "completed",
                    20_i64,
                    Some(time::Duration::milliseconds(1_000)),
                    None,
                ),
                (
                    2_i64,
                    "failed",
                    30_i64,
                    Some(time::Duration::milliseconds(1_500)),
                    Some("https://user:secret@provider.invalid/live?token=secret"),
                ),
            ] {
                let started_at = base + time::Duration::minutes(offset);
                let completed_at = completed.map(|duration| started_at + duration);
                sqlx::query(
                    r#"
                    INSERT INTO catalog_sync_runs (
                        id, virtual_folder_id, generation_id, status, item_count,
                        started_at, completed_at, error_message
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    "#,
                )
                .bind(Uuid::new_v4())
                .bind(folder.id)
                .bind(Uuid::new_v4())
                .bind(status)
                .bind(count)
                .bind(started_at)
                .bind(completed_at)
                .bind(error)
                .execute(&test.database.pool)
                .await?;
            }

            let diagnostics = test.database.catalog_sync_diagnostics().await?;
            assert_eq!(diagnostics.total, 3);
            assert_eq!(diagnostics.running, 1);
            assert_eq!(diagnostics.completed, 1);
            assert_eq!(diagnostics.failed, 1);
            let last = diagnostics.last_run.as_ref().unwrap();
            assert_eq!(last.status, "failed");
            assert_eq!(last.item_count, 30);
            assert_eq!(last.duration_millis, Some(1_500));
            let debug = format!("{diagnostics:?}");
            assert!(!debug.contains("secret"));
            assert!(!debug.contains("provider.invalid"));
            Ok::<_, anyhow::Error>(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_catalog_manages_virtual_folder_paths_case_insensitively() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let folder = test
                .database
                .upsert_virtual_folder(
                    " Movies ",
                    Some(" movies "),
                    vec![
                        " /srv/movies ".to_owned(),
                        "/srv/movies".to_owned(),
                        String::new(),
                    ],
                )
                .await?;
            assert_eq!(folder.name, "Movies");
            assert_eq!(folder.collection_type.as_deref(), Some("movies"));
            assert_eq!(folder.locations, ["/srv/movies"]);

            let same_folder = test
                .database
                .upsert_virtual_folder("movies", Some("movies"), vec!["/srv/library".to_owned()])
                .await?;
            assert_eq!(same_folder.id, folder.id);
            assert_eq!(same_folder.name, "Movies");

            test.database
                .add_virtual_folder_path("MOVIES", "/srv/extra")
                .await?;
            test.database
                .add_virtual_folder_path("movies", "/srv/extra")
                .await?;
            assert_eq!(
                test.database.virtual_folders().await?[0].locations,
                ["/srv/library", "/srv/extra"]
            );

            let first_database = test.database.clone();
            let second_database = test.database.clone();
            tokio::try_join!(
                first_database.add_virtual_folder_path("movies", "/srv/concurrent-a"),
                second_database.add_virtual_folder_path("MOVIES", "/srv/concurrent-b")
            )?;
            let locations = test.database.virtual_folders().await?[0]
                .locations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            assert!(locations.contains("/srv/concurrent-a"));
            assert!(locations.contains("/srv/concurrent-b"));

            assert!(
                test.database
                    .rename_virtual_folder("movies", "Cinema")
                    .await?
            );
            assert!(
                test.database
                    .update_virtual_folder_path("cinema", "/srv/library", "/srv/renamed")
                    .await?
            );

            let item_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Cinema",
                    "movies",
                    "/srv/renamed",
                    vec![remote_item(
                        item_id,
                        "Folder Path Test",
                        "/srv/renamed/sub/movie.mkv",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await?;
            assert!(
                test.database
                    .remove_virtual_folder_path("CINEMA", "/srv/renamed")
                    .await?
            );
            assert!(test.database.media_items().await?.is_empty());
            assert!(test.database.delete_virtual_folder("cinema").await?);
            assert!(test.database.virtual_folders().await?.is_empty());
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_remote_snapshot_supports_atomic_path_swaps() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let first_id = Uuid::new_v4();
            let second_id = Uuid::new_v4();
            let first_path = "provider://swap/first";
            let second_path = "provider://swap/second";
            test.database
                .replace_remote_media_library_snapshot(
                    "Swap Library",
                    "movies",
                    "provider://swap",
                    vec![
                        remote_item(first_id, "First", first_path, "Video", "movies", json!({})),
                        remote_item(
                            second_id,
                            "Second",
                            second_path,
                            "Video",
                            "movies",
                            json!({}),
                        ),
                    ],
                )
                .await?;

            test.database
                .replace_remote_media_library_snapshot(
                    "Swap Library",
                    "movies",
                    "provider://swap",
                    vec![
                        remote_item(first_id, "First", second_path, "Video", "movies", json!({})),
                        remote_item(
                            second_id,
                            "Second",
                            first_path,
                            "Video",
                            "movies",
                            json!({}),
                        ),
                    ],
                )
                .await?;

            assert_eq!(
                test.database.media_item_by_id(first_id).await?.path,
                second_path
            );
            assert_eq!(
                test.database.media_item_by_id(second_id).await?.path,
                first_path
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_identical_remote_snapshot_does_not_rewrite_media_rows() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let item_id = Uuid::new_v4();
            let snapshot = || {
                vec![remote_item(
                    item_id,
                    "Stable Movie",
                    "provider://noop/stable.mp4",
                    "Video",
                    "movies",
                    json!({"Provider": "xtream"}),
                )]
            };
            test.database
                .replace_remote_media_library_snapshot(
                    "No-op Library",
                    "movies",
                    "provider://noop",
                    snapshot(),
                )
                .await?;
            let sentinel = OffsetDateTime::from_unix_timestamp(946_684_800)?;
            sqlx::query("UPDATE media_items SET updated_at = $1, last_seen_at = $1 WHERE id = $2")
                .bind(sentinel)
                .bind(item_id)
                .execute(&test.database.pool)
                .await?;

            test.database
                .replace_remote_media_library_snapshot(
                    "No-op Library",
                    "movies",
                    "provider://noop",
                    snapshot(),
                )
                .await?;
            let timestamps = sqlx::query_as::<_, (OffsetDateTime, Option<OffsetDateTime>)>(
                "SELECT updated_at, last_seen_at FROM media_items WHERE id = $1",
            )
            .bind(item_id)
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(timestamps.0, sentinel);
            assert_eq!(timestamps.1, Some(sentinel));
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM catalog_sync_runs WHERE status = 'completed'",
                )
                .fetch_one(&test.database.pool)
                .await?,
                2
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_remote_library_batch_rolls_back_late_failure_and_accepts_both_empty() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let movie_id = Uuid::new_v4();
            let episode_id = Uuid::new_v4();
            let new_movie_id = Uuid::new_v4();
            let batch = |movie_items, series_items, revision: &str| {
                vec![
                    RemoteMediaLibrarySnapshot {
                        library_name: "Atomic Movies".to_string(),
                        collection_type: "movies".to_string(),
                        source_location: format!("provider://atomic/movies/{revision}"),
                        items: movie_items,
                    },
                    RemoteMediaLibrarySnapshot {
                        library_name: "Atomic Series".to_string(),
                        collection_type: "tvshows".to_string(),
                        source_location: format!("provider://atomic/series/{revision}"),
                        items: series_items,
                    },
                ]
            };
            test.database
                .replace_remote_media_library_snapshots(batch(
                    vec![remote_item(
                        movie_id,
                        "Original Movie",
                        "provider://atomic/movies/original.mp4",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                    vec![remote_item(
                        episode_id,
                        "Original Episode",
                        "provider://atomic/series/original.mp4",
                        "Video",
                        "tvshows",
                        json!({}),
                    )],
                    "v1",
                ))
                .await?;

            let failed = test
                .database
                .replace_remote_media_library_snapshots(batch(
                    vec![remote_item(
                        new_movie_id,
                        "Uncommitted Movie",
                        "provider://atomic/movies/new.mp4",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                    vec![remote_item(
                        movie_id,
                        "Cross-folder Conflict",
                        "provider://atomic/series/conflict.mp4",
                        "Video",
                        "tvshows",
                        json!({}),
                    )],
                    "v2",
                ))
                .await;
            assert!(failed.is_err());
            assert_eq!(
                test.database.media_item_by_id(movie_id).await?.name,
                "Original Movie"
            );
            assert_eq!(
                test.database.media_item_by_id(episode_id).await?.name,
                "Original Episode"
            );
            assert!(test.database.media_item_by_id(new_movie_id).await.is_err());
            let folders = test.database.virtual_folders().await?;
            assert!(folders.iter().any(|folder| {
                folder.name == "Atomic Movies"
                    && folder.locations == ["provider://atomic/movies/v1".to_string()]
            }));
            assert!(folders.iter().any(|folder| {
                folder.name == "Atomic Series"
                    && folder.locations == ["provider://atomic/series/v1".to_string()]
            }));
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM catalog_sync_runs WHERE status = 'completed'",
                )
                .fetch_one(&test.database.pool)
                .await?,
                2
            );

            test.database
                .replace_remote_media_library_snapshots(batch(Vec::new(), Vec::new(), "empty"))
                .await?;
            assert!(test.database.media_items().await?.is_empty());
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM media_items WHERE missing_since IS NOT NULL",
                )
                .fetch_one(&test.database.pool)
                .await?,
                2
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM catalog_sync_runs \
                     WHERE status = 'completed' AND item_count = 0",
                )
                .fetch_one(&test.database.pool)
                .await?,
                2
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_durable_remote_media_stage_publishes_atomically_with_projections() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let specs = || {
                vec![
                    RemoteMediaLibraryStageSpec {
                        key: "movies".to_string(),
                        library_name: "Durable Movies".to_string(),
                        collection_type: "movies".to_string(),
                        source_location: "xtream://durable/movies".to_string(),
                    },
                    RemoteMediaLibraryStageSpec {
                        key: "series".to_string(),
                        library_name: "Durable Series".to_string(),
                        collection_type: "tvshows".to_string(),
                        source_location: "xtream://durable/series".to_string(),
                    },
                ]
            };
            let external_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "External Owner",
                    "movies",
                    "provider://external",
                    vec![remote_item(
                        external_id,
                        "External",
                        "provider://external/item.mp4",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await?;

            let failed_stage = test
                .database
                .begin_remote_media_catalog_stage(specs())
                .await?;
            let uncommitted_id = Uuid::new_v4();
            test.database
                .append_remote_media_catalog_stage(
                    &failed_stage,
                    "movies",
                    vec![remote_item(
                        uncommitted_id,
                        "Uncommitted",
                        "xtream://durable/movies/uncommitted.mp4",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await?;
            test.database
                .append_remote_media_catalog_stage(
                    &failed_stage,
                    "series",
                    vec![remote_item(
                        external_id,
                        "Late Conflict",
                        "xtream://durable/series/conflict.mp4",
                        "Video",
                        "tvshows",
                        json!({}),
                    )],
                )
                .await?;
            assert!(
                test.database
                    .publish_remote_media_catalog_stage(&failed_stage)
                    .await
                    .is_err()
            );
            assert!(
                test.database
                    .media_item_by_id(uncommitted_id)
                    .await
                    .is_err()
            );
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT status FROM remote_media_catalog_stages WHERE id = $1",
                )
                .bind(failed_stage.parsed_id()?)
                .fetch_one(&test.database.worker_pool)
                .await?,
                "open"
            );
            test.database
                .abort_remote_media_catalog_stage(&failed_stage)
                .await?;

            let stage = test
                .database
                .begin_remote_media_catalog_stage(specs())
                .await?;
            let movie_id = Uuid::new_v4();
            let episode_id = Uuid::new_v4();
            test.database
                .append_remote_media_catalog_stage(
                    &stage,
                    "movies",
                    vec![remote_item(
                        movie_id,
                        "Projected Movie",
                        "xtream://durable/movies/projected.mp4",
                        "Video",
                        "movies",
                        json!({
                            "Genres": ["Drama"],
                            "People": [{"Name": "Jane Doe", "Id": "person-id"}],
                            "Studios": ["Studio One"],
                            "Tags": ["Featured"],
                            "PremiereDate": "2030-01-01T00:00:00Z"
                        }),
                    )],
                )
                .await?;
            test.database
                .append_remote_media_catalog_stage(
                    &stage,
                    "series",
                    vec![remote_item(
                        episode_id,
                        "Episode",
                        "xtream://durable/series/show/season-1/episode-1.mp4",
                        "Video",
                        "tvshows",
                        json!({"SeriesName": "Show"}),
                    )],
                )
                .await?;
            assert!(test.database.media_item_by_id(movie_id).await.is_err());
            let folders = test
                .database
                .publish_remote_media_catalog_stage(&stage)
                .await?;
            assert_eq!(folders.len(), 2);
            assert_eq!(
                test.database.media_item_by_id(movie_id).await?.name,
                "Projected Movie"
            );
            assert_eq!(
                test.database.media_item_by_id(episode_id).await?.name,
                "Episode"
            );
            for query in [
                "SELECT COUNT(*) FROM media_item_facets WHERE item_id = $1",
                "SELECT COUNT(*) FROM media_item_facet_aliases WHERE item_id = $1",
                "SELECT COUNT(*) FROM media_item_genre_selectors WHERE item_id = $1",
                "SELECT COUNT(*) FROM media_item_filter_selectors WHERE item_id = $1",
                "SELECT COUNT(*) FROM media_item_upcoming_dates WHERE item_id = $1",
            ] {
                let count = sqlx::query_scalar::<_, i64>(query)
                    .bind(movie_id)
                    .fetch_one(&test.database.pool)
                    .await?;
                assert!(count > 0);
            }
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_catalog_page_keeps_exact_total_and_joins_user_data() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let alpha_id = Uuid::new_v4();
            let beta_id = Uuid::new_v4();
            let mut alpha = remote_item(
                alpha_id,
                "Alpha Feature",
                "provider://paged/alpha.mkv",
                "Video",
                "movies",
                json!({
                    "Overview": "A hidden needle",
                    "Album": "Alpha Album",
                    "Artists": [{"Name": "Artist Two", "Id": "forbidden-id"}],
                    "Genres": [{"Name": "Drama", "Id": "Imported-Drama"}],
                    "seriesgenres": [{"Id": "Id-Only-Genre"}],
                    "pEoPlE": [{"Name": "Jane Doe", "Id": "Imported-Person"}],
                    "SeriesPeople": ["Suppressed Person"],
                    "Studios": [
                        {"Id": "Studio-Id-Only"},
                        {"Name": "HBO", "Id": "Imported-HBO"}
                    ],
                    "Tags": ["Featured"]
                }),
            );
            alpha.width = Some(3840);
            alpha.height = Some(2160);
            alpha.media_streams = vec![
                json!({"Type": "Video"}),
                json!({"Type": "Audio", "Language": "fre"}),
                json!({"Type": "Subtitle", "Language": "spa"}),
            ];
            test.database
                .replace_remote_media_library_snapshot(
                    "Paged Library",
                    "movies",
                    "provider://paged",
                    vec![
                        alpha,
                        remote_item(
                            beta_id,
                            "Beta Feature",
                            "provider://paged/beta.mp4",
                            "Video",
                            "movies",
                            // Metadata search must not accidentally match object keys.
                            json!({
                                "Needle": "absent",
                                "AlbumName": "100%_\\ Mix",
                                "Artists": ["Artist One"],
                                "Genres": ["Comedy"],
                                "People": ["Other Person"],
                                "Studios": ["Other Studio"],
                                "Tags": ["Archive"]
                            }),
                        ),
                    ],
                )
                .await?;

            let user_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            sqlx::query(
                "INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)",
            )
            .bind(user_id)
            .bind(format!("catalog-page-{user_id}"))
            .bind(now)
            .execute(&test.database.pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO playback_states (
                    user_id, item_id, position_ticks, played, is_favorite, updated_at
                ) VALUES ($1, $2, 90, true, false, $3)
                "#,
            )
            .bind(user_id)
            .bind(alpha_id)
            .bind(now)
            .execute(&test.database.pool)
            .await?;

            let page = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    start_index: 1,
                    limit: 1,
                    search_term: Some("feature".to_string()),
                    include_item_types: vec!["Movie".to_string()],
                    user_id: Some(user_id),
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(page.total_record_count, 2);
            assert_eq!(page.start_index, 1);
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].item.id, beta_id);
            assert!(page.items[0].playback_state.is_none());

            let count_only = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 0,
                    search_term: Some("feature".to_string()),
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(count_only.total_record_count, 2);
            assert!(count_only.items.is_empty());

            let metadata_search = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    search_term: Some("needle".to_string()),
                    search_scope: MediaItemCatalogSearchScope::AllMetadataScalars,
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(metadata_search.total_record_count, 1);
            assert_eq!(metadata_search.items[0].item.id, alpha_id);

            let hint_page = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 1,
                    search_term: Some("artist".to_string()),
                    search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(hint_page.total_record_count, 2);
            assert_eq!(hint_page.items.len(), 1);
            assert_eq!(hint_page.items[0].item.id, alpha_id);
            for excluded_term in ["hidden needle", "forbidden-id", "Needle"] {
                let excluded = test
                    .database
                    .media_item_catalog_page(&MediaItemCatalogQuery {
                        limit: 10,
                        search_term: Some(excluded_term.to_string()),
                        search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                        ..MediaItemCatalogQuery::default()
                    })
                    .await?;
                assert_eq!(excluded.total_record_count, 0, "term={excluded_term}");
            }
            let literal_wildcards = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    search_term: Some("%_\\".to_string()),
                    search_scope: MediaItemCatalogSearchScope::SearchHintFields,
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(literal_wildcards.total_record_count, 1);
            assert_eq!(literal_wildcards.items[0].item.id, beta_id);

            for selector in [
                "drama".to_string(),
                jellyrin_core::stable_entity_id("Genre", "Drama"),
                "imported-drama".to_string(),
                "id-only-genre".to_string(),
            ] {
                let genre = test
                    .database
                    .media_item_catalog_page(&MediaItemCatalogQuery {
                        limit: 10,
                        genre_ids: vec![selector],
                        ..MediaItemCatalogQuery::default()
                    })
                    .await?;
                assert_eq!(genre.total_record_count, 1);
                assert_eq!(genre.items[0].item.id, alpha_id);
            }
            let genre_or_page = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    start_index: 1,
                    limit: 1,
                    genre_ids: vec!["DRAMA".to_string(), "comedy".to_string()],
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(genre_or_page.total_record_count, 2);
            assert_eq!(genre_or_page.items[0].item.id, beta_id);

            for (field, selector) in [
                ("person", "jane doe".to_string()),
                (
                    "person",
                    jellyrin_core::stable_entity_id("Person", "Jane Doe"),
                ),
                ("person", "imported-person".to_string()),
                ("studio", "hbo".to_string()),
                ("studio", jellyrin_core::stable_entity_id("Studio", "HBO")),
                ("studio", "imported-hbo".to_string()),
                ("studio", "studio-id-only".to_string()),
                ("tag", "featured".to_string()),
            ] {
                let mut filter = MediaItemCatalogQuery {
                    limit: 10,
                    ..MediaItemCatalogQuery::default()
                };
                match field {
                    "person" => filter.person_ids.push(selector),
                    "studio" => filter.studio_ids.push(selector),
                    "tag" => filter.tags.push(selector),
                    _ => unreachable!(),
                }
                let page = test.database.media_item_catalog_page(&filter).await?;
                assert_eq!(page.total_record_count, 1, "field={field}");
                assert_eq!(page.items[0].item.id, alpha_id, "field={field}");
            }
            let combined = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    person_ids: vec!["Jane Doe".to_string()],
                    studio_ids: vec!["HBO".to_string()],
                    tags: vec!["FEATURED".to_string()],
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(combined.total_record_count, 1);
            assert_eq!(combined.items[0].item.id, alpha_id);
            for filter in [
                MediaItemCatalogQuery {
                    limit: 10,
                    person_ids: vec!["Suppressed Person".to_string()],
                    ..MediaItemCatalogQuery::default()
                },
                MediaItemCatalogQuery {
                    limit: 10,
                    person_ids: vec!["Jane Doe".to_string()],
                    tags: vec!["Archive".to_string()],
                    ..MediaItemCatalogQuery::default()
                },
            ] {
                assert_eq!(
                    test.database
                        .media_item_catalog_page(&filter)
                        .await?
                        .total_record_count,
                    0
                );
            }

            let played = test
                .database
                .media_item_catalog_page(&MediaItemCatalogQuery {
                    limit: 10,
                    user_id: Some(user_id),
                    is_played: Some(true),
                    favorite: Some(MediaItemFavoriteFilter::Favorite(false)),
                    audio_languages: vec!["fra".to_string()],
                    has_subtitles: Some(true),
                    is_4k: Some(true),
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(played.total_record_count, 1);
            assert_eq!(played.items[0].item.id, alpha_id);
            assert!(
                played.items[0]
                    .playback_state
                    .as_ref()
                    .is_some_and(|state| state.played)
            );

            let batched = test
                .database
                .playback_states_for_items(user_id, &[alpha_id, beta_id])
                .await?;
            assert_eq!(batched.len(), 1);
            assert_eq!(batched[0].item_id, alpha_id);

            let telemetry = test.database.telemetry_diagnostics();
            let operation = |name: &str, pool: DatabasePoolRole| {
                telemetry
                    .operations
                    .iter()
                    .find(|operation| operation.name == name && operation.pool == pool)
                    .unwrap_or_else(|| panic!("missing PostgreSQL telemetry operation {name}"))
            };
            let pages = operation("catalog.page", DatabasePoolRole::Api);
            assert_eq!((pages.calls, pages.succeeded, pages.errors), (25, 25, 0));
            assert_eq!(pages.rows.total, 19);
            let publish = operation("catalog_sync.publish", DatabasePoolRole::Worker);
            assert_eq!(
                (publish.calls, publish.succeeded, publish.rows.total),
                (1, 1, 2)
            );
            assert_eq!(
                operation("catalog_sync.stage", DatabasePoolRole::Worker)
                    .rows
                    .total,
                2
            );
            assert_eq!(
                operation("catalog_sync.merge", DatabasePoolRole::Worker)
                    .rows
                    .total,
                2
            );
            let debug = format!("{telemetry:?}");
            assert!(!debug.contains("provider://paged"));
            assert!(!debug.contains(&alpha_id.to_string()));
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_query_filter_noop_snapshot_preserves_projection_rows() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let item_id = Uuid::new_v4();
            let make_item = || {
                remote_item(
                    item_id,
                    "Stable Movie",
                    "provider://stable/movie.mkv",
                    "Video",
                    "movies",
                    json!({"Genres": ["Drama"], "Tags": ["Stable"]}),
                )
            };
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Stable Filters",
                    "movies",
                    "provider://stable",
                    vec![make_item()],
                )
                .await?;
            let before = sqlx::query_as::<_, (String, String, Vec<String>)>(
                r#"
                SELECT source.xmin::text, source.completed_at::text,
                       array_agg(value.xmin::text ORDER BY value.value_kind,
                                 value.source_key, value.source_position)
                FROM media_item_query_filter_sources AS source
                JOIN media_item_query_filter_values AS value
                  ON value.item_id = source.item_id
                 AND value.virtual_folder_id = source.virtual_folder_id
                WHERE source.item_id = $1 AND source.virtual_folder_id = $2
                GROUP BY source.xmin::text, source.completed_at
                "#,
            )
            .bind(item_id)
            .bind(folder.id)
            .fetch_one(&test.database.pool)
            .await?;

            let mut ensure_tx = test.database.worker_pool.begin().await?;
            let ensure_report = ensure_media_item_query_filter_projection(
                &mut ensure_tx,
                MediaItemFacetProjectionMode::EnsureCurrent,
            )
            .await?;
            ensure_tx.commit().await?;
            anyhow::ensure!(!ensure_report.rebuilt);
            anyhow::ensure!(ensure_report.source_item_count == 1);
            anyhow::ensure!(ensure_report.projected_value_count == 2);
            let after_ensure = sqlx::query_as::<_, (String, String, Vec<String>)>(
                r#"
                SELECT source.xmin::text, source.completed_at::text,
                       array_agg(value.xmin::text ORDER BY value.value_kind,
                                 value.source_key, value.source_position)
                FROM media_item_query_filter_sources AS source
                JOIN media_item_query_filter_values AS value
                  ON value.item_id = source.item_id
                 AND value.virtual_folder_id = source.virtual_folder_id
                WHERE source.item_id = $1 AND source.virtual_folder_id = $2
                GROUP BY source.xmin::text, source.completed_at
                "#,
            )
            .bind(item_id)
            .bind(folder.id)
            .fetch_one(&test.database.pool)
            .await?;
            anyhow::ensure!(before == after_ensure, "ensure rewrote query-filter rows");
            let marker_after_reconcile = sqlx::query_as::<_, (i64, i64, String, String)>(
                "SELECT source_item_count, projected_facet_count, completed_at::text, xmin::text \
                 FROM jellyrin_derived_projection_versions WHERE projection_name = $1",
            )
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
            .fetch_one(&test.database.pool)
            .await?;
            anyhow::ensure!(marker_after_reconcile.0 == 1 && marker_after_reconcile.1 == 2);
            let mut second_ensure_tx = test.database.worker_pool.begin().await?;
            let second_report = ensure_media_item_query_filter_projection(
                &mut second_ensure_tx,
                MediaItemFacetProjectionMode::EnsureCurrent,
            )
            .await?;
            second_ensure_tx.commit().await?;
            anyhow::ensure!(!second_report.rebuilt);
            let marker_after_noop = sqlx::query_as::<_, (i64, i64, String, String)>(
                "SELECT source_item_count, projected_facet_count, completed_at::text, xmin::text \
                 FROM jellyrin_derived_projection_versions WHERE projection_name = $1",
            )
            .bind(MEDIA_ITEM_QUERY_FILTER_PROJECTION_NAME)
            .fetch_one(&test.database.pool)
            .await?;
            anyhow::ensure!(marker_after_reconcile == marker_after_noop);

            test.database
                .replace_remote_media_library_snapshot(
                    "Stable Filters",
                    "movies",
                    "provider://stable",
                    vec![make_item()],
                )
                .await?;
            let after = sqlx::query_as::<_, (String, String, Vec<String>)>(
                r#"
                SELECT source.xmin::text, source.completed_at::text,
                       array_agg(value.xmin::text ORDER BY value.value_kind,
                                 value.source_key, value.source_position)
                FROM media_item_query_filter_sources AS source
                JOIN media_item_query_filter_values AS value
                  ON value.item_id = source.item_id
                 AND value.virtual_folder_id = source.virtual_folder_id
                WHERE source.item_id = $1 AND source.virtual_folder_id = $2
                GROUP BY source.xmin::text, source.completed_at
                "#,
            )
            .bind(item_id)
            .bind(folder.id)
            .fetch_one(&test.database.pool)
            .await?;
            anyhow::ensure!(before == after, "no-op snapshot rewrote query-filter rows");
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_query_filter_values_are_unpaged_scoped_and_exact() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let irrelevant = (0..512)
                .map(|index| {
                    remote_item(
                        Uuid::new_v4(),
                        &format!("Irrelevant {index:03}"),
                        &format!("provider://irrelevant/{index:03}.mp4"),
                        "Video",
                        "movies",
                        json!({
                            "Genres": [format!("Wrong Genre {index:03}")],
                            "Tags": [format!("Wrong Tag {index:03}")]
                        }),
                    )
                })
                .collect();
            test.database
                .replace_remote_media_library_snapshot(
                    "Irrelevant Filters",
                    "movies",
                    "provider://irrelevant",
                    irrelevant,
                )
                .await?;

            let target_id = Uuid::new_v4();
            let mut target = remote_item(
                target_id,
                "Target Movie",
                "provider://filters/target.MKV",
                "Video",
                "movies",
                json!({
                    "Album": "Album One",
                    "AlbumName": ["Album Two"],
                    "Artists": [{"Name": "Artist One"}],
                    "AlbumArtists": ["Artist Two"],
                    "Genres": [" Drama ", "drama"],
                    "MusicGenres": ["Must Not Leak"],
                    "OfficialRating": "PG-13",
                    "OfficialRatings": ["R"],
                    "SeriesStatus": "Continuing",
                    "People": [{"Name": "Actor One", "Role": "Lead"}],
                    "SeriesPeople": ["Actor Two"],
                    "Studios": [{"Name": "Studio One"}],
                    "SeriesStudios": ["Must Not Leak"],
                    "Tags": ["Featured", {"Name": 123}],
                    "ProductionYear": 2025,
                    "Years": [2024],
                    "rEmOtEtRaIlErS": [{"Url": "https://example.invalid/trailer"}],
                    "Trailers": [{"Url": "  "}]
                }),
            );
            target.media_streams = vec![
                json!({"Type": "Video"}),
                json!({"Type": "Audio", "Language": "fre"}),
                json!({"Type": "Audio", "Language": "und"}),
                json!({"Type": "Audio", "Language": 123}),
                json!({"Type": 123, "Language": "eng"}),
                json!({"Type": "Subtitle", "Language": "spa"}),
            ];
            let mut selected_items = (0..512)
                .map(|index| {
                    remote_item(
                        Uuid::new_v4(),
                        &format!("Selected Empty {index:03}"),
                        &format!("provider://filters/empty-{index:03}.mkv"),
                        "Video",
                        "movies",
                        json!({}),
                    )
                })
                .collect::<Vec<_>>();
            selected_items.push(remote_item(
                Uuid::new_v4(),
                "Selected Empty Extension",
                "provider://filters/trailing.",
                "Video",
                "movies",
                json!({}),
            ));
            selected_items.push(remote_item(
                Uuid::new_v4(),
                "Selected Hidden With Extension",
                "provider://filters/.foo.bar",
                "Video",
                "movies",
                json!({}),
            ));
            let misleading_trailer_id = Uuid::new_v4();
            selected_items.push(remote_item(
                misleading_trailer_id,
                "Selected Misleading Trailer",
                "provider://filters/misleading.mkv",
                "video",
                "movies",
                json!({
                    "Trailers": [{
                        "Url": "",
                        "url": "https://example.invalid/must-not-fallback"
                    }, {
                        "Url": null,
                        "url": "https://example.invalid/must-not-fallback-null"
                    }, {
                        "Url": 123,
                        "url": "https://example.invalid/must-not-fallback-number"
                    }]
                }),
            ));
            selected_items.push(remote_item(
                Uuid::new_v4(),
                "Selected Wrong Case Metadata",
                "provider://filters/wrong-case.mkv",
                "Video",
                "movies",
                json!({
                    "artists": ["Must Not Leak Case Artist"],
                    "tags": ["Must Not Leak Case Tag"],
                    "productionyear": 2030
                }),
            ));
            // The only metadata values live beyond the catalog page-size boundary.
            target.metadata["Albums"] = json!(["Must Not Leak Album"]);
            target.metadata["SeriesGenres"] = json!(["Must Not Leak Series Genre"]);
            target.metadata["Cast"] = json!(["Must Not Leak Cast"]);
            selected_items.push(target);
            test.database
                .replace_remote_media_library_snapshot(
                    "Target Filters",
                    "movies",
                    "provider://filters",
                    selected_items,
                )
                .await?;
            let target_folder = test
                .database
                .virtual_folders()
                .await?
                .into_iter()
                .find(|folder| folder.name == "Target Filters")
                .context("missing target filter folder")?;

            let filter_query = MediaItemCatalogQuery {
                start_index: 999,
                limit: 0,
                virtual_folder_ids: vec![target_folder.id],
                include_item_types: vec!["Movie".to_string()],
                sort: vec![(
                    MediaItemCatalogSortField::DateCreated,
                    SortDirection::Descending,
                )],
                ..MediaItemCatalogQuery::default()
            };
            let values = test
                .database
                .media_item_query_filter_values(&filter_query, MediaItemQueryFilterSelection::ALL)
                .await?;
            assert_eq!(values.albums, ["Album One", "Album Two"]);
            assert_eq!(values.artists, ["Artist One", "Artist Two"]);
            assert_eq!(values.audio_languages, ["fra"]);
            assert_eq!(values.containers, ["", "bar", "mkv"]);
            assert_eq!(values.genres, ["Drama"]);
            assert_eq!(values.media_types, ["Video"]);
            assert_eq!(values.official_ratings, ["PG-13", "R"]);
            assert_eq!(values.series_statuses, ["Continuing"]);
            assert_eq!(values.staff_names, ["Actor One", "Actor Two"]);
            assert_eq!(values.studios, ["Studio One"]);
            assert_eq!(values.subtitle_languages, ["spa"]);
            assert_eq!(values.tags, ["Featured"]);
            assert_eq!(values.video_types, ["VideoFile"]);
            assert_eq!(values.years, ["2024", "2025"]);
            assert!(values.has_subtitles);
            assert!(values.has_trailer);
            assert!(!format!("{values:?}").contains("Must Not Leak"));
            assert!(!format!("{values:?}").contains("Wrong Genre"));

            let filters2_values = test
                .database
                .media_item_query_filter_values(
                    &filter_query,
                    MediaItemQueryFilterSelection::FILTERS2,
                )
                .await?;
            assert_eq!(filters2_values.genres, values.genres);
            assert_eq!(filters2_values.tags, values.tags);
            assert_eq!(filters2_values.audio_languages, values.audio_languages);
            assert_eq!(
                filters2_values.subtitle_languages,
                values.subtitle_languages
            );
            assert!(filters2_values.official_ratings.is_empty());
            assert!(filters2_values.containers.is_empty());
            assert!(!filters2_values.has_subtitles);
            assert!(!filters2_values.has_trailer);

            sqlx::query("DELETE FROM media_item_query_filter_sources WHERE item_id = $1")
                .bind(target_id)
                .execute(&test.database.pool)
                .await?;
            let legacy_fallback = test
                .database
                .media_item_query_filter_values(&filter_query, MediaItemQueryFilterSelection::ALL)
                .await?;
            assert_eq!(legacy_fallback, values);
            let legacy_filters2 = test
                .database
                .media_item_query_filter_values(
                    &filter_query,
                    MediaItemQueryFilterSelection::FILTERS2,
                )
                .await?;
            assert_eq!(legacy_filters2, filters2_values);

            let projected_value_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM media_item_facets \
                 WHERE item_id = $1 \
                   AND facet_kind = ANY(ARRAY[\
                       'music_artist', 'music_album_artist', 'tag', 'year'\
                   ]::text[])",
            )
            .bind(target_id)
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(projected_value_count, 4);

            let misleading = test
                .database
                .media_item_query_filter_values(
                    &MediaItemCatalogQuery {
                        ids: vec![misleading_trailer_id],
                        virtual_folder_ids: vec![target_folder.id],
                        ..MediaItemCatalogQuery::default()
                    },
                    MediaItemQueryFilterSelection::ALL,
                )
                .await?;
            assert!(!misleading.has_trailer);
            let case_sensitive_media_types = test
                .database
                .media_item_query_filter_values(
                    &MediaItemCatalogQuery {
                        ids: vec![target_id, misleading_trailer_id],
                        virtual_folder_ids: vec![target_folder.id],
                        ..MediaItemCatalogQuery::default()
                    },
                    MediaItemQueryFilterSelection::ALL,
                )
                .await?;
            assert_eq!(case_sensitive_media_types.media_types, ["Video", "video"]);
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_sargable_item_types_match_the_legacy_case_expression() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let item = |name: &str, media_type: &str, collection_type: &str, path: &str| {
                remote_item(
                    Uuid::new_v4(),
                    name,
                    path,
                    media_type,
                    collection_type,
                    json!({}),
                )
            };
            let null_collection_id = Uuid::new_v4();
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Effective Type Matrix",
                    "mixed",
                    "provider://effective-types",
                    vec![
                        item(
                            "Movie",
                            "Video",
                            "movies",
                            "provider://effective-types/movie.mkv",
                        ),
                        item(
                            "Music Video",
                            "Video",
                            "musicvideos",
                            "provider://effective-types/music-video.mkv",
                        ),
                        item(
                            "Episode",
                            "Video",
                            "tvshows",
                            "provider://effective-types/Show/Season 01/Episode.mkv",
                        ),
                        item(
                            "Extra",
                            "Video",
                            "tvshows",
                            "provider://effective-types/Show/Extras/Extra.mkv",
                        ),
                        item(
                            "Generic Video",
                            "Video",
                            "homevideos",
                            "provider://effective-types/video.mkv",
                        ),
                        item(
                            "Case Sensitive Collection",
                            "Video",
                            "Movies",
                            "provider://effective-types/case-video.mkv",
                        ),
                        remote_item(
                            null_collection_id,
                            "Null Collection",
                            "provider://effective-types/null-video.mkv",
                            "Video",
                            "temporary",
                            json!({}),
                        ),
                        item(
                            "Audio",
                            "Audio",
                            "music",
                            "provider://effective-types/audio.flac",
                        ),
                        item(
                            "Photo",
                            "Photo",
                            "photos",
                            "provider://effective-types/photo.jpg",
                        ),
                        item(
                            "Book",
                            "Book",
                            "books",
                            "provider://effective-types/book.epub",
                        ),
                        item("Base", "Folder", "mixed", "provider://effective-types/base"),
                        item(
                            "Case Sensitive Media Type",
                            "video",
                            "movies",
                            "provider://effective-types/case-base.mkv",
                        ),
                    ],
                )
                .await?;
            sqlx::query("UPDATE media_items SET collection_type = NULL WHERE id = $1")
                .bind(null_collection_id)
                .execute(&test.database.pool)
                .await?;

            for item_type in [
                "movie",
                "musicvideo",
                "episode",
                "video",
                "audio",
                "photo",
                "book",
                "baseitem",
                "unknown",
            ] {
                let legacy_sql = sqlx::AssertSqlSafe(format!(
                    "SELECT item.id FROM media_items AS item \
                     WHERE item.missing_since IS NULL \
                       AND item.virtual_folder_id = $1 \
                       AND ({POSTGRES_MEDIA_ITEM_TYPE_SQL}) = $2 \
                     ORDER BY item.id"
                ));
                let legacy = sqlx::query_scalar::<_, Uuid>(legacy_sql)
                    .bind(folder.id)
                    .bind(item_type)
                    .fetch_all(&test.database.pool)
                    .await?;
                let mut pushed_down = test
                    .database
                    .media_item_catalog_page(&MediaItemCatalogQuery {
                        limit: 100,
                        virtual_folder_ids: vec![folder.id],
                        include_item_types: vec![item_type.to_string()],
                        ..MediaItemCatalogQuery::default()
                    })
                    .await?
                    .items
                    .into_iter()
                    .map(|entry| entry.item.id)
                    .collect::<Vec<_>>();
                pushed_down.sort_unstable();
                assert_eq!(pushed_down, legacy, "item type {item_type}");
            }
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_next_up_candidates_filter_played_and_unrelated_items_in_sql() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let user = test.database.create_user("next-up-postgres", None).await?;
            let played_id = Uuid::new_v4();
            let unplayed_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Next Up Shows",
                    "tvshows",
                    "provider://next-up",
                    vec![
                        remote_item(
                            played_id,
                            "SQL Show S01E01",
                            "provider://next-up/SQL Show/Season 01/SQL Show S01E01.mp4",
                            "Video",
                            "tvshows",
                            json!({"SeriesName": "SQL Show"}),
                        ),
                        remote_item(
                            unplayed_id,
                            "SQL Show S01E02",
                            "provider://next-up/SQL Show/Season 01/SQL Show S01E02.mp4",
                            "Video",
                            "tvshows",
                            json!({"SeriesName": "SQL Show"}),
                        ),
                    ],
                )
                .await?;
            test.database
                .replace_remote_media_library_snapshot(
                    "Unrelated Movies",
                    "movies",
                    "provider://movies",
                    vec![remote_item(
                        Uuid::new_v4(),
                        "Must Not Leak",
                        "provider://movies/leak.mp4",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await?;
            test.database
                .upsert_playback_state(crate::UpsertPlaybackState {
                    user_id: user.id,
                    item_id: played_id,
                    media_source_id: None,
                    audio_stream_index: None,
                    subtitle_stream_index: None,
                    position_ticks: 0,
                    is_paused: false,
                    played: true,
                })
                .await?;

            let candidates = test.database.tv_next_up_candidates(user.id).await?;
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].item.id, unplayed_id);
            assert_eq!(candidates[0].metadata["SeriesName"], "SQL Show");
            assert!(candidates[0].playback_state.is_none());
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_upcoming_candidates_scope_tv_videos_and_include_metadata() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let now = OffsetDateTime::UNIX_EPOCH;
            let dated_id = Uuid::new_v4();
            let undated_id = Uuid::new_v4();
            let audio_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Upcoming Shows",
                    "tvshows",
                    "provider://upcoming",
                    vec![
                        remote_item(
                            dated_id,
                            "Example Show S01E01",
                            "provider://upcoming/Example Show/Season 01/Example Show S01E01.mp4",
                            "Video",
                            "tvshows",
                            json!({"AirDate": "2035-02-03T04:05:06Z"}),
                        ),
                        remote_item(
                            undated_id,
                            "Example Show S01E02",
                            "provider://upcoming/Example Show/Season 01/Example Show S01E02.mp4",
                            "Video",
                            "tvshows",
                            json!({"SeriesName": "Example Show"}),
                        ),
                        remote_item(
                            audio_id,
                            "Example Show Theme",
                            "provider://upcoming/Example Show Theme.flac",
                            "Audio",
                            "tvshows",
                            json!({"PremiereDate": "2035-02-03T04:05:06Z"}),
                        ),
                    ],
                )
                .await?;
            test.database
                .replace_remote_media_library_snapshot(
                    "Unrelated Movies",
                    "movies",
                    "provider://movies",
                    vec![remote_item(
                        Uuid::new_v4(),
                        "Future Movie",
                        "provider://movies/future.mp4",
                        "Video",
                        "movies",
                        json!({"PremiereDate": "2035-02-03T04:05:06Z"}),
                    )],
                )
                .await?;

            let candidates = test.database.tv_upcoming_candidates(now).await?;
            let mut candidate_ids = candidates
                .iter()
                .map(|candidate| candidate.item.id)
                .collect::<Vec<_>>();
            candidate_ids.sort_unstable();
            let mut expected_ids = vec![dated_id];
            expected_ids.sort_unstable();
            assert_eq!(candidate_ids, expected_ids);
            let dated = candidates
                .iter()
                .find(|candidate| candidate.item.id == dated_id)
                .context("missing dated Upcoming candidate")?;
            assert_eq!(dated.metadata["AirDate"], "2035-02-03T04:05:06Z");
            assert!(dated.playback_state.is_none());
            let telemetry = test.database.telemetry_diagnostics();
            let operation = telemetry
                .operations
                .iter()
                .find(|operation| operation.name == "catalog.upcoming_candidates")
                .context("missing Upcoming telemetry")?;
            assert_eq!(
                (operation.calls, operation.succeeded, operation.rows.total),
                (1, 1, 1)
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_tv_series_lookup_candidates_exclude_unrelated_catalog_and_include_metadata() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let empty_page = test
                .database
                .tv_series_catalog_page(None, 0, 20)
                .await?
                .unwrap();
            assert_eq!(empty_page.total_record_count, 0);
            assert!(empty_page.episodes.is_empty());
            let movies = (0..512)
                .map(|index| {
                    remote_item(
                        Uuid::new_v4(),
                        &format!("Movie {index:04}"),
                        &format!("provider://movies/{index}.mp4"),
                        "Video",
                        "movies",
                        json!({"SeriesId": Uuid::new_v4()}),
                    )
                })
                .collect();
            test.database
                .replace_remote_media_library_snapshot(
                    "Many Movies",
                    "movies",
                    "provider://movies",
                    movies,
                )
                .await?;
            let episode_id = Uuid::new_v4();
            let canonical_series_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Shows",
                    "tvshows",
                    "provider://shows",
                    vec![remote_item(
                        episode_id,
                        "Example Show S01E01",
                        "provider://shows/Example Show/Season 01/Example Show S01E01.mp4",
                        "Video",
                        "tvshows",
                        json!({
                            "SeriesId": canonical_series_id.simple().to_string(),
                            "SeriesName": "Example Show"
                        }),
                    )],
                )
                .await?;

            let candidates = test.database.tv_series_lookup_candidates().await?;
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].item.id, episode_id);
            assert_eq!(
                candidates[0].metadata["SeriesId"],
                canonical_series_id.simple().to_string()
            );
            assert_eq!(candidates[0].metadata["SeriesName"], "Example Show");
            assert!(candidates[0].playback_state.is_none());
            let page = test
                .database
                .tv_series_catalog_page(None, 0, 1)
                .await?
                .unwrap();
            assert_eq!(page.total_record_count, 1);
            assert_eq!(page.series.len(), 1);
            assert_eq!(page.series[0].id, canonical_series_id.simple().to_string());
            assert_eq!(page.series[0].name, "Example Show");
            assert_eq!(page.episodes.len(), 1);
            assert_eq!(page.episodes[0].item.id, episode_id);
            let empty = test
                .database
                .tv_series_catalog_page(None, 1, 1)
                .await?
                .unwrap();
            assert_eq!(empty.total_record_count, 1);
            assert!(empty.series.is_empty());
            assert!(empty.episodes.is_empty());
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_tv_series_projection_is_invalidated_and_rebuilt_atomically() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let series_id = Uuid::new_v4();
            let episode_id = Uuid::new_v4();
            let item = remote_item(
                episode_id,
                "Projected S01E01",
                "provider://projected/Projected S01E01.mp4",
                "Video",
                "tvshows",
                json!({
                    "SeriesId": series_id.simple().to_string(),
                    "SeriesName": "Projected"
                }),
            );
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Projected Shows",
                    "tvshows",
                    "provider://projected",
                    vec![item.clone()],
                )
                .await?;
            let counts = sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT coverage.episode_count, coverage.series_count, count(member.item_id) \
                 FROM media_item_tv_series_coverage AS coverage \
                 LEFT JOIN media_item_tv_series_members AS member \
                   ON member.virtual_folder_id = coverage.virtual_folder_id \
                 WHERE coverage.virtual_folder_id = $1 \
                 GROUP BY coverage.episode_count, coverage.series_count",
            )
            .bind(folder.id)
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(counts, (1, 1, 1));

            sqlx::query("UPDATE media_items SET name = name || ' changed' WHERE id = $1")
                .bind(episode_id)
                .execute(&test.database.pool)
                .await?;
            assert!(
                test.database
                    .tv_series_catalog_page(Some(folder.id), 0, 20)
                    .await?
                    .is_none()
            );

            test.database
                .replace_remote_media_library_snapshot(
                    "Projected Shows",
                    "tvshows",
                    "provider://projected",
                    vec![item],
                )
                .await?;
            let restored = test
                .database
                .tv_series_catalog_page(Some(folder.id), 0, 20)
                .await?
                .context("projection was not restored by snapshot publication")?;
            assert_eq!(restored.total_record_count, 1);
            assert_eq!(restored.episodes.len(), 1);
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_effective_type_candidates_are_exact_and_include_visible_metadata() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let movie_id = Uuid::new_v4();
            let audio_id = Uuid::new_v4();
            let extra_id = Uuid::new_v4();
            let hidden_extra_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Typed Candidates",
                    "mixed",
                    "provider://typed",
                    vec![
                        remote_item(
                            Uuid::new_v4(),
                            "Episode",
                            "provider://typed/show/season/episode.mkv",
                            "Video",
                            "tvshows",
                            json!({"Marker": "excluded"}),
                        ),
                        remote_item(
                            movie_id,
                            "alpha",
                            "provider://typed/alpha.mp4",
                            "Video",
                            "movies",
                            json!({"Marker": "movie"}),
                        ),
                        remote_item(
                            audio_id,
                            "Beta",
                            "provider://typed/beta.flac",
                            "Audio",
                            "music",
                            json!({"Marker": "audio"}),
                        ),
                        remote_item(
                            extra_id,
                            "Final Extra",
                            "provider://typed/show/Season 01/ Extras /clip.mkv",
                            "Video",
                            "tvshows",
                            json!({"Marker": "extra"}),
                        ),
                        remote_item(
                            hidden_extra_id,
                            "Hidden Extra",
                            "provider://typed/show/Featurettes/hidden.mkv",
                            "Video",
                            "tvshows",
                            json!({"Marker": "hidden"}),
                        ),
                    ],
                )
                .await?;
            sqlx::query("UPDATE media_items SET missing_since = $1 WHERE id = $2")
                .bind(OffsetDateTime::now_utc())
                .bind(hidden_extra_id)
                .execute(&test.database.pool)
                .await?;

            let candidates = test
                .database
                .media_items_with_metadata_by_effective_types(&[
                    "aUdIo".to_string(),
                    "MOVIE".to_string(),
                    "Video".to_string(),
                ])
                .await?;
            let by_id = candidates
                .iter()
                .map(|entry| (entry.item.id, &entry.metadata))
                .collect::<HashMap<_, _>>();
            assert_eq!(
                by_id.keys().copied().collect::<HashSet<_>>(),
                HashSet::from([movie_id, audio_id, extra_id])
            );
            assert_eq!(by_id[&movie_id]["Marker"], "movie");
            assert_eq!(by_id[&audio_id]["Marker"], "audio");
            assert_eq!(by_id[&extra_id]["Marker"], "extra");
            assert!(
                candidates
                    .iter()
                    .all(|entry| entry.playback_state.is_none())
            );
            assert!(
                test.database
                    .media_items_with_metadata_by_effective_types(&[])
                    .await?
                    .is_empty()
            );
            let telemetry = test.database.telemetry_diagnostics();
            let operation = telemetry
                .operations
                .iter()
                .find(|operation| {
                    operation.name == "catalog.effective_type_candidates"
                        && operation.pool == DatabasePoolRole::Api
                })
                .expect("effective-type candidate telemetry");
            assert_eq!((operation.calls, operation.succeeded), (2, 2));
            assert_eq!(operation.rows.total, 3);
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_visible_item_point_contract_excludes_missing_rows() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let visible_id = Uuid::new_v4();
            let missing_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Point Lookups",
                    "movies",
                    "provider://point",
                    vec![
                        remote_item(
                            visible_id,
                            "Visible",
                            "provider://point/visible.mp4",
                            "Video",
                            "movies",
                            json!({}),
                        ),
                        remote_item(
                            missing_id,
                            "Missing",
                            "provider://point/missing.mp4",
                            "Video",
                            "movies",
                            json!({}),
                        ),
                    ],
                )
                .await?;
            sqlx::query("UPDATE media_items SET missing_since = CURRENT_TIMESTAMP WHERE id = $1")
                .bind(missing_id)
                .execute(&test.database.pool)
                .await?;

            assert!(test.database.media_item_exists(visible_id).await?);
            assert_eq!(
                test.database
                    .media_item_by_id_visible(visible_id)
                    .await?
                    .expect("visible item")
                    .name,
                "Visible"
            );
            assert!(!test.database.media_item_exists(missing_id).await?);
            assert!(
                test.database
                    .media_item_by_id_visible(missing_id)
                    .await?
                    .is_none()
            );
            let absent_id = Uuid::new_v4();
            assert!(!test.database.media_item_exists(absent_id).await?);
            assert!(
                test.database
                    .media_item_by_id_visible(absent_id)
                    .await?
                    .is_none()
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_catalog_searches_and_aggregates_native_jsonb_metadata() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "Search Library",
                    "movies",
                    "provider://search",
                    vec![
                        remote_item(
                            Uuid::new_v4(),
                            "Álpha Feature",
                            "provider://search/alpha.mkv",
                            "Video",
                            "movies",
                            json!({
                                "Genres": ["Drama", "Action"],
                                "Tags": ["Featured"]
                            }),
                        ),
                        remote_item(
                            Uuid::new_v4(),
                            "Beta Feature",
                            "provider://search/beta.mp4",
                            "Video",
                            "movies",
                            json!({
                                "Genres": ["Drama"],
                                "Tags": ["Archive"]
                            }),
                        ),
                    ],
                )
                .await?;

            let search = test
                .database
                .media_items_by_name_search("feature", &["MOVIES"], 1)
                .await?;
            assert_eq!(search.len(), 1);
            assert!(search[0].name.ends_with("Feature"));
            assert_eq!(
                test.database
                    .media_items_for_virtual_folders(&[folder.id])
                    .await?
                    .len(),
                2
            );
            assert_eq!(
                test.database
                    .media_item_counts_by_virtual_folder()
                    .await?
                    .get(&folder.id),
                Some(&2)
            );

            let summary = test
                .database
                .media_item_filter_summary_for_virtual_folders(&[folder.id])
                .await?;
            assert_eq!(summary.genres, ["Action", "Drama"]);
            assert_eq!(summary.tags, ["Archive", "Featured"]);
            assert_eq!(summary.containers, ["mkv", "mp4"]);
            assert_eq!(summary.media_types, ["Video"]);
            assert_eq!(
                test.database
                    .latest_media_items_for_virtual_folders(&[folder.id], 10)
                    .await?
                    .len(),
                2
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_remote_snapshot_is_atomic_and_preserves_unchanged_item_state() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let stable_item_id = Uuid::new_v4();
            let removed_item_id = Uuid::new_v4();
            test
                .database
                .replace_remote_media_library_snapshot(
                    "Target Library",
                    "movies",
                    "provider://target/v1",
                    vec![
                        remote_item(
                            stable_item_id,
                            "Stable Item",
                            "provider://target/stable.mkv",
                            "Video",
                            "movies",
                            json!({"Version": 1}),
                        ),
                        remote_item(
                            removed_item_id,
                            "Temporarily Removed Item",
                            "provider://target/removed.mkv",
                            "Video",
                            "movies",
                            json!({}),
                        ),
                    ],
                )
                .await?;

            let user_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            sqlx::query(
                "INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)",
            )
            .bind(user_id)
            .bind(format!("catalog-state-{user_id}"))
            .bind(now)
            .execute(&test.database.pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO playback_states (
                    user_id, item_id, position_ticks, played, updated_at
                ) VALUES ($1, $2, $3, false, $4)
                "#,
            )
            .bind(user_id)
            .bind(removed_item_id)
            .bind(17_000_i64)
            .bind(now)
            .execute(&test.database.pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO playback_states (
                    user_id, item_id, position_ticks, played, updated_at
                ) VALUES ($1, $2, $3, false, $4)
                "#,
            )
            .bind(user_id)
            .bind(stable_item_id)
            .bind(42_000_i64)
            .bind(now)
            .execute(&test.database.pool)
            .await?;
            sqlx::query(
                r#"
                INSERT INTO transcode_sessions (
                    play_session_id, user_id, item_id, output_path, status,
                    progress_percent, position_ticks, start_position_ticks,
                    created_at, updated_at
                ) VALUES ($1, $2, $3, $4, 'running', $5, $6, 0, $7, $7)
                "#,
            )
            .bind("catalog-snapshot-progress")
            .bind(user_id)
            .bind(stable_item_id)
            .bind("/tmp/catalog-snapshot-progress/master.m3u8")
            .bind(61.5_f64)
            .bind(31_000_i64)
            .bind(now)
            .execute(&test.database.pool)
            .await?;

            let added_item_id = Uuid::new_v4();
            test
                .database
                .replace_remote_media_library_snapshot(
                    "Target Library",
                    "movies",
                    "provider://target/v2",
                    vec![
                        remote_item(
                            stable_item_id,
                            "Stable Item Updated",
                            "provider://target/stable.mkv",
                            "Video",
                            "movies",
                            json!({"Version": 2}),
                        ),
                        remote_item(
                            added_item_id,
                            "Added Item",
                            "provider://target/added.mkv",
                            "Video",
                            "movies",
                            json!({}),
                        ),
                    ],
                )
                .await?;

            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT position_ticks FROM playback_states WHERE user_id = $1 AND item_id = $2",
                )
                .bind(user_id)
                .bind(stable_item_id)
                .fetch_one(&test.database.pool)
                .await?,
                42_000
            );
            assert_eq!(
                sqlx::query_scalar::<_, f64>(
                    "SELECT progress_percent FROM transcode_sessions WHERE play_session_id = $1",
                )
                .bind("catalog-snapshot-progress")
                .fetch_one(&test.database.pool)
                .await?,
                61.5
            );
            assert_eq!(
                test.database.media_item_by_id(stable_item_id).await?.name,
                "Stable Item Updated"
            );
            assert!(test.database.media_item_by_id(removed_item_id).await.is_err());
            assert!(
                sqlx::query_scalar::<_, Option<OffsetDateTime>>(
                    "SELECT missing_since FROM media_items WHERE id = $1",
                )
                .bind(removed_item_id)
                .fetch_one(&test.database.pool)
                .await?
                .is_some()
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT position_ticks FROM playback_states WHERE user_id = $1 AND item_id = $2",
                )
                .bind(user_id)
                .bind(removed_item_id)
                .fetch_one(&test.database.pool)
                .await?,
                17_000
            );

            test
                .database
                .replace_remote_media_library_snapshot(
                    "Other Library",
                    "movies",
                    "provider://other",
                    vec![remote_item(
                        Uuid::new_v4(),
                        "Externally Owned",
                        "provider://other/shared.mkv",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await?;

            let failed = test
                .database
                .replace_remote_media_library_snapshot(
                    "Target Library",
                    "movies",
                    "provider://target/should-rollback",
                    vec![remote_item(
                        Uuid::new_v4(),
                        "Conflict",
                        "provider://other/shared.mkv",
                        "Video",
                        "movies",
                        json!({}),
                    )],
                )
                .await;
            assert!(failed.is_err());

            let target_folder = test
                .database
                .virtual_folders()
                .await?
                .into_iter()
                .find(|folder| folder.name == "Target Library")
                .context("target folder disappeared after rolled-back snapshot")?;
            assert_eq!(target_folder.locations, ["provider://target/v2"]);
            assert_eq!(
                test
                    .database
                    .media_items_for_virtual_folders(&[target_folder.id])
                    .await?
                    .len(),
                2
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT position_ticks FROM playback_states WHERE user_id = $1 AND item_id = $2",
                )
                .bind(user_id)
                .bind(stable_item_id)
                .fetch_one(&test.database.pool)
                .await?,
                42_000
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_catalog_counts_preserve_exact_metadata_series_and_playback_semantics() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let movie_id = Uuid::new_v4();
            test.database
                .replace_remote_media_library_snapshot(
                    "Count Catalog",
                    "mixed",
                    "provider://counts",
                    vec![
                        remote_item(movie_id, "Count Movie", "provider://counts/movie.mkv", "Video", "movies", json!({
                            "Album": [[" Album "], 7, 7.0, {"Name": "Nested"}, "\u{a0}Écho\u{a0}", "écho"],
                            "AlbumName": "album",
                            "Artists": ["ARTIST", "artist", {"Name": "Other"}, [9]],
                            "RemoteTrailers": [" https://one ", [{"Url": "https://two"}, {"path": "https://three"}, {"Url": null, "url": "https://ignored"}, ""]],
                            "Trailers": {"Path": "https://four"}
                        })),
                        remote_item(Uuid::new_v4(), "Song", "provider://counts/song.flac", "Audio", "music", json!({})),
                        remote_item(Uuid::new_v4(), "Show S01E01", "provider://counts/Show/Season 01/Show S01E01.mkv", "Video", "tvshows", json!({})),
                        remote_item(Uuid::new_v4(), "Show S01E02", "provider://counts/Show/Season 01/Show S01E02.mkv", "Video", "tvshows", json!({})),
                        remote_item(Uuid::new_v4(), "Clip", "provider://counts/clip.mkv", "Video", "musicvideos", json!({})),
                        remote_item(Uuid::new_v4(), "Book", "provider://counts/book.epub", "Book", "books", json!({})),
                    ],
                )
                .await?;

            let counts = test
                .database
                .media_item_catalog_counts(&MediaItemCatalogQuery::default())
                .await?;
            assert_eq!(counts.item_count, 6);
            assert_eq!(counts.movie_count, 1);
            assert_eq!(counts.episode_count, 2);
            assert_eq!(counts.series_count, 1);
            assert_eq!(counts.song_count, 1);
            assert_eq!(counts.music_video_count, 1);
            assert_eq!(counts.book_count, 1);
            assert_eq!(counts.album_count, 6);
            assert_eq!(counts.artist_count, 3);
            assert_eq!(counts.trailer_count, 4);

            let user_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)")
                .bind(user_id)
                .bind(format!("count-user-{user_id}"))
                .bind(now)
                .execute(&test.database.pool)
                .await?;
            sqlx::query("INSERT INTO playback_states (user_id, item_id, position_ticks, played, is_favorite, updated_at) VALUES ($1, $2, 10, true, false, $3)")
                .bind(user_id)
                .bind(movie_id)
                .bind(now)
                .execute(&test.database.pool)
                .await?;
            let played = test
                .database
                .media_item_catalog_counts(&MediaItemCatalogQuery {
                    user_id: Some(user_id),
                    is_played: Some(true),
                    ..MediaItemCatalogQuery::default()
                })
                .await?;
            assert_eq!(played.item_count, 1);
            assert_eq!(played.movie_count, 1);
            assert_eq!(played.album_count, 6);
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn postgres_media_item_facets_match_sqlite_and_noop_snapshot_does_not_rewrite() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let result = async {
            let marker_before =
                sqlx::query_as::<_, (i32, OffsetDateTime, i64, i64, i64, String)>(
                "SELECT extractor_version, completed_at, source_item_count, \
                 projected_facet_count, projected_alias_count, xmin::text \
                 FROM jellyrin_derived_projection_versions \
                 WHERE projection_name = 'media_item_facets'",
            )
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(
                marker_before,
                (
                    MEDIA_ITEM_FACET_PROJECTION_VERSION,
                    marker_before.1,
                    0,
                    0,
                    0,
                    marker_before.5.clone()
                )
            );
            test.database.migrate().await?;
            let marker_after =
                sqlx::query_as::<_, (i32, OffsetDateTime, i64, i64, i64, String)>(
                "SELECT extractor_version, completed_at, source_item_count, \
                 projected_facet_count, projected_alias_count, xmin::text \
                 FROM jellyrin_derived_projection_versions \
                 WHERE projection_name = 'media_item_facets'",
            )
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(marker_after, marker_before, "current projection must be O(1) no-op");
            sqlx::query(
                "DELETE FROM jellyrin_derived_projection_versions \
                 WHERE projection_name = 'media_item_facets'",
            )
            .execute(&test.database.worker_pool)
            .await?;
            let health_error = test.database.schema_health().await.unwrap_err().to_string();
            assert!(health_error.contains("facet projection is not current"));
            test.database.migrate().await?;
            assert_eq!(
                sqlx::query_scalar::<_, i32>(
                    "SELECT extractor_version FROM jellyrin_derived_projection_versions \
                     WHERE projection_name = 'media_item_facets'",
                )
                .fetch_one(&test.database.pool)
                .await?,
                MEDIA_ITEM_FACET_PROJECTION_VERSION
            );
            sqlx::query(
                "UPDATE jellyrin_derived_projection_versions SET extractor_version = $1 \
                 WHERE projection_name = 'media_item_facets'",
            )
            .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION + 1)
            .execute(&test.database.worker_pool)
            .await?;
            let newer_health_error = test.database.schema_health().await.unwrap_err().to_string();
            assert!(newer_health_error.contains("facet projection is not current"));
            let newer_migration_error = format!("{:#}", test.database.migrate().await.unwrap_err());
            assert!(newer_migration_error.contains("newer than supported"));
            sqlx::query(
                "UPDATE jellyrin_derived_projection_versions SET extractor_version = $1 \
                 WHERE projection_name = 'media_item_facets'",
            )
            .bind(MEDIA_ITEM_FACET_PROJECTION_VERSION)
            .execute(&test.database.worker_pool)
            .await?;

            let item_ids = (0..501)
                .map(|index| Uuid::from_u128(index + 10_000))
                .collect::<Vec<_>>();
            let items = item_ids
                .iter()
                .enumerate()
                .map(|(index, item_id)| {
                    remote_item(
                        *item_id,
                        &format!("Facet Item {index:03}"),
                        &format!("provider://pg-facets/{index:03}.mp3"),
                        "Audio",
                        "music",
                        if index == 500 {
                            json!({
                                "Genres": [" Drama ", "drama"],
                                "Artists": ["Track Artist"],
                                "AlbumArtists": ["Album Artist"],
                                "People": [{ "Name": "Jane Doe", "Id": "IMPORTED-PERSON" }],
                                "Tags": [format!("Tag {index:03}")],
                                "PremiereDate": "2035-02-03T04:05:06.123456789Z"
                            })
                        } else {
                            json!({ "Tags": [format!("Tag {index:03}")] })
                        },
                    )
                })
                .collect::<Vec<_>>();
            let folder = test
                .database
                .replace_remote_media_library_snapshot(
                    "PG Facet Music",
                    "music",
                    "provider://pg-facets",
                    items.clone(),
                )
                .await?;
            assert_eq!(
                test.database
                    .media_item_facet_values(MediaItemFacetKind::Tag, &[folder.id])
                    .await?
                    .len(),
                501
            );
            let person = test
                .database
                .media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                .await?
                .context("missing imported person facet")?;
            assert_eq!(person.display_value, "Jane Doe");
            assert_eq!(
                test.database
                    .media_item_ids_for_facets(&MediaItemFacetCandidateQuery {
                        kind: Some(MediaItemFacetKind::Person),
                        entity_ids: vec![person.stable_id.clone()],
                        virtual_folder_ids: vec![folder.id],
                        ..MediaItemFacetCandidateQuery::default()
                    })
                    .await?,
                vec![item_ids[500]]
            );
            assert_eq!(
                test.database
                    .media_item_facet_values(MediaItemFacetKind::MusicArtist, &[folder.id])
                    .await?[0]
                    .display_value,
                "Track Artist"
            );
            assert_eq!(
                test.database
                    .media_item_facet_values(MediaItemFacetKind::MusicAlbumArtist, &[folder.id])
                    .await?[0]
                    .display_value,
                "Album Artist"
            );

            let upcoming_xmin_before: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_upcoming_dates WHERE item_id = $1",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;
            let filter_selector_xmin_before: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_filter_selectors \
                 WHERE item_id = $1 AND selector_kind = 'person' AND selector = 'imported-person'",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;

            let xmin_before: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_facets WHERE item_id = $1 AND facet_kind = 'person'",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;
            test.database
                .replace_remote_media_library_snapshot(
                    "PG Facet Music",
                    "music",
                    "provider://pg-facets",
                    items.clone(),
                )
                .await?;
            let xmin_after: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_facets WHERE item_id = $1 AND facet_kind = 'person'",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(xmin_before, xmin_after, "no-op snapshot rewrote facet row");
            let upcoming_xmin_after: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_upcoming_dates WHERE item_id = $1",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(
                upcoming_xmin_before, upcoming_xmin_after,
                "no-op snapshot rewrote Upcoming date row"
            );
            let filter_selector_xmin_after: String = sqlx::query_scalar(
                "SELECT xmin::text FROM media_item_filter_selectors \
                 WHERE item_id = $1 AND selector_kind = 'person' AND selector = 'imported-person'",
            )
            .bind(item_ids[500])
            .fetch_one(&test.database.pool)
            .await?;
            assert_eq!(
                filter_selector_xmin_before, filter_selector_xmin_after,
                "no-op snapshot rewrote an exact filter selector"
            );

            let facet_count_before: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM media_item_facets")
                    .fetch_one(&test.database.pool)
                    .await?;
            let upcoming_count_before: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM media_item_upcoming_dates")
                    .fetch_one(&test.database.pool)
                    .await?;
            let filter_selector_count_before: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM media_item_filter_selectors")
                    .fetch_one(&test.database.pool)
                    .await?;
            let marker_before_failed_rebuild =
                sqlx::query_as::<_, (i32, OffsetDateTime, i64, i64, i64, String)>(
                    "SELECT extractor_version, completed_at, source_item_count, \
                     projected_facet_count, projected_alias_count, xmin::text \
                     FROM jellyrin_derived_projection_versions \
                     WHERE projection_name = 'media_item_facets'",
                )
                .fetch_one(&test.database.pool)
                .await?;
            sqlx::query(
                r#"
                CREATE FUNCTION jellyrin_test_reject_facet_projection()
                RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN
                    IF NEW.display_value = 'Jane Doe' THEN
                        RAISE EXCEPTION 'forced facet projection failure';
                    END IF;
                    RETURN NEW;
                END
                $$
                "#,
            )
            .execute(&test.database.worker_pool)
            .await?;
            sqlx::query(
                "CREATE TRIGGER jellyrin_test_reject_facet_projection \
                 BEFORE INSERT ON media_item_facets FOR EACH ROW \
                 EXECUTE FUNCTION jellyrin_test_reject_facet_projection()",
            )
            .execute(&test.database.worker_pool)
            .await?;
            let rebuild_error = test
                .database
                .rebuild_media_item_facets()
                .await
                .unwrap_err()
                .to_string();
            assert!(rebuild_error.contains("forced facet projection failure"));
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_item_facets")
                    .fetch_one(&test.database.pool)
                    .await?,
                facet_count_before,
                "failed rebuild must restore the previous facet projection"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_item_upcoming_dates")
                    .fetch_one(&test.database.pool)
                    .await?,
                upcoming_count_before,
                "failed rebuild must restore the previous Upcoming date projection"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_item_filter_selectors")
                    .fetch_one(&test.database.pool)
                    .await?,
                filter_selector_count_before,
                "failed rebuild must restore the previous exact filter selector projection"
            );
            assert_eq!(
                sqlx::query_as::<_, (i32, OffsetDateTime, i64, i64, i64, String)>(
                    "SELECT extractor_version, completed_at, source_item_count, \
                     projected_facet_count, projected_alias_count, xmin::text \
                     FROM jellyrin_derived_projection_versions \
                     WHERE projection_name = 'media_item_facets'",
                )
                .fetch_one(&test.database.pool)
                .await?,
                marker_before_failed_rebuild,
                "failed rebuild must not publish a new marker"
            );
            sqlx::query("DROP TRIGGER jellyrin_test_reject_facet_projection ON media_item_facets")
                .execute(&test.database.worker_pool)
                .await?;
            sqlx::query("DROP FUNCTION jellyrin_test_reject_facet_projection()")
                .execute(&test.database.worker_pool)
                .await?;

            sqlx::query("DELETE FROM media_item_facets")
                .execute(&test.database.pool)
                .await?;
            test.database.rebuild_media_item_facets().await?;
            test.database.rebuild_media_item_facets().await?;
            assert_eq!(
                test.database
                    .media_item_facet_values(MediaItemFacetKind::Tag, &[folder.id])
                    .await?
                    .len(),
                501,
                "PostgreSQL rebuild must cross the 500-row batch"
            );

            test.database
                .replace_remote_media_library_snapshot(
                    "PG Facet Music",
                    "music",
                    "provider://pg-facets",
                    items[..500].to_vec(),
                )
                .await?;
            assert!(
                test.database
                    .media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                    .await?
                    .is_none()
            );
            test.database
                .replace_remote_media_library_snapshot(
                    "PG Facet Music",
                    "music",
                    "provider://pg-facets",
                    items,
                )
                .await?;
            assert!(
                test.database
                    .media_item_facet_by_entity_id(MediaItemFacetKind::Person, "imported-person")
                    .await?
                    .is_some()
            );
            anyhow::Ok(())
        }
        .await;
        test.cleanup().await;
        result.unwrap();
    }

    fn remote_item(
        id: Uuid,
        name: &str,
        path: &str,
        media_type: &str,
        collection_type: &str,
        metadata: Value,
    ) -> RemoteMediaItemUpsert {
        RemoteMediaItemUpsert {
            id: id.to_string(),
            name: name.to_owned(),
            path: path.to_owned(),
            media_type: media_type.to_owned(),
            collection_type: collection_type.to_owned(),
            runtime_ticks: Some(900_000),
            bitrate: Some(4_000_000),
            width: Some(1920),
            height: Some(1080),
            media_streams: vec![json!({"Type": media_type})],
            metadata,
        }
    }
}
