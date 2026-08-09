use std::{
    collections::HashMap,
    fmt, io,
    path::{Path, PathBuf},
    process::{ExitStatus, Output, Stdio},
    sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, OnceLock},
    time::Duration,
};

use jellyrin_core::{
    DEFAULT_HLS_SEGMENT_PATTERN, FfmpegCommandSpec, FfmpegProgress, FfmpegWorkload,
    parse_ffmpeg_progress_line,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncReadExt,
    process::{Child, ChildStdin, Command},
    sync::{Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError, broadcast, watch},
    task::JoinHandle,
    time::{Instant, sleep},
};

pub const HLS_MASTER_PLAYLIST_NAME: &str = "master.m3u8";
pub const HLS_MEDIA_PLAYLIST_NAME: &str = "main.m3u8";
pub const DEFAULT_HLS_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Time given to a transcode process group to handle `SIGTERM` before it is
/// forcibly killed.
pub const DEFAULT_TRANSCODE_STOP_GRACE_PERIOD: Duration = Duration::from_secs(2);
/// Sampling interval for per-process CPU and resident-memory observations on Linux.
pub const TRANSCODE_RESOURCE_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

pub const fn process_resource_sampling_supported() -> bool {
    cfg!(target_os = "linux")
}
#[cfg(unix)]
const TRANSCODE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const FFMPEG_STDERR_READ_CHUNK_BYTES: usize = 8 * 1024;
const FFMPEG_STDERR_MAX_LINE_BYTES: usize = 16 * 1024;
const COMMAND_OUTPUT_READ_CHUNK_BYTES: usize = 8 * 1024;
/// Keep FFmpeg below the API process in the Linux scheduler by default. A
/// positive nice value never grants the child more scheduling privilege than
/// Jellyrin itself.
pub const DEFAULT_FFMPEG_NICE: i32 = 10;
/// Disk headroom held by each admitted HLS writer unless the application
/// explicitly configures another value.
pub const DEFAULT_TRANSCODE_DISK_RESERVATION_BYTES: u64 = 64 * 1024 * 1024;

/// Configuration for atomic admission and shared monitoring of transcode
/// output storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscodeDiskQuotaConfig {
    pub quota_bytes: u64,
    pub reservation_bytes: u64,
    pub scan_interval: Duration,
}

impl TranscodeDiskQuotaConfig {
    pub const fn new(quota_bytes: u64, reservation_bytes: u64, scan_interval: Duration) -> Self {
        Self {
            quota_bytes,
            reservation_bytes,
            scan_interval,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscodeDiskQuotaSnapshot {
    pub usage_bytes: Option<u64>,
    pub reserved_bytes: u64,
    pub committed_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub quota_bytes: u64,
    pub reservation_bytes: u64,
    pub active_reservations: usize,
    pub quota_exceeded: bool,
    pub monitor_running: bool,
    pub successful_scans: u64,
    pub failed_scans: u64,
}

#[derive(Debug)]
pub enum TranscodeDiskQuotaError {
    Io(io::Error),
    Exhausted {
        quota_bytes: u64,
        usage_bytes: u64,
        reserved_bytes: u64,
        requested_bytes: u64,
    },
}

impl fmt::Display for TranscodeDiskQuotaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "failed to inspect transcode storage: {error}"),
            Self::Exhausted {
                quota_bytes,
                usage_bytes,
                reserved_bytes,
                requested_bytes,
            } => write!(
                formatter,
                "transcode storage quota of {quota_bytes} bytes cannot admit {requested_bytes} bytes of headroom ({usage_bytes} bytes used, {reserved_bytes} bytes already reserved)"
            ),
        }
    }
}

impl std::error::Error for TranscodeDiskQuotaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Exhausted { .. } => None,
        }
    }
}

impl From<io::Error> for TranscodeDiskQuotaError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Coordinates transcode disk admission across every clone. Admissions are
/// serialized around a cached filesystem measurement, while one background
/// monitor refreshes real usage for all active writers.
#[derive(Debug, Clone)]
pub struct TranscodeDiskQuota {
    inner: Arc<TranscodeDiskQuotaInner>,
}

#[derive(Debug)]
struct TranscodeDiskQuotaInner {
    root: PathBuf,
    config: TranscodeDiskQuotaConfig,
    admission_gate: tokio::sync::Mutex<()>,
    state: StdMutex<TranscodeDiskQuotaState>,
    usage_tx: watch::Sender<TranscodeDiskUsageSignal>,
    monitor_wake: Notify,
}

#[derive(Debug)]
struct TranscodeDiskQuotaState {
    usage_bytes: Option<u64>,
    reserved_bytes: u64,
    reservations: HashMap<u64, u64>,
    next_reservation_id: u64,
    last_scan: Option<Instant>,
    quota_exceeded: bool,
    monitor_running: bool,
    successful_scans: u64,
    failed_scans: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscodeDiskUsageSignal {
    quota_exceeded: bool,
}

/// RAII headroom reservation. Dropping it releases admission capacity and
/// invalidates the cached measurement so the next admission observes any
/// files left behind by the completed writer.
#[derive(Debug)]
pub struct TranscodeDiskReservation {
    inner: Arc<TranscodeDiskQuotaInner>,
    id: u64,
}

impl TranscodeDiskQuota {
    pub fn new(root: impl Into<PathBuf>, mut config: TranscodeDiskQuotaConfig) -> Self {
        config.quota_bytes = config.quota_bytes.max(1);
        config.reservation_bytes = config.reservation_bytes.clamp(1, config.quota_bytes);
        config.scan_interval = config.scan_interval.max(Duration::from_millis(1));
        let initial_signal = TranscodeDiskUsageSignal {
            quota_exceeded: false,
        };
        let (usage_tx, _) = watch::channel(initial_signal);
        Self {
            inner: Arc::new(TranscodeDiskQuotaInner {
                root: root.into(),
                config,
                admission_gate: tokio::sync::Mutex::new(()),
                state: StdMutex::new(TranscodeDiskQuotaState {
                    usage_bytes: None,
                    reserved_bytes: 0,
                    reservations: HashMap::new(),
                    next_reservation_id: 0,
                    last_scan: None,
                    quota_exceeded: false,
                    monitor_running: false,
                    successful_scans: 0,
                    failed_scans: 0,
                }),
                usage_tx,
                monitor_wake: Notify::new(),
            }),
        }
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn config(&self) -> TranscodeDiskQuotaConfig {
        self.inner.config
    }

    /// Atomically reserves configured startup headroom for one transcode
    /// writer. Concurrent callers share a fresh disk measurement and cannot
    /// all consume the same remaining capacity.
    pub async fn reserve(&self) -> Result<TranscodeDiskReservation, TranscodeDiskQuotaError> {
        let _admission_guard = self.inner.admission_gate.lock().await;
        refresh_transcode_disk_usage_locked(&self.inner, false).await?;

        let (id, start_monitor) = {
            let mut state = lock_transcode_disk_quota_state(&self.inner);
            let usage_bytes = state.usage_bytes.unwrap_or(0);
            let requested_bytes = self.inner.config.reservation_bytes;
            let projected_bytes = usage_bytes
                .saturating_add(state.reserved_bytes)
                .saturating_add(requested_bytes);
            if projected_bytes > self.inner.config.quota_bytes {
                return Err(TranscodeDiskQuotaError::Exhausted {
                    quota_bytes: self.inner.config.quota_bytes,
                    usage_bytes,
                    reserved_bytes: state.reserved_bytes,
                    requested_bytes,
                });
            }

            let id = next_transcode_disk_reservation_id(&mut state);
            state.reservations.insert(id, requested_bytes);
            state.reserved_bytes = state.reserved_bytes.saturating_add(requested_bytes);
            let start_monitor = !state.monitor_running;
            if start_monitor {
                state.monitor_running = true;
            }
            (id, start_monitor)
        };

        if start_monitor {
            tokio::spawn(run_transcode_disk_quota_monitor(Arc::clone(&self.inner)));
        }

        Ok(TranscodeDiskReservation {
            inner: Arc::clone(&self.inner),
            id,
        })
    }

    /// Returns a cached snapshot, refreshing the filesystem measurement only
    /// when it is older than the configured scan interval.
    pub async fn snapshot(&self) -> Result<TranscodeDiskQuotaSnapshot, TranscodeDiskQuotaError> {
        let _admission_guard = self.inner.admission_gate.lock().await;
        refresh_transcode_disk_usage_locked(&self.inner, false).await?;
        Ok(transcode_disk_quota_snapshot(&self.inner))
    }

    /// Returns the last in-memory state without touching the filesystem. This
    /// keeps scan-failure counters observable when an explicit refresh fails.
    pub fn cached_snapshot(&self) -> TranscodeDiskQuotaSnapshot {
        transcode_disk_quota_snapshot(&self.inner)
    }

    /// Forces a filesystem refresh. Intended for explicit diagnostics; normal
    /// admission and monitoring use the coalesced cached path.
    pub async fn refresh_snapshot(
        &self,
    ) -> Result<TranscodeDiskQuotaSnapshot, TranscodeDiskQuotaError> {
        let _admission_guard = self.inner.admission_gate.lock().await;
        refresh_transcode_disk_usage_locked(&self.inner, true).await?;
        Ok(transcode_disk_quota_snapshot(&self.inner))
    }

    /// Waits until observed on-disk usage reaches the hard quota. Every waiter
    /// subscribes to the same monitor; this method never scans the tree itself.
    pub async fn wait_until_exceeded(&self) {
        wait_for_transcode_disk_quota_exceeded(&self.inner).await;
    }
}

impl TranscodeDiskReservation {
    /// Waits on the exact quota manager that issued this reservation. Keeping
    /// the wait tied to the guard prevents callers from accidentally observing
    /// a different transcode root or configuration.
    pub async fn wait_until_exceeded(&self) {
        wait_for_transcode_disk_quota_exceeded(&self.inner).await;
    }
}

impl Drop for TranscodeDiskReservation {
    fn drop(&mut self) {
        {
            let mut state = lock_transcode_disk_quota_state(&self.inner);
            let Some(bytes) = state.reservations.remove(&self.id) else {
                return;
            };
            state.reserved_bytes = state.reserved_bytes.saturating_sub(bytes);
            // Files may have been written since the last monitor pass. Force a
            // fresh measurement before another writer is admitted.
            state.last_scan = None;
            if state.reservations.is_empty() {
                state.quota_exceeded = false;
                self.inner.usage_tx.send_replace(TranscodeDiskUsageSignal {
                    quota_exceeded: false,
                });
            }
        }
        self.inner.monitor_wake.notify_one();
    }
}

fn lock_transcode_disk_quota_state(
    inner: &TranscodeDiskQuotaInner,
) -> StdMutexGuard<'_, TranscodeDiskQuotaState> {
    inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn wait_for_transcode_disk_quota_exceeded(inner: &TranscodeDiskQuotaInner) {
    let mut usage_rx = inner.usage_tx.subscribe();
    loop {
        let signal = *usage_rx.borrow_and_update();
        if signal.quota_exceeded {
            return;
        }
        if usage_rx.changed().await.is_err() {
            return;
        }
    }
}

fn next_transcode_disk_reservation_id(state: &mut TranscodeDiskQuotaState) -> u64 {
    loop {
        let id = state.next_reservation_id;
        state.next_reservation_id = state.next_reservation_id.wrapping_add(1);
        if !state.reservations.contains_key(&id) {
            return id;
        }
    }
}

fn transcode_disk_quota_snapshot(inner: &TranscodeDiskQuotaInner) -> TranscodeDiskQuotaSnapshot {
    let state = lock_transcode_disk_quota_state(inner);
    let committed_bytes = state
        .usage_bytes
        .map(|usage_bytes| usage_bytes.saturating_add(state.reserved_bytes));
    TranscodeDiskQuotaSnapshot {
        usage_bytes: state.usage_bytes,
        reserved_bytes: state.reserved_bytes,
        committed_bytes,
        available_bytes: committed_bytes
            .map(|bytes| inner.config.quota_bytes.saturating_sub(bytes)),
        quota_bytes: inner.config.quota_bytes,
        reservation_bytes: inner.config.reservation_bytes,
        active_reservations: state.reservations.len(),
        quota_exceeded: state.quota_exceeded,
        monitor_running: state.monitor_running,
        successful_scans: state.successful_scans,
        failed_scans: state.failed_scans,
    }
}

async fn refresh_transcode_disk_usage_locked(
    inner: &TranscodeDiskQuotaInner,
    force: bool,
) -> io::Result<u64> {
    let cached = {
        let state = lock_transcode_disk_quota_state(inner);
        state
            .usage_bytes
            .zip(state.last_scan)
            .and_then(|(usage_bytes, last_scan)| {
                (!force && last_scan.elapsed() < inner.config.scan_interval).then_some(usage_bytes)
            })
    };
    if let Some(usage_bytes) = cached {
        return Ok(usage_bytes);
    }

    let usage = match measure_transcode_disk_usage(&inner.root).await {
        Ok(Some(usage_bytes)) => Ok(usage_bytes),
        Ok(None) => {
            let has_active_writers = !lock_transcode_disk_quota_state(inner)
                .reservations
                .is_empty();
            if has_active_writers {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "active transcode storage root disappeared",
                ))
            } else {
                // A fresh installation has no root until the first admitted
                // caller creates its session directory.
                Ok(0)
            }
        }
        Err(error) => Err(error),
    };

