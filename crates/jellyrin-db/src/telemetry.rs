use std::{
    array,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// Fixed upper bounds for database duration histograms, expressed in microseconds.
///
/// The final bucket in every snapshot contains values greater than 30 seconds. Keeping the
/// boundaries in the database crate prevents HTTP or metrics adapters from inventing labels with
/// unbounded cardinality.
pub const DATABASE_DURATION_BUCKET_UPPER_MICROSECONDS: [u64; 11] = [
    100, 500, 1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000, 30_000_000,
];
pub const DATABASE_DURATION_BUCKET_COUNT: usize =
    DATABASE_DURATION_BUCKET_UPPER_MICROSECONDS.len() + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseTelemetryCoverage {
    Uninstrumented,
    SelectedHotPaths,
}

impl DatabaseTelemetryCoverage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uninstrumented => "Uninstrumented",
            Self::SelectedHotPaths => "SelectedHotPaths",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabasePoolRole {
    Api,
    Worker,
}

impl DatabasePoolRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "Api",
            Self::Worker => "Worker",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Api => 0,
            Self::Worker => 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatabaseDurationHistogramDiagnostics {
    pub count: u64,
    pub sum_microseconds: u64,
    pub max_microseconds: u64,
    /// Cumulative bucket counts. The last entry is the unbounded `+Inf` bucket.
    pub cumulative_buckets: [u64; DATABASE_DURATION_BUCKET_COUNT],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DatabaseErrorClassDiagnostics {
    pub pool_timeout: u64,
    pub statement_timeout: u64,
    pub conflict: u64,
    pub constraint: u64,
    pub connection: u64,
    pub database: u64,
    pub other: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DatabaseRowDiagnostics {
    pub total: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatabaseAcquireDiagnostics {
    pub attempts: u64,
    pub succeeded: u64,
    pub timed_out: u64,
    pub errors: u64,
    pub cancelled: u64,
    pub waiting: u64,
    pub peak_waiting: u64,
    pub wait: DatabaseDurationHistogramDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseOperationDiagnostics {
    pub name: &'static str,
    pub pool: DatabasePoolRole,
    pub calls: u64,
    pub succeeded: u64,
    pub errors: u64,
    pub cancelled: u64,
    pub rows: DatabaseRowDiagnostics,
    pub duration: DatabaseDurationHistogramDiagnostics,
    pub errors_by_class: DatabaseErrorClassDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseTelemetryDiagnostics {
    pub coverage: DatabaseTelemetryCoverage,
    pub api_acquire: DatabaseAcquireDiagnostics,
    pub worker_acquire: Option<DatabaseAcquireDiagnostics>,
    /// Contains at most two entries per member of the private, fixed operation enum.
    pub operations: Vec<DatabaseOperationDiagnostics>,
}

impl DatabaseTelemetryDiagnostics {
    /// Safe default for external/test adapters that have not attached a
    /// collector. Production adapters override the backend method.
    pub fn uninstrumented() -> Self {
        Self {
            coverage: DatabaseTelemetryCoverage::Uninstrumented,
            api_acquire: DatabaseAcquireDiagnostics::default(),
            worker_acquire: None,
            operations: Vec::new(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum DatabaseOperation {
    AuthUserByToken,
    AuthUserByApiKey,
    CatalogNameSearch,
    CatalogItemExists,
    CatalogItemById,
    CatalogPage,
    CatalogCounts,
    CatalogEffectiveTypeCandidates,
    CatalogNextUpCandidates,
    CatalogFolderItems,
    CatalogFolderCounts,
    CatalogFilterSummary,
    CatalogMetadataValues,
    CatalogLatestItems,
    CatalogMetadataByIds,
    PlaybackStatesByItems,
    LiveTvPage,
    LiveTvCount,
    TranscodeProgressWrite,
    CatalogSyncPublish,
    CatalogSyncStage,
    CatalogSyncTombstone,
    CatalogSyncMerge,
    CatalogSyncCommit,
}

impl DatabaseOperation {
    const ALL: [Self; 24] = [
        Self::AuthUserByToken,
        Self::AuthUserByApiKey,
        Self::CatalogNameSearch,
        Self::CatalogItemExists,
        Self::CatalogItemById,
        Self::CatalogPage,
        Self::CatalogCounts,
        Self::CatalogEffectiveTypeCandidates,
        Self::CatalogNextUpCandidates,
        Self::CatalogFolderItems,
        Self::CatalogFolderCounts,
        Self::CatalogFilterSummary,
        Self::CatalogMetadataValues,
        Self::CatalogLatestItems,
        Self::CatalogMetadataByIds,
        Self::PlaybackStatesByItems,
        Self::LiveTvPage,
        Self::LiveTvCount,
        Self::TranscodeProgressWrite,
        Self::CatalogSyncPublish,
        Self::CatalogSyncStage,
        Self::CatalogSyncTombstone,
        Self::CatalogSyncMerge,
        Self::CatalogSyncCommit,
    ];
    const COUNT: usize = Self::ALL.len();

    const fn as_str(self) -> &'static str {
        match self {
            Self::AuthUserByToken => "auth.user_by_token",
            Self::AuthUserByApiKey => "auth.user_by_api_key",
            Self::CatalogNameSearch => "catalog.name_search",
            Self::CatalogItemExists => "catalog.item_exists",
            Self::CatalogItemById => "catalog.item_by_id",
            Self::CatalogPage => "catalog.page",
            Self::CatalogCounts => "catalog.counts",
            Self::CatalogEffectiveTypeCandidates => "catalog.effective_type_candidates",
            Self::CatalogNextUpCandidates => "catalog.next_up_candidates",
            Self::CatalogFolderItems => "catalog.folder_items",
            Self::CatalogFolderCounts => "catalog.folder_counts",
            Self::CatalogFilterSummary => "catalog.filter_summary",
            Self::CatalogMetadataValues => "catalog.metadata_values",
            Self::CatalogLatestItems => "catalog.latest_items",
            Self::CatalogMetadataByIds => "catalog.metadata_by_ids",
            Self::PlaybackStatesByItems => "playback.states_by_items",
            Self::LiveTvPage => "live_tv.page",
            Self::LiveTvCount => "live_tv.count",
            Self::TranscodeProgressWrite => "transcode.progress_write",
            Self::CatalogSyncPublish => "catalog_sync.publish",
            Self::CatalogSyncStage => "catalog_sync.stage",
            Self::CatalogSyncTombstone => "catalog_sync.tombstone",
            Self::CatalogSyncMerge => "catalog_sync.merge",
            Self::CatalogSyncCommit => "catalog_sync.commit",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum DatabaseErrorClass {
    PoolTimeout,
    StatementTimeout,
    Conflict,
    Constraint,
    Connection,
    Database,
    Other,
}

impl DatabaseErrorClass {
    const COUNT: usize = 7;

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Default)]
struct HistogramMetrics {
    count: AtomicU64,
    sum_microseconds: AtomicU64,
    max_microseconds: AtomicU64,
    buckets: [AtomicU64; DATABASE_DURATION_BUCKET_COUNT],
}

impl HistogramMetrics {
    fn record(&self, microseconds: u64) {
        atomic_saturating_add(&self.count, 1);
        atomic_saturating_add(&self.sum_microseconds, microseconds);
        atomic_fetch_max(&self.max_microseconds, microseconds);
        let bucket = DATABASE_DURATION_BUCKET_UPPER_MICROSECONDS
            .partition_point(|upper| *upper < microseconds);
        atomic_saturating_add(&self.buckets[bucket], 1);
    }

    fn snapshot(&self) -> DatabaseDurationHistogramDiagnostics {
        let mut cumulative = 0u64;
        let cumulative_buckets = array::from_fn(|index| {
            cumulative = cumulative.saturating_add(self.buckets[index].load(Ordering::Relaxed));
            cumulative
        });
        DatabaseDurationHistogramDiagnostics {
            count: self.count.load(Ordering::Relaxed),
            sum_microseconds: self.sum_microseconds.load(Ordering::Relaxed),
            max_microseconds: self.max_microseconds.load(Ordering::Relaxed),
            cumulative_buckets,
        }
    }
}

#[derive(Default)]
struct AcquireMetrics {
    attempts: AtomicU64,
    succeeded: AtomicU64,
    timed_out: AtomicU64,
    errors: AtomicU64,
    cancelled: AtomicU64,
    waiting: AtomicU64,
    peak_waiting: AtomicU64,
    wait: HistogramMetrics,
}

impl AcquireMetrics {
    fn snapshot(&self) -> DatabaseAcquireDiagnostics {
        DatabaseAcquireDiagnostics {
            attempts: self.attempts.load(Ordering::Relaxed),
            succeeded: self.succeeded.load(Ordering::Relaxed),
            timed_out: self.timed_out.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
            peak_waiting: self.peak_waiting.load(Ordering::Relaxed),
            wait: self.wait.snapshot(),
        }
    }
}

struct OperationMetrics {
    calls: AtomicU64,
    succeeded: AtomicU64,
    errors: AtomicU64,
    cancelled: AtomicU64,
    rows_total: AtomicU64,
    rows_max: AtomicU64,
    duration: HistogramMetrics,
    error_classes: [AtomicU64; DatabaseErrorClass::COUNT],
}

impl Default for OperationMetrics {
    fn default() -> Self {
        Self {
            calls: AtomicU64::new(0),
            succeeded: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            rows_total: AtomicU64::new(0),
            rows_max: AtomicU64::new(0),
            duration: HistogramMetrics::default(),
            error_classes: array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl OperationMetrics {
    fn error_snapshot(&self) -> DatabaseErrorClassDiagnostics {
        let load =
            |class: DatabaseErrorClass| self.error_classes[class.index()].load(Ordering::Relaxed);
        DatabaseErrorClassDiagnostics {
            pool_timeout: load(DatabaseErrorClass::PoolTimeout),
            statement_timeout: load(DatabaseErrorClass::StatementTimeout),
            conflict: load(DatabaseErrorClass::Conflict),
            constraint: load(DatabaseErrorClass::Constraint),
            connection: load(DatabaseErrorClass::Connection),
            database: load(DatabaseErrorClass::Database),
            other: load(DatabaseErrorClass::Other),
        }
    }

    fn snapshot(
        &self,
        operation: DatabaseOperation,
        pool: DatabasePoolRole,
    ) -> DatabaseOperationDiagnostics {
        DatabaseOperationDiagnostics {
            name: operation.as_str(),
            pool,
            calls: self.calls.load(Ordering::Relaxed),
            succeeded: self.succeeded.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            rows: DatabaseRowDiagnostics {
                total: self.rows_total.load(Ordering::Relaxed),
                max: self.rows_max.load(Ordering::Relaxed),
            },
            duration: self.duration.snapshot(),
            errors_by_class: self.error_snapshot(),
        }
    }
}

pub(crate) struct DatabaseTelemetry {
    operations: [[OperationMetrics; 2]; DatabaseOperation::COUNT],
    acquire: [AcquireMetrics; 2],
}

impl Default for DatabaseTelemetry {
    fn default() -> Self {
        Self {
            operations: array::from_fn(|_| array::from_fn(|_| OperationMetrics::default())),
            acquire: array::from_fn(|_| AcquireMetrics::default()),
        }
    }
}

impl DatabaseTelemetry {
    #[allow(dead_code)]
    pub(crate) fn start_operation(
        &self,
        operation: DatabaseOperation,
        pool: DatabasePoolRole,
    ) -> DatabaseOperationObservation<'_> {
        atomic_saturating_add(&self.operation(operation, pool).calls, 1);
        DatabaseOperationObservation {
            telemetry: self,
            operation,
            pool,
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn start_acquire(&self, pool: DatabasePoolRole) -> DatabaseAcquireObservation<'_> {
        let metrics = &self.acquire[pool.index()];
        atomic_saturating_add(&metrics.attempts, 1);
        let waiting = atomic_saturating_add(&metrics.waiting, 1);
        atomic_fetch_max(&metrics.peak_waiting, waiting);
        DatabaseAcquireObservation {
            telemetry: self,
            pool,
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn snapshot(&self, has_worker_pool: bool) -> DatabaseTelemetryDiagnostics {
        let mut operations = Vec::with_capacity(DatabaseOperation::COUNT * 2);
        for operation in DatabaseOperation::ALL {
            for pool in [DatabasePoolRole::Api, DatabasePoolRole::Worker] {
                if pool == DatabasePoolRole::Worker && !has_worker_pool {
                    continue;
                }
                let metrics = self.operation(operation, pool);
                if metrics.calls.load(Ordering::Relaxed) != 0 {
                    operations.push(metrics.snapshot(operation, pool));
                }
            }
        }
        DatabaseTelemetryDiagnostics {
            coverage: DatabaseTelemetryCoverage::SelectedHotPaths,
            api_acquire: self.acquire[DatabasePoolRole::Api.index()].snapshot(),
            worker_acquire: has_worker_pool
                .then(|| self.acquire[DatabasePoolRole::Worker.index()].snapshot()),
            operations,
        }
    }

    fn operation(&self, operation: DatabaseOperation, pool: DatabasePoolRole) -> &OperationMetrics {
        &self.operations[operation.index()][pool.index()]
    }
}

#[allow(dead_code)]
pub(crate) struct DatabaseOperationObservation<'a> {
    telemetry: &'a DatabaseTelemetry,
    operation: DatabaseOperation,
    pool: DatabasePoolRole,
    started_at: Instant,
    finished: bool,
}

#[allow(dead_code)]
impl DatabaseOperationObservation<'_> {
    pub(crate) fn start_acquire(&self) -> DatabaseAcquireObservation<'_> {
        self.telemetry.start_acquire(self.pool)
    }

    pub(crate) fn finish_success(mut self, rows: u64) {
        let metrics = self.telemetry.operation(self.operation, self.pool);
        atomic_saturating_add(&metrics.succeeded, 1);
        atomic_saturating_add(&metrics.rows_total, rows);
        atomic_fetch_max(&metrics.rows_max, rows);
        metrics
            .duration
            .record(elapsed_microseconds(self.started_at));
        self.finished = true;
    }

    pub(crate) fn finish_error(mut self, class: DatabaseErrorClass) {
        let metrics = self.telemetry.operation(self.operation, self.pool);
        atomic_saturating_add(&metrics.errors, 1);
        atomic_saturating_add(&metrics.error_classes[class.index()], 1);
        metrics
            .duration
            .record(elapsed_microseconds(self.started_at));
        self.finished = true;
    }

    pub(crate) fn finish_result<T>(
        self,
        result: &anyhow::Result<T>,
        success_rows: impl FnOnce(&T) -> u64,
    ) {
        match result {
            Ok(value) => self.finish_success(success_rows(value)),
            Err(error) => self.finish_error(classify_database_error(error)),
        }
    }
}

impl Drop for DatabaseOperationObservation<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let metrics = self.telemetry.operation(self.operation, self.pool);
        atomic_saturating_add(&metrics.cancelled, 1);
        metrics
            .duration
            .record(elapsed_microseconds(self.started_at));
    }
}

#[allow(dead_code)]
pub(crate) struct DatabaseAcquireObservation<'a> {
    telemetry: &'a DatabaseTelemetry,
    pool: DatabasePoolRole,
    started_at: Instant,
    finished: bool,
}

#[allow(dead_code)]
impl DatabaseAcquireObservation<'_> {
    pub(crate) fn finish_success(mut self) {
        self.finish(AcquireOutcome::Succeeded);
    }

    pub(crate) fn finish_timeout(mut self) {
        self.finish(AcquireOutcome::TimedOut);
    }

    pub(crate) fn finish_error(mut self) {
        self.finish(AcquireOutcome::Error);
    }

    pub(crate) fn finish_result<T>(self, result: &Result<T, sqlx::Error>) {
        match result {
            Ok(_) => self.finish_success(),
            Err(sqlx::Error::PoolTimedOut) => self.finish_timeout(),
            Err(_) => self.finish_error(),
        }
    }

    fn finish(&mut self, outcome: AcquireOutcome) {
        let metrics = &self.telemetry.acquire[self.pool.index()];
        match outcome {
            AcquireOutcome::Succeeded => atomic_saturating_add(&metrics.succeeded, 1),
            AcquireOutcome::TimedOut => atomic_saturating_add(&metrics.timed_out, 1),
            AcquireOutcome::Error => atomic_saturating_add(&metrics.errors, 1),
            AcquireOutcome::Cancelled => atomic_saturating_add(&metrics.cancelled, 1),
        };
        atomic_saturating_sub(&metrics.waiting, 1);
        metrics.wait.record(elapsed_microseconds(self.started_at));
        self.finished = true;
    }
}

impl Drop for DatabaseAcquireObservation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(AcquireOutcome::Cancelled);
        }
    }
}

#[derive(Clone, Copy)]
enum AcquireOutcome {
    Succeeded,
    TimedOut,
    Error,
    Cancelled,
}

fn elapsed_microseconds(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn classify_database_error(error: &anyhow::Error) -> DatabaseErrorClass {
    let Some(error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<sqlx::Error>())
    else {
        return DatabaseErrorClass::Other;
    };
    match error {
        sqlx::Error::PoolTimedOut => DatabaseErrorClass::PoolTimeout,
        sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed
        | sqlx::Error::Io(_)
        | sqlx::Error::Tls(_) => DatabaseErrorClass::Connection,
        sqlx::Error::Database(database_error) => {
            use sqlx::error::ErrorKind;
            match database_error.kind() {
                ErrorKind::UniqueViolation
                | ErrorKind::ForeignKeyViolation
                | ErrorKind::NotNullViolation
                | ErrorKind::CheckViolation => DatabaseErrorClass::Constraint,
                ErrorKind::Other => match database_error.code().as_deref() {
                    // Only classify known codes; the raw value is never retained or exported.
                    Some("57014" | "3024") => DatabaseErrorClass::StatementTimeout,
                    Some("40001" | "40P01" | "1205" | "1213") => DatabaseErrorClass::Conflict,
                    _ => DatabaseErrorClass::Database,
                },
                _ => DatabaseErrorClass::Database,
            }
        }
        _ => DatabaseErrorClass::Other,
    }
}

fn atomic_saturating_add(value: &AtomicU64, amount: u64) -> u64 {
    let mut current = value.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(amount);
        match value.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn atomic_saturating_sub(value: &AtomicU64, amount: u64) -> u64 {
    let mut current = value.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match value.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn atomic_fetch_max(value: &AtomicU64, candidate: u64) {
    let _ = value.fetch_max(candidate, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread, time::Duration};

    use super::*;

    #[test]
    fn duration_histogram_uses_inclusive_fixed_boundaries_and_cumulative_counts() {
        let histogram = HistogramMetrics::default();
        for value in [0, 100, 101, 500, 501, 1_000, 30_000_000, 30_000_001] {
            histogram.record(value);
        }

        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.count, 8);
        assert_eq!(snapshot.sum_microseconds, 60_002_203);
        assert_eq!(snapshot.max_microseconds, 30_000_001);
        assert_eq!(snapshot.cumulative_buckets[0], 2);
        assert_eq!(snapshot.cumulative_buckets[1], 4);
        assert_eq!(snapshot.cumulative_buckets[2], 6);
        assert_eq!(snapshot.cumulative_buckets[10], 7);
        assert_eq!(snapshot.cumulative_buckets[11], 8);
        assert!(
            snapshot
                .cumulative_buckets
                .windows(2)
                .all(|counts| counts[0] <= counts[1])
        );
    }

    #[test]
    fn successful_error_and_cancelled_operations_have_bounded_safe_snapshots() {
        let telemetry = DatabaseTelemetry::default();
        telemetry
            .start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api)
            .finish_success(7);
        telemetry
            .start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api)
            .finish_success(11);
        telemetry
            .start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api)
            .finish_error(DatabaseErrorClass::Constraint);
        drop(telemetry.start_operation(DatabaseOperation::CatalogPage, DatabasePoolRole::Api));

        let snapshot = telemetry.snapshot(false);
        assert_eq!(snapshot.coverage.as_str(), "SelectedHotPaths");
        assert!(snapshot.worker_acquire.is_none());
        assert_eq!(snapshot.operations.len(), 1);
        let operation = &snapshot.operations[0];
        assert_eq!(operation.name, "catalog.page");
        assert_eq!(operation.pool, DatabasePoolRole::Api);
        assert_eq!(operation.calls, 4);
        assert_eq!(operation.succeeded, 2);
        assert_eq!(operation.errors, 1);
        assert_eq!(operation.cancelled, 1);
        assert_eq!(
            operation.rows,
            DatabaseRowDiagnostics { total: 18, max: 11 }
        );
        assert_eq!(operation.errors_by_class.constraint, 1);
        assert_eq!(operation.duration.count, 4);
        assert_eq!(
            operation.duration.cumulative_buckets.last(),
            Some(&operation.calls)
        );
        let debug = format!("{snapshot:?}");
        for forbidden in [
            "postgresql://",
            "sqlite://",
            "SELECT ",
            "user_id",
            "item_id",
            "sqlstate",
        ] {
            assert!(
                !debug
                    .to_ascii_lowercase()
                    .contains(&forbidden.to_ascii_lowercase())
            );
        }
    }

    #[test]
    fn acquire_observation_tracks_each_terminal_state_and_drop_restores_waiters() {
        let telemetry = DatabaseTelemetry::default();
        let operation = telemetry.start_operation(
            DatabaseOperation::CatalogSyncPublish,
            DatabasePoolRole::Worker,
        );
        operation.start_acquire().finish_success();
        operation.start_acquire().finish_timeout();
        operation.start_acquire().finish_error();
        drop(operation.start_acquire());
        operation.finish_error(DatabaseErrorClass::PoolTimeout);

        let snapshot = telemetry.snapshot(true);
        let acquire = snapshot.worker_acquire.unwrap();
        assert_eq!(acquire.attempts, 4);
        assert_eq!(acquire.succeeded, 1);
        assert_eq!(acquire.timed_out, 1);
        assert_eq!(acquire.errors, 1);
        assert_eq!(acquire.cancelled, 1);
        assert_eq!(acquire.waiting, 0);
        assert_eq!(acquire.peak_waiting, 1);
        assert_eq!(acquire.wait.count, 4);
        assert_eq!(acquire.wait.cumulative_buckets.last(), Some(&4));
        assert_eq!(snapshot.operations[0].errors_by_class.pool_timeout, 1);
    }

    #[test]
    fn same_logical_operation_is_bounded_and_separated_by_pool_role() {
        let telemetry = DatabaseTelemetry::default();
        telemetry
            .start_operation(DatabaseOperation::CatalogSyncPublish, DatabasePoolRole::Api)
            .finish_success(2);
        telemetry
            .start_operation(
                DatabaseOperation::CatalogSyncPublish,
                DatabasePoolRole::Worker,
            )
            .finish_success(3);

        let snapshot = telemetry.snapshot(true);
        assert_eq!(snapshot.operations.len(), 2);
        assert_eq!(snapshot.operations[0].pool, DatabasePoolRole::Api);
        assert_eq!(snapshot.operations[1].pool, DatabasePoolRole::Worker);
        assert_eq!(snapshot.operations[0].rows.total, 2);
        assert_eq!(snapshot.operations[1].rows.total, 3);
        assert!(snapshot.operations.len() <= DatabaseOperation::COUNT * 2);
    }

    #[test]
    fn every_error_class_has_one_fixed_counter_and_operation_names_are_static_and_unique() {
        let telemetry = DatabaseTelemetry::default();
        for class in [
            DatabaseErrorClass::PoolTimeout,
            DatabaseErrorClass::StatementTimeout,
            DatabaseErrorClass::Conflict,
            DatabaseErrorClass::Constraint,
            DatabaseErrorClass::Connection,
            DatabaseErrorClass::Database,
            DatabaseErrorClass::Other,
        ] {
            telemetry
                .start_operation(DatabaseOperation::AuthUserByToken, DatabasePoolRole::Api)
                .finish_error(class);
        }

        let snapshot = telemetry.snapshot(false);
        assert_eq!(snapshot.operations[0].errors, 7);
        assert_eq!(
            snapshot.operations[0].errors_by_class,
            DatabaseErrorClassDiagnostics {
                pool_timeout: 1,
                statement_timeout: 1,
                conflict: 1,
                constraint: 1,
                connection: 1,
                database: 1,
                other: 1,
            }
        );

        let mut names = DatabaseOperation::ALL.map(DatabaseOperation::as_str);
        names.sort_unstable();
        assert!(names.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(names.iter().all(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'.' | b'_'))
        }));
    }

    #[test]
    fn concurrent_updates_are_lock_free_and_do_not_lose_counts() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 2_000;
        let telemetry = Arc::new(DatabaseTelemetry::default());
        let threads = (0..THREADS)
            .map(|_| {
                let telemetry = Arc::clone(&telemetry);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        let operation = telemetry.start_operation(
                            DatabaseOperation::PlaybackStatesByItems,
                            DatabasePoolRole::Api,
                        );
                        operation.start_acquire().finish_success();
                        operation.finish_success(1);
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        let snapshot = telemetry.snapshot(false);
        let expected = u64::try_from(THREADS * ITERATIONS).unwrap();
        assert_eq!(snapshot.operations[0].calls, expected);
        assert_eq!(snapshot.operations[0].succeeded, expected);
        assert_eq!(snapshot.operations[0].rows.total, expected);
        assert_eq!(snapshot.api_acquire.attempts, expected);
        assert_eq!(snapshot.api_acquire.succeeded, expected);
        assert_eq!(snapshot.api_acquire.waiting, 0);
        assert!(snapshot.api_acquire.peak_waiting >= 1);
    }

    #[test]
    fn observations_include_elapsed_time_without_floating_point_values() {
        let telemetry = DatabaseTelemetry::default();
        let operation = telemetry.start_operation(
            DatabaseOperation::TranscodeProgressWrite,
            DatabasePoolRole::Api,
        );
        thread::sleep(Duration::from_millis(2));
        operation.finish_success(1);

        let snapshot = telemetry.snapshot(false);
        let duration = &snapshot.operations[0].duration;
        assert!(duration.sum_microseconds >= 1_000);
        assert_eq!(duration.sum_microseconds, duration.max_microseconds);
        assert_eq!(duration.count, 1);
    }

    #[test]
    fn saturating_atomic_helpers_never_wrap() {
        let value = AtomicU64::new(u64::MAX - 1);
        assert_eq!(atomic_saturating_add(&value, 10), u64::MAX);
        assert_eq!(atomic_saturating_add(&value, 1), u64::MAX);
        assert_eq!(atomic_saturating_sub(&value, u64::MAX), 0);
        assert_eq!(atomic_saturating_sub(&value, 1), 0);
    }
}
