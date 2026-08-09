use anyhow::ensure;
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder};
use time::OffsetDateTime;

use super::{
    LiveTvCategoryRecord, LiveTvCategoryUpsert, LiveTvChannelQuery, LiveTvChannelRecord,
    LiveTvChannelUpsert, LiveTvPage, LiveTvTunerUpsert, postgres::PostgresDatabase,
};

// Keep every statement comfortably below PostgreSQL's bind-parameter limit while avoiding one
// round trip per provider record. A full external-provider catalog can contain tens of thousands of
// channels, so batching materially reduces synchronization CPU and connection occupancy.
const LIVE_TV_CATEGORY_BATCH_SIZE: usize = 4_000;
const LIVE_TV_CHANNEL_BATCH_SIZE: usize = 2_000;

impl PostgresDatabase {
    pub async fn replace_live_tv_tuner_snapshot(
        &self,
        mut tuner: LiveTvTunerUpsert,
        categories: Vec<LiveTvCategoryUpsert>,
        channels: Vec<LiveTvChannelUpsert>,
    ) -> anyhow::Result<Value> {
        let tuner_id = tuner.tuner_id.trim().to_string();
        validate_live_tv_snapshot_tuner_ids(&tuner_id, &categories, &channels)?;

        let mut transaction = self.pool.begin().await?;
        super::postgres_provider_secrets::lock_provider_configuration_mutation(
            &mut transaction,
            "tuner",
            &tuner_id,
        )
        .await?;
        let existing_configuration = sqlx::query_scalar::<_, Value>(
            r#"
            SELECT configuration
            FROM live_tv_tuners
            WHERE enabled AND lower(tuner_id) = lower($1)
            FOR UPDATE
            "#,
        )
        .bind(&tuner_id)
        .fetch_optional(&mut *transaction)
        .await?;
        super::inherit_provider_secret_reference_for_configuration(
            &mut tuner.configuration,
            existing_configuration.as_ref(),
            &tuner.provider_type,
        )?;
        let secret_namespace =
            if super::configuration_has_provider_secret_material(&tuner.configuration) {
                super::provider_secret_namespace_for_configuration(
                    &tuner.provider_type,
                    &tuner.configuration,
                )?
            } else {
                tuner.provider_type.clone()
            };
        tuner.configuration = self
            .protect_provider_configuration_in_connection(
                &mut transaction,
                &secret_namespace,
                tuner.configuration,
            )
            .await?;
        let protected_configuration = tuner.configuration;

        let now = OffsetDateTime::now_utc();

        // The per-tuner advisory lock was acquired before reading/protecting the configuration.
        // Holding it through this write and the catalog swap serializes both new and existing
        // tuners, so concurrent snapshots cannot leave an unused envelope or interleave rows.
        sqlx::query(
            r#"
            INSERT INTO live_tv_tuners (
                tuner_id, provider_type, name, source_url, enabled, configuration,
                last_sync_at, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, true, $5, $6, $6, $6)
            ON CONFLICT (tuner_id) DO UPDATE SET
                provider_type = EXCLUDED.provider_type,
                name = EXCLUDED.name,
                source_url = EXCLUDED.source_url,
                enabled = true,
                configuration = EXCLUDED.configuration,
                last_sync_at = EXCLUDED.last_sync_at,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(&tuner_id)
        .bind(tuner.provider_type.trim())
        .bind(tuner.name.trim())
        .bind(tuner.source_url.as_deref())
        .bind(&protected_configuration)
        .bind(now)
        .execute(&mut *transaction)
        .await?;

        sqlx::query("DELETE FROM live_tv_channels WHERE tuner_id = $1")
            .bind(&tuner_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM live_tv_categories WHERE tuner_id = $1")
            .bind(&tuner_id)
            .execute(&mut *transaction)
            .await?;

        for batch in categories.chunks(LIVE_TV_CATEGORY_BATCH_SIZE) {
            let mut builder = QueryBuilder::<Postgres>::new(
                r#"
                INSERT INTO live_tv_categories (
                    category_id, tuner_id, remote_id, name, sort_name, created_at, updated_at
                )
                "#,
            );
            builder.push_values(batch, |mut values, category| {
                let name = category.name.trim();
                values
                    .push_bind(category.category_id.trim())
                    .push_bind(&tuner_id)
                    .push_bind(category.remote_id.trim())
                    .push_bind(name)
                    .push_bind(name.to_ascii_lowercase())
                    .push_bind(now)
                    .push_bind(now);
            });
            builder.build().execute(&mut *transaction).await?;
        }

        for batch in channels.chunks(LIVE_TV_CHANNEL_BATCH_SIZE) {
            let mut builder = QueryBuilder::<Postgres>::new(
                r#"
                INSERT INTO live_tv_channels (
                    channel_id, tuner_id, remote_id, category_id, name, sort_name, number,
                    stream_url, logo_url, enabled, channel_type, metadata, created_at, updated_at
                )
                "#,
            );
            builder.push_values(batch, |mut values, channel| {
                values
                    .push_bind(channel.channel_id.trim())
                    .push_bind(&tuner_id)
                    .push_bind(channel.remote_id.trim())
                    .push_bind(normalize_live_tv_category_id(
                        channel.category_id.as_deref(),
                    ))
                    .push_bind(channel.name.trim())
                    .push_bind(channel.sort_name.trim())
                    .push_bind(channel.number.as_deref())
                    .push_bind(channel.stream_url.trim())
                    .push_bind(channel.logo_url.as_deref())
                    .push("true")
                    .push_bind(channel.channel_type.trim())
                    .push_bind(channel.metadata.clone())
                    .push_bind(now)
                    .push_bind(now);
            });
            builder.build().execute(&mut *transaction).await?;
        }

        transaction.commit().await?;
        Ok(protected_configuration)
    }

    pub async fn live_tv_channel_page(
        &self,
        query: LiveTvChannelQuery,
    ) -> anyhow::Result<LiveTvPage<LiveTvChannelRecord>> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;

        let mut count_builder = live_tv_channel_count_builder();
        append_live_tv_channel_filters(&mut count_builder, &query);
        let total_record_count = count_builder
            .build_query_scalar::<i64>()
            .fetch_one(&mut *transaction)
            .await?
            .max(0) as usize;

        let mut page_builder = live_tv_channel_select_builder();
        append_live_tv_channel_filters(&mut page_builder, &query);
        page_builder.push(" ORDER BY lower(c.sort_name), lower(c.name), c.channel_id");
        if let Some(limit) = query.limit {
            page_builder.push(" LIMIT ");
            page_builder.push_bind(limit as i64);
            page_builder.push(" OFFSET ");
            page_builder.push_bind(query.start_index as i64);
        }
        let rows = page_builder
            .build_query_as::<PostgresLiveTvChannelRow>()
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;

        Ok(LiveTvPage {
            items: rows.into_iter().map(Into::into).collect(),
            total_record_count,
            start_index: query.start_index,
        })
    }

    pub async fn live_tv_channel_count(&self, query: &LiveTvChannelQuery) -> anyhow::Result<usize> {
        let mut builder = live_tv_channel_count_builder();
        append_live_tv_channel_filters(&mut builder, query);
        let count = builder
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;
        Ok(count.max(0) as usize)
    }

    pub async fn live_tv_channel_by_id(
        &self,
        channel_id: &str,
    ) -> anyhow::Result<Option<LiveTvChannelRecord>> {
        let row = sqlx::query_as::<_, PostgresLiveTvChannelRow>(
            r#"
            SELECT c.channel_id,
                   c.tuner_id,
                   c.remote_id,
                   c.category_id,
                   category.name AS category_name,
                   c.name,
                   c.sort_name,
                   c.number,
                   c.stream_url,
                   c.logo_url,
                   c.channel_type,
                   c.metadata
            FROM live_tv_channels AS c
            LEFT JOIN live_tv_categories AS category
                ON category.category_id = c.category_id
            WHERE c.enabled AND c.channel_id = $1
            "#,
        )
        .bind(channel_id.trim())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn live_tv_categories(&self) -> anyhow::Result<Vec<LiveTvCategoryRecord>> {
        let rows = sqlx::query_as::<_, PostgresLiveTvCategoryRow>(
            r#"
            SELECT category_id, tuner_id, remote_id, name, sort_name
            FROM live_tv_categories
            ORDER BY lower(sort_name), lower(name), category_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn live_tv_tuner_configurations_by_provider(
        &self,
        provider_type: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let configurations = sqlx::query_scalar(
            r#"
            SELECT configuration
            FROM live_tv_tuners
            WHERE enabled AND lower(provider_type) = lower($1)
            ORDER BY lower(name), tuner_id
            "#,
        )
        .bind(provider_type.trim())
        .fetch_all(&self.pool)
        .await
        .map_err(anyhow::Error::from)?;
        let mut resolved = Vec::with_capacity(configurations.len());
        for configuration in configurations {
            resolved.push(self.resolve_provider_configuration(&configuration).await?);
        }
        Ok(resolved)
    }

    pub async fn live_tv_tuner_configuration_by_id(
        &self,
        tuner_id: &str,
    ) -> anyhow::Result<Option<Value>> {
        sqlx::query_scalar(
            r#"
            SELECT configuration
            FROM live_tv_tuners
            WHERE enabled AND lower(tuner_id) = lower($1)
            "#,
        )
        .bind(tuner_id.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn delete_live_tv_tuner_state(&self, tuner_id: &str) -> anyhow::Result<()> {
        let tuner_id = tuner_id.trim();
        let mut transaction = self.pool.begin().await?;
        super::postgres_provider_secrets::lock_provider_configuration_mutation(
            &mut transaction,
            "tuner",
            tuner_id,
        )
        .await?;
        let deleted_configuration = sqlx::query_scalar::<_, Value>(
            r#"
            SELECT configuration
            FROM live_tv_tuners
            WHERE lower(tuner_id) = lower($1)
            FOR UPDATE
            "#,
        )
        .bind(tuner_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let deleted_reference = deleted_configuration
            .as_ref()
            .and_then(super::ProviderSecretReference::from_configuration);

        // Every legitimate writer resolves an existing reference with FOR UPDATE before storing
        // it. Taking the same envelope lock closes the race between the reference scan and GC.
        let locked_secret = if let Some(reference) = deleted_reference.as_ref() {
            sqlx::query_scalar::<_, String>(
                r#"
                SELECT secret_id
                FROM provider_secrets
                WHERE secret_id = $1 AND lower(provider_type) = lower($2)
                FOR UPDATE
                "#,
            )
            .bind(&reference.id)
            .bind(&reference.provider_type)
            .fetch_optional(&mut *transaction)
            .await?
            .is_some()
        } else {
            false
        };

        sqlx::query("DELETE FROM live_tv_tuners WHERE lower(tuner_id) = lower($1)")
            .bind(tuner_id)
            .execute(&mut *transaction)
            .await?;

        if locked_secret && let Some(reference) = deleted_reference {
            let configurations = sqlx::query_scalar::<_, Value>(
                r#"
                SELECT configuration FROM live_tv_tuners
                UNION ALL
                SELECT configuration FROM plugin_configurations
                UNION ALL
                SELECT payload FROM named_configurations
                "#,
            )
            .fetch_all(&mut *transaction)
            .await?;
            let still_referenced = configurations.iter().any(|configuration| {
                super::configuration_references_provider_secret(configuration, &reference)
            });
            if !still_referenced {
                sqlx::query(
                    r#"
                    DELETE FROM provider_secrets
                    WHERE secret_id = $1 AND lower(provider_type) = lower($2)
                    "#,
                )
                .bind(&reference.id)
                .bind(&reference.provider_type)
                .execute(&mut *transaction)
                .await?;
            }
        }

        transaction.commit().await?;
        Ok(())
    }
}

fn validate_live_tv_snapshot_tuner_ids(
    tuner_id: &str,
    categories: &[LiveTvCategoryUpsert],
    channels: &[LiveTvChannelUpsert],
) -> anyhow::Result<()> {
    for category in categories {
        let category_tuner_id = category.tuner_id.trim();
        ensure!(
            category_tuner_id == tuner_id,
            "Live TV category `{}` has tuner_id `{category_tuner_id}`, expected snapshot tuner_id `{tuner_id}`",
            category.category_id.trim()
        );
    }
    for channel in channels {
        let channel_tuner_id = channel.tuner_id.trim();
        ensure!(
            channel_tuner_id == tuner_id,
            "Live TV channel `{}` has tuner_id `{channel_tuner_id}`, expected snapshot tuner_id `{tuner_id}`",
            channel.channel_id.trim()
        );
    }
    Ok(())
}

fn normalize_live_tv_category_id(category_id: Option<&str>) -> Option<&str> {
    category_id.map(str::trim).filter(|value| !value.is_empty())
}

fn live_tv_channel_select_builder() -> QueryBuilder<'static, Postgres> {
    QueryBuilder::new(
        r#"
        SELECT c.channel_id,
               c.tuner_id,
               c.remote_id,
               c.category_id,
               category.name AS category_name,
               c.name,
               c.sort_name,
               c.number,
               c.stream_url,
               c.logo_url,
               c.channel_type,
               c.metadata
        FROM live_tv_channels AS c
        LEFT JOIN live_tv_categories AS category
            ON category.category_id = c.category_id
        WHERE c.enabled
        "#,
    )
}

fn live_tv_channel_count_builder() -> QueryBuilder<'static, Postgres> {
    QueryBuilder::new(
        r#"
        SELECT count(*)
        FROM live_tv_channels AS c
        LEFT JOIN live_tv_categories AS category
            ON category.category_id = c.category_id
        WHERE c.enabled
        "#,
    )
}

fn append_live_tv_channel_filters(
    builder: &mut QueryBuilder<'_, Postgres>,
    query: &LiveTvChannelQuery,
) {
    let category_ids = query
        .category_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if !category_ids.is_empty() {
        builder.push(" AND c.category_id IN (");
        let mut separated = builder.separated(", ");
        for category_id in category_ids {
            separated.push_bind(category_id.to_string());
        }
        separated.push_unseparated(")");
    }
    if let Some(search_term) = query.search_term.as_deref().map(str::trim)
        && !search_term.is_empty()
    {
        // This is ILIKE-equivalent while remaining directly compatible with the baseline's
        // `lower(name) gin_trgm_ops` expression index for contains searches.
        builder.push(" AND lower(c.name) LIKE lower(");
        builder.push_bind(format!("%{search_term}%"));
        builder.push(")");
    }
}

#[derive(FromRow)]
struct PostgresLiveTvChannelRow {
    channel_id: String,
    tuner_id: String,
    remote_id: String,
    category_id: Option<String>,
    category_name: Option<String>,
    name: String,
    sort_name: String,
    number: Option<String>,
    stream_url: String,
    logo_url: Option<String>,
    channel_type: String,
    metadata: Value,
}

impl From<PostgresLiveTvChannelRow> for LiveTvChannelRecord {
    fn from(row: PostgresLiveTvChannelRow) -> Self {
        Self {
            channel_id: row.channel_id,
            tuner_id: row.tuner_id,
            remote_id: row.remote_id,
            category_id: row.category_id,
            category_name: row.category_name,
            name: row.name,
            sort_name: row.sort_name,
            number: row.number,
            stream_url: row.stream_url,
            logo_url: row.logo_url,
            channel_type: row.channel_type,
            metadata: row.metadata,
        }
    }
}

#[derive(FromRow)]
struct PostgresLiveTvCategoryRow {
    category_id: String,
    tuner_id: String,
    remote_id: String,
    name: String,
    sort_name: String,
}

impl From<PostgresLiveTvCategoryRow> for LiveTvCategoryRecord {
    fn from(row: PostgresLiveTvCategoryRow) -> Self {
        Self {
            category_id: row.category_id,
            tuner_id: row.tuner_id,
            remote_id: row.remote_id,
            name: row.name,
            sort_name: row.sort_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::PostgresSettings;

    async fn configured_database() -> Option<PostgresDatabase> {
        let database_url = std::env::var("JELLYRIN_TEST_POSTGRES_URL").ok()?;
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        Some(database)
    }

    fn tuner(tuner_id: &str, name: &str, version: i64) -> LiveTvTunerUpsert {
        LiveTvTunerUpsert {
            tuner_id: tuner_id.to_string(),
            provider_type: "ExternalProvider".to_string(),
            name: name.to_string(),
            source_url: Some("https://provider.invalid/player_api.php".to_string()),
            configuration: json!({"Version": version, "TunerId": tuner_id}),
        }
    }

    fn category(
        tuner_id: &str,
        category_id: &str,
        remote_id: &str,
        name: &str,
    ) -> LiveTvCategoryUpsert {
        LiveTvCategoryUpsert {
            category_id: category_id.to_string(),
            tuner_id: tuner_id.to_string(),
            remote_id: remote_id.to_string(),
            name: name.to_string(),
        }
    }

    fn channel(
        tuner_id: &str,
        channel_id: &str,
        remote_id: &str,
        category_id: &str,
        name: &str,
    ) -> LiveTvChannelUpsert {
        LiveTvChannelUpsert {
            channel_id: channel_id.to_string(),
            tuner_id: tuner_id.to_string(),
            remote_id: remote_id.to_string(),
            category_id: Some(category_id.to_string()),
            name: name.to_string(),
            sort_name: name.to_ascii_lowercase(),
            number: Some(remote_id.to_string()),
            stream_url: format!("https://stream.invalid/live/{remote_id}.ts"),
            logo_url: Some(format!("https://images.invalid/{remote_id}.png")),
            channel_type: "TV".to_string(),
            metadata: json!({"RemoteId": remote_id}),
        }
    }

    #[test]
    fn live_tv_snapshot_requires_every_record_to_use_the_parent_tuner_id() {
        let tuner_id = "snapshot-tuner";
        assert!(
            validate_live_tv_snapshot_tuner_ids(
                tuner_id,
                &[category(
                    "  snapshot-tuner  ",
                    "matching-category",
                    "category-1",
                    "Matching"
                )],
                &[channel(
                    " snapshot-tuner ",
                    "matching-channel",
                    "channel-1",
                    "matching-category",
                    "Matching"
                )]
            )
            .is_ok()
        );

        let category_error = validate_live_tv_snapshot_tuner_ids(
            tuner_id,
            &[category(
                "different-tuner",
                "foreign-category",
                "category-2",
                "Foreign",
            )],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(category_error.contains("Live TV category `foreign-category`"));
        assert!(category_error.contains("tuner_id `different-tuner`"));
        assert!(category_error.contains("snapshot tuner_id `snapshot-tuner`"));

        let channel_error = validate_live_tv_snapshot_tuner_ids(
            tuner_id,
            &[],
            &[channel(
                "DIFFERENT-TUNER",
                "foreign-channel",
                "channel-2",
                "foreign-category",
                "Foreign",
            )],
        )
        .unwrap_err()
        .to_string();
        assert!(channel_error.contains("Live TV channel `foreign-channel`"));
        assert!(channel_error.contains("tuner_id `DIFFERENT-TUNER`"));
        assert!(channel_error.contains("snapshot tuner_id `snapshot-tuner`"));

        assert_eq!(
            normalize_live_tv_category_id(Some("  matching-category  ")),
            Some("matching-category")
        );
        assert_eq!(normalize_live_tv_category_id(Some("   ")), None);
        assert_eq!(normalize_live_tv_category_id(None), None);
    }

    #[tokio::test]
    async fn postgres_plugin_tuner_encrypts_credentials_and_rolls_back_partial_updates() {
        let Some(database) = configured_database().await else {
            return;
        };
        assert!(database.validate_provider_secret_write_readiness().is_err());
        let database = database.with_provider_secret_vault(
            crate::ProviderSecretVault::new("postgres-plugin-test", vec![0x6d; 32]).unwrap(),
        );
        database.validate_provider_secret_write_readiness().unwrap();
        let plugin_id = Uuid::new_v4();
        let provider_type = format!("plugin:{plugin_id}");
        let tuner_id = format!("postgres-magstv-plugin-{plugin_id}");
        let public_configuration = json!({
            "PluginId": plugin_id,
            "Provider": "MAGSTV",
            "PortalUrl": "https://magstv.invalid",
            "SecretReference": {
                "Namespace": "magstv",
                "Key": format!("tuners/{tuner_id}/credentials")
            }
        });

        let persisted = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: public_configuration.clone(),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let (stored_provider_type, stored_configuration): (String, Value) = sqlx::query_as(
            "SELECT provider_type, configuration FROM live_tv_tuners WHERE tuner_id = $1",
        )
        .bind(&tuner_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();

        assert_eq!(persisted, public_configuration);
        assert_eq!(stored_provider_type, provider_type);
        assert_eq!(stored_configuration, public_configuration);

        let mut submitted = public_configuration.clone();
        submitted["Username"] = json!("postgres-magstv-user");
        submitted["Password"] = json!("postgres-magstv-password");
        let protected = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: submitted,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let reference = crate::ProviderSecretReference::from_configuration(&protected).unwrap();
        assert_eq!(reference.provider_type, format!("plugin-{plugin_id}"));
        assert!(protected.get("Username").is_none());
        assert!(protected.get("Password").is_none());
        assert_eq!(
            protected["SecretReference"],
            public_configuration["SecretReference"]
        );
        let (current_reference, credentials) = database
            .provider_credentials_for_configuration(&protected)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current_reference, reference);
        assert_eq!(credentials.username(), "postgres-magstv-user");
        assert_eq!(credentials.password(), "postgres-magstv-password");

        let updated = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": plugin_id,
                        "SecretReference": public_configuration["SecretReference"].clone(),
                        "Password": "postgres-updated-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let updated_reference =
            crate::ProviderSecretReference::from_configuration(&updated).unwrap();
        assert_eq!(updated_reference.id, reference.id);
        assert_eq!(updated_reference.revision, reference.revision + 1);

        let rollback_result = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": plugin_id,
                        "Password": "must-roll-back"
                    }),
                },
                vec![
                    category(
                        &tuner_id,
                        &format!("plugin-rollback-a-{plugin_id}"),
                        "duplicate-plugin-category",
                        "Duplicate A",
                    ),
                    category(
                        &tuner_id,
                        &format!("plugin-rollback-b-{plugin_id}"),
                        "duplicate-plugin-category",
                        "Duplicate B",
                    ),
                ],
                Vec::new(),
            )
            .await;
        assert!(rollback_result.is_err());
        let configuration_after_rollback: Value =
            sqlx::query_scalar("SELECT configuration FROM live_tv_tuners WHERE tuner_id = $1")
                .bind(&tuner_id)
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(configuration_after_rollback, updated);
        let (reference_after_rollback, credentials_after_rollback) = database
            .provider_credentials_for_configuration(&configuration_after_rollback)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reference_after_rollback, updated_reference);
        assert_eq!(
            credentials_after_rollback.password(),
            "postgres-updated-password"
        );

        let core_reference_result = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: tuner_id.clone(),
                    provider_type: provider_type.clone(),
                    name: "MAGSTV plugin tuner".to_string(),
                    source_url: None,
                    configuration: json!({
                        "PluginId": plugin_id,
                        "JellyrinProviderSecretRef": {
                            "Id": "ps_foreign",
                            "Provider": "xtream",
                            "Revision": 1
                        }
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await;
        let configuration_after_rejections: Value =
            sqlx::query_scalar("SELECT configuration FROM live_tv_tuners WHERE tuner_id = $1")
                .bind(&tuner_id)
                .fetch_one(&database.pool)
                .await
                .unwrap();

        database
            .delete_live_tv_tuner_state(&tuner_id)
            .await
            .unwrap();
        sqlx::query("DELETE FROM provider_secrets WHERE secret_id = $1")
            .bind(&updated_reference.id)
            .execute(&database.pool)
            .await
            .unwrap();
        database.close().await;

        assert!(core_reference_result.is_err());
        assert_eq!(configuration_after_rejections, updated);
    }

    #[tokio::test]
    async fn postgres_tuner_delete_collects_only_an_unreferenced_secret_envelope() {
        let Some(database) = configured_database().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let database = database.with_provider_secret_vault(
            crate::ProviderSecretVault::new(format!("postgres-delete-gc-{suffix}"), vec![0x67; 32])
                .unwrap(),
        );
        let first_tuner_id = format!("postgres-shared-secret-a-{suffix}");
        let second_tuner_id = format!("postgres-shared-secret-b-{suffix}");
        let first_configuration = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: first_tuner_id.clone(),
                    provider_type: "xtream".to_string(),
                    name: "PostgreSQL shared secret A".to_string(),
                    source_url: None,
                    configuration: json!({
                        "Id": first_tuner_id,
                        "Type": "xtream",
                        "Username": "postgres-shared-user",
                        "Password": "postgres-shared-password"
                    }),
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        let reference =
            crate::ProviderSecretReference::from_configuration(&first_configuration).unwrap();
        let mut second_configuration = first_configuration;
        second_configuration["Id"] = json!(second_tuner_id);
        let second_configuration = database
            .replace_live_tv_tuner_snapshot(
                LiveTvTunerUpsert {
                    tuner_id: second_tuner_id.clone(),
                    provider_type: "xtream".to_string(),
                    name: "PostgreSQL shared secret B".to_string(),
                    source_url: None,
                    configuration: second_configuration,
                },
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            crate::ProviderSecretReference::from_configuration(&second_configuration)
                .unwrap()
                .id,
            reference.id
        );

        database
            .delete_live_tv_tuner_state(&first_tuner_id.to_ascii_uppercase())
            .await
            .unwrap();
        let envelope_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM provider_secrets WHERE secret_id = $1)",
        )
        .bind(&reference.id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert!(envelope_exists);
        let (_, credentials) = database
            .provider_credentials_for_configuration(&second_configuration)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username(), "postgres-shared-user");
        assert_eq!(credentials.password(), "postgres-shared-password");

        database
            .delete_live_tv_tuner_state(&second_tuner_id)
            .await
            .unwrap();
        let envelope_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM provider_secrets WHERE secret_id = $1)",
        )
        .bind(&reference.id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        database.close().await;

        assert!(!envelope_exists);
    }

    #[tokio::test]
    async fn postgres_live_tv_snapshot_rolls_back_as_one_catalog() {
        let Some(database) = configured_database().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let tuner_id = format!("postgres-livetv-rollback-{suffix}");
        let category_id = format!("postgres-livetv-category-{suffix}");
        let channel_id = format!("postgres-livetv-channel-{suffix}");

        database
            .replace_live_tv_tuner_snapshot(
                tuner(&tuner_id, "Initial tuner", 1),
                vec![category(&tuner_id, &category_id, "category-1", "News")],
                vec![channel(
                    &tuner_id,
                    &channel_id,
                    "channel-1",
                    &category_id,
                    "News One",
                )],
            )
            .await
            .unwrap();

        let duplicate_remote_id = "duplicate-remote-id";
        let failed_snapshot = database
            .replace_live_tv_tuner_snapshot(
                tuner(&tuner_id, "Replacement tuner", 2),
                vec![
                    category(
                        &tuner_id,
                        &format!("postgres-livetv-bad-a-{suffix}"),
                        duplicate_remote_id,
                        "Bad A",
                    ),
                    category(
                        &tuner_id,
                        &format!("postgres-livetv-bad-b-{suffix}"),
                        duplicate_remote_id,
                        "Bad B",
                    ),
                ],
                Vec::new(),
            )
            .await;

        let preserved_channel = database.live_tv_channel_by_id(&channel_id).await.unwrap();
        let configuration: Value =
            sqlx::query_scalar("SELECT configuration FROM live_tv_tuners WHERE tuner_id = $1")
                .bind(&tuner_id)
                .fetch_one(&database.pool)
                .await
                .unwrap();

        database
            .delete_live_tv_tuner_state(&tuner_id)
            .await
            .unwrap();
        database.close().await;

        assert!(failed_snapshot.is_err());
        assert!(preserved_channel.is_some());
        assert_eq!(configuration["Version"], 1);
    }

    #[tokio::test]
    async fn postgres_live_tv_schema_rejects_a_category_owned_by_another_tuner() {
        let Some(database) = configured_database().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let category_tuner_id = format!("postgres-livetv-category-owner-{suffix}");
        let channel_tuner_id = format!("postgres-livetv-channel-owner-{suffix}");
        let category_id = format!("postgres-livetv-owned-category-{suffix}");
        let channel_id = format!("postgres-livetv-owned-channel-{suffix}");
        let uncategorized_channel_id = format!("postgres-livetv-uncategorized-channel-{suffix}");

        let mut owned_channel = channel(
            &category_tuner_id,
            &channel_id,
            "channel-1",
            &category_id,
            "News One",
        );
        owned_channel.category_id = Some(format!("  {category_id}  "));
        let mut uncategorized_channel = channel(
            &category_tuner_id,
            &uncategorized_channel_id,
            "channel-2",
            &category_id,
            "Uncategorized",
        );
        uncategorized_channel.category_id = Some("   ".to_string());
        database
            .replace_live_tv_tuner_snapshot(
                tuner(&category_tuner_id, "Category owner", 1),
                vec![category(
                    &category_tuner_id,
                    &category_id,
                    "category-1",
                    "News",
                )],
                vec![owned_channel, uncategorized_channel],
            )
            .await
            .unwrap();
        database
            .replace_live_tv_tuner_snapshot(
                tuner(&channel_tuner_id, "Different tuner", 1),
                Vec::new(),
                Vec::new(),
            )
            .await
            .unwrap();

        let cross_tuner_update =
            sqlx::query("UPDATE live_tv_channels SET tuner_id = $1 WHERE channel_id = $2")
                .bind(&channel_tuner_id)
                .bind(&channel_id)
                .execute(&database.pool)
                .await;
        let (persisted_tuner_id, persisted_category_id): (String, Option<String>) = sqlx::query_as(
            "SELECT tuner_id, category_id FROM live_tv_channels WHERE channel_id = $1",
        )
        .bind(&channel_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        let empty_category_id: Option<String> =
            sqlx::query_scalar("SELECT category_id FROM live_tv_channels WHERE channel_id = $1")
                .bind(&uncategorized_channel_id)
                .fetch_one(&database.pool)
                .await
                .unwrap();
        let constraint_definition: String = sqlx::query_scalar(
            r#"
            SELECT pg_get_constraintdef(oid)
            FROM pg_constraint
            WHERE conrelid = 'live_tv_channels'::regclass
              AND conname = 'live_tv_channels_category_tuner_fkey'
            "#,
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM live_tv_categories WHERE category_id = $1")
            .bind(&category_id)
            .execute(&database.pool)
            .await
            .unwrap();
        let (tuner_after_category_delete, category_after_category_delete): (
            String,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT tuner_id, category_id FROM live_tv_channels WHERE channel_id = $1",
        )
        .bind(&channel_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();

        database
            .delete_live_tv_tuner_state(&category_tuner_id)
            .await
            .unwrap();
        database
            .delete_live_tv_tuner_state(&channel_tuner_id)
            .await
            .unwrap();
        database.close().await;

        assert!(cross_tuner_update.is_err());
        assert_eq!(persisted_tuner_id, category_tuner_id);
        assert_eq!(persisted_category_id.as_deref(), Some(category_id.as_str()));
        assert!(empty_category_id.is_none());
        assert_eq!(tuner_after_category_delete, category_tuner_id);
        assert!(category_after_category_delete.is_none());
        assert!(constraint_definition.contains("FOREIGN KEY (category_id, tuner_id)"));
        assert!(constraint_definition.contains("ON DELETE SET NULL (category_id)"));
    }

    #[tokio::test]
    async fn postgres_live_tv_queries_filter_page_and_delete_tuner_state() {
        let Some(database) = configured_database().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let tuner_id = format!("postgres-livetv-query-{suffix}");
        let news_id = format!("postgres-livetv-news-{suffix}");
        let sports_id = format!("postgres-livetv-sports-{suffix}");
        let news_channel_id = format!("postgres-livetv-news-channel-{suffix}");
        let sports_alpha_id = format!("postgres-livetv-sports-alpha-{suffix}");
        let sports_beta_id = format!("postgres-livetv-sports-beta-{suffix}");

        database
            .replace_live_tv_tuner_snapshot(
                tuner(&tuner_id, "Queryable tuner", 7),
                vec![
                    category(&tuner_id, &news_id, "news", "News"),
                    category(&tuner_id, &sports_id, "sports", "Sports"),
                ],
                vec![
                    channel(&tuner_id, &news_channel_id, "1", &news_id, "News One"),
                    channel(&tuner_id, &sports_alpha_id, "2", &sports_id, "Sports Alpha"),
                    channel(&tuner_id, &sports_beta_id, "3", &sports_id, "sports Beta"),
                ],
            )
            .await
            .unwrap();

        let page = database
            .live_tv_channel_page(LiveTvChannelQuery {
                start_index: 1,
                limit: Some(1),
                search_term: Some("  SPORTS  ".to_string()),
                category_ids: vec!["".to_string(), format!("  {sports_id}  ")],
            })
            .await
            .unwrap();
        let count = database
            .live_tv_channel_count(&LiveTvChannelQuery {
                search_term: Some("sports".to_string()),
                category_ids: vec![sports_id.clone()],
                ..LiveTvChannelQuery::default()
            })
            .await
            .unwrap();
        let beta = database
            .live_tv_channel_by_id(&sports_beta_id)
            .await
            .unwrap()
            .unwrap();
        let categories = database.live_tv_categories().await.unwrap();
        let configurations = database
            .live_tv_tuner_configurations_by_provider("externalprovider")
            .await
            .unwrap();
        let browse_index_definitions = sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT indexname, indexdef
            FROM pg_indexes
            WHERE schemaname = current_schema()
              AND indexname = ANY($1)
            ORDER BY indexname
            "#,
        )
        .bind(vec![
            "live_tv_channels_enabled_sort_idx".to_string(),
            "live_tv_channels_enabled_category_sort_idx".to_string(),
        ])
        .fetch_all(&database.pool)
        .await
        .unwrap()
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
        let obsolete_category_index_exists = sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('live_tv_channels_category_sort_idx') IS NOT NULL",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();

        database
            .delete_live_tv_tuner_state(&tuner_id.to_ascii_uppercase())
            .await
            .unwrap();
        let deleted = database
            .live_tv_channel_by_id(&sports_beta_id)
            .await
            .unwrap();
        database.close().await;

        assert_eq!(page.total_record_count, 2);
        assert_eq!(page.start_index, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].channel_id, sports_beta_id);
        assert_eq!(count, 2);
        assert_eq!(beta.category_name.as_deref(), Some("Sports"));
        assert_eq!(beta.metadata["RemoteId"], "3");
        assert_eq!(
            categories
                .iter()
                .filter(|category| category.tuner_id == tuner_id)
                .count(),
            2
        );
        assert!(
            configurations
                .iter()
                .any(|configuration| configuration["TunerId"] == tuner_id)
        );
        let global_index = &browse_index_definitions["live_tv_channels_enabled_sort_idx"];
        assert!(global_index.contains("lower(sort_name)"));
        assert!(global_index.contains("lower(name)"));
        assert!(global_index.contains("channel_id"));
        assert!(global_index.contains("WHERE enabled"));
        let category_index =
            &browse_index_definitions["live_tv_channels_enabled_category_sort_idx"];
        assert!(category_index.contains("category_id"));
        assert!(category_index.contains("lower(sort_name)"));
        assert!(category_index.contains("lower(name)"));
        assert!(category_index.contains("channel_id"));
        assert!(category_index.contains("WHERE enabled"));
        assert!(!obsolete_category_index_exists);
        assert!(deleted.is_none());
    }
}