    match usage {
        Ok(usage_bytes) => {
            {
                let mut state = lock_transcode_disk_quota_state(inner);
                state.usage_bytes = Some(usage_bytes);
                state.last_scan = Some(Instant::now());
                if state.reservations.is_empty() {
                    state.quota_exceeded = usage_bytes >= inner.config.quota_bytes;
                } else if usage_bytes >= inner.config.quota_bytes {
                    // Keep the signal sticky until every writer that could
                    // have observed this quota breach has released its RAII
                    // reservation. A fast cleanup must not hide the event
                    // from a slower watch receiver.
                    state.quota_exceeded = true;
                }
                state.successful_scans = state.successful_scans.saturating_add(1);
            }
            let quota_exceeded = lock_transcode_disk_quota_state(inner).quota_exceeded;
            inner
                .usage_tx
                .send_replace(TranscodeDiskUsageSignal { quota_exceeded });
            Ok(usage_bytes)
        }
        Err(error) => {
            let quota_exceeded = {
                let mut state = lock_transcode_disk_quota_state(inner);
                state.last_scan = None;
                state.failed_scans = state.failed_scans.saturating_add(1);
                if !state.reservations.is_empty() {
                    // Once writers are active, losing visibility of the
                    // filesystem must stop them rather than silently disabling
                    // the hard-quota watchdog. Admission with no writers still
                    // returns the underlying I/O error directly.
                    state.quota_exceeded = true;
                }
                state.quota_exceeded
            };
            inner
                .usage_tx
                .send_replace(TranscodeDiskUsageSignal { quota_exceeded });
            Err(error)
        }
    }
}

async fn run_transcode_disk_quota_monitor(inner: Arc<TranscodeDiskQuotaInner>) {
    loop {
        tokio::select! {
            () = sleep(inner.config.scan_interval) => {}
            () = inner.monitor_wake.notified() => {}
        }

        let should_stop = {
            let mut state = lock_transcode_disk_quota_state(&inner);
            if state.reservations.is_empty() {
                state.monitor_running = false;
                true
            } else {
                false
            }
        };
        if should_stop {
            return;
        }

        let _admission_guard = inner.admission_gate.lock().await;
        let _ = refresh_transcode_disk_usage_locked(&inner, true).await;
    }
}

/// Measures regular-file bytes below a transcode root without following
/// symlinks. Entries removed concurrently by FFmpeg cleanup are ignored.
pub async fn transcode_disk_usage_bytes(root: &Path) -> io::Result<u64> {
    Ok(measure_transcode_disk_usage(root).await?.unwrap_or(0))
}

async fn measure_transcode_disk_usage(root: &Path) -> io::Result<Option<u64>> {
    let root_metadata = match fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transcode storage root must not be a symlink",
        ));
    }
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "transcode storage root must be a directory",
        ));
    }

    let mut total = 0_u64;
    let mut pending = vec![(root.to_path_buf(), true)];
    while let Some((directory, is_root)) = pending.pop() {
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if is_root && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
            };
            let metadata = match fs::symlink_metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push((entry.path(), false));
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(Some(total))
}

/// Returns the Unix niceness configured for FFmpeg children. `off` disables
/// the adjustment; invalid values fall back to the resource-safe default.
pub fn configured_ffmpeg_nice() -> Option<i32> {
    configured_multimedia_process_config().ffmpeg_nice
}

/// Applies process isolation and scheduler policy shared by long-running
/// transcodes and short-lived auxiliary FFmpeg jobs. On non-Unix targets this
/// is intentionally a no-op.
pub fn configure_ffmpeg_command(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Each FFmpeg invocation leads its own process group so cancellation
        // also reaches subprocesses spawned by FFmpeg or a wrapper script.
        command.as_std_mut().process_group(0);

        if let Some(nice) = configured_ffmpeg_nice() {
            // SAFETY: `setpriority` only mutates the calling child process and
            // the closure captures a Copy integer. It performs no allocation,
            // locking, or access to parent process memory after fork. Priority
            // adjustment is best-effort so restrictive seccomp profiles cannot
            // prevent playback.
            unsafe {
                command.as_std_mut().pre_exec(move || {
                    libc::setpriority(libc::PRIO_PROCESS, 0, nice);
                    Ok(())
                });
            }
        }
    }

    #[cfg(not(unix))]
    let _ = command;
}

fn ffmpeg_nice_from_value(value: Option<&str>) -> Option<i32> {
    match value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("off") => None,
        Some(value) => value
            .parse::<i32>()
            .ok()
            .filter(|value| (0..=19).contains(value))
            .or(Some(DEFAULT_FFMPEG_NICE)),
        None => Some(DEFAULT_FFMPEG_NICE),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeJobKind {
    VideoEncode,
    AudioEncode,
    Remux,
    Auxiliary,
    Probe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscodeLimits {
    /// Aggregate cap across every FFmpeg lane. Per-kind limits prevent one class from starving the
    /// others; this cap prevents several individually valid classes from saturating the host at
    /// the same time.
    pub max_total_jobs: usize,
    pub max_video_encodes: usize,
    pub max_audio_encodes: usize,
    pub max_remuxes: usize,
    pub max_auxiliary: usize,
    pub max_probes: usize,
}

impl Default for TranscodeLimits {
    fn default() -> Self {
        Self {
            max_total_jobs: 2,
            max_video_encodes: 1,
            max_audio_encodes: 2,
            max_remuxes: 3,
            max_auxiliary: 1,
            max_probes: 1,
        }
    }
}

const DEFAULT_MAX_QUEUED_PROBES: usize = 8;
const MAX_QUEUED_PROBES: usize = 128;
const DEFAULT_PROBE_QUEUE_TIMEOUT_SECONDS: u64 = 10;
const MAX_PROBE_QUEUE_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultimediaProcessConfig {
    pub limits: TranscodeLimits,
    pub max_queued_probes: usize,
    pub probe_queue_timeout: Duration,
    pub ffmpeg_nice: Option<i32>,
}

impl Default for MultimediaProcessConfig {
    fn default() -> Self {
        Self {
            limits: TranscodeLimits::default(),
            max_queued_probes: DEFAULT_MAX_QUEUED_PROBES,
            probe_queue_timeout: Duration::from_secs(DEFAULT_PROBE_QUEUE_TIMEOUT_SECONDS),
            ffmpeg_nice: Some(DEFAULT_FFMPEG_NICE),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAdmissionError {
    CoordinatorClosed,
    WaitQueueClosed,
    WaitQueueFull,
    TimedOut,
}

impl fmt::Display for ProbeAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CoordinatorClosed => "multimedia process coordinator is closed",
            Self::WaitQueueClosed => "probe wait queue is closed",
            Self::WaitQueueFull => "probe wait queue is full",
            Self::TimedOut => "timed out waiting for probe process capacity",
        })
    }
}

impl std::error::Error for ProbeAdmissionError {}

#[derive(Debug, Clone)]
pub struct TranscodeCoordinator {
    total: std::sync::Arc<Semaphore>,
    video_encodes: std::sync::Arc<Semaphore>,
    audio_encodes: std::sync::Arc<Semaphore>,
    remuxes: std::sync::Arc<Semaphore>,
    auxiliary: std::sync::Arc<Semaphore>,
    probes: std::sync::Arc<Semaphore>,
}

#[derive(Debug)]
pub struct TranscodeJobPermit {
    kind: TranscodeJobKind,
    _total_permit: OwnedSemaphorePermit,
    _lane_permit: OwnedSemaphorePermit,
}

/// Resource limits for [`run_bounded_command_output`]. The deadline covers
/// both process execution and draining the captured pipes; the termination
/// grace period begins only after that deadline expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedCommandOutputOptions {
    pub timeout: Duration,
    pub termination_grace_period: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl BoundedCommandOutputOptions {
    pub const fn new(timeout: Duration, max_stdout_bytes: usize, max_stderr_bytes: usize) -> Self {
        Self {
            timeout,
            termination_grace_period: DEFAULT_TRANSCODE_STOP_GRACE_PERIOD,
            max_stdout_bytes,
            max_stderr_bytes,
        }
    }

    pub const fn with_termination_grace_period(mut self, grace_period: Duration) -> Self {
        self.termination_grace_period = grace_period;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedCommandOutputStream {
    Stdout,
    Stderr,
}

impl fmt::Display for BoundedCommandOutputStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug)]
pub enum BoundedCommandOutputError {
    Io(io::Error),
    TimedOut,
    OutputLimitExceeded {
        stream: BoundedCommandOutputStream,
        limit: usize,
    },
}

impl fmt::Display for BoundedCommandOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "command execution failed: {error}"),
            Self::TimedOut => formatter.write_str("command exceeded its execution deadline"),
            Self::OutputLimitExceeded { stream, limit } => {
                write!(
                    formatter,
                    "command {stream} exceeded its {limit}-byte limit"
                )
            }
        }
    }
}

impl std::error::Error for BoundedCommandOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TimedOut | Self::OutputLimitExceeded { .. } => None,
        }
    }
}

impl From<io::Error> for BoundedCommandOutputError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl TranscodeCoordinator {
    pub fn new(limits: TranscodeLimits) -> Self {
        Self {
            total: std::sync::Arc::new(Semaphore::new(limits.max_total_jobs.max(1))),
            video_encodes: std::sync::Arc::new(Semaphore::new(limits.max_video_encodes.max(1))),
            audio_encodes: std::sync::Arc::new(Semaphore::new(limits.max_audio_encodes.max(1))),
            remuxes: std::sync::Arc::new(Semaphore::new(limits.max_remuxes.max(1))),
            auxiliary: std::sync::Arc::new(Semaphore::new(limits.max_auxiliary.max(1))),
            probes: std::sync::Arc::new(Semaphore::new(limits.max_probes.max(1))),
        }
    }

    pub async fn acquire(
        &self,
        kind: TranscodeJobKind,
    ) -> Result<TranscodeJobPermit, tokio::sync::AcquireError> {
        let semaphore = match kind {
            TranscodeJobKind::VideoEncode => &self.video_encodes,
            TranscodeJobKind::AudioEncode => &self.audio_encodes,
            TranscodeJobKind::Remux => &self.remuxes,
            TranscodeJobKind::Auxiliary => &self.auxiliary,
            TranscodeJobKind::Probe => &self.probes,
        };
        // Every caller takes permits in the same order, so the per-lane and aggregate caps cannot
        // deadlock. Waiting for the lane first also prevents a queued job in a saturated lane from
        // reserving otherwise usable aggregate capacity from a different lane.
        let lane_permit = semaphore.clone().acquire_owned().await?;
        let total_permit = self.total.clone().acquire_owned().await?;
        Ok(TranscodeJobPermit {
            kind,
            _total_permit: total_permit,
            _lane_permit: lane_permit,
        })
    }

