use std::{collections::HashSet, path::Path};

use anyhow::Context;
use jellyrin_core::VirtualFolder;
use serde_json::{Value, json};
use sqlx::Acquire;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PostgresDatabase, collect_media_files_if_root_available, media_type_for_path,
    merge_metadata_values, metadata_lock_data, metadata_lock_key, metadata_locked_fields,
    postgres_catalog::replace_postgres_media_item_facets, probe_media_info,
    read_local_nfo_metadata,
};

impl PostgresDatabase {
    pub async fn scan_virtual_folder_items(&self, folder_id: Uuid) -> anyhow::Result<usize> {
        let (mut lock_connection, lock_key) = self.acquire_scan_lock(folder_id).await?;
        let scan_result = self
            .scan_virtual_folder_items_locked(folder_id, &mut lock_connection)
            .await;
        let unlock_result =
            sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
                .bind(lock_key)
                .fetch_one(&mut *lock_connection)
                .await
                .context("failed to release PostgreSQL catalog scan lock");

        match (scan_result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(scanned), Ok(true)) => Ok(scanned),
            (Ok(_), Ok(false)) => anyhow::bail!("PostgreSQL catalog scan lock was not held"),
        }
    }

    async fn scan_virtual_folder_items_locked(
        &self,
        folder_id: Uuid,
        lock_connection: &mut sqlx::PgConnection,
    ) -> anyhow::Result<usize> {
        let folder = self
            .postgres_scan_virtual_folder(folder_id)
            .await?
            .context("virtual folder not found")?;
        let mut scanned = 0usize;
        let mut found_paths = HashSet::new();
        let mut can_reconcile_stale = true;

        for location in &folder.locations {
            let root = Path::new(location);
            let Some(media_files) = collect_media_files_if_root_available(root).await? else {
                can_reconcile_stale = false;
                continue;
            };
            for path in media_files {
                let path_string = path.to_string_lossy().to_string();
                if self
                    .postgres_media_item_path_is_deleted(&path_string)
                    .await?
                {
                    continue;
                }
                let Some(name) = media_name(&path) else {
                    continue;
                };
                let Some(media_type) = media_type_for_path(&path) else {
                    continue;
                };
                found_paths.insert(path_string);
                self.postgres_upsert_local_media_item(
                    lock_connection,
                    &folder,
                    &name,
                    &path,
                    media_type,
                )
                .await?;
                scanned += 1;
            }
        }

        if can_reconcile_stale {
            let found_paths = found_paths.into_iter().collect::<Vec<_>>();
            sqlx::query(
                r#"
                UPDATE media_items
                SET missing_since = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                WHERE virtual_folder_id = $1
                  AND missing_since IS NULL
                  AND NOT (path = ANY($2))
                "#,
            )
            .bind(folder.id)
            .bind(found_paths)
            .execute(&mut *lock_connection)
            .await?;
        }
        Ok(scanned)
    }

    /// Marks one indexed path as unavailable without cascading into irreplaceable user state.
    pub async fn mark_media_item_missing_by_path(&self, path: &str) -> anyhow::Result<bool> {
        let Some(folder_id) = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT virtual_folder_id
            FROM media_items
            WHERE path = $1 AND missing_since IS NULL
            LIMIT 1
            "#,
        )
        .bind(path)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false);
        };
        let mut transaction = self.pool.begin().await?;
        let lock_key = scan_lock_key(folder_id);
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *transaction)
            .await?;
        let result = sqlx::query(
            r#"
            UPDATE media_items
            SET missing_since = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
            WHERE path = $1 AND virtual_folder_id = $2 AND missing_since IS NULL
            "#,
        )
        .bind(path)
        .bind(folder_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    /// Incrementally indexes a local file. External-provider catalogs use the snapshot pipeline
    /// instead and therefore never require filesystem access.
    pub async fn scan_single_file(&self, path: &Path) -> anyhow::Result<bool> {
        let path_string = path.to_string_lossy().to_string();
        if self
            .postgres_media_item_path_is_deleted(&path_string)
            .await?
        {
            return Ok(false);
        }
        let Some(name) = media_name(path) else {
            return Ok(false);
        };
        let Some(media_type) = media_type_for_path(path) else {
            return Ok(false);
        };

        for folder in self.virtual_folders().await? {
            if folder
                .locations
                .iter()
                .any(|location| path.starts_with(Path::new(location)))
            {
                let (mut lock_connection, lock_key) = self.acquire_scan_lock(folder.id).await?;
                let scan_result = self
                    .postgres_upsert_local_media_item(
                        &mut lock_connection,
                        &folder,
                        &name,
                        path,
                        media_type,
                    )
                    .await;
                let unlock_result = sqlx::query_scalar::<_, bool>(
                    "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                )
                .bind(lock_key)
                .fetch_one(&mut *lock_connection)
                .await
                .context("failed to release PostgreSQL catalog scan lock");
                return match (scan_result, unlock_result) {
                    (Err(error), _) => Err(error),
                    (Ok(_), Err(error)) => Err(error),
                    (Ok(()), Ok(true)) => Ok(true),
                    (Ok(()), Ok(false)) => {
                        anyhow::bail!("PostgreSQL catalog scan lock was not held")
                    }
                };
            }
        }
        Ok(false)
    }

    async fn acquire_scan_lock(
        &self,
        folder_id: Uuid,
    ) -> anyhow::Result<(sqlx::pool::PoolConnection<sqlx::Postgres>, String)> {
        let mut connection = self.worker_pool.acquire().await?;
        // Session advisory locks survive a future cancellation. Closing instead of recycling the
        // connection guarantees PostgreSQL releases the lock even if a scan task is aborted.
        connection.close_on_drop();
        let lock_key = scan_lock_key(folder_id);
        sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
            .bind(&lock_key)
            .execute(&mut *connection)
            .await
            .context("failed to acquire PostgreSQL catalog scan lock")?;
        Ok((connection, lock_key))
    }

    async fn postgres_scan_virtual_folder(
        &self,
        folder_id: Uuid,
    ) -> anyhow::Result<Option<VirtualFolder>> {
        let row = sqlx::query_as::<_, PostgresScanVirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations, created_at, updated_at
            FROM virtual_folders
            WHERE id = $1
            "#,
        )
        .bind(folder_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn postgres_media_item_path_is_deleted(&self, path: &str) -> anyhow::Result<bool> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM media_item_deletions WHERE path = $1)")
            .bind(path)
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    async fn postgres_upsert_local_media_item(
        &self,
        lock_connection: &mut sqlx::PgConnection,
        folder: &VirtualFolder,
        name: &str,
        path: &Path,
        media_type: &str,
    ) -> anyhow::Result<()> {
        let path = path.to_string_lossy().to_string();
        let filesystem_metadata = tokio::fs::metadata(&path).await.ok();
        let file_size = filesystem_metadata
            .as_ref()
            .map(|metadata| i64::try_from(metadata.len()))
            .transpose()
            .context("local media file is too large to index")?;
        let modified_at = filesystem_metadata
            .and_then(|metadata| metadata.modified().ok())
            .map(OffsetDateTime::from);

        let mut media_info = probe_media_info(Path::new(&path), media_type).await;
        if let Some(nfo_metadata) = read_local_nfo_metadata(Path::new(&path)).await {
            media_info.metadata = merge_metadata_values(media_info.metadata, nfo_metadata);
        }

        let exact_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM media_items WHERE path = $1 ORDER BY missing_since NULLS FIRST LIMIT 1",
        )
        .bind(&path)
        .fetch_optional(&mut *lock_connection)
        .await?;
        let identity_id = if exact_id.is_none() {
            match (file_size, modified_at) {
                (Some(file_size), Some(modified_at)) => {
                    sqlx::query_scalar::<_, Uuid>(
                        r#"
                    SELECT id
                    FROM media_items
                    WHERE virtual_folder_id = $1
                      AND media_type = $2
                      AND file_size = $3
                      AND modified_at = $4
                      AND path <> $5
                      AND missing_since IS NOT NULL
                    ORDER BY missing_since DESC
                    LIMIT 1
                    "#,
                    )
                    .bind(folder.id)
                    .bind(media_type)
                    .bind(file_size)
                    .bind(modified_at)
                    .bind(&path)
                    .fetch_optional(&mut *lock_connection)
                    .await?
                }
                _ => None,
            }
        } else {
            None
        };
        let item_id = exact_id.or(identity_id).unwrap_or_else(Uuid::new_v4);

        let current_metadata =
            sqlx::query_scalar::<_, Value>("SELECT metadata FROM media_items WHERE id = $1")
                .bind(item_id)
                .fetch_optional(&mut *lock_connection)
                .await?
                .unwrap_or_else(|| json!({}));
        let metadata = merge_scanned_metadata(current_metadata, media_info.metadata);

        let mut transaction = lock_connection.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type,
                last_seen_at, missing_since, file_size, modified_at,
                runtime_ticks, bitrate, width, height, media_streams, metadata,
                created_at, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP, NULL, $7, $8,
                $9, $10, $11, $12, $13, $14, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            )
            ON CONFLICT (id) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                name = excluded.name,
                path = excluded.path,
                media_type = excluded.media_type,
                collection_type = excluded.collection_type,
                last_seen_at = excluded.last_seen_at,
                missing_since = NULL,
                file_size = excluded.file_size,
                modified_at = excluded.modified_at,
                runtime_ticks = excluded.runtime_ticks,
                bitrate = excluded.bitrate,
                width = excluded.width,
                height = excluded.height,
                media_streams = excluded.media_streams,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(item_id)
        .bind(folder.id)
        .bind(name)
        .bind(&path)
        .bind(media_type)
        .bind(&folder.collection_type)
        .bind(file_size)
        .bind(modified_at)
        .bind(media_info.runtime_ticks)
        .bind(media_info.bitrate)
        .bind(media_info.width)
        .bind(media_info.height)
        .bind(serde_json::to_value(media_info.media_streams)?)
        .bind(&metadata)
        .execute(&mut *transaction)
        .await?;
        replace_postgres_media_item_facets(&mut transaction, item_id, &metadata).await?;
        transaction.commit().await?;
        Ok(())
    }
}

