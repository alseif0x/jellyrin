use std::{fmt, str::FromStr};

/// Identifies a database adapter without forcing repositories onto a shared SQL dialect.
///
/// PostgreSQL is the only production backend today. The enum is non-exhaustive so a future native
/// MySQL (or other) adapter can be added without presenting it as currently supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DatabaseDriver {
    PostgreSql,
    /// Reserved selector for the future native MySQL adapter.
    ///
    /// Recognising the name now makes configuration failures explicit, but this variant must not
    /// be treated as runtime support. [`DatabaseDriver::is_production_supported`] remains false
    /// until the adapter, its migrations, and the repository conformance suite are complete.
    MySql,
    /// Recognised SQLite selector. Its real adapter is gated behind the canonical `sqlite`
    /// feature and remains confined to migration and test workloads until its production
    /// repository conformance work is complete.
    Sqlite,
}

impl DatabaseDriver {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PostgreSql => "postgresql",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }

    pub const fn is_production_supported(self) -> bool {
        matches!(self, Self::PostgreSql)
    }

    /// Identifies the adapter implied by a URL without retaining or exposing credentials.
    ///
    /// Planned adapters are recognised here so the manager can report an intentional
    /// "not available" error instead of pretending that the driver name is unknown.
    pub fn from_url(database_url: &str) -> anyhow::Result<Self> {
        let database_url = database_url.trim();
        // Do not lowercase the complete URL: it may contain credentials. Only inspect the public
        // scheme prefix and keep the secret-bearing value untouched.
        let scheme = if database_url
            .get(.."sqlite:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sqlite:"))
        {
            "sqlite"
        } else {
            database_url
                .split_once("://")
                .map(|(scheme, _)| scheme)
                .filter(|scheme| !scheme.is_empty())
                .ok_or_else(|| anyhow::anyhow!("database URL must include a scheme"))?
        };
        match scheme.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => Ok(Self::PostgreSql),
            "mysql" => Ok(Self::MySql),
            "sqlite" => Ok(Self::Sqlite),
            _ => anyhow::bail!("DATABASE_URL uses an unrecognised database scheme"),
        }
    }

    /// Validates only the driver-to-URL relationship. Error messages deliberately omit the URL.
    pub fn validate_url_scheme(self, database_url: &str) -> anyhow::Result<()> {
        let url_driver = Self::from_url(database_url)?;
        anyhow::ensure!(
            self == url_driver,
            "configured database driver does not match the DATABASE_URL scheme"
        );
        Ok(())
    }

    /// Resolves an adapter URL that is available in the production runtime today.
    pub fn from_production_url(database_url: &str) -> anyhow::Result<Self> {
        let driver = Self::from_url(database_url)?;
        anyhow::ensure!(
            driver.is_production_supported(),
            "database driver is recognised but not production-ready; Jellyrin currently supports PostgreSQL"
        );
        Ok(driver)
    }
}

impl fmt::Display for DatabaseDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DatabaseDriver {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => Ok(Self::PostgreSql),
            "mysql" => Ok(Self::MySql),
            "sqlite" | "sqlite-legacy" => Ok(Self::Sqlite),
            _ => anyhow::bail!("unknown database driver; expected postgresql, mysql, or sqlite"),
        }
    }
}

/// Marker implemented by dialect-native adapters. Domain repository traits remain narrow and can
/// evolve independently instead of becoming a lowest-common-denominator database interface.
pub trait DatabaseBackend: Clone + Send + Sync + 'static {
    const DRIVER: DatabaseDriver;

    fn driver(&self) -> DatabaseDriver {
        Self::DRIVER
    }

    /// Returns a bounded, credential-free telemetry snapshot for this adapter instance.
    fn telemetry_diagnostics(&self) -> crate::DatabaseTelemetryDiagnostics {
        crate::DatabaseTelemetryDiagnostics::uninstrumented()
    }
}

#[cfg(test)]
mod tests {
    use super::{DatabaseBackend, DatabaseDriver};

    fn assert_backend_driver<B: DatabaseBackend>(expected: DatabaseDriver) {
        assert_eq!(B::DRIVER, expected);
    }

    #[test]
    fn selectors_round_trip_through_their_canonical_names() {
        for driver in [
            DatabaseDriver::PostgreSql,
            DatabaseDriver::MySql,
            DatabaseDriver::Sqlite,
        ] {
            assert_eq!(driver.as_str().parse::<DatabaseDriver>().unwrap(), driver);
            assert_eq!(driver.to_string(), driver.as_str());
        }

        let legacy_alias = "sqlite-legacy".parse::<DatabaseDriver>().unwrap();
        assert_eq!(legacy_alias, DatabaseDriver::Sqlite);
        assert_eq!(legacy_alias.as_str(), "sqlite");
        assert_eq!(legacy_alias.to_string(), "sqlite");
    }

    #[test]
    fn native_backend_markers_match_their_explicit_drivers() {
        assert_backend_driver::<crate::PostgresDatabase>(DatabaseDriver::PostgreSql);
        assert_backend_driver::<crate::SqliteDatabase>(DatabaseDriver::Sqlite);
    }

    #[test]
    fn production_driver_selector_accepts_only_postgresql_urls() {
        assert_eq!(
            DatabaseDriver::from_production_url("postgresql://db.invalid/jellyrin").unwrap(),
            DatabaseDriver::PostgreSql
        );
        assert_eq!(
            DatabaseDriver::from_production_url("POSTGRES://db.invalid/jellyrin").unwrap(),
            DatabaseDriver::PostgreSql
        );
        assert!(DatabaseDriver::from_production_url("mysql://db.invalid/jellyrin").is_err());
        assert!(DatabaseDriver::from_production_url("sqlite::memory:").is_err());
    }

    #[test]
    fn planned_mysql_driver_is_recognised_but_not_production_supported() {
        let driver = "mysql".parse::<DatabaseDriver>().unwrap();

        assert_eq!(driver, DatabaseDriver::MySql);
        assert_eq!(
            DatabaseDriver::from_url("mysql://db.invalid/jellyrin").unwrap(),
            DatabaseDriver::MySql
        );
        assert!(!driver.is_production_supported());
    }

    #[test]
    fn sqlite_driver_is_publicly_recognised_but_not_production_supported() {
        let driver = "sqlite".parse::<DatabaseDriver>().unwrap();

        assert_eq!(driver, DatabaseDriver::Sqlite);
        assert_eq!(
            DatabaseDriver::from_url("sqlite://data/jellyrin.db").unwrap(),
            DatabaseDriver::Sqlite
        );
        assert_eq!(
            DatabaseDriver::from_url("sqlite::memory:").unwrap(),
            DatabaseDriver::Sqlite
        );
        assert_eq!(
            DatabaseDriver::from_url("SQLITE::memory:").unwrap(),
            DatabaseDriver::Sqlite
        );
        assert_eq!(
            "sqlite-legacy".parse::<DatabaseDriver>().unwrap(),
            DatabaseDriver::Sqlite
        );
        assert!(DatabaseDriver::from_url("sqlite-legacy:///data/jellyrin.db").is_err());
        assert!(!driver.is_production_supported());
    }

    #[test]
    fn url_scheme_mismatch_error_never_contains_credentials() {
        let error = DatabaseDriver::PostgreSql
            .validate_url_scheme("mysql://jellyrin:super-secret@db/jellyrin")
            .unwrap_err()
            .to_string();

        assert!(!error.contains("super-secret"));
        assert!(!error.contains("mysql://"));
    }
}
