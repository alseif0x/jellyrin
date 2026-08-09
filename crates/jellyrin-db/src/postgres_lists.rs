use std::collections::{HashMap, HashSet};

use jellyrin_core::{MediaItem, User};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{MediaList, MediaListItem, MediaListUserPermission, PostgresDatabase};

impl PostgresDatabase {
    pub async fn create_media_list(
        &self,
        kind: &str,
        name: &str,
        collection_type: Option<&str>,
        owner_user_id: Option<Uuid>,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<MediaList> {
        let kind = kind.trim();
        let name = name.trim();
        anyhow::ensure!(!kind.is_empty(), "media list kind must not be empty");
        anyhow::ensure!(!name.is_empty(), "media list name must not be empty");

        let list_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let item_ids = dedupe_uuids(item_ids);
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, PostgresMediaListRow>(
            r#"
            INSERT INTO media_lists (
                id, kind, name, collection_type, owner_user_id, metadata,
                created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, '{}'::jsonb, $6, $6)
            RETURNING id, kind, name, collection_type, owner_user_id, metadata,
                      created_at, updated_at
            "#,
        )
        .bind(list_id)
        .bind(kind)
        .bind(name)
        .bind(collection_type)
        .bind(owner_user_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        ensure_visible_media_items(&mut tx, &item_ids).await?;
        insert_media_list_items(&mut tx, list_id, &item_ids, 0, now).await?;
        tx.commit().await?;
        Ok(row.into())
    }

    pub async fn media_lists(&self, kind: &str) -> anyhow::Result<Vec<MediaList>> {
        let rows = sqlx::query_as::<_, PostgresMediaListRow>(
            r#"
            SELECT id, kind, name, collection_type, owner_user_id, metadata,
                   created_at, updated_at
            FROM media_lists
            WHERE kind = $1
            ORDER BY lower(name), name
            "#,
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_list_by_id(&self, list_id: Uuid) -> anyhow::Result<MediaList> {
        let row = sqlx::query_as::<_, PostgresMediaListRow>(
            r#"
            SELECT id, kind, name, collection_type, owner_user_id, metadata,
                   created_at, updated_at
            FROM media_lists
            WHERE id = $1
            "#,
        )
        .bind(list_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn media_list_item_counts(
        &self,
        list_ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, usize>> {
        if list_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query_as::<_, (Uuid, i64)>(
            r#"
            SELECT list_item.list_id, COUNT(*)::bigint AS item_count
            FROM media_list_items AS list_item
            INNER JOIN media_items AS item ON item.id = list_item.item_id
            WHERE item.missing_since IS NULL AND list_item.list_id = ANY($1)
            GROUP BY list_item.list_id
            "#,
        )
        .bind(list_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(list_id, item_count)| (list_id, item_count.max(0) as usize))
            .collect())
    }

    pub async fn media_list_ids_with_user_permission(
        &self,
        user_id: Uuid,
        list_ids: &[Uuid],
    ) -> anyhow::Result<HashSet<Uuid>> {
        if list_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT list_id
            FROM media_list_user_permissions
            WHERE user_id = $1 AND list_id = ANY($2)
            "#,
        )
        .bind(user_id)
        .bind(list_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn update_media_list_name(
        &self,
        list_id: Uuid,
        name: &str,
    ) -> anyhow::Result<MediaList> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "media list name must not be empty");
        let row = sqlx::query_as::<_, PostgresMediaListRow>(
            r#"
            UPDATE media_lists
            SET name = $2, updated_at = $3
            WHERE id = $1
            RETURNING id, kind, name, collection_type, owner_user_id, metadata,
                      created_at, updated_at
            "#,
        )
        .bind(list_id)
        .bind(name)
        .bind(OffsetDateTime::now_utc())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn add_media_list_items(
        &self,
        list_id: Uuid,
        item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        let item_ids = dedupe_uuids(item_ids);
        let mut tx = self.pool.begin().await?;
        lock_media_list(&mut tx, list_id).await?;
        ensure_visible_media_items(&mut tx, &item_ids).await?;

        let existing = if item_ids.is_empty() {
            HashSet::new()
        } else {
            sqlx::query_scalar::<_, Uuid>(
                r#"
                SELECT item_id
                FROM media_list_items
                WHERE list_id = $1 AND item_id = ANY($2)
                "#,
            )
            .bind(list_id)
            .bind(&item_ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect()
        };
        let new_item_ids = item_ids
            .into_iter()
            .filter(|item_id| !existing.contains(item_id))
            .collect::<Vec<_>>();
        let next_position: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(MAX(position) + 1, 0)
            FROM media_list_items
            WHERE list_id = $1
            "#,
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await?;
        let now = OffsetDateTime::now_utc();
        insert_media_list_items(&mut tx, list_id, &new_item_ids, next_position, now).await?;
        touch_media_list(&mut tx, list_id, now).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn remove_media_list_items(
        &self,
        list_id: Uuid,
        item_ids: Vec<Uuid>,
        playlist_item_ids: Vec<Uuid>,
    ) -> anyhow::Result<()> {
        let item_ids = dedupe_uuids(item_ids);
        let playlist_item_ids = dedupe_uuids(playlist_item_ids);
        let mut tx = self.pool.begin().await?;
        lock_media_list(&mut tx, list_id).await?;
        sqlx::query(
            r#"
            DELETE FROM media_list_items
            WHERE list_id = $1
              AND (item_id = ANY($2) OR playlist_item_id = ANY($3))
            "#,
        )
        .bind(list_id)
        .bind(item_ids)
        .bind(playlist_item_ids)
        .execute(&mut *tx)
        .await?;
        reindex_media_list(&mut tx, list_id).await?;
        touch_media_list(&mut tx, list_id, OffsetDateTime::now_utc()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn move_media_list_item(
        &self,
        list_id: Uuid,
        target_id: Uuid,
        new_index: i64,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        lock_media_list(&mut tx, list_id).await?;
        let mut rows = sqlx::query_as::<_, PostgresMediaListItemIdRow>(
            r#"
            SELECT item_id, playlist_item_id
            FROM media_list_items
            WHERE list_id = $1
            ORDER BY position, added_at, playlist_item_id
            "#,
        )
        .bind(list_id)
        .fetch_all(&mut *tx)
        .await?;
        let Some(current_index) = rows
            .iter()
            .position(|row| row.item_id == target_id || row.playlist_item_id == target_id)
        else {
            anyhow::bail!("media list item not found");
        };
        let row = rows.remove(current_index);
        let target = new_index.max(0).min(rows.len() as i64) as usize;
        rows.insert(target, row);
        update_media_list_positions(&mut tx, list_id, &rows).await?;
        touch_media_list(&mut tx, list_id, OffsetDateTime::now_utc()).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn media_list_items(&self, list_id: Uuid) -> anyhow::Result<Vec<MediaListItem>> {
        self.media_list_by_id(list_id).await?;
        let rows = sqlx::query_as::<_, PostgresMediaListItemRow>(
            r#"
            SELECT list_item.playlist_item_id, list_item.position, list_item.added_at,
                   item.id, item.virtual_folder_id, item.name, item.path,
                   item.media_type, item.collection_type, item.file_size,
                   item.runtime_ticks, item.bitrate, item.width, item.height,
                   item.media_streams, item.created_at, item.updated_at
            FROM media_list_items AS list_item
            INNER JOIN media_items AS item ON item.id = list_item.item_id
            WHERE list_item.list_id = $1 AND item.missing_since IS NULL
            ORDER BY list_item.position, lower(item.name), item.name
            "#,
        )
        .bind(list_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_list_user_permissions(
        &self,
        list_id: Uuid,
    ) -> anyhow::Result<Vec<MediaListUserPermission>> {
        self.media_list_by_id(list_id).await?;
        let rows = sqlx::query_as::<_, PostgresMediaListUserPermissionRow>(
            r#"
            SELECT permission.list_id, permission.can_edit,
                   permission.created_at AS permission_created_at,
                   permission.updated_at AS permission_updated_at,
                   users.id, users.name, users.is_administrator, users.is_disabled,
                   users.sync_play_access, users.created_at, users.updated_at
            FROM media_list_user_permissions AS permission
            INNER JOIN users ON users.id = permission.user_id
            WHERE permission.list_id = $1
            ORDER BY lower(users.name), users.name
            "#,
        )
        .bind(list_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<MediaListUserPermission>> {
        let row = sqlx::query_as::<_, PostgresMediaListUserPermissionRow>(
            r#"
            SELECT permission.list_id, permission.can_edit,
                   permission.created_at AS permission_created_at,
                   permission.updated_at AS permission_updated_at,
                   users.id, users.name, users.is_administrator, users.is_disabled,
                   users.sync_play_access, users.created_at, users.updated_at
            FROM media_list_user_permissions AS permission
            INNER JOIN users ON users.id = permission.user_id
            WHERE permission.list_id = $1 AND permission.user_id = $2
            "#,
        )
        .bind(list_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn upsert_media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
        can_edit: bool,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        lock_media_list(&mut tx, list_id).await?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE id = $1 FOR KEY SHARE")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO media_list_user_permissions (
                list_id, user_id, can_edit, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (list_id, user_id) DO UPDATE SET
                can_edit = excluded.can_edit,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(list_id)
        .bind(user_id)
        .bind(can_edit)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_media_list_user_permission(
        &self,
        list_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM media_list_user_permissions WHERE list_id = $1 AND user_id = $2")
            .bind(list_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

async fn lock_media_list(tx: &mut Transaction<'_, Postgres>, list_id: Uuid) -> anyhow::Result<()> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM media_lists WHERE id = $1 FOR UPDATE")
        .bind(list_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

async fn ensure_visible_media_items(
    tx: &mut Transaction<'_, Postgres>,
    item_ids: &[Uuid],
) -> anyhow::Result<()> {
    if item_ids.is_empty() {
        return Ok(());
    }
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM media_items
        WHERE id = ANY($1) AND missing_since IS NULL
        "#,
    )
    .bind(item_ids)
    .fetch_one(&mut **tx)
    .await?;
    anyhow::ensure!(
        count == item_ids.len() as i64,
        "one or more media items were not found"
    );
    Ok(())
}

async fn insert_media_list_items(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
    item_ids: &[Uuid],
    start_position: i64,
    added_at: OffsetDateTime,
) -> anyhow::Result<()> {
    if item_ids.is_empty() {
        return Ok(());
    }
    let playlist_item_ids = item_ids.iter().map(|_| Uuid::new_v4()).collect::<Vec<_>>();
    let positions = (start_position..).take(item_ids.len()).collect::<Vec<_>>();
    sqlx::query(
        r#"
        INSERT INTO media_list_items (
            list_id, item_id, playlist_item_id, position, added_at
        )
        SELECT $1, input.item_id, input.playlist_item_id, input.position, $5
        FROM UNNEST($2::uuid[], $3::uuid[], $4::bigint[])
             AS input(item_id, playlist_item_id, position)
        ON CONFLICT (list_id, item_id) DO NOTHING
        "#,
    )
    .bind(list_id)
    .bind(item_ids)
    .bind(playlist_item_ids)
    .bind(positions)
    .bind(added_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn reindex_media_list(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        WITH ranked AS (
            SELECT playlist_item_id,
                   ROW_NUMBER() OVER (
                       ORDER BY position, added_at, playlist_item_id
                   ) - 1 AS new_position
            FROM media_list_items
            WHERE list_id = $1
        )
        UPDATE media_list_items AS item
        SET position = ranked.new_position
        FROM ranked
        WHERE item.list_id = $1
          AND item.playlist_item_id = ranked.playlist_item_id
        "#,
    )
    .bind(list_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_media_list_positions(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
    rows: &[PostgresMediaListItemIdRow],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let playlist_item_ids = rows
        .iter()
        .map(|row| row.playlist_item_id)
        .collect::<Vec<_>>();
    let positions = (0_i64..).take(rows.len()).collect::<Vec<_>>();
    sqlx::query(
        r#"
        UPDATE media_list_items AS item
        SET position = ordering.position
        FROM UNNEST($2::uuid[], $3::bigint[])
             AS ordering(playlist_item_id, position)
        WHERE item.list_id = $1
          AND item.playlist_item_id = ordering.playlist_item_id
        "#,
    )
    .bind(list_id)
    .bind(playlist_item_ids)
    .bind(positions)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn touch_media_list(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
    updated_at: OffsetDateTime,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE media_lists SET updated_at = $2 WHERE id = $1")
        .bind(list_id)
        .bind(updated_at)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn dedupe_uuids(values: Vec<Uuid>) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(*value))
        .collect()
}

#[derive(sqlx::FromRow)]
struct PostgresMediaListRow {
    id: Uuid,
    kind: String,
    name: String,
    collection_type: Option<String>,
    owner_user_id: Option<Uuid>,
    metadata: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresMediaListRow> for MediaList {
    fn from(row: PostgresMediaListRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            name: row.name,
            collection_type: row.collection_type,
            owner_user_id: row.owner_user_id,
            metadata: row.metadata,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaListItemRow {
    playlist_item_id: Uuid,
    position: i64,
    added_at: OffsetDateTime,
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

impl From<PostgresMediaListItemRow> for MediaListItem {
    fn from(row: PostgresMediaListItemRow) -> Self {
        Self {
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
                media_streams: row.media_streams.as_array().cloned().unwrap_or_default(),
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
            playlist_item_id: row.playlist_item_id,
            position: row.position,
            added_at: row.added_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresMediaListItemIdRow {
    item_id: Uuid,
    playlist_item_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct PostgresMediaListUserPermissionRow {
    list_id: Uuid,
    can_edit: bool,
    permission_created_at: OffsetDateTime,
    permission_updated_at: OffsetDateTime,
    id: Uuid,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresMediaListUserPermissionRow> for MediaListUserPermission {
    fn from(row: PostgresMediaListUserPermissionRow) -> Self {
        Self {
            list_id: row.list_id,
            user: User {
                id: row.id,
                name: row.name,
                is_administrator: row.is_administrator,
                is_disabled: row.is_disabled,
                sync_play_access: row.sync_play_access,
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
            can_edit: row.can_edit,
            created_at: row.permission_created_at,
            updated_at: row.permission_updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::{
        PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    };

    use super::*;

    struct IsolatedPostgres {
        database: PostgresDatabase,
        admin_pool: PgPool,
        schema: String,
    }

    struct TestDatabase {
        database: PostgresDatabase,
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
                .expect("failed to connect PostgreSQL list-test admin pool");
            let schema = format!("jellyrin_lists_test_{}", Uuid::new_v4().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&admin_pool)
                .await
                .expect("failed to create isolated PostgreSQL list-test schema");

            let search_path = format!("{schema},public");
            let scoped_options = base_options.options([("search_path", &search_path)]);
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect_with(scoped_options.clone())
                .await
                .expect("failed to connect isolated PostgreSQL list API pool");
            let worker_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(scoped_options)
                .await
                .expect("failed to connect isolated PostgreSQL list worker pool");
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
                    .expect("failed to clean list schema after migration failure");
                panic!("failed to migrate isolated PostgreSQL list schema: {error:#}");
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
            .expect("failed to drop isolated PostgreSQL list-test schema");
            self.admin_pool.close().await;
        }
    }

    #[tokio::test]
    async fn postgres_media_lists_support_atomic_crud_order_move_and_remove() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let database = test.database.clone();
        let result = tokio::spawn(async move {
            let test = TestDatabase { database };
            let owner_id = seed_user(&test.database.pool, "Owner").await?;
            let item_ids = seed_media_items(&test.database.pool, 4).await?;
            let list = test
                .database
                .create_media_list(
                    " playlist ",
                    " Road Trip ",
                    Some("music"),
                    Some(owner_id),
                    vec![item_ids[0], item_ids[1], item_ids[0]],
                )
                .await?;
            assert_eq!(list.kind, "playlist");
            assert_eq!(list.name, "Road Trip");
            assert_eq!(list.owner_user_id, Some(owner_id));
            assert_eq!(list.metadata, serde_json::json!({}));
            assert_eq!(test.database.media_lists("playlist").await?.len(), 1);

            let renamed = test
                .database
                .update_media_list_name(list.id, " Updated Trip ")
                .await?;
            assert_eq!(renamed.name, "Updated Trip");
            assert_eq!(
                test.database.media_list_by_id(list.id).await?.name,
                "Updated Trip"
            );

            test.database
                .add_media_list_items(list.id, vec![item_ids[2], item_ids[1]])
                .await?;
            let mut items = test.database.media_list_items(list.id).await?;
            assert_eq!(
                items.iter().map(|item| item.position).collect::<Vec<_>>(),
                [0, 1, 2]
            );
            assert_eq!(
                items.iter().map(|item| item.item.id).collect::<Vec<_>>(),
                item_ids[..3]
            );
            let counts = test
                .database
                .media_list_item_counts(&[list.id, Uuid::new_v4()])
                .await?;
            assert_eq!(counts.get(&list.id), Some(&3));
            assert_eq!(counts.len(), 1);

            let third_playlist_id = items[2].playlist_item_id;
            test.database
                .move_media_list_item(list.id, third_playlist_id, 0)
                .await?;
            items = test.database.media_list_items(list.id).await?;
            assert_eq!(
                items.iter().map(|item| item.item.id).collect::<Vec<_>>(),
                [item_ids[2], item_ids[0], item_ids[1]]
            );
            test.database
                .move_media_list_item(list.id, item_ids[0], i64::MAX)
                .await?;
            items = test.database.media_list_items(list.id).await?;
            assert_eq!(
                items.iter().map(|item| item.item.id).collect::<Vec<_>>(),
                [item_ids[2], item_ids[1], item_ids[0]]
            );

            let last_playlist_id = items[2].playlist_item_id;
            test.database
                .remove_media_list_items(list.id, vec![item_ids[2]], vec![last_playlist_id])
                .await?;
            items = test.database.media_list_items(list.id).await?;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].item.id, item_ids[1]);
            assert_eq!(items[0].position, 0);
            anyhow::Ok(())
        })
        .await;
        test.cleanup().await;
        finish_test(result);
    }

    #[tokio::test]
    async fn postgres_media_list_permissions_enforce_users_and_preserve_ownership_fk() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let database = test.database.clone();
        let result = tokio::spawn(async move {
            let test = TestDatabase { database };
            let owner_id = seed_user(&test.database.pool, "Owner").await?;
            let bob_id = seed_user(&test.database.pool, "Bob").await?;
            let alice_id = seed_user(&test.database.pool, "Alice").await?;
            let list = test
                .database
                .create_media_list("playlist", "Shared", None, Some(owner_id), Vec::new())
                .await?;

            test.database
                .upsert_media_list_user_permission(list.id, bob_id, true)
                .await?;
            test.database
                .upsert_media_list_user_permission(list.id, alice_id, false)
                .await?;
            assert_eq!(
                test.database
                    .media_list_ids_with_user_permission(bob_id, &[list.id, Uuid::new_v4()],)
                    .await?,
                HashSet::from([list.id])
            );
            let permissions = test.database.media_list_user_permissions(list.id).await?;
            assert_eq!(
                permissions
                    .iter()
                    .map(|permission| permission.user.name.as_str())
                    .collect::<Vec<_>>(),
                ["Alice", "Bob"]
            );
            let alice_created_at = permissions[0].created_at;

            test.database
                .upsert_media_list_user_permission(list.id, alice_id, true)
                .await?;
            let alice = test
                .database
                .media_list_user_permission(list.id, alice_id)
                .await?
                .expect("Alice permission should exist");
            assert!(alice.can_edit);
            assert_eq!(alice.created_at, alice_created_at);

            test.database
                .delete_media_list_user_permission(list.id, bob_id)
                .await?;
            assert!(
                test.database
                    .media_list_user_permission(list.id, bob_id)
                    .await?
                    .is_none()
            );
            assert!(
                test.database
                    .media_list_ids_with_user_permission(bob_id, &[list.id])
                    .await?
                    .is_empty()
            );
            assert!(
                test.database
                    .upsert_media_list_user_permission(list.id, Uuid::new_v4(), true)
                    .await
                    .is_err()
            );

            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(owner_id)
                .execute(&test.database.pool)
                .await?;
            assert_eq!(
                test.database.media_list_by_id(list.id).await?.owner_user_id,
                None
            );
            anyhow::Ok(())
        })
        .await;
        test.cleanup().await;
        finish_test(result);
    }

    #[tokio::test]
    async fn postgres_media_list_mutations_serialize_and_roll_back_as_a_unit() {
        let Some(test) = IsolatedPostgres::create().await else {
            return;
        };
        let database = test.database.clone();
        let result = tokio::spawn(async move {
            let test = TestDatabase { database };
            let item_ids = seed_media_items(&test.database.pool, 7).await?;
            let list = test
                .database
                .create_media_list("playlist", "Concurrent", None, None, vec![item_ids[0]])
                .await?;

            let invalid_item_id = Uuid::new_v4();
            assert!(
                test.database
                    .add_media_list_items(list.id, vec![item_ids[1], invalid_item_id])
                    .await
                    .is_err()
            );
            assert_eq!(test.database.media_list_items(list.id).await?.len(), 1);

            assert!(
                test.database
                    .create_media_list(
                        "playlist",
                        "Must Roll Back",
                        None,
                        None,
                        vec![invalid_item_id],
                    )
                    .await
                    .is_err()
            );
            assert!(
                !test
                    .database
                    .media_lists("playlist")
                    .await?
                    .iter()
                    .any(|candidate| candidate.name == "Must Roll Back")
            );

            let first_database = test.database.clone();
            let second_database = test.database.clone();
            let first_add = first_database
                .add_media_list_items(list.id, vec![item_ids[1], item_ids[2], item_ids[3]]);
            let second_add = second_database
                .add_media_list_items(list.id, vec![item_ids[3], item_ids[4], item_ids[5]]);
            let (first_result, second_result) = tokio::join!(first_add, second_add);
            first_result?;
            second_result?;

            let items = test.database.media_list_items(list.id).await?;
            assert_eq!(items.len(), 6);
            assert_eq!(
                items.iter().map(|item| item.position).collect::<Vec<_>>(),
                [0, 1, 2, 3, 4, 5]
            );
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.item.id)
                    .collect::<HashSet<_>>()
                    .len(),
                6
            );

            let order_before_failed_move =
                items.iter().map(|item| item.item.id).collect::<Vec<_>>();
            assert!(
                test.database
                    .move_media_list_item(list.id, Uuid::new_v4(), 0)
                    .await
                    .is_err()
            );
            assert_eq!(
                test.database
                    .media_list_items(list.id)
                    .await?
                    .iter()
                    .map(|item| item.item.id)
                    .collect::<Vec<_>>(),
                order_before_failed_move
            );
            anyhow::Ok(())
        })
        .await;
        test.cleanup().await;
        finish_test(result);
    }

    fn finish_test(result: Result<anyhow::Result<()>, tokio::task::JoinError>) {
        match result {
            Ok(result) => result.unwrap(),
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => panic!("PostgreSQL list test task was cancelled: {error}"),
        }
    }

    async fn seed_user(pool: &PgPool, label: &str) -> anyhow::Result<Uuid> {
        let user_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, $2, $3, $3)")
            .bind(user_id)
            .bind(label)
            .bind(now)
            .execute(pool)
            .await?;
        Ok(user_id)
    }

    async fn seed_media_items(pool: &PgPool, count: usize) -> anyhow::Result<Vec<Uuid>> {
        let folder_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations, created_at, updated_at
            ) VALUES ($1, $2, 'movies', '[]'::jsonb, $3, $3)
            "#,
        )
        .bind(folder_id)
        .bind(format!("Lists Fixture {folder_id}"))
        .bind(now)
        .execute(pool)
        .await?;

        let mut item_ids = Vec::with_capacity(count);
        for index in 0..count {
            let item_id = Uuid::new_v4();
            sqlx::query(
                r#"
                INSERT INTO media_items (
                    id, virtual_folder_id, name, path, media_type, collection_type,
                    created_at, updated_at
                ) VALUES ($1, $2, $3, $4, 'Video', 'movies', $5, $5)
                "#,
            )
            .bind(item_id)
            .bind(folder_id)
            .bind(format!("List Item {index}"))
            .bind(format!("fixture://lists/{folder_id}/{item_id}.mkv"))
            .bind(now)
            .execute(pool)
            .await?;
            item_ids.push(item_id);
        }
        Ok(item_ids)
    }
}
