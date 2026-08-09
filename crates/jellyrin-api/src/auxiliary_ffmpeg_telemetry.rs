//! Fixed-cardinality, payload-free telemetry for auxiliary FFmpeg commands.

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

static AUXILIARY_FFMPEG_TELEMETRY: OnceLock<AuxiliaryFfmpegTelemetry> = OnceLock::new();

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AuxiliaryFfmpegTelemetrySnapshot {
    pub active: u64,
    pub peak_active: u64,
    pub started: u64,
    pub succeeded: u64,
    pub capacity_unavailable: u64,
    pub timed_out: u64,
    pub output_limited: u64,
    pub non_zero_exit: u64,
    pub io_failed: u64,
    pub cancelled: u64,
    pub duration_le_10_ms: u64,
    pub duration_le_100_ms: u64,
    pub duration_le_1_000_ms: u64,
    pub duration_le_5_000_ms: u64,
    pub duration_le_15_000_ms: u64,
    pub duration_gt_15_000_ms: u64,
}

impl AuxiliaryFfmpegTelemetrySnapshot {
    pub(crate) fn completed(self) -> u64 {
        [
            self.succeeded,
            self.capacity_unavailable,
            self.timed_out,
            self.output_limited,
            self.non_zero_exit,
            self.io_failed,
            self.cancelled,
        ]
        .into_iter()
        .fold(0_u64, u64::saturating_add)
    }

    #[cfg(test)]
    fn duration_samples(self) -> u64 {
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

/// Terminal outcomes carry no command, argv, path, stderr, or identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuxiliaryFfmpegOutcome {
    Succeeded,
    CapacityUnavailable,
    TimedOut,
    OutputLimited,
    NonZeroExit,
    IoFailed,
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct AuxiliaryFfmpegTelemetry {
    inner: Arc<AuxiliaryFfmpegTelemetryInner>,
}

struct AuxiliaryFfmpegTelemetryInner {
    active: AtomicU64,
    peak_active: AtomicU64,
    started: AtomicU64,
    succeeded: AtomicU64,
    capacity_unavailable: AtomicU64,
    timed_out: AtomicU64,
    output_limited: AtomicU64,
    non_zero_exit: AtomicU64,
    io_failed: AtomicU64,
    cancelled: AtomicU64,
    duration_buckets: [AtomicU64; DURATION_BUCKET_COUNT],
}

#[must_use = "dropping an unfinished auxiliary FFmpeg attempt records cancellation"]
pub(crate) struct AuxiliaryFfmpegTelemetryAttempt {
    telemetry: AuxiliaryFfmpegTelemetry,
    started_at: Instant,
    finished: bool,
}

impl AuxiliaryFfmpegTelemetry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(AuxiliaryFfmpegTelemetryInner {
                active: AtomicU64::new(0),
                peak_active: AtomicU64::new(0),
                started: AtomicU64::new(0),
                succeeded: AtomicU64::new(0),
                capacity_unavailable: AtomicU64::new(0),
                timed_out: AtomicU64::new(0),
                output_limited: AtomicU64::new(0),
                non_zero_exit: AtomicU64::new(0),
                io_failed: AtomicU64::new(0),
                cancelled: AtomicU64::new(0),
                duration_buckets: array::from_fn(|_| AtomicU64::new(0)),
            }),
        }
    }

    pub(crate) fn start(&self) -> AuxiliaryFfmpegTelemetryAttempt {
        saturating_increment(&self.inner.started);
        let active = saturating_increment(&self.inner.active);
        self.inner.peak_active.fetch_max(active, Ordering::Relaxed);
        AuxiliaryFfmpegTelemetryAttempt {
            telemetry: self.clone(),
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn snapshot(&self) -> AuxiliaryFfmpegTelemetrySnapshot {
        let bucket = |index: usize| self.inner.duration_buckets[index].load(Ordering::Relaxed);
        AuxiliaryFfmpegTelemetrySnapshot {
            active: self.inner.active.load(Ordering::Relaxed),
            peak_active: self.inner.peak_active.load(Ordering::Relaxed),
            started: self.inner.started.load(Ordering::Relaxed),
            succeeded: self.inner.succeeded.load(Ordering::Relaxed),
            capacity_unavailable: self.inner.capacity_unavailable.load(Ordering::Relaxed),
            timed_out: self.inner.timed_out.load(Ordering::Relaxed),
            output_limited: self.inner.output_limited.load(Ordering::Relaxed),
            non_zero_exit: self.inner.non_zero_exit.load(Ordering::Relaxed),
            io_failed: self.inner.io_failed.load(Ordering::Relaxed),
            cancelled: self.inner.cancelled.load(Ordering::Relaxed),
            duration_le_10_ms: bucket(0),
            duration_le_100_ms: bucket(1),
            duration_le_1_000_ms: bucket(2),
            duration_le_5_000_ms: bucket(3),
            duration_le_15_000_ms: bucket(4),
            duration_gt_15_000_ms: bucket(5),
        }
    }

    fn complete(&self, outcome: AuxiliaryFfmpegOutcome, elapsed: Duration) {
        let outcome_counter = match outcome {
            AuxiliaryFfmpegOutcome::Succeeded => &self.inner.succeeded,
            AuxiliaryFfmpegOutcome::CapacityUnavailable => &self.inner.capacity_unavailable,
            AuxiliaryFfmpegOutcome::TimedOut => &self.inner.timed_out,
            AuxiliaryFfmpegOutcome::OutputLimited => &self.inner.output_limited,
            AuxiliaryFfmpegOutcome::NonZeroExit => &self.inner.non_zero_exit,
            AuxiliaryFfmpegOutcome::IoFailed => &self.inner.io_failed,
            AuxiliaryFfmpegOutcome::Cancelled => &self.inner.cancelled,
        };
        saturating_increment(outcome_counter);
        saturating_increment(&self.inner.duration_buckets[duration_bucket_index(elapsed)]);
        saturating_decrement(&self.inner.active);
    }
}

impl AuxiliaryFfmpegTelemetryAttempt {
    pub(crate) fn finish(mut self, outcome: AuxiliaryFfmpegOutcome) {
        self.finished = true;
        self.telemetry.complete(outcome, self.started_at.elapsed());
    }

    #[cfg(test)]
    fn finish_with_elapsed(mut self, outcome: AuxiliaryFfmpegOutcome, elapsed: Duration) {
        self.finished = true;
        self.telemetry.complete(outcome, elapsed);
    }
}

impl Drop for AuxiliaryFfmpegTelemetryAttempt {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.telemetry
                .complete(AuxiliaryFfmpegOutcome::Cancelled, self.started_at.elapsed());
        }
    }
}

