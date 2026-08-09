//! Bounded, payload-free telemetry for `ffprobe` executions.
//!
//! The collector deliberately accepts no command, path, URL, stderr, or error
//! text. Every started attempt has exactly one terminal outcome: an explicit
//! call to [`FfprobeTelemetryAttempt::finish`] records the supplied outcome,
//! while dropping an unfinished attempt records cancellation.

use std::{
    array,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const DURATION_BUCKET_UPPER_MILLIS: [u64; 5] = [10, 100, 1_000, 5_000, 15_000];
const DURATION_BUCKET_COUNT: usize = DURATION_BUCKET_UPPER_MILLIS.len() + 1;

static FFPROBE_TELEMETRY: OnceLock<FfprobeTelemetry> = OnceLock::new();

/// Point-in-time, fixed-cardinality counters for all observed `ffprobe` runs.
///
/// Counters are monotonic and saturate at `u64::MAX`. Because atomics are read
/// independently, invariants such as `started == active + completed` are
/// guaranteed for a quiescent collector but may be transiently unequal while
/// another thread is starting or completing an attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FfprobeTelemetrySnapshot {
    pub active: u64,
    pub peak_active: u64,
    pub started: u64,
    pub succeeded: u64,
    pub capacity_unavailable: u64,
    pub non_zero_exit: u64,
    pub timed_out: u64,
    pub output_limited: u64,
    pub io_failed: u64,
    pub invalid_json: u64,
    pub cancelled: u64,
    pub duration_le_10_ms: u64,
    pub duration_le_100_ms: u64,
    pub duration_le_1_000_ms: u64,
    pub duration_le_5_000_ms: u64,
    pub duration_le_15_000_ms: u64,
    pub duration_gt_15_000_ms: u64,
}

impl FfprobeTelemetrySnapshot {
    /// Number of attempts that reached any terminal outcome.
    pub fn completed(self) -> u64 {
        [
            self.succeeded,
            self.capacity_unavailable,
            self.non_zero_exit,
            self.timed_out,
            self.output_limited,
            self.io_failed,
            self.invalid_json,
            self.cancelled,
        ]
        .into_iter()
        .fold(0_u64, u64::saturating_add)
    }

    /// Number of completed attempts represented in the duration histogram.
    pub fn duration_samples(self) -> u64 {
        [
            self.duration_le_10_ms,
            self.duration_le_100_ms,
            self.duration_le_1_000_ms,
            self.duration_le_5_000_ms,
            self.duration_le_15_000_ms,
            self.duration_gt_15_000_ms,
        ]
        .into_iter()
        .fold(0_u64, u64::saturating_add)
    }
}

/// Closed set of terminal outcomes. It intentionally carries no payload so a
/// caller cannot accidentally retain a provider URL, argv, stderr, or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfprobeOutcome {
    Succeeded,
    CapacityUnavailable,
    NonZeroExit,
    TimedOut,
    OutputLimited,
    IoFailed,
    InvalidJson,
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct FfprobeTelemetry {
    inner: Arc<FfprobeTelemetryInner>,
}

struct FfprobeTelemetryInner {
    active: AtomicU64,
    peak_active: AtomicU64,
    started: AtomicU64,
    succeeded: AtomicU64,
    capacity_unavailable: AtomicU64,
    non_zero_exit: AtomicU64,
    timed_out: AtomicU64,
    output_limited: AtomicU64,
    io_failed: AtomicU64,
    invalid_json: AtomicU64,
    cancelled: AtomicU64,
    duration_buckets: [AtomicU64; DURATION_BUCKET_COUNT],
}

/// RAII lifecycle token returned for one admitted `ffprobe` execution.
///
/// Dropping this value before calling [`Self::finish`] explicitly records a
/// cancellation, including cancellation caused by aborting the owning future.
#[must_use = "dropping an unfinished ffprobe attempt records cancellation"]
pub(crate) struct FfprobeTelemetryAttempt {
    telemetry: FfprobeTelemetry,
    started_at: Instant,
    finished: bool,
}

