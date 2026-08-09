use anyhow::{Context, ensure};
use jellyrin_core::{DeviceToken, ServerState, StartupConfig, User};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ApiKey, DEFAULT_SYNC_PLAY_ACCESS, DatabasePoolRole, PostgresDatabase, hash_password,
    telemetry::DatabaseOperation, verify_password,
};

const ADMIN_MUTATION_LOCK: &str = "jellyrin:postgres:admin-mutations";
// User creation shares the administrator lock so a concurrent placeholder/admin upsert cannot
// race the case-insensitive users-name constraint or invalidate the last-admin invariant.
const FIRST_USER_LOCK: &str = ADMIN_MUTATION_LOCK;

impl PostgresDatabase {
    pub async fn server_state(&self) -> anyhow::Result<ServerState> {
        let row = sqlx::query_as::<_, PostgresServerStateRow>(
            r#"
            SELECT server_id, server_name, startup_wizard_completed, created_at, updated_at
            FROM server_state
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL server state")?;

        match row {
            Some(row) => Ok(row.into()),
            None => self.create_initial_server_state().await,
        }
    }

    pub async fn startup_config(&self) -> anyhow::Result<StartupConfig> {
        let state = self.server_state().await?;
        let row = sqlx::query_as::<_, PostgresStartupConfigRow>(
            r#"
            SELECT ui_culture, metadata_country_code, preferred_metadata_language,
                   dummy_chapter_duration, chapter_image_resolution, enable_remote_access
            FROM startup_config
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL startup configuration")?;

        match row {
            Some(row) => Ok(row.into_config(state.server_name)),
            None => self.create_initial_startup_config(state.server_name).await,
        }
    }

    pub async fn update_startup_config(&self, config: StartupConfig) -> anyhow::Result<()> {
        // Ensure the singleton exists before updating both rows atomically.
        self.server_state().await?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;

        sqlx::query(
            r#"
            UPDATE server_state
            SET server_name = $1, updated_at = $2
            WHERE id = 1
            "#,
        )
        .bind(&config.server_name)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .context("failed to update PostgreSQL server name")?;

        sqlx::query(
            r#"
            INSERT INTO startup_config (
                id, ui_culture, metadata_country_code, preferred_metadata_language,
                dummy_chapter_duration, chapter_image_resolution, enable_remote_access,
                updated_at
            )
            VALUES (1, $1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (id) DO UPDATE SET
                ui_culture = excluded.ui_culture,
                metadata_country_code = excluded.metadata_country_code,
                preferred_metadata_language = excluded.preferred_metadata_language,
                dummy_chapter_duration = excluded.dummy_chapter_duration,
                chapter_image_resolution = excluded.chapter_image_resolution,
                enable_remote_access = excluded.enable_remote_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.ui_culture)
        .bind(config.metadata_country_code)
        .bind(config.preferred_metadata_language)
        .bind(config.dummy_chapter_duration)
        .bind(config.chapter_image_resolution)
        .bind(config.enable_remote_access)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .context("failed to update PostgreSQL startup configuration")?;

        transaction.commit().await?;
        Ok(())
    }

    pub async fn set_remote_access(&self, enabled: bool) -> anyhow::Result<()> {
        // Initialize defaults if needed, then update only this setting so concurrent edits to
        // other startup fields cannot be overwritten by a stale read/modify/write cycle.
        self.startup_config().await?;
        sqlx::query(
            r#"
            UPDATE startup_config
            SET enable_remote_access = $1, updated_at = $2
            WHERE id = 1
            "#,
        )
        .bind(enabled)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await
        .context("failed to update PostgreSQL remote-access setting")?;
        Ok(())
    }

    pub async fn complete_startup_wizard(&self) -> anyhow::Result<()> {
        self.server_state().await?;
        sqlx::query(
            r#"
            UPDATE server_state
            SET startup_wizard_completed = true, updated_at = $1
            WHERE id = 1
            "#,
        )
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await
        .context("failed to complete PostgreSQL startup wizard")?;
        Ok(())
    }

    pub async fn first_user(&self) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access,
                   created_at, updated_at
            FROM users
            ORDER BY created_at, id
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to load first PostgreSQL user")?;

        match row {
            Some(row) => Ok(row.into()),
            None => self.create_placeholder_admin_user().await,
        }
    }

    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        let rows = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access,
                   created_at, updated_at
            FROM users
            ORDER BY lower(name), id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list PostgreSQL users")?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn upsert_admin_user(&self, name: &str, password: &str) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        ensure!(
            !trimmed_name.is_empty(),
            "admin user name must not be empty"
        );
        ensure!(!password.is_empty(), "admin password must not be empty");

        let password_hash = hash_password(password)?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, ADMIN_MUTATION_LOCK).await?;

        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access,
                created_at, updated_at
            )
            VALUES ($1, $2, true, false, $3, $4, $4)
            ON CONFLICT (lower(name)) DO UPDATE SET
                name = excluded.name,
                is_administrator = true,
                is_disabled = false,
                sync_play_access = excluded.sync_play_access,
                updated_at = excluded.updated_at
            RETURNING id, name, is_administrator, is_disabled, sync_play_access,
                      created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(trimmed_name)
        .bind(DEFAULT_SYNC_PLAY_ACCESS)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to upsert PostgreSQL administrator")?;

        upsert_password(&mut transaction, row.id, &password_hash, now).await?;
        transaction.commit().await?;
        Ok(row.into())
    }

    pub async fn update_first_user(&self, name: String, password: &str) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        ensure!(!trimmed_name.is_empty(), "user name must not be empty");
        ensure!(!password.is_empty(), "user password must not be empty");

        // This initializes the placeholder outside the administrator transaction if necessary.
        self.first_user().await?;
        let password_hash = hash_password(password)?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, ADMIN_MUTATION_LOCK).await?;

        let first_user_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM users
            ORDER BY created_at, id
            LIMIT 1
            FOR UPDATE
            "#,
        )
        .fetch_optional(&mut *transaction)
        .await?
        .context("user not found")?;

        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            UPDATE users
            SET name = $1, is_administrator = true, is_disabled = false, updated_at = $2
            WHERE id = $3
            RETURNING id, name, is_administrator, is_disabled, sync_play_access,
                      created_at, updated_at
            "#,
        )
        .bind(trimmed_name)
        .bind(now)
        .bind(first_user_id)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to update first PostgreSQL user")?;

        upsert_password(&mut transaction, row.id, &password_hash, now).await?;
        transaction.commit().await?;
        Ok(row.into())
    }

    pub async fn create_user(&self, name: &str, password: Option<&str>) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        ensure!(!trimmed_name.is_empty(), "user name must not be empty");

        let password_hash = password
            .filter(|password| !password.is_empty())
            .map(hash_password)
            .transpose()?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, FIRST_USER_LOCK).await?;
        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access,
                created_at, updated_at
            )
            VALUES ($1, $2, false, false, $3, $4, $4)
            RETURNING id, name, is_administrator, is_disabled, sync_play_access,
                      created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(trimmed_name)
        .bind(DEFAULT_SYNC_PLAY_ACCESS)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to create PostgreSQL user")?;

        if let Some(password_hash) = password_hash.as_deref() {
            upsert_password(&mut transaction, row.id, password_hash, now).await?;
        }

        transaction.commit().await?;
        Ok(row.into())
    }

    pub async fn delete_user(&self, user_id: Uuid) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, ADMIN_MUTATION_LOCK).await?;
        let user = user_by_id_for_update(&mut transaction, user_id).await?;

        if user.is_administrator && !user.is_disabled {
            ensure_more_than_one_enabled_admin(&mut transaction).await?;
        }

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .context("failed to delete PostgreSQL user")?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn set_user_password(&self, user_id: Uuid, password: &str) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        let password_hash = hash_password(password)?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        upsert_password(&mut transaction, user_id, &password_hash, now).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn reset_user_password(&self, user_id: Uuid) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        sqlx::query("DELETE FROM user_passwords WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .context("failed to reset PostgreSQL user password")?;
        Ok(())
    }

    pub async fn user_has_password(&self, user_id: Uuid) -> anyhow::Result<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_passwords WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to check PostgreSQL user password")
    }

    pub async fn update_user_profile(
        &self,
        user_id: Uuid,
        name: &str,
        is_administrator: bool,
        is_disabled: bool,
        sync_play_access: &str,
    ) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        ensure!(!trimmed_name.is_empty(), "user name must not be empty");

        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, ADMIN_MUTATION_LOCK).await?;
        let existing = user_by_id_for_update(&mut transaction, user_id).await?;
        if existing.is_administrator && !existing.is_disabled && (!is_administrator || is_disabled)
        {
            ensure_more_than_one_enabled_admin(&mut transaction).await?;
        }

        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            UPDATE users
            SET name = $1,
                is_administrator = $2,
                is_disabled = $3,
                sync_play_access = $4,
                updated_at = $5
            WHERE id = $6
            RETURNING id, name, is_administrator, is_disabled, sync_play_access,
                      created_at, updated_at
            "#,
        )
        .bind(trimmed_name)
        .bind(is_administrator)
        .bind(is_disabled)
        .bind(sync_play_access.trim())
        .bind(OffsetDateTime::now_utc())
        .bind(user_id)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to update PostgreSQL user profile")?;

        transaction.commit().await?;
        Ok(row.into())
    }

    pub async fn authenticate_user_by_name(
        &self,
        username: &str,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_name(username).await?;
        ensure!(!user.is_disabled, "user is disabled");

        let password_hash = sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM user_passwords WHERE user_id = $1",
        )
        .bind(user.id)
        .fetch_optional(&self.pool)
        .await?
        .context("password is not configured")?;
        verify_password(password, &password_hash)?;

        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn authenticate_user_by_id(
        &self,
        user_id: Uuid,
        password: &str,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_id(user_id).await?;
        ensure!(!user.is_disabled, "user is disabled");
        self.verify_user_password(user.id, password).await?;
        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn issue_device_token_for_user(
        &self,
        user_id: Uuid,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let user = self.user_by_id(user_id).await?;
        ensure!(!user.is_disabled, "user is disabled");
        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn verify_user_password(&self, user_id: Uuid, password: &str) -> anyhow::Result<()> {
        self.user_by_id(user_id).await?;
        let password_hash = sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM user_passwords WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .context("password is not configured")?;
        verify_password(password, &password_hash)
    }

    pub async fn user_by_token(&self, token: &str) -> anyhow::Result<(User, DeviceToken)> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::AuthUserByToken, DatabasePoolRole::Api);
        let result = self.user_by_token_unobserved(token).await;
        observation.finish_result(&result, |_| 1);
        result
    }

    async fn user_by_token_unobserved(&self, token: &str) -> anyhow::Result<(User, DeviceToken)> {
        let row = sqlx::query_as::<_, PostgresUserDeviceRow>(
            r#"
            SELECT
                users.id, users.name, users.is_administrator, users.is_disabled,
                users.sync_play_access, users.created_at, users.updated_at,
                devices.access_token, devices.device_id, devices.device_name,
                devices.client, devices.version
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE devices.access_token = $1
            "#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .context("failed to resolve PostgreSQL device token")?
        .context("invalid token")?;
        ensure!(!row.is_disabled, "user is disabled");

        self.touch_device_token(token).await?;
        Ok(row.into_parts())
    }

    pub async fn user_by_api_key(&self, api_key: &str) -> anyhow::Result<(User, DeviceToken)> {
        let observation = self
            .telemetry
            .start_operation(DatabaseOperation::AuthUserByApiKey, DatabasePoolRole::Api);
        let result = self.user_by_api_key_unobserved(api_key).await;
        observation.finish_result(&result, |_| 1);
        result
    }

    async fn user_by_api_key_unobserved(
        &self,
        api_key: &str,
    ) -> anyhow::Result<(User, DeviceToken)> {
        let row = sqlx::query_as::<_, PostgresUserApiKeyRow>(
            r#"
            SELECT
                users.id, users.name AS user_name, users.is_administrator,
                users.is_disabled, users.sync_play_access, users.created_at,
                users.updated_at, api_keys.access_token, api_keys.name AS api_key_name
            FROM api_keys
            INNER JOIN users ON users.id = api_keys.user_id
            WHERE api_keys.access_token = $1
            "#,
        )
        .bind(api_key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to resolve PostgreSQL API key")?
        .context("invalid api key")?;
        ensure!(!row.is_disabled, "user is disabled");

        self.touch_api_key(api_key).await?;
        Ok(row.into_parts())
    }

    pub async fn issue_api_key_for_user(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> anyhow::Result<String> {
        let trimmed_name = name.trim();
        ensure!(!trimmed_name.is_empty(), "api key name must not be empty");
        let user = self.user_by_id(user_id).await?;
        ensure!(!user.is_disabled, "user is disabled");

        let access_token = Uuid::new_v4().simple().to_string();
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            INSERT INTO api_keys (access_token, user_id, name, created_at, last_activity_at)
            VALUES ($1, $2, $3, $4, $4)
            "#,
        )
        .bind(&access_token)
        .bind(user_id)
        .bind(trimmed_name)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to issue PostgreSQL API key")?;

        Ok(access_token)
    }

    pub async fn api_keys(&self) -> anyhow::Result<Vec<ApiKey>> {
        let rows = sqlx::query_as::<_, PostgresApiKeyListRow>(
            r#"
            SELECT
                api_keys.access_token,
                api_keys.user_id,
                users.name AS user_name,
                api_keys.name,
                api_keys.created_at,
                api_keys.last_activity_at
            FROM api_keys
            INNER JOIN users ON users.id = api_keys.user_id
            ORDER BY api_keys.created_at DESC, lower(api_keys.name), api_keys.access_token
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list PostgreSQL API keys")?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn revoke_api_key(&self, api_key: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM api_keys WHERE access_token = $1")
            .bind(api_key)
            .execute(&self.pool)
            .await
            .context("failed to revoke PostgreSQL API key")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn revoke_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM devices WHERE access_token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .context("failed to revoke PostgreSQL device token")?;
        Ok(())
    }

    pub async fn user_by_id(&self, user_id: Uuid) -> anyhow::Result<User> {
        sqlx::query_as::<_, PostgresUserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access,
                   created_at, updated_at
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL user")?
        .map(Into::into)
        .context("user not found")
    }

    async fn create_initial_server_state(&self) -> anyhow::Result<ServerState> {
        let now = OffsetDateTime::now_utc();
        let candidate = ServerState {
            server_id: Uuid::new_v4(),
            server_name: "Jellyrin".to_string(),
            startup_wizard_completed: false,
            created_at: now,
            updated_at: now,
        };

        let inserted = sqlx::query_as::<_, PostgresServerStateRow>(
            r#"
            INSERT INTO server_state (
                id, server_id, server_name, startup_wizard_completed, created_at, updated_at
            )
            VALUES (1, $1, $2, $3, $4, $5)
            ON CONFLICT (id) DO NOTHING
            RETURNING server_id, server_name, startup_wizard_completed, created_at, updated_at
            "#,
        )
        .bind(candidate.server_id)
        .bind(&candidate.server_name)
        .bind(candidate.startup_wizard_completed)
        .bind(candidate.created_at)
        .bind(candidate.updated_at)
        .fetch_optional(&self.pool)
        .await
        .context("failed to initialize PostgreSQL server state")?;

        if let Some(row) = inserted {
            return Ok(row.into());
        }

        sqlx::query_as::<_, PostgresServerStateRow>(
            r#"
            SELECT server_id, server_name, startup_wizard_completed, created_at, updated_at
            FROM server_state
            WHERE id = 1
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map(Into::into)
        .context("failed to reload concurrently initialized PostgreSQL server state")
    }

    async fn create_initial_startup_config(
        &self,
        server_name: String,
    ) -> anyhow::Result<StartupConfig> {
        let defaults = PostgresStartupConfigRow {
            ui_culture: "en-US".to_string(),
            metadata_country_code: "US".to_string(),
            preferred_metadata_language: "en".to_string(),
            dummy_chapter_duration: 0,
            chapter_image_resolution: "MatchSource".to_string(),
            enable_remote_access: false,
        };

        sqlx::query(
            r#"
            INSERT INTO startup_config (
                id, ui_culture, metadata_country_code, preferred_metadata_language,
                dummy_chapter_duration, chapter_image_resolution, enable_remote_access,
                updated_at
            )
            VALUES (1, $1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(&defaults.ui_culture)
        .bind(&defaults.metadata_country_code)
        .bind(&defaults.preferred_metadata_language)
        .bind(defaults.dummy_chapter_duration)
        .bind(&defaults.chapter_image_resolution)
        .bind(defaults.enable_remote_access)
        .bind(OffsetDateTime::now_utc())
        .execute(&self.pool)
        .await
        .context("failed to initialize PostgreSQL startup configuration")?;

        let row = sqlx::query_as::<_, PostgresStartupConfigRow>(
            r#"
            SELECT ui_culture, metadata_country_code, preferred_metadata_language,
                   dummy_chapter_duration, chapter_image_resolution, enable_remote_access
            FROM startup_config
            WHERE id = 1
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .context("failed to reload PostgreSQL startup configuration")?;
        Ok(row.into_config(server_name))
    }

    async fn create_placeholder_admin_user(&self) -> anyhow::Result<User> {
        let mut transaction = self.pool.begin().await?;
        lock_transaction(&mut transaction, FIRST_USER_LOCK).await?;

        if let Some(row) = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access,
                   created_at, updated_at
            FROM users
            ORDER BY created_at, id
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *transaction)
        .await?
        {
            transaction.commit().await?;
            return Ok(row.into());
        }

        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            INSERT INTO users (
                id, name, is_administrator, is_disabled, sync_play_access,
                created_at, updated_at
            )
            VALUES ($1, 'admin', true, false, $2, $3, $3)
            RETURNING id, name, is_administrator, is_disabled, sync_play_access,
                      created_at, updated_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(DEFAULT_SYNC_PLAY_ACCESS)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to initialize PostgreSQL administrator")?;

        transaction.commit().await?;
        Ok(row.into())
    }

    async fn user_by_name(&self, username: &str) -> anyhow::Result<User> {
        self.optional_user_by_name(username)
            .await?
            .context("user not found")
    }

    async fn optional_user_by_name(&self, username: &str) -> anyhow::Result<Option<User>> {
        let row = sqlx::query_as::<_, PostgresUserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, sync_play_access,
                   created_at, updated_at
            FROM users
            WHERE lower(name) = lower($1)
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load PostgreSQL user by name")?;
        Ok(row.map(Into::into))
    }

    async fn touch_device_token(&self, token: &str) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            UPDATE devices
            SET last_activity_at = $1
            WHERE access_token = $2
              AND last_activity_at < $1 - INTERVAL '1 minute'
            "#,
        )
        .bind(now)
        .bind(token)
        .execute(&self.pool)
        .await
        .context("failed to touch PostgreSQL device token")?;
        Ok(())
    }

    async fn touch_api_key(&self, api_key: &str) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r#"
            UPDATE api_keys
            SET last_activity_at = $1
            WHERE access_token = $2
              AND last_activity_at < $1 - INTERVAL '1 minute'
            "#,
        )
        .bind(now)
        .bind(api_key)
        .execute(&self.pool)
        .await
        .context("failed to touch PostgreSQL API key")?;
        Ok(())
    }

    async fn issue_device_token(
        &self,
        user: &User,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<DeviceToken> {
        let now = OffsetDateTime::now_utc();
        let access_token = Uuid::new_v4().simple().to_string();
        let mut transaction = self.pool.begin().await?;
        let lock_key = format!("jellyrin:postgres:device:{}:{device_id}", user.id);
        lock_transaction(&mut transaction, &lock_key).await?;

        let is_disabled =
            sqlx::query_scalar::<_, bool>("SELECT is_disabled FROM users WHERE id = $1 FOR SHARE")
                .bind(user.id)
                .fetch_optional(&mut *transaction)
                .await?
                .context("user not found")?;
        ensure!(!is_disabled, "user is disabled");

        // Deleting first deliberately preserves SQLite's replacement semantics and allows
        // dependent session rows to be cleaned through ON DELETE CASCADE.
        sqlx::query("DELETE FROM devices WHERE user_id = $1 AND device_id = $2")
            .bind(user.id)
            .bind(device_id)
            .execute(&mut *transaction)
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                access_token, user_id, device_id, device_name, client, version,
                capabilities, created_at, last_activity_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULL::jsonb, $7, $7)
            "#,
        )
        .bind(&access_token)
        .bind(user.id)
        .bind(device_id)
        .bind(device_name)
        .bind(client)
        .bind(version)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .context("failed to issue PostgreSQL device token")?;

        transaction.commit().await?;
        Ok(DeviceToken {
            access_token,
            user_id: user.id,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            client: client.to_string(),
            version: version.to_string(),
        })
    }
}

