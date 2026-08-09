use std::{fmt, time::Duration};

use anyhow::ensure;

use super::{
    DatabaseDriver, PostgresDatabase, PostgresSettings, ProductionDatabase, ProviderSecretVault,
};

const DEFAULT_API_MAX_CONNECTIONS: u32 = 6;
const DEFAULT_WORKER_MAX_CONNECTIONS: u32 = 2;
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);
const DEFAULT_API_STATEMENT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_WORKER_STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(3);

/// Driver-neutral runtime configuration owned by the database boundary.
///
/// The URL is intentionally private and its `Debug` representation is always redacted. Pool
/// limits are operational policy shared by adapters; each adapter remains responsible for
/// translating them into native connection options and validating any dialect-specific details.
#[derive(Clone)]
pub struct DatabaseConfig {
    driver: DatabaseDriver,
    database_url: String,
    api_max_connections: u32,
    worker_max_connections: u32,
    acquire_timeout: Duration,
    idle_timeout: Duration,
    max_lifetime: Duration,
    api_statement_timeout: Duration,
    worker_statement_timeout: Duration,
    lock_timeout: Duration,
}

impl DatabaseConfig {
    pub fn new(driver: DatabaseDriver, database_url: impl AsRef<str>) -> anyhow::Result<Self> {
        let database_url = database_url.as_ref().trim().to_owned();
        driver.validate_url_scheme(&database_url)?;
        let config = Self {
            driver,
            database_url,
            api_max_connections: DEFAULT_API_MAX_CONNECTIONS,
            worker_max_connections: DEFAULT_WORKER_MAX_CONNECTIONS,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_lifetime: DEFAULT_MAX_LIFETIME,
            api_statement_timeout: DEFAULT_API_STATEMENT_TIMEOUT,
            worker_statement_timeout: DEFAULT_WORKER_STATEMENT_TIMEOUT,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub const fn driver(&self) -> DatabaseDriver {
        self.driver
    }

    pub const fn api_max_connections(&self) -> u32 {
        self.api_max_connections
    }

    pub const fn worker_max_connections(&self) -> u32 {
        self.worker_max_connections
    }

    pub const fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }

    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub const fn max_lifetime(&self) -> Duration {
        self.max_lifetime
    }

    pub const fn api_statement_timeout(&self) -> Duration {
        self.api_statement_timeout
    }

    pub const fn worker_statement_timeout(&self) -> Duration {
        self.worker_statement_timeout
    }

    pub const fn lock_timeout(&self) -> Duration {
        self.lock_timeout
    }

    pub fn with_api_max_connections(mut self, max_connections: u32) -> anyhow::Result<Self> {
        self.api_max_connections = max_connections;
        self.validate()?;
        Ok(self)
    }

    pub fn with_worker_max_connections(mut self, max_connections: u32) -> anyhow::Result<Self> {
        self.worker_max_connections = max_connections;
        self.validate()?;
        Ok(self)
    }

    pub fn with_acquire_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        self.acquire_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        self.idle_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn with_max_lifetime(mut self, timeout: Duration) -> anyhow::Result<Self> {
        self.max_lifetime = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn with_statement_timeouts(
        mut self,
        api: Duration,
        worker: Duration,
    ) -> anyhow::Result<Self> {
        self.api_statement_timeout = api;
        self.worker_statement_timeout = worker;
        self.validate()?;
        Ok(self)
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        self.lock_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.driver.validate_url_scheme(&self.database_url)?;
        ensure!(
            (1..=64).contains(&self.api_max_connections),
            "database API pool size must be between 1 and 64"
        );
        ensure!(
            (1..=16).contains(&self.worker_max_connections),
            "database worker pool size must be between 1 and 16"
        );
        ensure!(
            !self.acquire_timeout.is_zero() && self.acquire_timeout <= Duration::from_secs(60),
            "database acquire timeout must be greater than zero and at most 60s"
        );
        ensure!(
            !self.idle_timeout.is_zero() && self.idle_timeout <= Duration::from_secs(60 * 60),
            "database idle timeout must be greater than zero and at most 1h"
        );
        ensure!(
            !self.max_lifetime.is_zero() && self.max_lifetime <= Duration::from_secs(24 * 60 * 60),
            "database connection lifetime must be greater than zero and at most 24h"
        );
        ensure!(
            !self.api_statement_timeout.is_zero()
                && self.api_statement_timeout <= Duration::from_secs(60),
            "database API statement timeout must be greater than zero and at most 60s"
        );
        ensure!(
            !self.worker_statement_timeout.is_zero()
                && self.worker_statement_timeout <= Duration::from_secs(30 * 60),
            "database worker statement timeout must be greater than zero and at most 30m"
        );
        ensure!(
            !self.lock_timeout.is_zero() && self.lock_timeout <= Duration::from_secs(60),
            "database lock timeout must be greater than zero and at most 60s"
        );
        Ok(())
    }

    fn postgres_settings(&self) -> anyhow::Result<PostgresSettings> {
        // Discard the parser's source error so malformed URLs can never be echoed with credentials.
        PostgresSettings::new(self.database_url.clone())
            .map_err(|_| anyhow::anyhow!("PostgreSQL DATABASE_URL is invalid"))?
            .with_max_connections(self.api_max_connections)?
            .with_worker_max_connections(self.worker_max_connections)?
            .with_acquire_timeout(self.acquire_timeout)?
            .with_idle_timeout(self.idle_timeout)?
            .with_max_lifetime(self.max_lifetime)?
            .with_statement_timeouts(self.api_statement_timeout, self.worker_statement_timeout)?
            .with_lock_timeout(self.lock_timeout)
    }
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("driver", &self.driver)
            .field("database_url", &"[REDACTED]")
            .field("api_max_connections", &self.api_max_connections)
            .field("worker_max_connections", &self.worker_max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("max_lifetime", &self.max_lifetime)
            .field("api_statement_timeout", &self.api_statement_timeout)
            .field("worker_statement_timeout", &self.worker_statement_timeout)
            .field("lock_timeout", &self.lock_timeout)
            .finish()
    }
}

/// Central adapter factory used by production binaries.
///
/// PostgreSQL is deliberately the only constructible production adapter. New drivers are wired
/// into this one match only after they provide native migrations and pass every repository
/// conformance test; request handlers never select a SQL dialect themselves.
#[derive(Debug, Clone)]
pub struct DatabaseManager {
    config: DatabaseConfig,
    provider_secret_vault: Option<ProviderSecretVault>,
}

impl DatabaseManager {
    pub fn new(config: DatabaseConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            provider_secret_vault: None,
        })
    }