impl FfprobeTelemetry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(FfprobeTelemetryInner {
                active: AtomicU64::new(0),
                peak_active: AtomicU64::new(0),
                started: AtomicU64::new(0),
                succeeded: AtomicU64::new(0),
                capacity_unavailable: AtomicU64::new(0),
                non_zero_exit: AtomicU64::new(0),
                timed_out: AtomicU64::new(0),
                output_limited: AtomicU64::new(0),
                io_failed: AtomicU64::new(0),
                invalid_json: AtomicU64::new(0),
                cancelled: AtomicU64::new(0),
                duration_buckets: array::from_fn(|_| AtomicU64::new(0)),
            }),
        }
    }

    pub(crate) fn start(&self) -> FfprobeTelemetryAttempt {
        saturating_increment(&self.inner.started);
        let active = saturating_increment(&self.inner.active);
        self.inner.peak_active.fetch_max(active, Ordering::Relaxed);
        FfprobeTelemetryAttempt {
            telemetry: self.clone(),
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn snapshot(&self) -> FfprobeTelemetrySnapshot {
        let bucket = |index: usize| self.inner.duration_buckets[index].load(Ordering::Relaxed);
        FfprobeTelemetrySnapshot {
            active: self.inner.active.load(Ordering::Relaxed),
            peak_active: self.inner.peak_active.load(Ordering::Relaxed),
            started: self.inner.started.load(Ordering::Relaxed),
            succeeded: self.inner.succeeded.load(Ordering::Relaxed),
            capacity_unavailable: self.inner.capacity_unavailable.load(Ordering::Relaxed),
            non_zero_exit: self.inner.non_zero_exit.load(Ordering::Relaxed),
            timed_out: self.inner.timed_out.load(Ordering::Relaxed),
            output_limited: self.inner.output_limited.load(Ordering::Relaxed),
            io_failed: self.inner.io_failed.load(Ordering::Relaxed),
            invalid_json: self.inner.invalid_json.load(Ordering::Relaxed),
            cancelled: self.inner.cancelled.load(Ordering::Relaxed),
            duration_le_10_ms: bucket(0),
            duration_le_100_ms: bucket(1),
            duration_le_1_000_ms: bucket(2),
            duration_le_5_000_ms: bucket(3),
            duration_le_15_000_ms: bucket(4),
            duration_gt_15_000_ms: bucket(5),
        }
    }

    fn complete(&self, outcome: FfprobeOutcome, elapsed: Duration) {
        let outcome_counter = match outcome {
            FfprobeOutcome::Succeeded => &self.inner.succeeded,
            FfprobeOutcome::CapacityUnavailable => &self.inner.capacity_unavailable,
            FfprobeOutcome::NonZeroExit => &self.inner.non_zero_exit,
            FfprobeOutcome::TimedOut => &self.inner.timed_out,
            FfprobeOutcome::OutputLimited => &self.inner.output_limited,
            FfprobeOutcome::IoFailed => &self.inner.io_failed,
            FfprobeOutcome::InvalidJson => &self.inner.invalid_json,
            FfprobeOutcome::Cancelled => &self.inner.cancelled,
        };
        saturating_increment(outcome_counter);
        saturating_increment(&self.inner.duration_buckets[duration_bucket_index(elapsed)]);
        saturating_decrement(&self.inner.active);
    }
}

impl FfprobeTelemetryAttempt {
    pub(crate) fn finish(mut self, outcome: FfprobeOutcome) {
        self.finished = true;
        self.telemetry.complete(outcome, self.started_at.elapsed());
    }

    #[cfg(test)]
    fn finish_with_elapsed(mut self, outcome: FfprobeOutcome, elapsed: Duration) {
        self.finished = true;
        self.telemetry.complete(outcome, elapsed);
    }
}

impl Drop for FfprobeTelemetryAttempt {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.telemetry
                .complete(FfprobeOutcome::Cancelled, self.started_at.elapsed());
        }
    }
}

/// Shared process-wide collector used by production probe execution.
pub(crate) fn ffprobe_telemetry() -> &'static FfprobeTelemetry {
    FFPROBE_TELEMETRY.get_or_init(FfprobeTelemetry::new)
}

/// Returns a payload-free snapshot suitable for an operational API.
pub fn ffprobe_telemetry_snapshot() -> FfprobeTelemetrySnapshot {
    ffprobe_telemetry().snapshot()
}

fn duration_bucket_index(elapsed: Duration) -> usize {
    let millis = elapsed.as_millis();
    DURATION_BUCKET_UPPER_MILLIS
        .iter()
        .position(|upper| millis <= u128::from(*upper))
        .unwrap_or(DURATION_BUCKET_COUNT - 1)
}