async fn lock_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key: &str,
) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut **transaction)
        .await
        .context("failed to acquire PostgreSQL domain lock")?;
    Ok(())
}

async fn upsert_password(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    password_hash: &str,
    updated_at: OffsetDateTime,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
        VALUES ($1, 'argon2id', $2, $3)
        ON CONFLICT (user_id) DO UPDATE SET
            algorithm = excluded.algorithm,
            password_hash = excluded.password_hash,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(user_id)
    .bind(password_hash)
    .bind(updated_at)
    .execute(&mut **transaction)
    .await
    .context("failed to update PostgreSQL user password")?;
    Ok(())
}

async fn user_by_id_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> anyhow::Result<User> {
    sqlx::query_as::<_, PostgresUserRow>(
        r#"
        SELECT id, name, is_administrator, is_disabled, sync_play_access,
               created_at, updated_at
        FROM users
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await?
    .map(Into::into)
    .context("user not found")
}

async fn ensure_more_than_one_enabled_admin(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let admin_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE is_administrator = true AND is_disabled = false",
    )
    .fetch_one(&mut **transaction)
    .await?;
    ensure!(
        admin_count > 1,
        "cannot remove or disable the last enabled administrator"
    );
    Ok(())
}

#[derive(sqlx::FromRow)]
struct PostgresServerStateRow {
    server_id: Uuid,
    server_name: String,
    startup_wizard_completed: bool,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresServerStateRow> for ServerState {
    fn from(row: PostgresServerStateRow) -> Self {
        Self {
            server_id: row.server_id,
            server_name: row.server_name,
            startup_wizard_completed: row.startup_wizard_completed,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresStartupConfigRow {
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
    dummy_chapter_duration: i64,
    chapter_image_resolution: String,
    enable_remote_access: bool,
}

impl PostgresStartupConfigRow {
    fn into_config(self, server_name: String) -> StartupConfig {
        StartupConfig {
            server_name,
            ui_culture: self.ui_culture,
            metadata_country_code: self.metadata_country_code,
            preferred_metadata_language: self.preferred_metadata_language,
            dummy_chapter_duration: self.dummy_chapter_duration,
            chapter_image_resolution: self.chapter_image_resolution,
            enable_remote_access: self.enable_remote_access,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresUserRow {
    id: Uuid,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<PostgresUserRow> for User {
    fn from(row: PostgresUserRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            is_administrator: row.is_administrator,
            is_disabled: row.is_disabled,
            sync_play_access: row.sync_play_access,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct PostgresUserDeviceRow {
    id: Uuid,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    access_token: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
}

impl PostgresUserDeviceRow {
    fn into_parts(self) -> (User, DeviceToken) {
        let user_id = self.id;
        (
            User {
                id: user_id,
                name: self.name,
                is_administrator: self.is_administrator,
                is_disabled: self.is_disabled,
                sync_play_access: self.sync_play_access,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            DeviceToken {
                access_token: self.access_token,
                user_id,
                device_id: self.device_id,
                device_name: self.device_name,
                client: self.client,
                version: self.version,
            },
        )
    }
}

#[derive(sqlx::FromRow)]
struct PostgresUserApiKeyRow {
    id: Uuid,
    user_name: String,
    is_administrator: bool,
    is_disabled: bool,
    sync_play_access: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    access_token: String,
    api_key_name: String,
}

impl PostgresUserApiKeyRow {
    fn into_parts(self) -> (User, DeviceToken) {
        let user_id = self.id;
        (
            User {
                id: user_id,
                name: self.user_name,
                is_administrator: self.is_administrator,
                is_disabled: self.is_disabled,
                sync_play_access: self.sync_play_access,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            DeviceToken {
                access_token: self.access_token,
                user_id,
                device_id: format!("api-key:{}", self.api_key_name),
                device_name: self.api_key_name,
                client: "API Key".to_string(),
                version: "dev".to_string(),
            },
        )
    }
}

#[derive(sqlx::FromRow)]
struct PostgresApiKeyListRow {
    access_token: String,
    user_id: Uuid,
    user_name: String,
    name: String,
    created_at: OffsetDateTime,
    last_activity_at: OffsetDateTime,
}

impl From<PostgresApiKeyListRow> for ApiKey {
    fn from(row: PostgresApiKeyListRow) -> Self {
        Self {
            access_token: row.access_token,
            user_id: row.user_id,
            user_name: row.user_name,
            name: row.name,
            created_at: row.created_at,
            last_activity_at: row.last_activity_at,
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
        administration_pool: PgPool,
        schema: String,
    }

    impl IsolatedPostgres {
        async fn configured() -> Option<Self> {
            let database_url = std::env::var("JELLYRIN_TEST_POSTGRES_URL").ok()?;
            let connect_options = PgConnectOptions::from_str(&database_url)
                .expect("JELLYRIN_TEST_POSTGRES_URL must be a valid PostgreSQL URL");
            let administration_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(connect_options.clone())
                .await
                .expect("failed to connect to the PostgreSQL test database");

            // pg_trgm is global database state. Install it explicitly in public before changing
            // search_path so dropping the isolated schema can never drop the extension.
            let mut extension_lock = administration_pool
                .begin()
                .await
                .expect("failed to start pg_trgm preparation transaction");
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtextextended('jellyrin:schema:migration', 0))",
            )
            .execute(&mut *extension_lock)
            .await
            .expect("failed to lock pg_trgm preparation");
            sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
                .execute(&mut *extension_lock)
                .await
                .expect("failed to prepare pg_trgm for PostgreSQL auth tests");
            extension_lock
                .commit()
                .await
                .expect("failed to commit pg_trgm preparation");

            let schema = format!("jellyrin_auth_test_{}", Uuid::new_v4().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&administration_pool)
                .await
                .expect("failed to create isolated PostgreSQL auth-test schema");

            let search_path = format!("{schema}, public");
            let pool = connect_isolated_pool(
                connect_options.clone(),
                search_path.clone(),
                4,
                "jellyrin-auth-test-api",
            )
            .await;
            let worker_pool =
                connect_isolated_pool(connect_options, search_path, 1, "jellyrin-auth-test-worker")
                    .await;
            let database = PostgresDatabase {
                pool,
                worker_pool,
                provider_secret_vault: None,
                telemetry: std::sync::Arc::new(crate::telemetry::DatabaseTelemetry::default()),
            };
            database
                .migrate()
                .await
                .expect("failed to migrate isolated PostgreSQL auth-test schema");

            Some(Self {
                database,
                administration_pool,
                schema,
            })
        }

        async fn cleanup(self) {
            self.database.close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&self.administration_pool)
            .await
            .expect("failed to remove isolated PostgreSQL auth-test schema");
            self.administration_pool.close().await;
        }
    }

    async fn connect_isolated_pool(
        connect_options: PgConnectOptions,
        search_path: String,
        max_connections: u32,
        application_name: &'static str,
    ) -> PgPool {
        PgPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(move |connection, _metadata| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(
                        r#"
                        SELECT set_config('search_path', $1, false),
                               set_config('TimeZone', 'UTC', false)
                        "#,
                    )
                    .bind(search_path)
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(connect_options.application_name(application_name))
            .await
            .expect("failed to connect an isolated PostgreSQL auth-test pool")
    }

    #[tokio::test]
    async fn postgres_auth_password_and_device_token_contract() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;

        let initial_state = database.server_state().await.unwrap();
        assert_eq!(
            database.server_state().await.unwrap().server_id,
            initial_state.server_id
        );
        assert!(!initial_state.startup_wizard_completed);

        let mut startup = database.startup_config().await.unwrap();
        startup.server_name = "Casa".to_string();
        startup.ui_culture = "es-ES".to_string();
        database.update_startup_config(startup).await.unwrap();
        database.set_remote_access(true).await.unwrap();
        database.complete_startup_wizard().await.unwrap();
        assert_eq!(database.server_state().await.unwrap().server_name, "Casa");
        assert!(
            database
                .server_state()
                .await
                .unwrap()
                .startup_wizard_completed
        );
        assert!(
            database
                .startup_config()
                .await
                .unwrap()
                .enable_remote_access
        );

        let placeholder = database.first_user().await.unwrap();
        assert!(placeholder.is_administrator);
        assert!(!database.user_has_password(placeholder.id).await.unwrap());

        let user = database
            .update_first_user("Root".to_string(), "correct horse")
            .await
            .unwrap();
        assert!(database.user_has_password(user.id).await.unwrap());
        assert!(
            database
                .verify_user_password(user.id, "wrong")
                .await
                .is_err()
        );

        let (_, first_token) = database
            .authenticate_user_by_name(
                "root",
                "correct horse",
                "browser-1",
                "Firefox",
                "Jellyrin Web",
                "test",
            )
            .await
            .unwrap();
        let (resolved_user, resolved_token) = database
            .user_by_token(&first_token.access_token)
            .await
            .unwrap();
        assert_eq!(resolved_user.id, user.id);
        assert_eq!(resolved_token.device_id, "browser-1");

        let (_, replacement_token) = database
            .issue_device_token_for_user(user.id, "browser-1", "Firefox", "Jellyrin Web", "test-2")
            .await
            .unwrap();
        assert_ne!(replacement_token.access_token, first_token.access_token);
        assert!(
            database
                .user_by_token(&first_token.access_token)
                .await
                .is_err()
        );
        assert!(
            database
                .user_by_token(&replacement_token.access_token)
                .await
                .is_ok()
        );

        database
            .revoke_token(&replacement_token.access_token)
            .await
            .unwrap();
        assert!(
            database
                .user_by_token(&replacement_token.access_token)
                .await
                .is_err()
        );

        database.reset_user_password(user.id).await.unwrap();
        assert!(!database.user_has_password(user.id).await.unwrap());
        database
            .set_user_password(user.id, "new secret")
            .await
            .unwrap();
        database
            .authenticate_user_by_id(
                user.id,
                "new secret",
                "browser-2",
                "Firefox",
                "Jellyrin Web",
                "test",
            )
            .await
            .unwrap();

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_auth_preserves_an_enabled_administrator() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;

        let first = database
            .update_first_user("admin-one".to_string(), "secret-one")
            .await
            .unwrap();
        assert!(database.delete_user(first.id).await.is_err());
        assert!(
            database
                .update_user_profile(
                    first.id,
                    "admin-one",
                    false,
                    false,
                    DEFAULT_SYNC_PLAY_ACCESS,
                )
                .await
                .is_err()
        );
        assert!(
            database
                .update_user_profile(first.id, "admin-one", true, true, DEFAULT_SYNC_PLAY_ACCESS,)
                .await
                .is_err()
        );

        let second = database.create_user("admin-two", None).await.unwrap();
        let second = database
            .update_user_profile(
                second.id,
                "admin-two",
                true,
                false,
                DEFAULT_SYNC_PLAY_ACCESS,
            )
            .await
            .unwrap();
        database.delete_user(first.id).await.unwrap();
        assert_eq!(database.user_by_id(second.id).await.unwrap().id, second.id);
        assert!(database.user_by_id(first.id).await.is_err());

        test.cleanup().await;
    }

    #[tokio::test]
    async fn postgres_auth_api_key_contract_and_disabled_user_rejection() {
        let Some(test) = IsolatedPostgres::configured().await else {
            return;
        };
        let database = &test.database;

        let admin = database
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let user = database.create_user("service", None).await.unwrap();
        let api_key = database
            .issue_api_key_for_user(user.id, "indexer")
            .await
            .unwrap();

        let (resolved_user, token) = database.user_by_api_key(&api_key).await.unwrap();
        assert_eq!(resolved_user.id, user.id);
        assert_eq!(token.client, "API Key");
        assert_eq!(token.device_id, "api-key:indexer");
        let keys = database.api_keys().await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].access_token, api_key);
        assert_eq!(keys[0].user_name, "service");

        database
            .update_user_profile(user.id, "service", false, true, DEFAULT_SYNC_PLAY_ACCESS)
            .await
            .unwrap();
        assert!(database.user_by_api_key(&api_key).await.is_err());
        assert!(
            database
                .issue_api_key_for_user(user.id, "disabled")
                .await
                .is_err()
        );

        assert!(database.revoke_api_key(&api_key).await.unwrap());
        assert!(!database.revoke_api_key(&api_key).await.unwrap());
        assert!(database.user_by_api_key(&api_key).await.is_err());
        assert!(
            database
                .user_by_id(admin.id)
                .await
                .unwrap()
                .is_administrator
        );

        test.cleanup().await;
    }
}