    /// Acquires capacity without joining Tokio's unbounded semaphore wait list.
    /// Callers can use this fast path before admitting a request to their own
    /// explicitly bounded queue.
    pub fn try_acquire(
        &self,
        kind: TranscodeJobKind,
    ) -> Result<TranscodeJobPermit, TryAcquireError> {
        let semaphore = match kind {
            TranscodeJobKind::VideoEncode => &self.video_encodes,
            TranscodeJobKind::AudioEncode => &self.audio_encodes,
            TranscodeJobKind::Remux => &self.remuxes,
            TranscodeJobKind::Auxiliary => &self.auxiliary,
            TranscodeJobKind::Probe => &self.probes,
        };
        let lane_permit = semaphore.clone().try_acquire_owned()?;
        let total_permit = self.total.clone().try_acquire_owned()?;
        Ok(TranscodeJobPermit {
            kind,
            _total_permit: total_permit,
            _lane_permit: lane_permit,
        })
    }

    pub fn available_total_permits(&self) -> usize {
        self.total.available_permits()
    }

    pub fn available_permits(&self, kind: TranscodeJobKind) -> usize {
        match kind {
            TranscodeJobKind::VideoEncode => self.video_encodes.available_permits(),
            TranscodeJobKind::AudioEncode => self.audio_encodes.available_permits(),
            TranscodeJobKind::Remux => self.remuxes.available_permits(),
            TranscodeJobKind::Auxiliary => self.auxiliary.available_permits(),
            TranscodeJobKind::Probe => self.probes.available_permits(),
        }
    }
}

static MULTIMEDIA_PROCESS_CONFIG: OnceLock<MultimediaProcessConfig> = OnceLock::new();
static MULTIMEDIA_PROCESS_COORDINATOR: OnceLock<TranscodeCoordinator> = OnceLock::new();
static MULTIMEDIA_PROBE_WAIT_QUEUE: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub fn configured_multimedia_process_config() -> &'static MultimediaProcessConfig {
    MULTIMEDIA_PROCESS_CONFIG.get_or_init(multimedia_process_config_from_env)
}

pub fn multimedia_process_coordinator() -> &'static TranscodeCoordinator {
    MULTIMEDIA_PROCESS_COORDINATOR
        .get_or_init(|| TranscodeCoordinator::new(configured_multimedia_process_config().limits))
}

pub fn multimedia_probe_wait_queue_available_permits() -> usize {
    multimedia_probe_wait_queue().available_permits()
}

pub async fn acquire_multimedia_probe() -> Result<TranscodeJobPermit, ProbeAdmissionError> {
    let config = configured_multimedia_process_config();
    acquire_probe_with_queue(
        multimedia_process_coordinator(),
        multimedia_probe_wait_queue().clone(),
        config.probe_queue_timeout,
    )
    .await
}

fn multimedia_probe_wait_queue() -> &'static Arc<Semaphore> {
    MULTIMEDIA_PROBE_WAIT_QUEUE.get_or_init(|| {
        Arc::new(Semaphore::new(
            configured_multimedia_process_config().max_queued_probes,
        ))
    })
}

async fn acquire_probe_with_queue(
    coordinator: &TranscodeCoordinator,
    wait_queue: Arc<Semaphore>,
    timeout: Duration,
) -> Result<TranscodeJobPermit, ProbeAdmissionError> {
    match coordinator.try_acquire(TranscodeJobKind::Probe) {
        Ok(permit) => return Ok(permit),
        Err(TryAcquireError::Closed) => return Err(ProbeAdmissionError::CoordinatorClosed),
        Err(TryAcquireError::NoPermits) => {}
    }
    let queue_permit = match wait_queue.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(TryAcquireError::Closed) => return Err(ProbeAdmissionError::WaitQueueClosed),
        Err(TryAcquireError::NoPermits) => return Err(ProbeAdmissionError::WaitQueueFull),
    };
    let result =
        match tokio::time::timeout(timeout, coordinator.acquire(TranscodeJobKind::Probe)).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(ProbeAdmissionError::CoordinatorClosed),
            Err(_) => Err(ProbeAdmissionError::TimedOut),
        };
    drop(queue_permit);
    result
}

fn multimedia_process_config_from_env() -> MultimediaProcessConfig {
    let defaults = MultimediaProcessConfig::default();
    MultimediaProcessConfig {
        limits: TranscodeLimits {
            max_total_jobs: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_FFMPEG_JOBS").ok().as_deref(),
                defaults.limits.max_total_jobs,
            ),
            max_video_encodes: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_VIDEO_TRANSCODES")
                    .ok()
                    .as_deref(),
                defaults.limits.max_video_encodes,
            ),
            max_audio_encodes: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_AUDIO_TRANSCODES")
                    .ok()
                    .as_deref(),
                defaults.limits.max_audio_encodes,
            ),
            max_remuxes: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_REMUXES").ok().as_deref(),
                defaults.limits.max_remuxes,
            ),
            max_auxiliary: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_AUXILIARY_FFMPEG_JOBS")
                    .ok()
                    .as_deref(),
                defaults.limits.max_auxiliary,
            ),
            max_probes: process_limit_from_value(
                std::env::var("JELLYRIN_MAX_PROBE_JOBS").ok().as_deref(),
                defaults.limits.max_probes,
            ),
        },
        max_queued_probes: bounded_usize_from_value(
            std::env::var("JELLYRIN_MAX_QUEUED_PROBES").ok().as_deref(),
            0,
            MAX_QUEUED_PROBES,
            defaults.max_queued_probes,
        ),
        probe_queue_timeout: Duration::from_secs(bounded_u64_from_value(
            std::env::var("JELLYRIN_PROBE_QUEUE_TIMEOUT_SECONDS")
                .ok()
                .as_deref(),
            1,
            MAX_PROBE_QUEUE_TIMEOUT_SECONDS,
            defaults.probe_queue_timeout.as_secs(),
        )),
        ffmpeg_nice: ffmpeg_nice_from_value(std::env::var("JELLYRIN_FFMPEG_NICE").ok().as_deref()),
    }
}

fn process_limit_from_value(value: Option<&str>, default: usize) -> usize {
    bounded_usize_from_value(value, 1, 64, default)
}

fn bounded_usize_from_value(
    value: Option<&str>,
    minimum: usize,
    maximum: usize,
    default: usize,
) -> usize {
    value
        .map(str::trim)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default)
}

fn bounded_u64_from_value(value: Option<&str>, minimum: u64, maximum: u64, default: u64) -> u64 {
    value
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default)
}

impl TranscodeJobPermit {
    pub fn kind(&self) -> TranscodeJobKind {
        self.kind
    }
}

pub fn classify_transcode_command(command: &FfmpegCommandSpec) -> TranscodeJobKind {
    let Some(declared) = command.workload() else {
        // FfmpegCommandSpec is public and may be constructed by future providers. Only the core
        // builder is trusted to declare a cheap workload; arbitrary CLI is fail-closed.
        return TranscodeJobKind::VideoEncode;
    };
    let declared = match declared {
        FfmpegWorkload::Remux => TranscodeJobKind::Remux,
        FfmpegWorkload::AudioEncode => TranscodeJobKind::AudioEncode,
        FfmpegWorkload::VideoEncode => TranscodeJobKind::VideoEncode,
    };
    max_job_kind(declared, classify_ffmpeg_args(command.args()))
}

fn classify_ffmpeg_args(args: &[String]) -> TranscodeJobKind {
    let mut saw_codec_directive = false;
    let mut observed = TranscodeJobKind::Remux;
    let mut global_copy = false;
    let mut video_copy = false;
    let mut audio_copy = false;
    let mut subtitle_copy = false;
    let mut mapped_video = false;
    let mut mapped_audio = false;
    let mut mapped_subtitle = false;
    let mut mapped_unknown = false;

    for (index, arg) in args.iter().enumerate() {
        if let Some(kind) = filter_job_kind(arg) {
            observed = max_job_kind(observed, kind);
        }
        let Some(target) = codec_target(arg) else {
            if arg.eq_ignore_ascii_case("-map") {
                match args
                    .get(index + 1)
                    .and_then(|value| mapped_stream_target(value))
                {
                    Some(CodecTarget::Video) => mapped_video = true,
                    Some(CodecTarget::Audio) => mapped_audio = true,
                    Some(CodecTarget::Subtitle) => mapped_subtitle = true,
                    Some(CodecTarget::Unknown) | None => mapped_unknown = true,
                }
            }
            continue;
        };
        saw_codec_directive = true;
        let Some(codec) = args.get(index + 1) else {
            return TranscodeJobKind::VideoEncode;
        };
        if codec_is_copy_or_disabled(codec) {
            match target {
                CodecTarget::Video => video_copy = true,
                CodecTarget::Audio => audio_copy = true,
                CodecTarget::Subtitle => subtitle_copy = true,
                CodecTarget::Unknown if !arg.contains(':') => global_copy = true,
                CodecTarget::Unknown => {}
            }
        } else {
            let kind = match target {
                CodecTarget::Audio | CodecTarget::Subtitle => TranscodeJobKind::AudioEncode,
                CodecTarget::Video | CodecTarget::Unknown => TranscodeJobKind::VideoEncode,
            };
            observed = max_job_kind(observed, kind);
        }
    }

    if !saw_codec_directive {
        // FFmpeg selects encoders implicitly when no codec is specified.
        return TranscodeJobKind::VideoEncode;
    }
    if observed != TranscodeJobKind::Remux || global_copy {
        return observed;
    }
    if mapped_unknown || mapped_video && !video_copy {
        return TranscodeJobKind::VideoEncode;
    }
    if mapped_audio && !audio_copy || mapped_subtitle && !subtitle_copy {
        return TranscodeJobKind::AudioEncode;
    }
    if !mapped_video && !mapped_audio && !mapped_subtitle {
        return TranscodeJobKind::VideoEncode;
    }
    TranscodeJobKind::Remux
}

#[derive(Debug, Clone, Copy)]
enum CodecTarget {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

fn codec_target(option: &str) -> Option<CodecTarget> {
    let lower = option.trim().to_ascii_lowercase();
    let mut parts = lower.split(':');
    let base = parts.next()?;
    let stream_type = parts.next();
    match base {
        "-vcodec" => Some(CodecTarget::Video),
        "-acodec" => Some(CodecTarget::Audio),
        "-scodec" => Some(CodecTarget::Subtitle),
        "-c" | "-codec" => Some(match stream_type {
            Some("v") => CodecTarget::Video,
            Some("a") => CodecTarget::Audio,
            Some("s") => CodecTarget::Subtitle,
            _ => CodecTarget::Unknown,
        }),
        _ => None,
    }
}

fn mapped_stream_target(value: &str) -> Option<CodecTarget> {
    let value = value.trim().trim_end_matches('?');
    if value.starts_with('[') || !value.starts_with("0:") {
        return Some(CodecTarget::Unknown);
    }
    match value.split(':').nth(1) {
        Some("v") => Some(CodecTarget::Video),
        Some("a") => Some(CodecTarget::Audio),
        Some("s") => Some(CodecTarget::Subtitle),
        Some(_) => Some(CodecTarget::Unknown),
        None => None,
    }
}

fn filter_job_kind(option: &str) -> Option<TranscodeJobKind> {
    let lower = option.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "-af" | "-filter:a")
        || lower.starts_with("-filter:a:")
        || lower.starts_with("-filter_script:a")
    {
        Some(TranscodeJobKind::AudioEncode)
    } else if matches!(lower.as_str(), "-vf" | "-filter:v")
        || lower.starts_with("-filter:v:")
        || matches!(
            lower.as_str(),
            "-filter"
                | "-filter_complex"
                | "-filter_script"
                | "-filter_complex_script"
                | "-lavfi"
                | "-target"
        )
        || lower.starts_with("-filter:")
        || lower.starts_with("-filter_script:")
        || lower.starts_with("-filter_complex:")
        || lower.starts_with("-filter_complex_script:")
    {
        Some(TranscodeJobKind::VideoEncode)
    } else {
        None
    }
}