fn saturating_increment(counter: &AtomicU64) -> u64 {
    let previous = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        })
        .unwrap_or_else(|value| value);
    previous.saturating_add(1)
}

fn saturating_decrement(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    use super::{FfprobeOutcome, FfprobeTelemetry};

    #[test]
    fn every_outcome_and_duration_bucket_is_accounted_exactly_once() {
        let telemetry = FfprobeTelemetry::new();
        for (outcome, millis) in [
            (FfprobeOutcome::CapacityUnavailable, 0),
            (FfprobeOutcome::Succeeded, 10),
            (FfprobeOutcome::NonZeroExit, 100),
            (FfprobeOutcome::TimedOut, 1_000),
            (FfprobeOutcome::OutputLimited, 5_000),
            (FfprobeOutcome::IoFailed, 15_000),
            (FfprobeOutcome::InvalidJson, 15_001),
        ] {
            telemetry
                .start()
                .finish_with_elapsed(outcome, Duration::from_millis(millis));
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.peak_active, 1);
        assert_eq!(snapshot.started, 7);
        assert_eq!(snapshot.succeeded, 1);
        assert_eq!(snapshot.capacity_unavailable, 1);
        assert_eq!(snapshot.non_zero_exit, 1);
        assert_eq!(snapshot.timed_out, 1);
        assert_eq!(snapshot.output_limited, 1);
        assert_eq!(snapshot.io_failed, 1);
        assert_eq!(snapshot.invalid_json, 1);
        assert_eq!(snapshot.cancelled, 0);
        assert_eq!(snapshot.completed(), snapshot.started);
        assert_eq!(snapshot.duration_samples(), snapshot.completed());
        assert_eq!(snapshot.duration_le_10_ms, 2);
        assert_eq!(snapshot.duration_le_100_ms, 1);
        assert_eq!(snapshot.duration_le_1_000_ms, 1);
        assert_eq!(snapshot.duration_le_5_000_ms, 1);
        assert_eq!(snapshot.duration_le_15_000_ms, 1);
        assert_eq!(snapshot.duration_gt_15_000_ms, 1);
    }

    #[test]
    fn dropping_an_attempt_records_explicit_cancellation() {
        let telemetry = FfprobeTelemetry::new();
        let attempt = telemetry.start();
        assert_eq!(telemetry.snapshot().active, 1);
        drop(attempt);

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.started, 1);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.cancelled, 1);
        assert_eq!(snapshot.completed(), 1);
        assert_eq!(snapshot.duration_samples(), 1);
    }

    #[test]
    fn concurrent_attempts_preserve_quiescent_invariants_and_peak() {
        const WORKERS: usize = 16;
        let telemetry = FfprobeTelemetry::new();
        let ready = Arc::new(Barrier::new(WORKERS + 1));
        let release = Arc::new(Barrier::new(WORKERS + 1));
        let threads = (0..WORKERS)
            .map(|_| {
                let telemetry = telemetry.clone();
                let ready = Arc::clone(&ready);
                let release = Arc::clone(&release);
                thread::spawn(move || {
                    let attempt = telemetry.start();
                    ready.wait();
                    release.wait();
                    attempt.finish(FfprobeOutcome::Succeeded);
                })
            })
            .collect::<Vec<_>>();

        ready.wait();
        let active = telemetry.snapshot();
        assert_eq!(active.active, WORKERS as u64);
        assert_eq!(active.peak_active, WORKERS as u64);
        release.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.started, WORKERS as u64);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.peak_active, WORKERS as u64);
        assert_eq!(snapshot.succeeded, WORKERS as u64);
        assert_eq!(snapshot.completed(), snapshot.started);
        assert_eq!(snapshot.duration_samples(), snapshot.completed());
    }

    #[test]
    fn telemetry_surface_cannot_retain_secret_bearing_payloads() {
        let telemetry = FfprobeTelemetry::new();
        telemetry.start().finish(FfprobeOutcome::IoFailed);

        let rendered = format!("{:?}", telemetry.snapshot());
        assert!(!rendered.contains("http://user:password@example.invalid/stream"));
        assert!(!rendered.contains("password"));
        assert!(!rendered.contains("stream"));
    }
}