pub(crate) fn auxiliary_ffmpeg_telemetry() -> &'static AuxiliaryFfmpegTelemetry {
    AUXILIARY_FFMPEG_TELEMETRY.get_or_init(AuxiliaryFfmpegTelemetry::new)
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

    use super::{AuxiliaryFfmpegOutcome, AuxiliaryFfmpegTelemetry};

    #[test]
    fn outcomes_and_duration_buckets_are_exclusive_and_complete() {
        let telemetry = AuxiliaryFfmpegTelemetry::new();
        for (outcome, millis) in [
            (AuxiliaryFfmpegOutcome::Succeeded, 0),
            (AuxiliaryFfmpegOutcome::CapacityUnavailable, 11),
            (AuxiliaryFfmpegOutcome::TimedOut, 101),
            (AuxiliaryFfmpegOutcome::OutputLimited, 1_001),
            (AuxiliaryFfmpegOutcome::NonZeroExit, 5_001),
            (AuxiliaryFfmpegOutcome::IoFailed, 15_001),
        ] {
            telemetry
                .start()
                .finish_with_elapsed(outcome, Duration::from_millis(millis));
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.peak_active, 1);
        assert_eq!(snapshot.started, 6);
        assert_eq!(snapshot.succeeded, 1);
        assert_eq!(snapshot.capacity_unavailable, 1);
        assert_eq!(snapshot.timed_out, 1);
        assert_eq!(snapshot.output_limited, 1);
        assert_eq!(snapshot.non_zero_exit, 1);
        assert_eq!(snapshot.io_failed, 1);
        assert_eq!(snapshot.cancelled, 0);
        assert_eq!(snapshot.completed(), snapshot.started);
        assert_eq!(snapshot.duration_samples(), snapshot.completed());
        assert_eq!(snapshot.duration_le_10_ms, 1);
        assert_eq!(snapshot.duration_le_100_ms, 1);
        assert_eq!(snapshot.duration_le_1_000_ms, 1);
        assert_eq!(snapshot.duration_le_5_000_ms, 1);
        assert_eq!(snapshot.duration_le_15_000_ms, 1);
        assert_eq!(snapshot.duration_gt_15_000_ms, 1);
    }

    #[test]
    fn drop_records_cancellation_and_releases_active_slot() {
        let telemetry = AuxiliaryFfmpegTelemetry::new();
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
    fn concurrent_attempts_track_exact_peak_and_quiescent_invariants() {
        const WORKERS: usize = 16;
        let telemetry = AuxiliaryFfmpegTelemetry::new();
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
                    attempt.finish(AuxiliaryFfmpegOutcome::Succeeded);
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
    fn telemetry_types_have_no_secret_bearing_payload_surface() {
        let telemetry = AuxiliaryFfmpegTelemetry::new();
        telemetry.start().finish(AuxiliaryFfmpegOutcome::IoFailed);
        let rendered = format!("{:?}", telemetry.snapshot());
        for forbidden in ["password", "argv", "stderr", "http://", "/media/"] {
            assert!(!rendered.contains(forbidden));
        }
    }
}