fn max_job_kind(left: TranscodeJobKind, right: TranscodeJobKind) -> TranscodeJobKind {
    match (left, right) {
        (TranscodeJobKind::VideoEncode, _) | (_, TranscodeJobKind::VideoEncode) => {
            TranscodeJobKind::VideoEncode
        }
        (TranscodeJobKind::AudioEncode, _) | (_, TranscodeJobKind::AudioEncode) => {
            TranscodeJobKind::AudioEncode
        }
        _ => TranscodeJobKind::Remux,
    }
}

fn codec_is_copy_or_disabled(codec: &str) -> bool {
    matches!(codec.trim().to_ascii_lowercase().as_str(), "copy" | "none")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeProcessExit {
    pub code: Option<i32>,
    pub success: bool,
}

/// A bounded, numeric observation of the FFmpeg process itself.
///
/// `cpu_percent` is measured between consecutive samples. It can exceed 100%
/// when FFmpeg uses more than one CPU core.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TranscodeProcessResourceSample {
    pub cpu_time_millis: u64,
    pub cpu_percent: Option<f64>,
    pub rss_bytes: u64,
    pub sampled_after_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsTranscodeLayout {
    pub session_dir: PathBuf,
    pub master_playlist_path: PathBuf,
    pub media_playlist_path: PathBuf,
    pub segment_pattern_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsVariantInfo {
    pub uri: String,
    pub bandwidth: u32,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HlsSegment {
    pub duration_seconds: f64,
    pub uri: String,
}

pub struct TranscodeProcess {
    child: Option<Child>,
    #[cfg(unix)]
    process_group_id: Option<libc::pid_t>,
    progress_tx: broadcast::Sender<FfmpegProgress>,
    stderr_task: Option<JoinHandle<io::Result<()>>>,
    resource_rx: watch::Receiver<Option<TranscodeProcessResourceSample>>,
    resource_task: Option<JoinHandle<()>>,
    exit: Option<TranscodeProcessExit>,
}

struct BoundedCommandChild {
    child: Option<Child>,
    #[cfg(unix)]
    process_group_id: Option<libc::pid_t>,
}

struct BoundedCommandCapture {
    bytes: Vec<u8>,
    limit_exceeded: bool,
}

impl BoundedCommandChild {
    fn new(child: Child) -> Self {
        #[cfg(unix)]
        let process_group_id = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
        Self {
            child: Some(child),
            #[cfg(unix)]
            process_group_id,
        }
    }

    fn child_mut(&mut self) -> io::Result<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("command process handle is missing"))
    }

    fn finish_after_wait(&mut self) {
        // A wrapper may exit after spawning a descendant which inherited one
        // of the output pipes. Closing out the entire group here guarantees
        // the readers can observe EOF instead of hanging after the leader was
        // already reaped.
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id.take() {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
        }
        self.child = None;
    }

    async fn terminate_and_reap(&mut self, grace_period: Duration) -> io::Result<()> {
        #[cfg(unix)]
        {
            let process_group_id = self.process_group_id;
            terminate_process_group(self.child_mut()?, process_group_id, grace_period).await?;
        }

        #[cfg(not(unix))]
        {
            let _ = grace_period;
            force_kill_and_reap(self.child_mut()?).await?;
        }

        self.finish_after_wait();
        Ok(())
    }
}

impl Drop for BoundedCommandChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id.take() {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
        }

        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

impl HlsTranscodeLayout {
    pub fn new(root: impl AsRef<Path>, play_session_id: &str) -> Self {
        let session_dir = root
            .as_ref()
            .join(sanitize_hls_path_component(play_session_id));
        Self::from_session_dir(session_dir)
    }

    pub fn from_media_playlist_path(media_playlist_path: impl AsRef<Path>) -> Self {
        let media_playlist_path = media_playlist_path.as_ref().to_path_buf();
        let session_dir = media_playlist_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            master_playlist_path: session_dir.join(HLS_MASTER_PLAYLIST_NAME),
            segment_pattern_path: session_dir.join(DEFAULT_HLS_SEGMENT_PATTERN),
            media_playlist_path,
            session_dir,
        }
    }

    fn from_session_dir(session_dir: PathBuf) -> Self {
        Self {
            master_playlist_path: session_dir.join(HLS_MASTER_PLAYLIST_NAME),
            media_playlist_path: session_dir.join(HLS_MEDIA_PLAYLIST_NAME),
            segment_pattern_path: session_dir.join(DEFAULT_HLS_SEGMENT_PATTERN),
            session_dir,
        }
    }

    pub fn segment_path(&self, index: u32) -> PathBuf {
        self.session_dir.join(format!("segment_{index:05}.ts"))
    }

    pub fn segment_pattern_string(&self) -> String {
        self.segment_pattern_path.to_string_lossy().to_string()
    }
}

/// Runs a short-lived child with bounded output capture and a hard execution
/// deadline.
///
/// Standard input is disabled and both output pipes are drained concurrently,
/// even after their capture limits are reached, so a chatty child cannot
/// deadlock on a full pipe. The command receives the same Unix process-group
/// isolation and niceness policy as FFmpeg. On timeout the group receives
/// `SIGTERM`, then `SIGKILL` after the configured grace period, and the owned
/// child is reaped before this function returns.
pub async fn run_bounded_command_output(
    mut command: Command,
    options: BoundedCommandOutputOptions,
) -> Result<Output, BoundedCommandOutputError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_ffmpeg_command(&mut command);

    let mut process = BoundedCommandChild::new(command.spawn()?);
    let stdout = process
        .child_mut()?
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("command stdout was not captured"))?;
    let stderr = process
        .child_mut()?
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("command stderr was not captured"))?;
    let mut stdout_task = tokio::spawn(read_bounded_command_output(
        stdout,
        options.max_stdout_bytes,
    ));
    let mut stderr_task = tokio::spawn(read_bounded_command_output(
        stderr,
        options.max_stderr_bytes,
    ));
    let deadline = Instant::now() + options.timeout;

    let status = match tokio::time::timeout_at(deadline, process.child_mut()?.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = process
                .terminate_and_reap(options.termination_grace_period)
                .await;
            abort_command_output_readers(&mut stdout_task, &mut stderr_task).await;
            return Err(BoundedCommandOutputError::Io(error));
        }
        Err(_) => {
            let cleanup = process
                .terminate_and_reap(options.termination_grace_period)
                .await;
            abort_command_output_readers(&mut stdout_task, &mut stderr_task).await;
            cleanup?;
            return Err(BoundedCommandOutputError::TimedOut);
        }
    };

    process.finish_after_wait();
    let captures = tokio::time::timeout_at(deadline, async {
        let (stdout, stderr) = tokio::join!(&mut stdout_task, &mut stderr_task);
        let stdout = stdout
            .map_err(|error| io::Error::other(format!("stdout reader task failed: {error}")))??;
        let stderr = stderr
            .map_err(|error| io::Error::other(format!("stderr reader task failed: {error}")))??;
        Ok::<_, BoundedCommandOutputError>((stdout, stderr))
    })
    .await;
    let (stdout, stderr) = match captures {
        Ok(captures) => captures?,
        Err(_) => {
            abort_command_output_readers(&mut stdout_task, &mut stderr_task).await;
            return Err(BoundedCommandOutputError::TimedOut);
        }
    };

    if stdout.limit_exceeded {
        return Err(BoundedCommandOutputError::OutputLimitExceeded {
            stream: BoundedCommandOutputStream::Stdout,
            limit: options.max_stdout_bytes,
        });
    }
    if stderr.limit_exceeded {
        return Err(BoundedCommandOutputError::OutputLimitExceeded {
            stream: BoundedCommandOutputStream::Stderr,
            limit: options.max_stderr_bytes,
        });
    }

    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

async fn read_bounded_command_output<R>(
    mut reader: R,
    max_bytes: usize,
) -> io::Result<BoundedCommandCapture>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(COMMAND_OUTPUT_READ_CHUNK_BYTES));
    let mut limit_exceeded = false;
    let mut buffer = [0_u8; COMMAND_OUTPUT_READ_CHUNK_BYTES];
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }

        let retained = max_bytes.saturating_sub(bytes.len()).min(bytes_read);
        bytes.extend_from_slice(&buffer[..retained]);
        limit_exceeded |= retained < bytes_read;
    }
    Ok(BoundedCommandCapture {
        bytes,
        limit_exceeded,
    })
}

async fn abort_command_output_readers(
    stdout_task: &mut JoinHandle<io::Result<BoundedCommandCapture>>,
    stderr_task: &mut JoinHandle<io::Result<BoundedCommandCapture>>,
) {
    stdout_task.abort();
    stderr_task.abort();
    let _ = stdout_task.await;
    let _ = stderr_task.await;
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinuxProcessStat {
    cpu_ticks: u64,
    start_time_ticks: u64,
    rss_pages: u64,
}

#[cfg(target_os = "linux")]
fn parse_linux_process_stat(input: &str) -> Option<LinuxProcessStat> {
    // `/proc/<pid>/stat` encloses comm in parentheses; comm itself may contain
    // spaces or parentheses, so fields are anchored after its final `)`.
    let fields = input
        .get(input.rfind(')')?.checked_add(1)?..)?
        .split_whitespace();
    let fields = fields.collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    let start_time_ticks = fields.get(19)?.parse::<u64>().ok()?;
    let rss_pages = fields.get(21)?.parse::<u64>().ok()?;
    Some(LinuxProcessStat {
        cpu_ticks: user_ticks.checked_add(system_ticks)?,
        start_time_ticks,
        rss_pages,
    })
}

#[cfg(target_os = "linux")]
fn linux_clock_ticks_and_page_size() -> Option<(u64, u64)> {
    static SYSTEM_UNITS: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *SYSTEM_UNITS.get_or_init(|| {
        // SAFETY: sysconf reads immutable process/system configuration and has no
        // pointer arguments. Non-positive results are treated as unsupported.
        let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        // SAFETY: same reasoning as the `_SC_CLK_TCK` call above.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some((
            u64::try_from(clock_ticks).ok().filter(|value| *value > 0)?,
            u64::try_from(page_size).ok().filter(|value| *value > 0)?,
        ))
    })
}

