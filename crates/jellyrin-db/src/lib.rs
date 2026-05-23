use std::path::{Path, PathBuf};

use anyhow::Context;
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use jellyrin_core::{DeviceToken, MediaItem, ServerState, StartupConfig, User, VirtualFolder};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .with_context(|| format!("failed to connect SQLite database at {database_url}"))?;

        MIGRATOR
            .run(&pool)
            .await
            .context("failed to run migrations")?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn server_state(&self) -> anyhow::Result<ServerState> {
        let row = sqlx::query_as::<_, ServerStateRow>(
            r#"
            SELECT server_id, server_name, startup_wizard_completed, created_at, updated_at
            FROM server_state
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => self.create_initial_server_state().await,
        }
    }

    pub async fn startup_config(&self) -> anyhow::Result<StartupConfig> {
        let state = self.server_state().await?;
        let row = sqlx::query_as::<_, StartupConfigRow>(
            r#"
            SELECT ui_culture, metadata_country_code, preferred_metadata_language, enable_remote_access
            FROM startup_config
            WHERE id = 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(StartupConfig {
                server_name: state.server_name,
                ui_culture: row.ui_culture,
                metadata_country_code: row.metadata_country_code,
                preferred_metadata_language: row.preferred_metadata_language,
                enable_remote_access: row.enable_remote_access,
            }),
            None => self.create_initial_startup_config(state.server_name).await,
        }
    }

    pub async fn update_startup_config(&self, config: StartupConfig) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE server_state
            SET server_name = ?1, updated_at = ?2
            WHERE id = 1
            "#,
        )
        .bind(&config.server_name)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO startup_config (
                id, ui_culture, metadata_country_code, preferred_metadata_language, enable_remote_access, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                ui_culture = excluded.ui_culture,
                metadata_country_code = excluded.metadata_country_code,
                preferred_metadata_language = excluded.preferred_metadata_language,
                enable_remote_access = excluded.enable_remote_access,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(config.ui_culture)
        .bind(config.metadata_country_code)
        .bind(config.preferred_metadata_language)
        .bind(config.enable_remote_access)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn set_remote_access(&self, enabled: bool) -> anyhow::Result<()> {
        let mut config = self.startup_config().await?;
        config.enable_remote_access = enabled;
        self.update_startup_config(config).await
    }

    pub async fn complete_startup_wizard(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE server_state
            SET startup_wizard_completed = 1, updated_at = ?1
            WHERE id = 1
            "#,
        )
        .bind(format_time(OffsetDateTime::now_utc())?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn first_user(&self) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            ORDER BY created_at
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => row.try_into(),
            None => self.create_placeholder_admin_user().await,
        }
    }

    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn upsert_admin_user(&self, name: &str, password: &str) -> anyhow::Result<User> {
        let trimmed_name = name.trim();
        anyhow::ensure!(
            !trimmed_name.is_empty(),
            "admin user name must not be empty"
        );
        anyhow::ensure!(!password.is_empty(), "admin password must not be empty");

        let now = format_time(OffsetDateTime::now_utc())?;
        let existing = self.optional_user_by_name(trimmed_name).await?;
        let id = existing.as_ref().map_or_else(Uuid::new_v4, |user| user.id);

        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, created_at, updated_at)
            VALUES (?1, ?2, 1, 0, ?3, ?3)
            ON CONFLICT(name) DO UPDATE SET
                is_administrator = 1,
                is_disabled = 0,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_name)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let password_hash = hash_password(password)?;
        sqlx::query(
            r#"
            INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
            VALUES (?1, 'argon2id', ?2, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                algorithm = excluded.algorithm,
                password_hash = excluded.password_hash,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(password_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.user_by_id(id).await
    }

    pub async fn update_first_user(&self, name: String, password: &str) -> anyhow::Result<User> {
        let user = self.first_user().await?;
        let now = format_time(OffsetDateTime::now_utc())?;
        sqlx::query(
            r#"
            UPDATE users
            SET name = ?1, is_administrator = 1, is_disabled = 0, updated_at = ?2
            WHERE id = ?3
            "#,
        )
        .bind(name.trim())
        .bind(&now)
        .bind(user.id.to_string())
        .execute(&self.pool)
        .await?;

        let password_hash = hash_password(password)?;
        sqlx::query(
            r#"
            INSERT INTO user_passwords (user_id, algorithm, password_hash, updated_at)
            VALUES (?1, 'argon2id', ?2, ?3)
            ON CONFLICT(user_id) DO UPDATE SET
                algorithm = excluded.algorithm,
                password_hash = excluded.password_hash,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(user.id.to_string())
        .bind(password_hash)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.user_by_id(user.id).await
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
        anyhow::ensure!(!user.is_disabled, "user is disabled");

        let password_row = sqlx::query_as::<_, PasswordRow>(
            r#"
            SELECT password_hash
            FROM user_passwords
            WHERE user_id = ?1
            "#,
        )
        .bind(user.id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("password is not configured")?;

        verify_password(password, &password_row.password_hash)?;
        let token = self
            .issue_device_token(&user, device_id, device_name, client, version)
            .await?;
        Ok((user, token))
    }

    pub async fn user_by_token(&self, token: &str) -> anyhow::Result<(User, DeviceToken)> {
        let token_row = sqlx::query_as::<_, DeviceTokenRow>(
            r#"
            SELECT access_token, user_id, device_id, device_name, client, version
            FROM devices
            WHERE access_token = ?1
            "#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?
        .context("invalid token")?;

        let token: DeviceToken = token_row.try_into()?;
        let user = self.user_by_id(token.user_id).await?;
        Ok((user, token))
    }

    pub async fn user_by_api_key(&self, api_key: &str) -> anyhow::Result<(User, DeviceToken)> {
        let row = sqlx::query_as::<_, ApiKeyRow>(
            r#"
            SELECT access_token, user_id, name
            FROM api_keys
            WHERE access_token = ?1
            "#,
        )
        .bind(api_key)
        .fetch_optional(&self.pool)
        .await?
        .context("invalid api key")?;

        sqlx::query("UPDATE api_keys SET last_activity_at = ?1 WHERE access_token = ?2")
            .bind(format_time(OffsetDateTime::now_utc())?)
            .bind(api_key)
            .execute(&self.pool)
            .await?;

        let user = self.user_by_id(Uuid::parse_str(&row.user_id)?).await?;
        Ok((
            user,
            DeviceToken {
                access_token: row.access_token,
                user_id: Uuid::parse_str(&row.user_id)?,
                device_id: format!("api-key:{}", row.name),
                device_name: row.name,
                client: "API Key".to_string(),
                version: "dev".to_string(),
            },
        ))
    }

    pub async fn issue_api_key_for_user(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> anyhow::Result<String> {
        let trimmed_name = name.trim();
        anyhow::ensure!(!trimmed_name.is_empty(), "api key name must not be empty");

        let now = format_time(OffsetDateTime::now_utc())?;
        let access_token = Uuid::new_v4().simple().to_string();
        sqlx::query(
            r#"
            INSERT INTO api_keys (access_token, user_id, name, created_at, last_activity_at)
            VALUES (?1, ?2, ?3, ?4, ?4)
            "#,
        )
        .bind(&access_token)
        .bind(user_id.to_string())
        .bind(trimmed_name)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(access_token)
    }

    pub async fn revoke_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM devices WHERE access_token = ?1")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn virtual_folders(&self) -> anyhow::Result<Vec<VirtualFolder>> {
        let rows = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            ORDER BY name COLLATE NOCASE
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

        let now = format_time(OffsetDateTime::now_utc())?;
        let existing = self.virtual_folder_by_name(trimmed_name).await?;
        let id = existing
            .as_ref()
            .map_or_else(Uuid::new_v4, |folder| folder.id);
        let locations_json = serde_json::to_string(&normalized_locations(locations))?;

        sqlx::query(
            r#"
            INSERT INTO virtual_folders (
                id, name, collection_type, locations_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(name) DO UPDATE SET
                collection_type = excluded.collection_type,
                locations_json = excluded.locations_json,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(id.to_string())
        .bind(trimmed_name)
        .bind(
            collection_type
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        )
        .bind(locations_json)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.virtual_folder_by_name(trimmed_name)
            .await?
            .context("virtual folder was not persisted")
    }

    pub async fn add_virtual_folder_path(&self, name: &str, path: &str) -> anyhow::Result<()> {
        let mut folder = self
            .virtual_folder_by_name(name)
            .await?
            .context("virtual folder not found")?;
        let trimmed_path = path.trim();
        anyhow::ensure!(
            !trimmed_path.is_empty(),
            "virtual folder path must not be empty"
        );

        if !folder
            .locations
            .iter()
            .any(|location| location == trimmed_path)
        {
            folder.locations.push(trimmed_path.to_string());
            self.upsert_virtual_folder(
                &folder.name,
                folder.collection_type.as_deref(),
                folder.locations,
            )
            .await?;
        }

        Ok(())
    }

    pub async fn media_items(&self) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type, created_at, updated_at
            FROM media_items
            ORDER BY name COLLATE NOCASE
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn latest_media_items(&self, limit: i64) -> anyhow::Result<Vec<MediaItem>> {
        let rows = sqlx::query_as::<_, MediaItemRow>(
            r#"
            SELECT id, virtual_folder_id, name, path, media_type, collection_type, created_at, updated_at
            FROM media_items
            ORDER BY created_at DESC, name COLLATE NOCASE
            LIMIT ?1
            "#,
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn scan_virtual_folder_items(&self, folder_id: Uuid) -> anyhow::Result<usize> {
        let folder = self
            .virtual_folder_by_id(folder_id)
            .await?
            .context("virtual folder not found")?;
        let mut scanned = 0usize;

        for location in &folder.locations {
            for path in collect_media_files(Path::new(location)).await? {
                let Some(name) = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
                else {
                    continue;
                };
                let Some(media_type) = media_type_for_path(&path) else {
                    continue;
                };

                self.upsert_media_item(&folder, &name, &path, media_type)
                    .await?;
                scanned += 1;
            }
        }

        Ok(scanned)
    }

    async fn upsert_media_item(
        &self,
        folder: &VirtualFolder,
        name: &str,
        path: &Path,
        media_type: &str,
    ) -> anyhow::Result<()> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let path = path.to_string_lossy().to_string();
        let existing_id =
            sqlx::query_as::<_, MediaItemIdRow>("SELECT id FROM media_items WHERE path = ?1")
                .bind(&path)
                .fetch_optional(&self.pool)
                .await?
                .map_or_else(|| Uuid::new_v4().to_string(), |row| row.id);

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id, virtual_folder_id, name, path, media_type, collection_type, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(path) DO UPDATE SET
                virtual_folder_id = excluded.virtual_folder_id,
                name = excluded.name,
                media_type = excluded.media_type,
                collection_type = excluded.collection_type,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(existing_id)
        .bind(folder.id.to_string())
        .bind(name)
        .bind(path)
        .bind(media_type)
        .bind(&folder.collection_type)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn create_initial_server_state(&self) -> anyhow::Result<ServerState> {
        let now = OffsetDateTime::now_utc();
        let state = ServerState {
            server_id: Uuid::new_v4(),
            server_name: "Jellyrin".to_string(),
            startup_wizard_completed: false,
            created_at: now,
            updated_at: now,
        };

        sqlx::query(
            r#"
            INSERT INTO server_state (
                id, server_id, server_name, startup_wizard_completed, created_at, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(state.server_id.to_string())
        .bind(&state.server_name)
        .bind(state.startup_wizard_completed)
        .bind(format_time(state.created_at)?)
        .bind(format_time(state.updated_at)?)
        .execute(&self.pool)
        .await?;

        Ok(state)
    }

    async fn create_initial_startup_config(
        &self,
        server_name: String,
    ) -> anyhow::Result<StartupConfig> {
        let config = StartupConfig {
            server_name,
            ui_culture: "en-US".to_string(),
            metadata_country_code: "US".to_string(),
            preferred_metadata_language: "en".to_string(),
            enable_remote_access: false,
        };
        self.update_startup_config(config.clone()).await?;
        Ok(config)
    }

    async fn create_placeholder_admin_user(&self) -> anyhow::Result<User> {
        let now = OffsetDateTime::now_utc();
        let user = User {
            id: Uuid::new_v4(),
            name: "admin".to_string(),
            is_administrator: true,
            is_disabled: false,
            created_at: now,
            updated_at: now,
        };

        sqlx::query(
            r#"
            INSERT INTO users (id, name, is_administrator, is_disabled, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(user.id.to_string())
        .bind(&user.name)
        .bind(user.is_administrator)
        .bind(user.is_disabled)
        .bind(format_time(user.created_at)?)
        .bind(format_time(user.updated_at)?)
        .execute(&self.pool)
        .await?;

        Ok(user)
    }

    async fn user_by_name(&self, username: &str) -> anyhow::Result<User> {
        self.optional_user_by_name(username)
            .await?
            .context("user not found")
    }

    async fn optional_user_by_name(&self, username: &str) -> anyhow::Result<Option<User>> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            WHERE name = ?1
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    async fn user_by_id(&self, user_id: Uuid) -> anyhow::Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT id, name, is_administrator, is_disabled, created_at, updated_at
            FROM users
            WHERE id = ?1
            "#,
        )
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .context("user not found")?;

        row.try_into()
    }

    async fn issue_device_token(
        &self,
        user: &User,
        device_id: &str,
        device_name: &str,
        client: &str,
        version: &str,
    ) -> anyhow::Result<DeviceToken> {
        let now = format_time(OffsetDateTime::now_utc())?;
        let access_token = Uuid::new_v4().simple().to_string();
        sqlx::query("DELETE FROM devices WHERE user_id = ?1 AND device_id = ?2")
            .bind(user.id.to_string())
            .bind(device_id)
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                access_token, user_id, device_id, device_name, client, version, created_at, last_activity_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            "#,
        )
        .bind(&access_token)
        .bind(user.id.to_string())
        .bind(device_id)
        .bind(device_name)
        .bind(client)
        .bind(version)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(DeviceToken {
            access_token,
            user_id: user.id,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            client: client.to_string(),
            version: version.to_string(),
        })
    }

    async fn virtual_folder_by_name(&self, name: &str) -> anyhow::Result<Option<VirtualFolder>> {
        let row = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            WHERE name = ?1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    async fn virtual_folder_by_id(&self, id: Uuid) -> anyhow::Result<Option<VirtualFolder>> {
        let row = sqlx::query_as::<_, VirtualFolderRow>(
            r#"
            SELECT id, name, collection_type, locations_json, created_at, updated_at
            FROM virtual_folders
            WHERE id = ?1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }
}

#[derive(sqlx::FromRow)]
struct StartupConfigRow {
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
    enable_remote_access: bool,
}

#[derive(sqlx::FromRow)]
struct ServerStateRow {
    server_id: String,
    server_name: String,
    startup_wizard_completed: bool,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    name: String,
    is_administrator: bool,
    is_disabled: bool,
    created_at: String,
    updated_at: String,
}

impl TryFrom<UserRow> for User {
    type Error = anyhow::Error;

    fn try_from(row: UserRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid user id in database")?,
            name: row.name,
            is_administrator: row.is_administrator,
            is_disabled: row.is_disabled,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct PasswordRow {
    password_hash: String,
}

#[derive(sqlx::FromRow)]
struct DeviceTokenRow {
    access_token: String,
    user_id: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
}

#[derive(sqlx::FromRow)]
struct ApiKeyRow {
    access_token: String,
    user_id: String,
    name: String,
}

#[derive(sqlx::FromRow)]
struct VirtualFolderRow {
    id: String,
    name: String,
    collection_type: Option<String>,
    locations_json: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemRow {
    id: String,
    virtual_folder_id: String,
    name: String,
    path: String,
    media_type: String,
    collection_type: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MediaItemIdRow {
    id: String,
}

impl TryFrom<VirtualFolderRow> for VirtualFolder {
    type Error = anyhow::Error;

    fn try_from(row: VirtualFolderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid virtual folder id in database")?,
            name: row.name,
            collection_type: row.collection_type,
            locations: serde_json::from_str(&row.locations_json)
                .context("invalid virtual folder locations in database")?,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<MediaItemRow> for MediaItem {
    type Error = anyhow::Error;

    fn try_from(row: MediaItemRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::parse_str(&row.id).context("invalid media item id in database")?,
            virtual_folder_id: Uuid::parse_str(&row.virtual_folder_id)
                .context("invalid media item virtual folder id in database")?,
            name: row.name,
            path: row.path,
            media_type: row.media_type,
            collection_type: row.collection_type,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

impl TryFrom<DeviceTokenRow> for DeviceToken {
    type Error = anyhow::Error;

    fn try_from(row: DeviceTokenRow) -> Result<Self, Self::Error> {
        Ok(Self {
            access_token: row.access_token,
            user_id: Uuid::parse_str(&row.user_id).context("invalid token user id in database")?,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
        })
    }
}

impl TryFrom<ServerStateRow> for ServerState {
    type Error = anyhow::Error;

    fn try_from(row: ServerStateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            server_id: Uuid::parse_str(&row.server_id).context("invalid server_id in database")?,
            server_name: row.server_name,
            startup_wizard_completed: row.startup_wizard_completed,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        })
    }
}

fn format_time(value: OffsetDateTime) -> anyhow::Result<String> {
    value.format(&Rfc3339).context("failed to format timestamp")
}

fn parse_time(value: &str) -> anyhow::Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).context("failed to parse timestamp")
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(anyhow::Error::msg)
}

fn verify_password(password: &str, password_hash: &str) -> anyhow::Result<()> {
    let parsed_hash = PasswordHash::new(password_hash).map_err(anyhow::Error::msg)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| anyhow::anyhow!("invalid username or password"))
}

async fn collect_media_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut media_files = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(path) = pending.pop() {
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };

        if metadata.is_file() {
            if media_type_for_path(&path).is_some() {
                media_files.push(path);
            }
            continue;
        }

        if !metadata.is_dir() {
            continue;
        }

        let Ok(mut entries) = tokio::fs::read_dir(&path).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            pending.push(entry.path());
        }
    }

    media_files.sort();
    Ok(media_files)
}

fn media_type_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "mkv" | "mp4" | "avi" | "mov" | "wmv" | "m4v" | "webm" => Some("Video"),
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav" => Some("Audio"),
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" => Some("Photo"),
        "epub" | "pdf" | "cbz" | "cbr" => Some("Book"),
        _ => None,
    }
}

fn normalized_locations(locations: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for location in locations {
        let location = location.trim();
        if !location.is_empty() && !normalized.iter().any(|value| value == location) {
            normalized.push(location.to_string());
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::Database;

    #[tokio::test]
    async fn creates_initial_server_state_once() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let first = db.server_state().await.unwrap();
        let second = db.server_state().await.unwrap();

        assert_eq!(first.server_id, second.server_id);
        assert_eq!(first.server_name, "Jellyrin");
        assert!(!first.startup_wizard_completed);
    }

    #[tokio::test]
    async fn config_user_and_token_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let mut config = db.startup_config().await.unwrap();
        config.server_name = "Casa".to_string();
        config.ui_culture = "es-ES".to_string();
        db.update_startup_config(config).await.unwrap();

        let first = db.first_user().await.unwrap();
        assert_eq!(first.name, "admin");

        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();
        let (logged_in, token) = db
            .authenticate_user_by_name(
                "root",
                "secret",
                "device-1",
                "Browser",
                "Jellyfin Web",
                "dev",
            )
            .await
            .unwrap();

        assert_eq!(user.id, logged_in.id);
        assert!(!token.access_token.is_empty());

        let (token_user, _) = db.user_by_token(&token.access_token).await.unwrap();
        assert_eq!(token_user.id, user.id);

        db.revoke_token(&token.access_token).await.unwrap();
        assert!(db.user_by_token(&token.access_token).await.is_err());
    }

    #[tokio::test]
    async fn api_key_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("root".to_string(), "secret")
            .await
            .unwrap();

        let api_key = db.issue_api_key_for_user(user.id, "qa").await.unwrap();
        let (api_user, token) = db.user_by_api_key(&api_key).await.unwrap();

        assert_eq!(api_user.id, user.id);
        assert_eq!(token.access_token, api_key);
        assert_eq!(token.client, "API Key");
    }

    #[tokio::test]
    async fn virtual_folders_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec!["/media/movies".to_string(), "/media/movies".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(folder.name, "Movies");
        assert_eq!(folder.collection_type.as_deref(), Some("movies"));
        assert_eq!(folder.locations, vec!["/media/movies"]);

        db.add_virtual_folder_path("Movies", "/media/more-movies")
            .await
            .unwrap();
        let folders = db.virtual_folders().await.unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(
            folders[0].locations,
            vec!["/media/movies", "/media/more-movies"]
        );
    }

    #[tokio::test]
    async fn scans_media_items_from_virtual_folder_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Movies").join("Example Movie.mkv");
        tokio::fs::create_dir_all(movie.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&movie, b"fake video").await.unwrap();
        tokio::fs::write(tmp.path().join("ignore.txt"), b"not media")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();

        let scanned = db.scan_virtual_folder_items(folder.id).await.unwrap();
        assert_eq!(scanned, 1);

        let items = db.media_items().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Example Movie");
        assert_eq!(items[0].path, movie.to_string_lossy());
        assert_eq!(items[0].media_type, "Video");
        assert_eq!(items[0].collection_type.as_deref(), Some("movies"));
    }

    #[tokio::test]
    async fn upsert_admin_user_creates_separate_login_account() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.update_first_user("admin".to_string(), "admin-secret")
            .await
            .unwrap();

        let user = db
            .upsert_admin_user("jellyrin-e2e-admin", "e2e-secret")
            .await
            .unwrap();
        assert_eq!(user.name, "jellyrin-e2e-admin");
        assert!(user.is_administrator);
        assert!(!user.is_disabled);

        let (auth_user, _) = db
            .authenticate_user_by_name(
                "jellyrin-e2e-admin",
                "e2e-secret",
                "e2e-device",
                "E2E Device",
                "Jellyrin E2E",
                "dev",
            )
            .await
            .unwrap();
        assert_eq!(auth_user.id, user.id);

        let users = db.users().await.unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|user| user.name == "admin"));
        assert!(users.iter().any(|user| user.name == "jellyrin-e2e-admin"));
    }
}
