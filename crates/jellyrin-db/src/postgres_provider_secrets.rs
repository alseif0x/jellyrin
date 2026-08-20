use std::collections::HashSet;

use anyhow::{Context, ensure};
use serde_json::Value;
use sqlx::{PgConnection, Row};

use super::postgres::POSTGRES_SERIALIZABLE_BEGIN;
use super::{
    PostgresDatabase, ProviderCredentials, ProviderSecretEnvelope, ProviderSecretReference,
    collect_provider_secret_reference_identities, configuration_has_provider_secret_input_field,
    configuration_has_provider_secret_material, configuration_has_provider_secret_reference_field,
    inherit_provider_secret_reference_for_configuration, new_provider_secret_id,
    normalize_provider_type, provider_credentials_from_configuration,
    provider_secret_namespace_for_configuration, redacted_provider_configuration,
    resolved_provider_configuration, set_provider_secret_reference,
};

pub(super) async fn lock_provider_configuration_mutation(
    connection: &mut PgConnection,
    scope: &str,
    id: &str,
) -> anyhow::Result<()> {
    let lock_key = format!(
        "jellyrin:postgres:provider-secret:{}:{}",
        scope.trim().to_ascii_lowercase(),
        id.trim().to_ascii_lowercase()
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(connection)
        .await
        .context("failed to lock PostgreSQL provider configuration mutation")?;
    Ok(())
}

impl PostgresDatabase {
    pub async fn backfill_legacy_provider_secrets(&self) -> anyhow::Result<usize> {
        // Keep every envelope and every configuration rewrite in one transaction.  Besides
        // avoiding orphan envelopes on a failed rewrite, locking the source rows prevents a
        // concurrent configuration update from being silently replaced by the backfill.
        let mut transaction = self.pool.begin().await?;
        lock_provider_configuration_mutation(
            &mut transaction,
            "plugin",
            "jellyrin-xtream-provider",
        )
        .await?;
        lock_provider_configuration_mutation(&mut transaction, "named", "livetv").await?;
        let tuner_ids = sqlx::query_scalar::<_, String>(
            "SELECT tuner_id FROM live_tv_tuners ORDER BY lower(tuner_id), tuner_id",
        )
        .fetch_all(&mut *transaction)
        .await?;
        for tuner_id in &tuner_ids {
            lock_provider_configuration_mutation(&mut transaction, "tuner", tuner_id).await?;
        }
        let plugin_configuration = sqlx::query_scalar::<_, Value>(
            "SELECT configuration FROM plugin_configurations WHERE lower(plugin_id) = lower($1) FOR UPDATE",
        )
        .bind("jellyrin-xtream-provider")
        .fetch_optional(&mut *transaction)
        .await?;
        let tuner_rows = sqlx::query(
            "SELECT tuner_id, provider_type, configuration FROM live_tv_tuners ORDER BY tuner_id FOR UPDATE",
        )
        .fetch_all(&mut *transaction)
        .await?;
        let tuner_configurations = tuner_rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("tuner_id"),
                    row.get::<String, _>("provider_type"),
                    row.get::<Value, _>("configuration"),
                )
            })
            .collect::<Vec<_>>();
        let named_configuration = sqlx::query_scalar::<_, Value>(
            "SELECT payload FROM named_configurations WHERE key = 'livetv' FOR UPDATE",
        )
        .fetch_optional(&mut *transaction)
        .await?;

        let builtin_tuner_configuration = tuner_configurations
            .iter()
            .find(|(tuner_id, _, _)| tuner_id.eq_ignore_ascii_case("xtream-plugin"))
            .map(|(_, _, configuration)| configuration);
        let named_builtin_configuration = named_configuration
            .as_ref()
            .and_then(|configuration| configuration.get("TunerHosts"))
            .and_then(Value::as_array)
            .and_then(|hosts| {
                hosts.iter().find(|host| {
                    host.get("Id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.eq_ignore_ascii_case("xtream-plugin"))
                })
            });
        let canonical_seed = [
            plugin_configuration.as_ref(),
            builtin_tuner_configuration,
            named_builtin_configuration,
        ]
        .into_iter()
        .flatten()
        .find(|configuration| configuration_has_provider_secret_material(configuration))
        .cloned();

        let canonical = if let Some(seed) = canonical_seed {
            let canonical_credentials = self
                .configuration_credentials_in_connection(&mut transaction, &seed)
                .await?
                .context("builtin Xtream credentials are incomplete")?;
            for configuration in [
                plugin_configuration.as_ref(),
                builtin_tuner_configuration,
                named_builtin_configuration,
            ]
            .into_iter()
            .flatten()
            .filter(|configuration| configuration_has_provider_secret_material(configuration))
            {
                let credentials = self
                    .configuration_credentials_in_connection(&mut transaction, configuration)
                    .await?
                    .context("builtin Xtream credentials are incomplete")?;
                ensure!(
                    credentials == canonical_credentials,
                    "conflicting legacy Xtream credentials; provider secret backfill was not applied"
                );
            }
            Some(
                self.protect_provider_configuration_in_connection(&mut transaction, "xtream", seed)
                    .await?,
            )
        } else {
            None
        };
        let canonical_reference = canonical
            .as_ref()
            .and_then(ProviderSecretReference::from_configuration);

        let plugin_rewrite = if let (Some(original), Some(reference)) =
            (plugin_configuration.as_ref(), canonical_reference.as_ref())
        {
            let mut candidate = original.clone();
            set_provider_secret_reference(&mut candidate, reference)?;
            let protected = self
                .protect_provider_configuration_in_connection(&mut transaction, "xtream", candidate)
                .await?;
            (protected != *original).then_some(protected)
        } else {
            None
        };

        let mut tuner_rewrites = Vec::new();
        for (tuner_id, provider_type, configuration) in &tuner_configurations {
            let mut candidate = configuration.clone();
            if tuner_id.eq_ignore_ascii_case("xtream-plugin")
                && let Some(reference) = canonical_reference.as_ref()
            {
                set_provider_secret_reference(&mut candidate, reference)?;
            }
            let secret_namespace = if configuration_has_provider_secret_material(&candidate) {
                provider_secret_namespace_for_configuration(provider_type, &candidate)?
            } else {
                provider_type.clone()
            };
            let protected = self
                .protect_provider_configuration_in_connection(
                    &mut transaction,
                    &secret_namespace,
                    candidate,
                )
                .await?;
            if protected != *configuration {
                tuner_rewrites.push((tuner_id.clone(), protected));
            }
        }

        let named_rewrite = if let Some(original) = named_configuration.as_ref() {
            let mut candidate = original.clone();
            if let Some(reference) = canonical_reference.as_ref()
                && let Some(host) = candidate
                    .get_mut("TunerHosts")
                    .and_then(Value::as_array_mut)
                    .and_then(|hosts| {
                        hosts.iter_mut().find(|host| {
                            host.get("Id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id.eq_ignore_ascii_case("xtream-plugin"))
                        })
                    })
            {
                set_provider_secret_reference(host, reference)?;
            }
            let protected = self
                .protect_live_tv_named_configuration_in_connection(
                    &mut transaction,
                    candidate,
                    Some(original),
                )
                .await?;
            (protected != *original).then_some(protected)
        } else {
            None
        };

        let rewritten = usize::from(plugin_rewrite.is_some())
            + tuner_rewrites.len()
            + usize::from(named_rewrite.is_some());
        if rewritten > 0 {
            if let Some(protected) = plugin_rewrite {
                sqlx::query(
                    "UPDATE plugin_configurations SET configuration = $1, updated_at = now() WHERE lower(plugin_id) = lower($2)",
                )
                .bind(protected)
                .bind("jellyrin-xtream-provider")
                .execute(&mut *transaction)
                .await?;
            }
            for (tuner_id, protected) in tuner_rewrites {
                sqlx::query(
                    "UPDATE live_tv_tuners SET configuration = $1, updated_at = now() WHERE tuner_id = $2",
                )
                .bind(protected)
                .bind(tuner_id)
                .execute(&mut *transaction)
                .await?;
            }
            if let Some(protected) = named_rewrite {
                sqlx::query(
                    "UPDATE named_configurations SET payload = $1, updated_at = now() WHERE key = 'livetv'",
                )
                .bind(protected)
                .execute(&mut *transaction)
                .await?;
            }
        }

        transaction.commit().await?;

        self.validate_provider_secret_readiness().await?;
        Ok(rewritten)
    }

    async fn configuration_credentials_in_connection(
        &self,
        connection: &mut PgConnection,
        configuration: &Value,
    ) -> anyhow::Result<Option<ProviderCredentials>> {
        if let Some(reference) = ProviderSecretReference::from_configuration(configuration) {
            let (_, credentials) = self
                .provider_secret_in_connection(connection, &reference, false)
                .await?;
            return Ok(Some(credentials));
        }
        let Some((username, password)) = provider_credentials_from_configuration(configuration)?
        else {
            return Ok(None);
        };
        match (username, password) {
            (Some(username), Some(password)) => Ok(Some(
                ProviderCredentials::from_protected_parts(username, password)?,
            )),
            _ => anyhow::bail!("provider credentials are incomplete"),
        }
    }

    pub(crate) async fn protect_live_tv_named_configuration_in_connection(
        &self,
        connection: &mut PgConnection,
        mut configuration: Value,
        existing: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let Some(hosts) = configuration
            .get_mut("TunerHosts")
            .and_then(Value::as_array_mut)
        else {
            return Ok(configuration);
        };
        let existing_hosts = existing
            .and_then(|value| value.get("TunerHosts"))
            .and_then(Value::as_array);
        for host in hosts {
            let provider_type = host
                .get("Type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let host_id = host.get("Id").and_then(Value::as_str);
            let existing_host = host_id.and_then(|host_id| {
                existing_hosts?.iter().find(|candidate| {
                    candidate
                        .get("Id")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.eq_ignore_ascii_case(host_id))
                })
            });
            let is_xtream = provider_type.eq_ignore_ascii_case("xtream");
            let is_plugin = provider_type.eq_ignore_ascii_case("plugin")
                || provider_type
                    .split_once(':')
                    .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case("plugin"));
            ensure!(
                !configuration_has_provider_secret_input_field(host) || is_xtream || is_plugin,
                "Live TV core credentials require an explicit xtream or plugin provider type"
            );
            if !is_xtream && !is_plugin {
                continue;
            }
            inherit_provider_secret_reference_for_configuration(
                host,
                existing_host,
                &provider_type,
            )?;
            let has_core_secret = configuration_has_provider_secret_material(host);
            if !has_core_secret {
                continue;
            }
            let secret_namespace =
                provider_secret_namespace_for_configuration(&provider_type, host)?;
            *host = self
                .protect_provider_configuration_in_connection(
                    connection,
                    &secret_namespace,
                    host.clone(),
                )
                .await?;
        }
        Ok(configuration)
    }

    pub(crate) async fn protect_provider_configuration_in_connection(
        &self,
        connection: &mut PgConnection,
        provider_type: &str,
        configuration: Value,
    ) -> anyhow::Result<Value> {
        let existing_reference = ProviderSecretReference::from_configuration(&configuration);
        let submitted = provider_credentials_from_configuration(&configuration)?;
        let has_reference_field = configuration_has_provider_secret_reference_field(&configuration);
        ensure!(
            !has_reference_field || existing_reference.is_some(),
            "provider secret reference is invalid"
        );
        if submitted.is_none() && !has_reference_field {
            return Ok(configuration);
        }
        let provider_type = normalize_provider_type(provider_type)?;

        let reference = match (submitted, existing_reference) {
            (None, None) => return Ok(configuration),
            (None, Some(reference)) => {
                let (current, _) = self
                    .provider_secret_in_connection(connection, &reference, true)
                    .await?;
                current
            }
            (Some((username, password)), existing_reference) => {
                let previous = match existing_reference.as_ref() {
                    Some(reference) => Some(
                        self.provider_secret_in_connection(connection, reference, true)
                            .await?,
                    ),
                    None => None,
                };
                let username = username
                    .or_else(|| {
                        previous
                            .as_ref()
                            .map(|(_, value)| value.protected_username_copy())
                    })
                    .context("provider username is required")?;
                let password = password
                    .or_else(|| {
                        previous
                            .as_ref()
                            .map(|(_, value)| value.protected_password_copy())
                    })
                    .context("provider password is required")?;
                let credentials = ProviderCredentials::from_protected_parts(username, password)?;
                match previous.as_ref() {
                    Some((current_reference, previous_credentials))
                        if previous_credentials == &credentials =>
                    {
                        current_reference.clone()
                    }
                    _ => {
                        self.upsert_provider_secret_in_connection(
                            connection,
                            &provider_type,
                            existing_reference.as_ref().map(|value| value.id.as_str()),
                            &credentials,
                        )
                        .await?
                    }
                }
            }
        };
        ensure!(
            reference.provider_type.eq_ignore_ascii_case(&provider_type),
            "provider secret reference belongs to a different provider"
        );
        redacted_provider_configuration(configuration, &reference)
    }

    pub async fn resolve_provider_configuration(
        &self,
        configuration: &Value,
    ) -> anyhow::Result<Value> {
        let Some(reference) = ProviderSecretReference::from_configuration(configuration) else {
            // Dual-read for pre-vault rows. The startup backfill normally removes this path, but
            // retaining it keeps rolling upgrades usable until the row is rewritten.
            return Ok(configuration.clone());
        };
        let (current_reference, credentials) = self.provider_secret(&reference).await?;
        resolved_provider_configuration(configuration.clone(), &current_reference, &credentials)
    }

    /// Resolves a vault reference directly for just-in-time use without constructing a JSON value
    /// containing plaintext credentials.
    pub async fn provider_credentials_for_configuration(
        &self,
        configuration: &Value,
    ) -> anyhow::Result<Option<(ProviderSecretReference, ProviderCredentials)>> {
        let Some(reference) = ProviderSecretReference::from_configuration(configuration) else {
            return Ok(None);
        };
        self.provider_secret(&reference).await.map(Some)
    }

    pub async fn provider_secret_count(&self) -> anyhow::Result<i64> {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_secrets")
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    /// Deletes vault envelopes that no persisted provider configuration references.
    ///
    /// Serializable isolation closes the scan/delete race with configuration writers. All
    /// envelopes selected as candidates are row-locked, and any invalid nested reference aborts
    /// the whole transaction rather than being interpreted as an orphan.
    pub async fn reconcile_orphaned_provider_secrets(&self) -> anyhow::Result<usize> {
        let mut transaction = self.pool.begin_with(POSTGRES_SERIALIZABLE_BEGIN).await?;
        let envelopes = sqlx::query(
            "SELECT secret_id, provider_type FROM provider_secrets ORDER BY secret_id FOR UPDATE",
        )
        .fetch_all(&mut *transaction)
        .await?;
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
        let mut references = HashSet::new();
        for configuration in configurations {
            collect_provider_secret_reference_identities(&configuration, &mut references)?;
        }

        let mut deleted = 0usize;
        for envelope in envelopes {
            let secret_id = envelope.get::<String, _>("secret_id");
            let provider_type = envelope.get::<String, _>("provider_type");
            if references.contains(&(secret_id.clone(), provider_type.to_ascii_lowercase())) {
                continue;
            }
            deleted += sqlx::query(
                "DELETE FROM provider_secrets WHERE secret_id = $1 AND lower(provider_type) = lower($2)",
            )
            .bind(secret_id)
            .bind(provider_type)
            .execute(&mut *transaction)
            .await?
            .rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(deleted)
    }

    pub async fn validate_provider_secret_readiness(&self) -> anyhow::Result<()> {
        if self.provider_secret_vault.is_none() && self.provider_secret_count().await? > 0 {
            anyhow::bail!(
                "provider secrets exist but no provider secret key was configured; set JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE"
            );
        }
        Ok(())
    }

    /// Fails before a write path invokes an external provider if encryption is unavailable.
    pub fn validate_provider_secret_write_readiness(&self) -> anyhow::Result<()> {
        ensure!(
            self.provider_secret_vault.is_some(),
            "provider credentials cannot be stored without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE"
        );
        Ok(())
    }

    pub async fn rotate_provider_secrets_to_active_key(&self) -> anyhow::Result<usize> {
        let Some(vault) = self.provider_secret_vault.as_ref() else {
            self.validate_provider_secret_readiness().await?;
            return Ok(0);
        };
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT secret_id, provider_type, revision FROM provider_secrets WHERE key_id <> $1 ORDER BY secret_id FOR UPDATE",
        )
        .bind(vault.active_key_id())
        .fetch_all(&mut *transaction)
        .await?;
        let mut rotated = 0usize;
        for row in rows {
            let reference = ProviderSecretReference {
                id: row.get("secret_id"),
                provider_type: row.get("provider_type"),
                revision: row.get("revision"),
            };
            let (_, credentials) = self
                .provider_secret_in_connection(&mut transaction, &reference, false)
                .await?;
            self.upsert_provider_secret_in_connection(
                &mut transaction,
                &reference.provider_type,
                Some(&reference.id),
                &credentials,
            )
            .await?;
            rotated += 1;
        }
        transaction.commit().await?;
        Ok(rotated)
    }

    pub async fn rotate_provider_secret(
        &self,
        reference: &ProviderSecretReference,
    ) -> anyhow::Result<ProviderSecretReference> {
        let mut transaction = self.pool.begin().await?;
        let (_, credentials) = self
            .provider_secret_in_connection(&mut transaction, reference, true)
            .await?;
        let current = self
            .upsert_provider_secret_in_connection(
                &mut transaction,
                &reference.provider_type,
                Some(&reference.id),
                &credentials,
            )
            .await?;
        transaction.commit().await?;
        Ok(current)
    }

    async fn upsert_provider_secret_in_connection(
        &self,
        connection: &mut PgConnection,
        provider_type: &str,
        secret_id: Option<&str>,
        credentials: &ProviderCredentials,
    ) -> anyhow::Result<ProviderSecretReference> {
        let vault = self.provider_secret_vault.as_ref().context(
            "provider credentials cannot be stored without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE",
        )?;
        let secret_id = secret_id
            .map(str::to_owned)
            .unwrap_or_else(new_provider_secret_id);
        let envelope = vault.seal(&secret_id, provider_type, credentials)?;
        let revision = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO provider_secrets (
                secret_id, provider_type, envelope_version, key_id, nonce, ciphertext,
                revision, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, 1, now(), now())
            ON CONFLICT (secret_id) DO UPDATE SET
                envelope_version = EXCLUDED.envelope_version,
                key_id = EXCLUDED.key_id,
                nonce = EXCLUDED.nonce,
                ciphertext = EXCLUDED.ciphertext,
                revision = provider_secrets.revision + 1,
                updated_at = now()
            WHERE lower(provider_secrets.provider_type) = lower(EXCLUDED.provider_type)
            RETURNING revision
            "#,
        )
        .bind(&secret_id)
        .bind(provider_type)
        .bind(i16::try_from(envelope.version)?)
        .bind(&envelope.key_id)
        .bind(envelope.nonce.as_slice())
        .bind(&envelope.ciphertext)
        .fetch_optional(connection)
        .await?
        .context("provider secret id belongs to a different provider")?;
        Ok(ProviderSecretReference {
            id: secret_id,
            provider_type: provider_type.to_owned(),
            revision,
        })
    }

    async fn provider_secret(
        &self,
        reference: &ProviderSecretReference,
    ) -> anyhow::Result<(ProviderSecretReference, ProviderCredentials)> {
        let mut connection = self.pool.acquire().await?;
        self.provider_secret_in_connection(&mut connection, reference, false)
            .await
    }

    async fn provider_secret_in_connection(
        &self,
        connection: &mut PgConnection,
        reference: &ProviderSecretReference,
        for_update: bool,
    ) -> anyhow::Result<(ProviderSecretReference, ProviderCredentials)> {
        let vault = self.provider_secret_vault.as_ref().context(
            "provider credentials cannot be resolved without JELLYRIN_PROVIDER_SECRET_KEY or JELLYRIN_PROVIDER_SECRET_KEY_FILE",
        )?;
        let statement = if for_update {
            r#"
            SELECT provider_type, envelope_version, key_id, nonce, ciphertext, revision
            FROM provider_secrets
            WHERE secret_id = $1 AND lower(provider_type) = lower($2)
            FOR UPDATE
            "#
        } else {
            r#"
            SELECT provider_type, envelope_version, key_id, nonce, ciphertext, revision
            FROM provider_secrets
            WHERE secret_id = $1 AND lower(provider_type) = lower($2)
            "#
        };
        let row = sqlx::query(statement)
            .bind(&reference.id)
            .bind(&reference.provider_type)
            .fetch_optional(connection)
            .await?
            .context("provider secret reference is unavailable")?;
        let nonce = row.get::<Vec<u8>, _>("nonce");
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| anyhow::anyhow!("provider secret envelope is invalid"))?;
        let provider_type = row.get::<String, _>("provider_type");
        let revision = row.get::<i64, _>("revision");
        let envelope = ProviderSecretEnvelope {
            version: u16::try_from(row.get::<i16, _>("envelope_version"))?,
            key_id: row.get("key_id"),
            nonce,
            ciphertext: row.get("ciphertext"),
        };
        let credentials = vault.open(&reference.id, &provider_type, &envelope)?;
        Ok((
            ProviderSecretReference {
                id: reference.id.clone(),
                provider_type,
                revision,
            },
            credentials,
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{PostgresSettings, ProviderSecretVault};

    #[tokio::test]
    async fn postgres_named_plugin_credentials_are_canonical_and_transactional() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        let database = database.with_provider_secret_vault(
            ProviderSecretVault::new("postgres-named-plugin-test", vec![0x73; 32]).unwrap(),
        );
        let plugin_id = Uuid::new_v4();
        let opaque_reference = json!({
            "Namespace": "magstv",
            "Key": format!("tuners/{plugin_id}/credentials")
        });
        let mut transaction = database.pool.begin().await.unwrap();
        let protected = database
            .protect_live_tv_named_configuration_in_connection(
                &mut transaction,
                json!({
                    "TunerHosts": [{
                        "Id": "postgres-named-plugin",
                        "Type": "plugin",
                        "PluginId": plugin_id,
                        "SecretReference": opaque_reference,
                        "Username": "postgres-named-user",
                        "Password": "postgres-named-password"
                    }]
                }),
                None,
            )
            .await
            .unwrap();
        let host = &protected["TunerHosts"][0];
        let reference = ProviderSecretReference::from_configuration(host).unwrap();
        assert_eq!(reference.provider_type, format!("plugin-{plugin_id}"));
        assert_eq!(host["Type"], "plugin");
        assert_eq!(host["SecretReference"], opaque_reference);
        assert!(host.get("Username").is_none());
        assert!(host.get("Password").is_none());
        let (current_reference, credentials) = database
            .provider_secret_in_connection(&mut transaction, &reference, false)
            .await
            .unwrap();
        assert_eq!(current_reference, reference);
        assert_eq!(credentials.username(), "postgres-named-user");
        assert_eq!(credentials.password(), "postgres-named-password");

        let partially_updated = database
            .protect_live_tv_named_configuration_in_connection(
                &mut transaction,
                json!({
                    "TunerHosts": [{
                        "Id": "postgres-named-plugin",
                        "Type": "plugin",
                        "PluginId": plugin_id,
                        "SecretReference": opaque_reference,
                        "PortalUrl": "https://updated.magstv.invalid"
                    }]
                }),
                Some(&protected),
            )
            .await
            .unwrap();
        assert_eq!(
            ProviderSecretReference::from_configuration(&partially_updated["TunerHosts"][0])
                .unwrap(),
            reference
        );

        let malformed = database
            .protect_live_tv_named_configuration_in_connection(
                &mut transaction,
                json!({
                    "TunerHosts": [{
                        "Id": "postgres-named-plugin",
                        "Type": "plugin",
                        "PluginId": plugin_id,
                        "JellyrinProviderSecretRef": {"Id": "", "Provider": "bad", "Revision": 0}
                    }]
                }),
                Some(&protected),
            )
            .await;
        assert!(malformed.is_err());

        transaction.rollback().await.unwrap();
        assert!(
            database
                .provider_credentials_for_configuration(host)
                .await
                .is_err()
        );
        database.close().await;
    }

    #[tokio::test]
    async fn postgres_named_live_tv_rejects_core_secret_fields_without_a_supported_provider_type() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        let database = database.with_provider_secret_vault(
            ProviderSecretVault::new("postgres-fail-closed-test", vec![0x66; 32]).unwrap(),
        );
        let plugin_id = Uuid::new_v4();
        let opaque_reference = json!({
            "Namespace": "magstv",
            "Key": format!("tuners/{plugin_id}/credentials")
        });
        let mut transaction = database.pool.begin().await.unwrap();
        let public_configuration = database
            .protect_live_tv_named_configuration_in_connection(
                &mut transaction,
                json!({
                    "TunerHosts": [{
                        "Id": "postgres-opaque-plugin-tuner",
                        "Type": format!("plugin:{plugin_id}"),
                        "PluginId": plugin_id,
                        "SecretReference": opaque_reference
                    }]
                }),
                None,
            )
            .await
            .unwrap();
        let public_host = &public_configuration["TunerHosts"][0];
        assert_eq!(public_host["SecretReference"], opaque_reference);
        assert!(ProviderSecretReference::from_configuration(public_host).is_none());

        let invalid_hosts = [
            json!({
                "Id": "postgres-missing-type-credentials",
                "Username": "must-not-persist",
                "Password": "must-not-persist"
            }),
            json!({
                "Id": "postgres-missing-type-reference",
                "JellyrinProviderSecretRef": {
                    "Id": "ps_must_not_persist",
                    "Provider": "xtream",
                    "Revision": 1
                }
            }),
            json!({
                "Id": "postgres-unknown-type-credentials",
                "Type": "magstv",
                "UserName": "must-not-persist",
                "Password": "must-not-persist"
            }),
            json!({
                "Id": "postgres-unknown-type-placeholder",
                "Type": "unsupported-provider",
                "Password": "********"
            }),
        ];
        for host in invalid_hosts {
            let result = database
                .protect_live_tv_named_configuration_in_connection(
                    &mut transaction,
                    json!({"TunerHosts": [host]}),
                    Some(&public_configuration),
                )
                .await;
            assert!(result.is_err());
        }

        transaction.rollback().await.unwrap();
        database.close().await;
    }

    #[tokio::test]
    async fn postgres_provider_secret_reconciliation_collects_orphans_and_fails_closed() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        let vault = ProviderSecretVault::new("postgres-reconciliation", vec![0x27; 32]).unwrap();
        let database = database.with_provider_secret_vault(vault.clone());
        let suffix = Uuid::new_v4().simple().to_string();
        let referenced_id = format!("ps_referenced_{suffix}");
        let orphan_id = format!("ps_orphan_{suffix}");
        let retained_id = format!("ps_retained_{suffix}");
        let plugin_id = format!("reconciliation-plugin-{suffix}");
        let malformed_key = format!("reconciliation-malformed-{suffix}");
        let credentials = ProviderCredentials::new("reconcile-user", "reconcile-password").unwrap();

        for secret_id in [&referenced_id, &orphan_id] {
            let envelope = vault.seal(secret_id, "xtream", &credentials).unwrap();
            sqlx::query(
                r#"
                INSERT INTO provider_secrets (
                    secret_id, provider_type, envelope_version, key_id, nonce, ciphertext, revision
                ) VALUES ($1, 'xtream', $2, $3, $4, $5, 1)
                "#,
            )
            .bind(secret_id)
            .bind(envelope.version as i16)
            .bind(&envelope.key_id)
            .bind(envelope.nonce.as_slice())
            .bind(&envelope.ciphertext)
            .execute(&database.pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO plugin_configurations (plugin_id, configuration, updated_at) VALUES ($1, $2, now())",
        )
        .bind(&plugin_id)
        .bind(json!({
            "JellyrinProviderSecretRef": {
                "Id": referenced_id,
                "Provider": "XTREAM",
                "Revision": 77
            }
        }))
        .execute(&database.pool)
        .await
        .unwrap();

        assert!(
            database
                .reconcile_orphaned_provider_secrets()
                .await
                .unwrap()
                >= 1
        );
        let referenced_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_secrets WHERE secret_id = $1)",
        )
        .bind(&referenced_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        let orphan_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_secrets WHERE secret_id = $1)",
        )
        .bind(&orphan_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert!(referenced_exists);
        assert!(!orphan_exists);

        let envelope = vault.seal(&retained_id, "xtream", &credentials).unwrap();
        sqlx::query(
            r#"
            INSERT INTO provider_secrets (
                secret_id, provider_type, envelope_version, key_id, nonce, ciphertext, revision
            ) VALUES ($1, 'xtream', $2, $3, $4, $5, 1)
            "#,
        )
        .bind(&retained_id)
        .bind(envelope.version as i16)
        .bind(&envelope.key_id)
        .bind(envelope.nonce.as_slice())
        .bind(&envelope.ciphertext)
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO named_configurations (key, payload, updated_at) VALUES ($1, $2, now())",
        )
        .bind(&malformed_key)
        .bind(json!({
            "JellyrinProviderSecretRef": {
                "Id": "unknown",
                "Provider": "xtream"
            }
        }))
        .execute(&database.pool)
        .await
        .unwrap();

        assert!(
            database
                .reconcile_orphaned_provider_secrets()
                .await
                .is_err()
        );
        let retained_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_secrets WHERE secret_id = $1)",
        )
        .bind(&retained_id)
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert!(retained_exists);

        sqlx::query("DELETE FROM named_configurations WHERE key = $1")
            .bind(&malformed_key)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM plugin_configurations WHERE plugin_id = $1")
            .bind(&plugin_id)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM provider_secrets WHERE secret_id = ANY($1)")
            .bind(vec![referenced_id, orphan_id, retained_id])
            .execute(&database.pool)
            .await
            .unwrap();
        database.close().await;
    }
}