#[cfg(target_os = "linux")]
async fn sample_linux_process_resources(
    process_id: u32,
    samples: watch::Sender<Option<TranscodeProcessResourceSample>>,
    sample_interval: Duration,
) {
    let Some((clock_ticks, page_size)) = linux_clock_ticks_and_page_size() else {
        return;
    };
    let stat_path = format!("/proc/{process_id}/stat");
    let started_at = Instant::now();
    let mut previous: Option<(LinuxProcessStat, Instant)> = None;
    let mut interval = tokio::time::interval(sample_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Consume tokio's immediate first tick; the initial observation is read
    // explicitly below and subsequent reads remain spaced by the interval.
    interval.tick().await;

    loop {
        let observed_at = Instant::now();
        let Ok(raw_stat) = fs::read_to_string(&stat_path).await else {
            return;
        };
        let Some(stat) = parse_linux_process_stat(&raw_stat) else {
            return;
        };
        if previous.is_some_and(|(prior, _)| prior.start_time_ticks != stat.start_time_ticks) {
            // The PID was reused after FFmpeg exited.
            return;
        }
        let cpu_percent = previous.and_then(|(prior, prior_at)| {
            let elapsed = observed_at.duration_since(prior_at).as_secs_f64();
            let elapsed_ticks = stat.cpu_ticks.checked_sub(prior.cpu_ticks)?;
            (elapsed > 0.0).then_some((elapsed_ticks as f64 / clock_ticks as f64) / elapsed * 100.0)
        });
        let sample = TranscodeProcessResourceSample {
            cpu_time_millis: stat.cpu_ticks.saturating_mul(1_000) / clock_ticks,
            cpu_percent,
            rss_bytes: stat.rss_pages.saturating_mul(page_size),
            sampled_after_millis: u64::try_from(started_at.elapsed().as_millis())
                .unwrap_or(u64::MAX),
        };
        if samples.send(Some(sample)).is_err() {
            return;
        }
        previous = Some((stat, observed_at));
        interval.tick().await;
    }
}

pub fn spawn_transcode_process(command: &FfmpegCommandSpec) -> io::Result<TranscodeProcess> {
    spawn_transcode_process_with_stdin_mode(command, false).map(|(process, _stdin)| process)
}

pub fn spawn_transcode_process_with_stdin(
    command: &FfmpegCommandSpec,
) -> io::Result<(TranscodeProcess, ChildStdin)> {
    let (process, stdin) = spawn_transcode_process_with_stdin_mode(command, true)?;
    let stdin =
        stdin.ok_or_else(|| io::Error::other("transcode process stdin was not captured"))?;
    Ok((process, stdin))
}

fn spawn_transcode_process_with_stdin_mode(
    command: &FfmpegCommandSpec,
    pipe_stdin: bool,
) -> io::Result<(TranscodeProcess, Option<ChildStdin>)> {
    let mut child_command = Command::new(command.program());
    child_command
        .args(command.args())
        .stdin(if pipe_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_ffmpeg_command(&mut child_command);
    let mut child = child_command.spawn()?;
    #[cfg(unix)]
    let process_group_id = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let stdin = if pipe_stdin { child.stdin.take() } else { None };
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("transcode process stderr was not captured"))?;
    let (progress_tx, _) = broadcast::channel(64);
    let stderr_task = tokio::spawn(read_ffmpeg_progress(stderr, progress_tx.clone()));
    let (resource_tx, resource_rx) = watch::channel(None);
    #[cfg(target_os = "linux")]
    let resource_task = child.id().map(|process_id| {
        tokio::spawn(sample_linux_process_resources(
            process_id,
            resource_tx,
            TRANSCODE_RESOURCE_SAMPLE_INTERVAL,
        ))
    });
    #[cfg(not(target_os = "linux"))]
    let resource_task = {
        drop(resource_tx);
        None
    };

    let process = TranscodeProcess {
        child: Some(child),
        #[cfg(unix)]
        process_group_id,
        progress_tx,
        stderr_task: Some(stderr_task),
        resource_rx,
        resource_task,
        exit: None,
    };
    Ok((process, stdin))
}

pub fn render_hls_master_playlist(variant: &HlsVariantInfo) -> String {
    let mut attributes = vec![format!("BANDWIDTH={}", variant.bandwidth)];
    if let Some((width, height)) = variant.resolution {
        attributes.push(format!("RESOLUTION={width}x{height}"));
    }
    if let Some(codecs) = variant
        .codecs
        .as_deref()
        .filter(|codecs| !codecs.is_empty())
    {
        attributes.push(format!("CODECS=\"{}\"", escape_hls_attribute(codecs)));
    }

    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:{}\n{}\n",
        attributes.join(","),
        variant.uri
    )
}

pub fn render_hls_media_playlist(
    target_duration_seconds: u32,
    media_sequence: u64,
    segments: &[HlsSegment],
    end_list: bool,
) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
        target_duration_seconds.max(1),
        media_sequence
    );
    for segment in segments {
        playlist.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            segment.duration_seconds.max(0.0),
            segment.uri
        ));
    }
    if end_list {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

pub async fn wait_for_hls_readiness(
    media_playlist_path: impl AsRef<Path>,
    first_segment_path: impl AsRef<Path>,
    timeout: Duration,
) -> io::Result<bool> {
    let media_playlist_path = media_playlist_path.as_ref();
    let first_segment_path = first_segment_path.as_ref();
    let deadline = Instant::now() + timeout;

    loop {
        if non_empty_file_exists(media_playlist_path).await?
            && non_empty_file_exists(first_segment_path).await?
        {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(DEFAULT_HLS_POLL_INTERVAL).await;
    }
}

impl TranscodeProcess {
    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    pub fn subscribe_progress(&self) -> broadcast::Receiver<FfmpegProgress> {
        self.progress_tx.subscribe()
    }

    /// Subscribes to the latest per-process CPU/RSS sample. On unsupported
    /// platforms, and before the first Linux sample, the value is `None`.
    pub fn subscribe_resources(&self) -> watch::Receiver<Option<TranscodeProcessResourceSample>> {
        self.resource_rx.clone()
    }

    pub fn latest_resource_sample(&self) -> Option<TranscodeProcessResourceSample> {
        *self.resource_rx.borrow()
    }

    pub async fn wait(&mut self) -> io::Result<TranscodeProcessExit> {
        if let Some(exit) = self.exit.clone() {
            return Ok(exit);
        }

        let status = if let Some(child) = self.child.as_mut() {
            child.wait().await?
        } else {
            return Err(io::Error::other("transcode process handle is missing"));
        };
        self.finish_process(status).await
    }

    /// Requests graceful shutdown and waits for the process to be reaped. On
    /// Unix, `SIGTERM` is sent to the whole process group; after the default
    /// grace period, any remaining members receive `SIGKILL`.
    pub async fn stop(&mut self) -> io::Result<TranscodeProcessExit> {
        self.stop_with_grace_period(DEFAULT_TRANSCODE_STOP_GRACE_PERIOD)
            .await
    }

    /// Equivalent to [`Self::stop`], with a caller-selected Unix `SIGTERM`
    /// grace period. The owned child is always waited after forced termination.
    pub async fn stop_with_grace_period(
        &mut self,
        grace_period: Duration,
    ) -> io::Result<TranscodeProcessExit> {
        if let Some(exit) = self.exit.clone() {
            return Ok(exit);
        }

        #[cfg(unix)]
        let status = {
            let process_group_id = self.process_group_id;
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| io::Error::other("transcode process handle is missing"))?;
            terminate_process_group(child, process_group_id, grace_period).await?
        };

        #[cfg(not(unix))]
        let status = {
            let _ = grace_period;
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| io::Error::other("transcode process handle is missing"))?;
            force_kill_and_reap(child).await?
        };

        self.finish_process(status).await
    }

    async fn finish_process(&mut self, status: ExitStatus) -> io::Result<TranscodeProcessExit> {
        // A wrapper can exit while a subprocess remains alive with the stderr
        // pipe inherited. Do not let such a subprocess leak or keep the reader
        // task open indefinitely after the owned child has been reaped.
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id.take() {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
        }

        self.child = None;
        let exit = TranscodeProcessExit {
            code: status.code(),
            success: status.success(),
        };
        self.exit = Some(exit.clone());
        self.finish_resource_sampler().await;
        self.finish_stderr_reader().await?;
        Ok(exit)
    }

    async fn finish_resource_sampler(&mut self) {
        if let Some(resource_task) = self.resource_task.take() {
            resource_task.abort();
            let _ = resource_task.await;
        }
    }

    async fn finish_stderr_reader(&mut self) -> io::Result<()> {
        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.await.map_err(|error| {
                io::Error::other(format!("stderr reader task failed: {error}"))
            })??;
        }
        Ok(())
    }
}

impl Drop for TranscodeProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.child.is_some()
            && let Some(process_group_id) = self.process_group_id.take()
        {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
        }

        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.abort();
        }
        if let Some(resource_task) = self.resource_task.take() {
            resource_task.abort();
        }
    }
}

async fn force_kill_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }

    if let Err(error) = child.start_kill() {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        return Err(error);
    }
    child.wait().await
}

#[cfg(unix)]
async fn terminate_process_group(
    child: &mut Child,
    process_group_id: Option<libc::pid_t>,
    grace_period: Duration,
) -> io::Result<ExitStatus> {
    let Some(process_group_id) = process_group_id.filter(|id| *id > 0) else {
        return force_kill_and_reap(child).await;
    };

    if let Some(status) = child.try_wait()? {
        let _ = signal_process_group(process_group_id, libc::SIGKILL);
        return Ok(status);
    }

    if signal_process_group(process_group_id, libc::SIGTERM).is_err() {
        let _ = signal_process_group(process_group_id, libc::SIGKILL);
        return force_kill_and_reap(child).await;
    }

    let deadline = Instant::now() + grace_period;
    let mut status = None;
    loop {
        if status.is_none() {
            status = child.try_wait()?;
        }

        let group_exists = process_group_exists(process_group_id).unwrap_or(true);
        if !group_exists {
            break;
        }

        let now = Instant::now();
        if now >= deadline {
            let _ = signal_process_group(process_group_id, libc::SIGKILL);
            break;
        }
        sleep(TRANSCODE_STOP_POLL_INTERVAL.min(deadline.saturating_duration_since(now))).await;
    }

    match status {
        Some(status) => Ok(status),
        None => force_kill_and_reap(child).await,
    }
}

#[cfg(unix)]
fn signal_process_group(process_group_id: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if process_group_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be positive",
        ));
    }

    // SAFETY: the process group id is captured directly from the spawned child
    // and validated as positive, so negation cannot target the caller's group.
    if unsafe { libc::kill(-process_group_id, signal) } == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn process_group_exists(process_group_id: libc::pid_t) -> io::Result<bool> {
    if process_group_id <= 0 {
        return Ok(false);
    }

    // SAFETY: signal zero performs an existence/permission check only, and the
    // positive id validation prevents special kill(2) target semantics.
    if unsafe { libc::kill(-process_group_id, 0) } == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

fn sanitize_hls_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn escape_hls_attribute(value: &str) -> String {
    value.replace('"', "\\\"")
}

async fn non_empty_file_exists(path: &Path) -> io::Result<bool> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

async fn read_ffmpeg_progress<R>(
    mut reader: R,
    progress_tx: broadcast::Sender<FfmpegProgress>,
) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut read_buffer = [0_u8; FFMPEG_STDERR_READ_CHUNK_BYTES];
    let mut line_buffer = Vec::with_capacity(FFMPEG_STDERR_MAX_LINE_BYTES);
    let mut discarding_oversized_line = false;
    let mut progress = FfmpegProgress::default();

    loop {
        let bytes_read = reader.read(&mut read_buffer).await?;
        if bytes_read == 0 {
            break;
        }
        consume_ffmpeg_stderr_chunk(
            &read_buffer[..bytes_read],
            &mut line_buffer,
            &mut discarding_oversized_line,
            &mut progress,
            &progress_tx,
        );
    }

    if !discarding_oversized_line && !line_buffer.is_empty() {
        publish_ffmpeg_progress_line(&line_buffer, &mut progress, &progress_tx);
    }
    Ok(())
}

fn consume_ffmpeg_stderr_chunk(
    chunk: &[u8],
    line_buffer: &mut Vec<u8>,
    discarding_oversized_line: &mut bool,
    progress: &mut FfmpegProgress,
    progress_tx: &broadcast::Sender<FfmpegProgress>,
) {
    for &byte in chunk {
        if byte == b'\n' {
            if !*discarding_oversized_line {
                publish_ffmpeg_progress_line(line_buffer, progress, progress_tx);
            }
            line_buffer.clear();
            *discarding_oversized_line = false;
        } else if !*discarding_oversized_line {
            if line_buffer.len() < FFMPEG_STDERR_MAX_LINE_BYTES {
                line_buffer.push(byte);
            } else {
                // Continue draining the pipe, but retain none of an oversized
                // line. Memory therefore stays bounded even without a newline.
                line_buffer.clear();
                *discarding_oversized_line = true;
            }
        }
    }
}

fn publish_ffmpeg_progress_line(
    line: &[u8],
    progress: &mut FfmpegProgress,
    progress_tx: &broadcast::Sender<FfmpegProgress>,
) {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let Ok(line) = std::str::from_utf8(line) else {
        return;
    };
    if parse_ffmpeg_progress_line_has_snapshot(progress, line) {
        let _ = progress_tx.send(progress.clone());
    }
}

fn parse_ffmpeg_progress_line_has_snapshot(progress: &mut FfmpegProgress, line: &str) -> bool {
    let Some((key, _)) = line.trim().split_once('=') else {
        return false;
    };
    let key = key.trim();
    let known = matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time_us"
            | "out_time_ms"
            | "out_time"
            | "speed"
            | "progress"
    );
    if known {
        parse_ffmpeg_progress_line(progress, line);
    }
    key == "progress"
}