    pub fn with_provider_secret_vault(mut self, vault: ProviderSecretVault) -> Self {
        self.provider_secret_vault = Some(vault);
        self
    }

    pub const fn driver(&self) -> DatabaseDriver {
        self.config.driver()
    }

    pub const fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    pub async fn connect(&self) -> anyhow::Result<ProductionDatabase> {
        match self.config.driver {
            DatabaseDriver::PostgreSql => {
                let settings = self.config.postgres_settings()?;
                let database = PostgresDatabase::connect_with_settings(&settings).await?;
                Ok(match self.provider_secret_vault.clone() {
                    Some(vault) => database.with_provider_secret_vault(vault),
                    None => database,
                })
            }
            DatabaseDriver::MySql => anyhow::bail!(
                "MySQL database driver is planned but unavailable; use PostgreSQL until its native adapter, migrations, and conformance suite are complete"
            ),
            DatabaseDriver::Sqlite => anyhow::bail!(
                "SQLite database driver is recognised but unavailable in the production runtime; use PostgreSQL until SQLite completes its production repository and migration conformance suite"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DatabaseConfig, DatabaseDriver, DatabaseManager};

    #[test]
    fn configuration_redacts_database_credentials() {
        let config = DatabaseConfig::new(
            DatabaseDriver::PostgreSql,
            "postgresql://jellyrin:super-secret@db/jellyrin",
        )
        .unwrap();

        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("postgresql://"));
    }

    #[test]
    fn explicit_driver_and_url_must_match_without_fallback() {
        for (driver, database_url) in [
            (
                DatabaseDriver::PostgreSql,
                "sqlite:///srv/jellyrin/should-not-be-selected.db",
            ),
            (
                DatabaseDriver::Sqlite,
                "postgresql://jellyrin:super-secret@db.invalid/jellyrin",
            ),
            (
                DatabaseDriver::MySql,
                "postgresql://jellyrin:super-secret@db.invalid/jellyrin",
            ),
        ] {
            let error = DatabaseConfig::new(driver, database_url)
                .unwrap_err()
                .to_string();
            assert!(error.contains("does not match"));
            assert!(!error.contains("super-secret"));
            assert!(!error.contains("should-not-be-selected"));
            assert!(!error.contains("://"));
        }
    }

    #[test]
    fn configuration_validates_pool_and_timeout_policy_before_connecting() {
        let config = DatabaseConfig::new(
            DatabaseDriver::PostgreSql,
            "postgresql://db.invalid/jellyrin",
        )
        .unwrap()
        .with_api_max_connections(4)
        .unwrap()
        .with_worker_max_connections(1)
        .unwrap()
        .with_idle_timeout(Duration::from_secs(90))
        .unwrap()
        .with_max_lifetime(Duration::from_secs(300))
        .unwrap();

        assert_eq!(config.api_max_connections(), 4);
        assert_eq!(config.worker_max_connections(), 1);
        assert_eq!(config.idle_timeout(), Duration::from_secs(90));
        assert_eq!(config.max_lifetime(), Duration::from_secs(300));
        assert!(
            config
                .clone()
                .with_api_max_connections(0)
                .unwrap_err()
                .to_string()
                .contains("API pool")
        );
        assert!(
            config
                .with_statement_timeouts(Duration::ZERO, Duration::from_secs(30))
                .is_err()
        );
    }

    #[tokio::test]
    async fn planned_mysql_selector_fails_before_any_connection_attempt() {
        let config = DatabaseConfig::new(
            DatabaseDriver::MySql,
            "mysql://jellyrin:super-secret@db.invalid/jellyrin",
        )
        .unwrap();
        let manager = DatabaseManager::new(config).unwrap();

        let error = manager
            .connect()
            .await
            .err()
            .expect("planned MySQL adapter must fail before connecting")
            .to_string();
        assert!(error.contains("planned but unavailable"));
        assert!(!error.contains("super-secret"));
        assert!(!error.contains("mysql://"));
    }

    #[tokio::test]
    async fn sqlite_selector_is_recognised_but_rejected_by_production_manager() {
        let config =
            DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite:///srv/jellyrin/data.db").unwrap();
        let manager = DatabaseManager::new(config).unwrap();

        let error = manager
            .connect()
            .await
            .err()
            .expect("SQLite production selector must fail before connecting")
            .to_string();
        assert!(error.contains("recognised but unavailable"));
        assert!(!error.contains("/srv/jellyrin/data.db"));
    }
}
