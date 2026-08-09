use anyhow::{Context, ensure};
use jellyrin_core::DeviceToken;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{DeviceSession, PostgresDatabase};

impl PostgresDatabase {
    pub async fn revoke_user_tokens_except(
        &self,
        user_id: Uuid,
        keep_token: &str,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM active_playback_sessions
            WHERE session_id IN (
                SELECT access_token FROM devices
                WHERE user_id = $1 AND access_token <> $2
            )
            "#,
        )
        .bind(user_id)
        .bind(keep_token)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM devices WHERE user_id = $1 AND access_token <> $2")
            .bind(user_id)
            .bind(keep_token)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn revoke_device(&self, id: &str) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM active_playback_sessions
            WHERE session_id IN (
                SELECT access_token FROM devices
                WHERE access_token = $1 OR device_id = $1
            )
            "#,
        )
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM devices WHERE access_token = $1 OR device_id = $1")
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn device_sessions(&self) -> anyhow::Result<Vec<DeviceSession>> {
        self.postgres_device_sessions(None).await
    }

    pub async fn device_sessions_for_user(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<DeviceSession>> {
        self.postgres_device_sessions(Some(user_id)).await
    }

    pub async fn device_session_by_id(&self, id: &str) -> anyhow::Result<Option<DeviceSession>> {
        let row = sqlx::query_as::<_, PostgresDeviceSessionRow>(
            r#"
            SELECT devices.access_token, devices.user_id, users.name AS user_name,
                   devices.device_id, devices.device_name, devices.client, devices.version,
                   devices.last_activity_at, devices.capabilities
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE users.is_disabled = false
              AND (devices.access_token = $1 OR devices.device_id = $1)
            ORDER BY devices.last_activity_at DESC, devices.access_token
            LIMIT 1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn update_device_name(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        ensure!(!name.is_empty(), "device name must not be empty");
        sqlx::query(
            r#"
            UPDATE devices
            SET device_name = $1, last_activity_at = $2
            WHERE access_token = $3 OR device_id = $3
            "#,
        )
        .bind(name)
        .bind(OffsetDateTime::now_utc())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_device_capabilities(
        &self,
        access_token: &str,
        capabilities: Value,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE devices
            SET capabilities = $1, last_activity_at = $2
            WHERE access_token = $3
            "#,
        )
        .bind(capabilities)
        .bind(OffsetDateTime::now_utc())
        .bind(access_token)
        .execute(&self.pool)
        .await?;
        ensure!(result.rows_affected() > 0, "device not found");
        Ok(())
    }

    pub async fn ensure_device_session(&self, token: &DeviceToken) -> anyhow::Result<()> {
        ensure!(
            !token.access_token.trim().is_empty(),
            "device access token must not be empty"
        );
        ensure!(
            !token.device_id.trim().is_empty(),
            "device id must not be empty"
        );
        self.user_by_id(token.user_id).await?;

        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await?;
        // Use the same domain lock as authentication token issuance. This makes replacing a
        // (user, device) token deterministic even when reconnect and login race on two nodes.
        let lock_key = format!(
            "jellyrin:postgres:device:{}:{}",
            token.user_id, token.device_id
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&lock_key)
            .execute(&mut *transaction)
            .await
            .context("failed to lock PostgreSQL device session")?;
        sqlx::query(
            r#"
            DELETE FROM devices
            WHERE user_id = $1 AND device_id = $2 AND access_token <> $3
            "#,
        )
        .bind(token.user_id)
        .bind(&token.device_id)
        .bind(&token.access_token)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO devices (
                access_token, user_id, device_id, device_name, client, version,
                created_at, last_activity_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
            ON CONFLICT (access_token) DO UPDATE SET
                user_id = excluded.user_id,
                device_id = excluded.device_id,
                device_name = excluded.device_name,
                client = excluded.client,
                version = excluded.version,
                last_activity_at = excluded.last_activity_at
            "#,
        )
        .bind(&token.access_token)
        .bind(token.user_id)
        .bind(&token.device_id)
        .bind(&token.device_name)
        .bind(&token.client)
        .bind(&token.version)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .context("failed to ensure PostgreSQL device session")?;
        transaction.commit().await?;
        Ok(())
    }

    async fn postgres_device_sessions(
        &self,
        visible_to_user: Option<Uuid>,
    ) -> anyhow::Result<Vec<DeviceSession>> {
        let rows = sqlx::query_as::<_, PostgresDeviceSessionRow>(
            r#"
            SELECT devices.access_token, devices.user_id, users.name AS user_name,
                   devices.device_id, devices.device_name, devices.client, devices.version,
                   devices.last_activity_at, devices.capabilities
            FROM devices
            INNER JOIN users ON users.id = devices.user_id
            WHERE users.is_disabled = false
              AND (
                    $1::uuid IS NULL
                    OR devices.user_id = $1
                    OR EXISTS (
                        SELECT 1 FROM active_session_users
                        WHERE active_session_users.session_id = devices.access_token
                          AND active_session_users.user_id = $1
                    )
              )
            ORDER BY devices.last_activity_at DESC, devices.access_token
            "#,
        )
        .bind(visible_to_user)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}

#[derive(sqlx::FromRow)]
struct PostgresDeviceSessionRow {
    access_token: String,
    user_id: Uuid,
    user_name: String,
    device_id: String,
    device_name: String,
    client: String,
    version: String,
    last_activity_at: OffsetDateTime,
    capabilities: Option<Value>,
}

impl From<PostgresDeviceSessionRow> for DeviceSession {
    fn from(row: PostgresDeviceSessionRow) -> Self {
        Self {
            access_token: row.access_token,
            user_id: row.user_id,
            user_name: row.user_name,
            device_id: row.device_id,
            device_name: row.device_name,
            client: row.client,
            version: row.version,
            last_activity_at: row.last_activity_at,
            capabilities: row.capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use jellyrin_core::DeviceToken;
    use serde_json::json;
    use uuid::Uuid;

    use super::{super::PostgresSettings, PostgresDatabase};

    #[tokio::test]
    async fn postgres_device_session_contract_round_trips_when_configured() {
        let Ok(database_url) = std::env::var("JELLYRIN_TEST_POSTGRES_URL") else {
            return;
        };
        let database =
            PostgresDatabase::connect_with_settings(&PostgresSettings::new(database_url).unwrap())
                .await
                .unwrap();
        database.migrate().await.unwrap();
        let user = database
            .create_user(&format!("device-test-{}", Uuid::new_v4()), None)
            .await
            .unwrap();
        let first = DeviceToken {
            access_token: Uuid::new_v4().simple().to_string(),
            user_id: user.id,
            device_id: format!("device-{}", Uuid::new_v4()),
            device_name: "First".to_string(),
            client: "Tests".to_string(),
            version: "1".to_string(),
        };
        database.ensure_device_session(&first).await.unwrap();
        database
            .update_device_capabilities(&first.access_token, json!({"SupportsMediaControl": true}))
            .await
            .unwrap();
        database
            .update_device_name(&first.device_id, "Renamed")
            .await
            .unwrap();
        let loaded = database
            .device_session_by_id(&first.access_token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.device_name, "Renamed");
        assert_eq!(
            loaded.capabilities,
            Some(json!({"SupportsMediaControl": true}))
        );

        let replacement = DeviceToken {
            access_token: Uuid::new_v4().simple().to_string(),
            device_name: "Replacement".to_string(),
            ..first.clone()
        };
        database.ensure_device_session(&replacement).await.unwrap();
        assert!(
            database
                .device_session_by_id(&first.access_token)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            database
                .device_sessions_for_user(user.id)
                .await
                .unwrap()
                .len(),
            1
        );

        let racing_device_id = format!("device-race-{}", Uuid::new_v4());
        let racing_first = DeviceToken {
            access_token: Uuid::new_v4().simple().to_string(),
            device_id: racing_device_id.clone(),
            device_name: "Racing First".to_string(),
            ..replacement.clone()
        };
        let racing_second = DeviceToken {
            access_token: Uuid::new_v4().simple().to_string(),
            device_name: "Racing Second".to_string(),
            ..racing_first.clone()
        };
        let first_database = database.clone();
        let second_database = database.clone();
        let (first_result, second_result) = tokio::join!(
            first_database.ensure_device_session(&racing_first),
            second_database.ensure_device_session(&racing_second)
        );
        first_result.unwrap();
        second_result.unwrap();
        assert_eq!(
            database
                .device_sessions_for_user(user.id)
                .await
                .unwrap()
                .into_iter()
                .filter(|session| session.device_id == racing_device_id)
                .count(),
            1
        );

        let other = DeviceToken {
            access_token: Uuid::new_v4().simple().to_string(),
            device_id: format!("device-{}", Uuid::new_v4()),
            device_name: "Other".to_string(),
            ..replacement.clone()
        };
        database.ensure_device_session(&other).await.unwrap();
        database
            .revoke_user_tokens_except(user.id, &replacement.access_token)
            .await
            .unwrap();
        assert!(
            database
                .device_session_by_id(&other.access_token)
                .await
                .unwrap()
                .is_none()
        );
        database
            .revoke_device(&replacement.device_id)
            .await
            .unwrap();
        assert!(
            database
                .device_session_by_id(&replacement.access_token)
                .await
                .unwrap()
                .is_none()
        );
        database.delete_user(user.id).await.unwrap();
        database.close().await;
    }
}