#[cfg(test)]
mod tests {
    use jellyrin_core::FfmpegCommandSpec;
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    use super::{
        BoundedCommandOutputError, BoundedCommandOutputOptions, BoundedCommandOutputStream,
        DEFAULT_FFMPEG_NICE, FFMPEG_STDERR_MAX_LINE_BYTES, HLS_MASTER_PLAYLIST_NAME,
        HLS_MEDIA_PLAYLIST_NAME, HlsSegment, HlsTranscodeLayout, HlsVariantInfo,
        TranscodeCoordinator, TranscodeDiskQuota, TranscodeDiskQuotaConfig,
        TranscodeDiskQuotaError, TranscodeJobKind, TranscodeLimits, classify_ffmpeg_args,
        classify_transcode_command, consume_ffmpeg_stderr_chunk, ffmpeg_nice_from_value,
        read_ffmpeg_progress, render_hls_master_playlist, render_hls_media_playlist,
        run_bounded_command_output, spawn_transcode_process, spawn_transcode_process_with_stdin,
        transcode_disk_usage_bytes, wait_for_hls_readiness,
    };

    #[cfg(target_os = "linux")]
    use super::{LinuxProcessStat, parse_linux_process_stat};

    #[cfg(unix)]
    async fn wait_for_unix_process_exit(process_id: libc::pid_t) {
        timeout(Duration::from_secs(2), async {
            loop {
                // SAFETY: signal zero only inspects whether the positive pid
                // written by the test child still exists.
                let exists = unsafe { libc::kill(process_id, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
                if !exists {
                    return;
                }
                #[cfg(target_os = "linux")]
                if tokio::fs::read_to_string(format!("/proc/{process_id}/stat"))
                    .await
                    .ok()
                    .and_then(|stat| stat.rsplit_once(") ").map(|(_, suffix)| suffix.to_string()))
                    .is_some_and(|suffix| suffix.starts_with("Z "))
                {
                    // A killed grandchild can be briefly visible as a zombie
                    // until the host init process adopts it. It is no longer
                    // executable and the helper never owned a handle to reap.
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process {process_id} was not terminated"));
    }

    #[test]
    fn resource_caps_ffmpeg_niceness_is_bounded_and_can_be_disabled() {
        assert_eq!(
            super::MultimediaProcessConfig::default().ffmpeg_nice,
            Some(DEFAULT_FFMPEG_NICE)
        );
        assert_eq!(ffmpeg_nice_from_value(None), Some(DEFAULT_FFMPEG_NICE));
        assert_eq!(ffmpeg_nice_from_value(Some("0")), Some(0));
        assert_eq!(ffmpeg_nice_from_value(Some("19")), Some(19));
        assert_eq!(ffmpeg_nice_from_value(Some(" off ")), None);
        assert_eq!(
            ffmpeg_nice_from_value(Some("-1")),
            Some(DEFAULT_FFMPEG_NICE)
        );
        assert_eq!(
            ffmpeg_nice_from_value(Some("20")),
            Some(DEFAULT_FFMPEG_NICE)
        );
        assert_eq!(
            ffmpeg_nice_from_value(Some("invalid")),
            Some(DEFAULT_FFMPEG_NICE)
        );
    }

    #[tokio::test]
    async fn transcode_disk_usage_counts_nested_files_and_missing_roots() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("session");
        tokio::fs::create_dir(&nested).await.unwrap();
        tokio::fs::write(root.path().join("playlist.m3u8"), [0_u8; 5])
            .await
            .unwrap();
        tokio::fs::write(nested.join("segment.ts"), [0_u8; 7])
            .await
            .unwrap();

        assert_eq!(transcode_disk_usage_bytes(root.path()).await.unwrap(), 12);
        assert_eq!(
            transcode_disk_usage_bytes(&root.path().join("already-cleaned"))
                .await
                .unwrap(),
            0
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_disk_usage_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(root.path().join("owned.ts"), [0_u8; 11])
            .await
            .unwrap();
        tokio::fs::write(outside.path().join("outside.ts"), [0_u8; 97])
            .await
            .unwrap();
        symlink(
            outside.path().join("outside.ts"),
            root.path().join("file-link.ts"),
        )
        .unwrap();
        symlink(outside.path(), root.path().join("directory-link")).unwrap();

        assert_eq!(transcode_disk_usage_bytes(root.path()).await.unwrap(), 11);

        let link_parent = tempfile::tempdir().unwrap();
        let linked_root = link_parent.path().join("linked-root");
        symlink(root.path(), &linked_root).unwrap();
        assert_eq!(
            transcode_disk_usage_bytes(&linked_root)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn transcode_disk_quota_admission_is_atomic_across_concurrent_callers() {
        let root = tempfile::tempdir().unwrap();
        let quota = TranscodeDiskQuota::new(
            root.path(),
            TranscodeDiskQuotaConfig::new(100, 60, Duration::from_secs(60)),
        );

        let (first, second) = tokio::join!(quota.reserve(), quota.reserve());
        assert_ne!(first.is_ok(), second.is_ok());
        let rejected = if first.is_err() { &first } else { &second };
        assert!(matches!(
            rejected,
            Err(TranscodeDiskQuotaError::Exhausted { .. })
        ));

        let reservation = first.ok().or_else(|| second.ok()).unwrap();
        let snapshot = quota.snapshot().await.unwrap();
        assert_eq!(snapshot.usage_bytes, Some(0));
        assert_eq!(snapshot.reserved_bytes, 60);
        assert_eq!(snapshot.committed_bytes, Some(60));
        assert_eq!(snapshot.available_bytes, Some(40));
        assert_eq!(snapshot.active_reservations, 1);
        assert_eq!(snapshot.successful_scans, 1);

        drop(reservation);
        let replacement = quota.reserve().await.unwrap();
        assert_eq!(quota.snapshot().await.unwrap().active_reservations, 1);
        drop(replacement);
    }

    #[tokio::test]
    async fn transcode_disk_quota_coalesces_fresh_admission_scans() {
        let root = tempfile::tempdir().unwrap();
        let quota = TranscodeDiskQuota::new(
            root.path(),
            TranscodeDiskQuotaConfig::new(1_000, 100, Duration::from_secs(60)),
        );

        let first = quota.reserve().await.unwrap();
        let second = quota.reserve().await.unwrap();
        let snapshot = quota.snapshot().await.unwrap();
        assert_eq!(snapshot.successful_scans, 1);
        assert_eq!(snapshot.active_reservations, 2);
        assert_eq!(snapshot.reserved_bytes, 200);

        drop(first);
        drop(second);
    }

    #[tokio::test]
    async fn transcode_disk_quota_remeasures_after_a_writer_releases_its_reservation() {
        let root = tempfile::tempdir().unwrap();
        let quota = TranscodeDiskQuota::new(
            root.path(),
            TranscodeDiskQuotaConfig::new(100, 40, Duration::from_secs(60)),
        );
        let reservation = quota.reserve().await.unwrap();
        tokio::fs::write(root.path().join("output.ts"), [0_u8; 80])
            .await
            .unwrap();
        drop(reservation);

        let error = quota.reserve().await.unwrap_err();
        assert!(matches!(
            error,
            TranscodeDiskQuotaError::Exhausted {
                usage_bytes: 80,
                reserved_bytes: 0,
                requested_bytes: 40,
                ..
            }
        ));
        let snapshot = quota.snapshot().await.unwrap();
        assert_eq!(snapshot.usage_bytes, Some(80));
        assert_eq!(snapshot.successful_scans, 2);
    }

    #[tokio::test]
    async fn transcode_disk_quota_shared_monitor_wakes_every_waiter() {
        let root = tempfile::tempdir().unwrap();
        let quota = TranscodeDiskQuota::new(
            root.path(),
            TranscodeDiskQuotaConfig::new(100, 10, Duration::from_millis(10)),
        );
        let reservation = quota.reserve().await.unwrap();
        let first_quota = quota.clone();
        let second_quota = quota.clone();
        let first = tokio::spawn(async move { first_quota.wait_until_exceeded().await });
        let second = tokio::spawn(async move { second_quota.wait_until_exceeded().await });

        tokio::fs::write(root.path().join("full.ts"), [0_u8; 100])
            .await
            .unwrap();
        timeout(Duration::from_secs(1), async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await
        .unwrap();

        let snapshot = quota.snapshot().await.unwrap();
        assert_eq!(snapshot.usage_bytes, Some(100));
        assert!(snapshot.quota_exceeded);
        assert!(snapshot.monitor_running);
        assert_eq!(snapshot.active_reservations, 1);
        assert!(snapshot.successful_scans >= 2);

        drop(reservation);
        timeout(Duration::from_secs(1), async {
            loop {
                if !quota.snapshot().await.unwrap().monitor_running {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn transcode_disk_quota_breach_stays_visible_until_all_writers_release() {
        let root = tempfile::tempdir().unwrap();
        let quota = TranscodeDiskQuota::new(
            root.path(),
            TranscodeDiskQuotaConfig::new(100, 10, Duration::from_secs(60)),
        );
        let reservation = quota.reserve().await.unwrap();
        let output = root.path().join("full.ts");
        tokio::fs::write(&output, [0_u8; 100]).await.unwrap();
        assert!(quota.refresh_snapshot().await.unwrap().quota_exceeded);

        tokio::fs::remove_file(output).await.unwrap();
        let cleaned = quota.refresh_snapshot().await.unwrap();
        assert_eq!(cleaned.usage_bytes, Some(0));
        assert!(cleaned.quota_exceeded);
        timeout(Duration::from_millis(100), quota.wait_until_exceeded())
            .await
            .expect("a cleanup must not erase a breach before active waiters observe it");

        drop(reservation);
        let reset = quota.refresh_snapshot().await.unwrap();
        assert!(!reset.quota_exceeded);
    }

    #[tokio::test]
    async fn transcode_disk_quota_fails_closed_when_the_root_cannot_be_scanned() {
        let root = tempfile::tempdir().unwrap();
        let not_a_directory = root.path().join("file");
        tokio::fs::write(&not_a_directory, b"not a directory")
            .await
            .unwrap();
        let quota = TranscodeDiskQuota::new(
            &not_a_directory,
            TranscodeDiskQuotaConfig::new(100, 10, Duration::from_secs(60)),
        );

        assert!(matches!(
            quota.reserve().await,
            Err(TranscodeDiskQuotaError::Io(_))
        ));
        let snapshot = quota.cached_snapshot();
        assert_eq!(snapshot.usage_bytes, None);
        assert_eq!(snapshot.failed_scans, 1);
    }

    #[tokio::test]
    async fn transcode_disk_quota_scan_failure_stops_active_writers() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("transcodes");
        tokio::fs::create_dir(&root).await.unwrap();
        let quota = TranscodeDiskQuota::new(
            &root,
            TranscodeDiskQuotaConfig::new(100, 10, Duration::from_secs(60)),
        );
        let reservation = quota.reserve().await.unwrap();

        tokio::fs::remove_dir(&root).await.unwrap();
        assert!(matches!(
            quota.refresh_snapshot().await,
            Err(TranscodeDiskQuotaError::Io(_))
        ));
        let failed = quota.cached_snapshot();
        assert_eq!(failed.active_reservations, 1);
        assert_eq!(failed.failed_scans, 1);
        assert!(failed.quota_exceeded);
        timeout(
            Duration::from_millis(100),
            reservation.wait_until_exceeded(),
        )
        .await
        .expect("an active writer must stop when its quota root cannot be inspected");

        drop(reservation);
        assert!(!quota.cached_snapshot().quota_exceeded);
    }

    #[test]
    fn hls_layout_uses_sanitized_session_directory_and_expected_names() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "../play session:1");

        assert_eq!(layout.session_dir, root.path().join("___play_session_1"));
        assert_eq!(
            layout.master_playlist_path,
            layout.session_dir.join(HLS_MASTER_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.media_playlist_path,
            layout.session_dir.join(HLS_MEDIA_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.segment_pattern_path,
            layout.session_dir.join("segment_%05d.ts")
        );
        assert_eq!(
            layout.segment_path(7),
            layout.session_dir.join("segment_00007.ts")
        );
    }

    #[test]
    fn hls_layout_can_be_derived_from_persisted_output_path() {
        let root = tempfile::tempdir().unwrap();
        let output_path = root.path().join("play-1").join(HLS_MEDIA_PLAYLIST_NAME);
        let layout = HlsTranscodeLayout::from_media_playlist_path(&output_path);

        assert_eq!(layout.session_dir, root.path().join("play-1"));
        assert_eq!(layout.media_playlist_path, output_path);
        assert_eq!(
            layout.master_playlist_path,
            root.path().join("play-1").join(HLS_MASTER_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.segment_pattern_path,
            root.path().join("play-1").join("segment_%05d.ts")
        );
    }

    #[test]
    fn renders_hls_master_playlist_snapshot() {
        let playlist = render_hls_master_playlist(&HlsVariantInfo {
            uri: HLS_MEDIA_PLAYLIST_NAME.to_string(),
            bandwidth: 4_000_000,
            resolution: Some((1280, 720)),
            codecs: Some("avc1.4d401f,mp4a.40.2".to_string()),
        });

        assert_eq!(
            playlist,
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\n\
             main.m3u8\n"
        );
    }

    #[test]
    fn renders_hls_media_playlist_snapshot() {
        let playlist = render_hls_media_playlist(
            3,
            0,
            &[
                HlsSegment {
                    duration_seconds: 3.003,
                    uri: "segment_00000.ts".to_string(),
                },
                HlsSegment {
                    duration_seconds: 2.5,
                    uri: "segment_00001.ts".to_string(),
                },
            ],
            true,
        );

        assert_eq!(
            playlist,
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:3\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:3.003,\n\
             segment_00000.ts\n\
             #EXTINF:2.500,\n\
             segment_00001.ts\n\
             #EXT-X-ENDLIST\n"
        );
    }

    #[tokio::test]
    async fn hls_readiness_waits_for_playlist_and_first_segment() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "play-1");
        tokio::fs::create_dir_all(&layout.session_dir)
            .await
            .unwrap();
        let media_playlist_path = layout.media_playlist_path.clone();
        let first_segment_path = layout.segment_path(0);

        let write_playlist = media_playlist_path.clone();
        let write_segment = first_segment_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::fs::write(write_playlist, b"#EXTM3U\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::fs::write(write_segment, b"ts").await.unwrap();
        });

        assert!(
            wait_for_hls_readiness(
                &media_playlist_path,
                &first_segment_path,
                Duration::from_secs(5)
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn hls_readiness_times_out_without_first_segment() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "play-1");
        tokio::fs::create_dir_all(&layout.session_dir)
            .await
            .unwrap();
        tokio::fs::write(&layout.media_playlist_path, b"#EXTM3U\n")
            .await
            .unwrap();

        assert!(
            !wait_for_hls_readiness(
                &layout.media_playlist_path,
                layout.segment_path(0),
                Duration::from_millis(100)
            )
            .await
            .unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_command_output_captures_unterminated_pipes_and_cleans_descendant() {
        let root = tempfile::tempdir().unwrap();
        let child_pid_path = root.path().join("child.pid");
        let mut command = Command::new("sh");
        let script = "sleep 30 & child=$!; printf '%s' \"$child\" > \"$1\"; printf stdout-without-newline; printf stderr-without-newline >&2; exit 0";
        command
            .args(["-c", script, "jellyrin-bounded-output"])
            .arg(&child_pid_path);

        let output = timeout(
            Duration::from_secs(3),
            run_bounded_command_output(
                command,
                BoundedCommandOutputOptions::new(Duration::from_secs(2), 1024, 1024),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout-without-newline");
        assert_eq!(output.stderr, b"stderr-without-newline");
        let child_pid = tokio::fs::read_to_string(child_pid_path)
            .await
            .unwrap()
            .parse::<libc::pid_t>()
            .unwrap();
        wait_for_unix_process_exit(child_pid).await;
    }

    #[tokio::test]
    async fn bounded_command_output_drains_oversized_unterminated_pipes() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 20000 ]; do printf x; printf y >&2; i=$((i + 1)); done",
        ]);

        let error = timeout(
            Duration::from_secs(5),
            run_bounded_command_output(
                command,
                BoundedCommandOutputOptions::new(Duration::from_secs(4), 1024, 1024),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();

        assert!(matches!(
            error,
            BoundedCommandOutputError::OutputLimitExceeded {
                stream: BoundedCommandOutputStream::Stdout,
                limit: 1024
            }
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_command_output_timeout_terminates_and_reaps_process_group() {
        let root = tempfile::tempdir().unwrap();
        let process_ids_path = root.path().join("process-ids");
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "trap '' TERM; sh -c 'trap \"\" TERM; while :; do sleep 30; done' & child=$!; printf '%s %s' \"$$\" \"$child\" > \"$1\"; while :; do sleep 30; done",
                "jellyrin-bounded-timeout",
            ])
            .arg(&process_ids_path);

        let error = timeout(
            Duration::from_secs(3),
            run_bounded_command_output(
                command,
                BoundedCommandOutputOptions::new(Duration::from_millis(100), 1024, 1024)
                    .with_termination_grace_period(Duration::from_millis(100)),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, BoundedCommandOutputError::TimedOut));

        let process_ids = tokio::fs::read_to_string(process_ids_path).await.unwrap();
        let process_ids = process_ids
            .split_whitespace()
            .map(|value| value.parse::<libc::pid_t>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(process_ids.len(), 2);
        for process_id in process_ids {
            wait_for_unix_process_exit(process_id).await;
        }
    }

    #[tokio::test]
    async fn transcode_process_streams_progress_and_waits_for_exit() {
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "printf 'out_time_us=1000\\nprogress=continue\\nout_time_us=2000\\nprogress=end\\n' >&2"
                    .to_string(),
            ],
        );

        let mut process = spawn_transcode_process(&command).unwrap();
        let mut progress = process.subscribe_progress();
        assert!(process.process_id().is_some());

        let first = timeout(Duration::from_secs(5), progress.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.position_ticks(), Some(10000));
        assert_eq!(first.progress.as_deref(), Some("continue"));

        let second = timeout(Duration::from_secs(5), progress.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.position_ticks(), Some(20000));
        assert!(second.is_complete());

        let exit = process.wait().await.unwrap();
        assert!(exit.success);
        assert_eq!(exit, process.wait().await.unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_linux_process_stats_with_complex_command_names() {
        let stat = "123 (ffmpeg worker) S 1 2 3 4 5 6 7 8 9 10 120 30 13 14 15 16 17 18 4242 20 7";
        assert_eq!(
            parse_linux_process_stat(stat),
            Some(LinuxProcessStat {
                cpu_ticks: 150,
                start_time_ticks: 4242,
                rss_pages: 7,
            })
        );
        assert_eq!(parse_linux_process_stat("invalid"), None);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn transcode_process_exposes_linux_resource_samples() {
        let command = FfmpegCommandSpec::new("sh", vec!["-c".to_string(), "sleep 1".to_string()]);
        let mut process = spawn_transcode_process(&command).unwrap();
        let mut resources = process.subscribe_resources();

        timeout(Duration::from_secs(1), resources.changed())
            .await
            .unwrap()
            .unwrap();
        let sample = resources
            .borrow_and_update()
            .expect("Linux resource sample");
        assert!(sample.rss_bytes > 0);
        assert_eq!(process.latest_resource_sample(), Some(sample));
        assert!(process.wait().await.unwrap().success);
        assert!(process.resource_task.is_none());
        assert!(resources.changed().await.is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_resource_sampler_computes_cpu_delta_without_polling_processes() {
        let mut child = Command::new("sh")
            .args(["-c", "while :; do :; done"])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let process_id = child.id().unwrap();
        let (samples_tx, mut samples_rx) = tokio::sync::watch::channel(None);
        let sampler = tokio::spawn(super::sample_linux_process_resources(
            process_id,
            samples_tx,
            Duration::from_millis(25),
        ));

        timeout(Duration::from_secs(1), samples_rx.changed())
            .await
            .unwrap()
            .unwrap();
        let first = samples_rx.borrow_and_update().unwrap();
        timeout(Duration::from_secs(1), samples_rx.changed())
            .await
            .unwrap()
            .unwrap();
        let second = samples_rx.borrow_and_update().unwrap();
        assert!(second.cpu_time_millis >= first.cpu_time_millis);
        assert!(
            second
                .cpu_percent
                .is_some_and(|value| value.is_finite() && value >= 0.0)
        );
        assert!(second.rss_bytes > 0);

        child.kill().await.unwrap();
        let _ = child.wait().await;
        sampler.abort();
        let _ = sampler.await;
    }

    #[tokio::test]
    async fn transcode_process_drains_stderr_without_progress_subscriber() {
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "i=0; while [ $i -lt 200 ]; do printf 'out_time_us=%s\\nprogress=continue\\n' \"$i\" >&2; i=$((i + 1)); done"
                    .to_string(),
            ],
        );

        let mut process = spawn_transcode_process(&command).unwrap();
        let exit = timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(exit.success);
    }

    #[test]
    fn stderr_parser_bounds_an_unterminated_line_and_resumes_after_newline() {
        let (progress_tx, mut progress_rx) = tokio::sync::broadcast::channel(4);
        let mut progress = jellyrin_core::FfmpegProgress::default();
        let mut line_buffer = Vec::with_capacity(FFMPEG_STDERR_MAX_LINE_BYTES);
        let mut discarding_oversized_line = false;
        let oversized_line = vec![b'x'; FFMPEG_STDERR_MAX_LINE_BYTES * 8];

        for chunk in oversized_line.chunks(997) {
            consume_ffmpeg_stderr_chunk(
                chunk,
                &mut line_buffer,
                &mut discarding_oversized_line,
                &mut progress,
                &progress_tx,
            );
            assert!(line_buffer.len() <= FFMPEG_STDERR_MAX_LINE_BYTES);
        }
        assert!(discarding_oversized_line);
        assert!(line_buffer.is_empty());

        consume_ffmpeg_stderr_chunk(
            b"\nout_time_us=2000\nprogress=end\n",
            &mut line_buffer,
            &mut discarding_oversized_line,
            &mut progress,
            &progress_tx,
        );

        let snapshot = progress_rx.try_recv().unwrap();
        assert_eq!(snapshot.position_ticks(), Some(20000));
        assert!(snapshot.is_complete());
    }

    #[tokio::test]
    async fn stderr_reader_continues_draining_an_unterminated_oversized_line() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let (progress_tx, mut progress_rx) = tokio::sync::broadcast::channel(4);
        let reader_task = tokio::spawn(read_ffmpeg_progress(reader, progress_tx));
        let oversized_line = vec![b'x'; FFMPEG_STDERR_MAX_LINE_BYTES * 8];

        timeout(Duration::from_secs(2), async {
            writer.write_all(&oversized_line).await.unwrap();
            writer
                .write_all(b"\nout_time_us=3000\nprogress=end\n")
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        })
        .await
        .unwrap();
        timeout(Duration::from_secs(2), reader_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let snapshot = progress_rx.try_recv().unwrap();
        assert_eq!(snapshot.position_ticks(), Some(30000));
        assert!(snapshot.is_complete());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_leads_a_dedicated_process_group() {
        let command = FfmpegCommandSpec::new("sleep", vec!["30".to_string()]);
        let mut process = spawn_transcode_process(&command).unwrap();
        let process_id = libc::pid_t::try_from(process.process_id().unwrap()).unwrap();

        // SAFETY: the child is kept alive by `process`, and `getpgid` only
        // inspects kernel process metadata.
        let process_group_id = unsafe { libc::getpgid(process_id) };
        assert_eq!(process_group_id, process_id);

        let exit = process
            .stop_with_grace_period(Duration::ZERO)
            .await
            .unwrap();
        assert!(!exit.success);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_stop_allows_a_graceful_group_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let ready_path = root.path().join("ready");
        let terminated_path = root.path().join("terminated");
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "trap 'printf terminated > \"$2\"; exit 0' TERM; printf ready > \"$1\"; while :; do :; done"
                    .to_string(),
                "jellyrin-test".to_string(),
                ready_path.to_string_lossy().into_owned(),
                terminated_path.to_string_lossy().into_owned(),
            ],
        );
        let mut process = spawn_transcode_process(&command).unwrap();

        timeout(Duration::from_secs(2), async {
            while tokio::fs::metadata(&ready_path).await.is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let exit = timeout(
            Duration::from_secs(2),
            process.stop_with_grace_period(Duration::from_secs(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(exit.success);
        assert_eq!(
            tokio::fs::read_to_string(terminated_path).await.unwrap(),
            "terminated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_stop_escalates_and_kills_the_whole_group() {
        let root = tempfile::tempdir().unwrap();
        let ready_path = root.path().join("ready");
        let term_path = root.path().join("term");
        let child_ready_path = root.path().join("child-ready");
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "sh -c 'trap \"\" TERM; printf child-ready > \"$1\"; while :; do sleep 30; done' jellyrin-child \"$3\" & trap 'printf term > \"$2\"' TERM; while [ ! -f \"$3\" ]; do :; done; printf ready > \"$1\"; while :; do :; done"
                    .to_string(),
                "jellyrin-test".to_string(),
                ready_path.to_string_lossy().into_owned(),
                term_path.to_string_lossy().into_owned(),
                child_ready_path.to_string_lossy().into_owned(),
            ],
        );
        let mut process = spawn_transcode_process(&command).unwrap();

        timeout(Duration::from_secs(2), async {
            while tokio::fs::metadata(&ready_path).await.is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let exit = timeout(
            Duration::from_secs(2),
            process.stop_with_grace_period(Duration::from_millis(100)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!exit.success);
        assert_eq!(tokio::fs::read_to_string(term_path).await.unwrap(), "term");
    }

    #[tokio::test]
    async fn transcode_process_stop_is_idempotent() {
        let command = FfmpegCommandSpec::new("sleep", vec!["30".to_string()]);
        let mut process = spawn_transcode_process(&command).unwrap();
        assert!(process.process_id().is_some());

        let exit = timeout(Duration::from_secs(5), process.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(!exit.success);
        assert_eq!(exit, process.stop().await.unwrap());
    }

    #[tokio::test]
    async fn transcode_process_with_stdin_forwards_pipe_bytes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("stdin.out");
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                format!("cat > {}", output.to_string_lossy()),
            ],
        );

        let (mut process, mut stdin) = spawn_transcode_process_with_stdin(&command).unwrap();
        stdin.write_all(b"pipe-bytes").await.unwrap();
        stdin.shutdown().await.unwrap();
        drop(stdin);

        let exit = timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(exit.success);
        assert_eq!(tokio::fs::read(output).await.unwrap(), b"pipe-bytes");
    }

    #[tokio::test]
    async fn transcode_process_spawn_failure_is_reported() {
        let command = FfmpegCommandSpec::new("definitely-not-a-jellyrin-command", Vec::new());

        assert!(spawn_transcode_process(&command).is_err());
    }

    #[test]
    fn command_classification_separates_video_audio_and_remux() {
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map", "0:v", "-map", "0:a", "-c:v", "libx264", "-c:a", "aac",
            ])),
            TranscodeJobKind::VideoEncode
        );
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map", "0:v", "-map", "0:a", "-c:v", "copy", "-c:a", "aac",
            ])),
            TranscodeJobKind::AudioEncode
        );
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map",
                "0:a",
                "-filter:a:0",
                "volume=0.5",
                "-c:a",
                "copy",
            ])),
            TranscodeJobKind::AudioEncode
        );
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map", "0:v", "-map", "0:a", "-map", "0:s", "-codec:v", "COPY", "-acodec", "copy",
                "-c:s", "webvtt",
            ])),
            TranscodeJobKind::AudioEncode
        );
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map", "0:v", "-map", "0:a", "-c:v", "copy", "-c:a", "copy",
            ])),
            TranscodeJobKind::Remux
        );
    }

    #[test]
    fn command_classification_fails_closed_for_aliases_filters_and_untrusted_specs() {
        let adversarial = [
            vec!["-c", "libx264"],
            vec!["-codec", "libx264"],
            vec!["-c:v:0", "libx264"],
            vec!["-codec:v:0", "libx264"],
            vec!["-c:0", "libx264"],
            vec!["-vcodec:0", "libx264"],
            vec!["-vf", "scale=640:360", "-c:v", "copy"],
            vec!["-filter_complex", "[0:v]scale=640:360[v]", "-c:v", "copy"],
            vec!["-filter_script:v", "/tmp/filter", "-c:v", "copy"],
            vec!["-filter_complex_script", "/tmp/filter", "-c:v", "copy"],
            vec!["-target", "pal-dvd", "-c:v", "copy"],
        ];
        for args in adversarial {
            assert_eq!(
                classify_ffmpeg_args(&args.into_iter().map(str::to_string).collect::<Vec<_>>()),
                TranscodeJobKind::VideoEncode
            );
        }

        assert_eq!(
            classify_ffmpeg_args(&strings(&["output.mp4"])),
            TranscodeJobKind::VideoEncode
        );
        assert_eq!(
            classify_ffmpeg_args(&strings(&[
                "-map",
                "0:v",
                "-map",
                "0:a",
                "-c:a",
                "copy",
                "output.mp4",
            ])),
            TranscodeJobKind::VideoEncode
        );

        let untrusted = FfmpegCommandSpec::new(
            "ffmpeg",
            vec!["-c:v".into(), "copy".into(), "-c:a".into(), "copy".into()],
        );
        assert_eq!(
            classify_transcode_command(&untrusted),
            TranscodeJobKind::VideoEncode
        );
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[tokio::test]
    async fn resource_caps_coordinator_holds_each_job_class_to_its_limit() {
        let coordinator = TranscodeCoordinator::new(TranscodeLimits {
            max_total_jobs: 7,
            max_video_encodes: 1,
            max_audio_encodes: 2,
            max_remuxes: 3,
            max_auxiliary: 1,
            max_probes: 1,
        });
        let first = coordinator
            .acquire(TranscodeJobKind::VideoEncode)
            .await
            .unwrap();
        assert_eq!(first.kind(), TranscodeJobKind::VideoEncode);
        assert_eq!(
            coordinator.available_permits(TranscodeJobKind::VideoEncode),
            0
        );
        assert!(matches!(
            coordinator.try_acquire(TranscodeJobKind::VideoEncode),
            Err(tokio::sync::TryAcquireError::NoPermits)
        ));

        assert!(
            timeout(
                Duration::from_millis(25),
                coordinator.acquire(TranscodeJobKind::VideoEncode)
            )
            .await
            .is_err()
        );
        drop(first);
        assert!(
            timeout(
                Duration::from_secs(1),
                coordinator.acquire(TranscodeJobKind::VideoEncode)
            )
            .await
            .unwrap()
            .is_ok()
        );

        let auxiliary = coordinator
            .acquire(TranscodeJobKind::Auxiliary)
            .await
            .unwrap();
        assert_eq!(auxiliary.kind(), TranscodeJobKind::Auxiliary);
        assert_eq!(
            coordinator.available_permits(TranscodeJobKind::Auxiliary),
            0
        );
    }

    #[tokio::test]
    async fn resource_caps_coordinator_enforces_the_aggregate_ffmpeg_limit() {
        let coordinator = TranscodeCoordinator::new(TranscodeLimits {
            max_total_jobs: 1,
            max_video_encodes: 2,
            max_audio_encodes: 2,
            max_remuxes: 2,
            max_auxiliary: 2,
            max_probes: 2,
        });
        let video = coordinator
            .acquire(TranscodeJobKind::VideoEncode)
            .await
            .unwrap();
        assert_eq!(coordinator.available_total_permits(), 0);
        assert_eq!(
            coordinator.available_permits(TranscodeJobKind::Auxiliary),
            2
        );
        assert!(matches!(
            coordinator.try_acquire(TranscodeJobKind::Auxiliary),
            Err(tokio::sync::TryAcquireError::NoPermits)
        ));

        drop(video);
        let auxiliary = coordinator
            .try_acquire(TranscodeJobKind::Auxiliary)
            .unwrap();
        assert_eq!(auxiliary.kind(), TranscodeJobKind::Auxiliary);
        assert_eq!(coordinator.available_total_permits(), 0);
    }

    #[test]
    fn multimedia_process_configuration_values_are_bounded() {
        assert_eq!(super::process_limit_from_value(None, 2), 2);
        assert_eq!(super::process_limit_from_value(Some(" 1 "), 2), 1);
        assert_eq!(super::process_limit_from_value(Some("64"), 2), 64);
        assert_eq!(super::process_limit_from_value(Some("0"), 2), 2);
        assert_eq!(super::process_limit_from_value(Some("65"), 2), 2);
        assert_eq!(super::process_limit_from_value(Some("invalid"), 2), 2);
        assert_eq!(super::bounded_usize_from_value(Some("0"), 0, 128, 8), 0);
        assert_eq!(super::bounded_usize_from_value(Some("128"), 0, 128, 8), 128);
        assert_eq!(super::bounded_usize_from_value(Some("129"), 0, 128, 8), 8);
        assert_eq!(super::bounded_u64_from_value(Some("1"), 1, 120, 10), 1);
        assert_eq!(super::bounded_u64_from_value(Some("121"), 1, 120, 10), 10);
        assert!(std::ptr::eq(
            super::multimedia_process_coordinator(),
            super::multimedia_process_coordinator()
        ));
    }

    #[tokio::test]
    async fn multimedia_process_probe_and_encode_share_the_aggregate_limit() {
        let coordinator = TranscodeCoordinator::new(TranscodeLimits {
            max_total_jobs: 1,
            max_video_encodes: 1,
            max_audio_encodes: 1,
            max_remuxes: 1,
            max_auxiliary: 1,
            max_probes: 1,
        });
        let encode = coordinator
            .acquire(TranscodeJobKind::VideoEncode)
            .await
            .unwrap();
        assert!(matches!(
            coordinator.try_acquire(TranscodeJobKind::Probe),
            Err(tokio::sync::TryAcquireError::NoPermits)
        ));
        drop(encode);
        let probe = coordinator.try_acquire(TranscodeJobKind::Probe).unwrap();
        assert_eq!(probe.kind(), TranscodeJobKind::Probe);
        assert_eq!(coordinator.available_total_permits(), 0);
    }

    #[tokio::test]
    async fn multimedia_process_probe_queue_is_bounded_timeout_and_cancel_safe() {
        let coordinator = TranscodeCoordinator::new(TranscodeLimits {
            max_total_jobs: 1,
            max_video_encodes: 1,
            max_audio_encodes: 1,
            max_remuxes: 1,
            max_auxiliary: 1,
            max_probes: 1,
        });
        let encode = coordinator
            .acquire(TranscodeJobKind::VideoEncode)
            .await
            .unwrap();
        let wait_queue = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let error = super::acquire_probe_with_queue(
            &coordinator,
            wait_queue.clone(),
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error, super::ProbeAdmissionError::TimedOut);
        assert_eq!(wait_queue.available_permits(), 1);

        let error = super::acquire_probe_with_queue(
            &coordinator,
            std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error, super::ProbeAdmissionError::WaitQueueFull);
        drop(encode);
        assert!(coordinator.try_acquire(TranscodeJobKind::Probe).is_ok());
    }

    #[tokio::test]
    async fn saturated_lane_does_not_reserve_capacity_from_other_ffmpeg_lanes() {
        let coordinator = TranscodeCoordinator::new(TranscodeLimits {
            max_total_jobs: 2,
            max_video_encodes: 1,
            max_audio_encodes: 1,
            max_remuxes: 1,
            max_auxiliary: 1,
            max_probes: 1,
        });
        let first_video = coordinator
            .acquire(TranscodeJobKind::VideoEncode)
            .await
            .unwrap();
        let waiting_coordinator = coordinator.clone();
        let waiting_video = tokio::spawn(async move {
            waiting_coordinator
                .acquire(TranscodeJobKind::VideoEncode)
                .await
                .unwrap()
        });
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }

        assert_eq!(coordinator.available_total_permits(), 1);
        let remux = coordinator
            .try_acquire(TranscodeJobKind::Remux)
            .expect("a video-lane waiter must not block free remux capacity");
        assert_eq!(coordinator.available_total_permits(), 0);

        drop(first_video);
        let second_video = timeout(Duration::from_secs(1), waiting_video)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second_video.kind(), TranscodeJobKind::VideoEncode);
        drop(second_video);
        drop(remux);
    }
}