fn scan_lock_key(folder_id: Uuid) -> String {
    format!("jellyrin:postgres:catalog-scan:{folder_id}")
}

#[derive(sqlx::FromRow)]
struct PostgresScanVirtualFolderRow {
    id: Uuid,
    name: String,
    collection_type: Option<String>,
    locations: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<PostgresScanVirtualFolderRow> for VirtualFolder {
    type Error = anyhow::Error;

    fn try_from(row: PostgresScanVirtualFolderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            collection_type: row.collection_type,
            locations: serde_json::from_value(row.locations)
                .context("invalid PostgreSQL virtual folder locations")?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn media_name(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn merge_scanned_metadata(current: Value, scanned: Value) -> Value {
    let mut current = current.as_object().cloned().unwrap_or_default();
    if metadata_lock_data(&current) {
        return Value::Object(current);
    }
    let locked_fields = metadata_locked_fields(&current);
    if let Some(scanned) = scanned.as_object() {
        for (key, value) in scanned {
            if !locked_fields.contains(&metadata_lock_key(key)) {
                current.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(current)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        super::{MediaItemCatalogQuery, PostgresSettings},
        PostgresDatabase,
    };

    #[tokio::test]
    async fn postgres_incremental_scan_resurrects_tombstoned_item_when_configured() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scan-test.mkv");
        tokio::fs::write(&path, b"not-a-real-container")
            .await
            .unwrap();
        tokio::fs::write(
            path.with_extension("nfo"),
            "<movie><genre>Scan Genre</genre></movie>",
        )
        .await
        .unwrap();
        let folder = database
            .upsert_virtual_folder(
                &format!("scan-test-{}", Uuid::new_v4()),
                Some("movies"),
                vec![directory.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        let first_scanner = database.clone();
        let second_scanner = database.clone();
        let (first_scan, second_scan) = tokio::join!(
            first_scanner.scan_single_file(&path),
            second_scanner.scan_single_file(&path)
        );
        assert!(first_scan.unwrap());
        assert!(second_scan.unwrap());
        let first = database
            .media_items_for_virtual_folders(&[folder.id])
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        let genre_page = database
            .media_item_catalog_page(&MediaItemCatalogQuery {
                limit: 10,
                genre_ids: vec!["scan genre".to_string()],
                ..MediaItemCatalogQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(genre_page.total_record_count, 1);
        assert_eq!(genre_page.items[0].item.id, first[0].id);
        assert!(
            database
                .mark_media_item_missing_by_path(&path.to_string_lossy())
                .await
                .unwrap()
        );
        assert!(
            database
                .media_items_for_virtual_folders(&[folder.id])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(database.scan_single_file(&path).await.unwrap());
        let resurrected = database
            .media_items_for_virtual_folders(&[folder.id])
            .await
            .unwrap();
        assert_eq!(resurrected.len(), 1);
        assert_eq!(resurrected[0].id, first[0].id);
        assert_eq!(
            database.scan_virtual_folder_items(folder.id).await.unwrap(),
            1
        );

        database.delete_virtual_folder(&folder.name).await.unwrap();
        database.close().await;
    }
}
