use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use jellyrin_db::Database;
use notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_mini::{DebounceEventResult, DebouncedEventKind, new_debouncer};
use tokio::sync::mpsc;

/// Describes a change detected by the file watcher.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub path: PathBuf,
    pub change_type: FileChangeType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

/// Extensions of media files we care about.
const WATCHED_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "wmv", "m4v", "webm", // Video
    "mp3", "flac", "m4a", "aac", "ogg", "wav", // Audio
    "jpg", "jpeg", "png", "webp", "gif", "bmp", // Photo
    "epub", "pdf", "cbz", "cbr", // Book
    "nfo", // Metadata
];

/// Directories to ignore.
const IGNORED_DIR_PREFIXES: &[&str] = &[".jellyrin-", "."];
const IGNORED_DIR_NAMES: &[&str] = &["metadata", "node_modules", "target"];

/// Debounce duration before triggering a scan.
const DEBOUNCE_DURATION: Duration = Duration::from_secs(5);
/// Allow an in-flight raw notification to reach the initialization callback before hand-off.
const INITIAL_WATCHER_HANDOFF_SETTLE: Duration = Duration::from_millis(100);

/// Start the file watcher for all virtual folder locations.
/// Returns the debouncer handle (dropping it stops watching) and a receiver for change events.
pub async fn start_file_watcher(
    db: &Database,
) -> anyhow::Result<(
    notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    mpsc::Receiver<Vec<FileChangeEvent>>,
)> {
    let locations = all_watch_locations(db).await?;
    let (tx, rx) = mpsc::channel(64);
    let debouncer = tokio::task::spawn_blocking(move || spawn_watcher(locations, tx))
        .await
        .map_err(|error| anyhow::anyhow!("file watcher startup task failed: {error}"))??;
    Ok((debouncer, rx))
}

/// Get all filesystem locations from all virtual folders.
async fn all_watch_locations(db: &Database) -> anyhow::Result<Vec<PathBuf>> {
    let folders = db.virtual_folders().await?;
    let mut locations = Vec::new();
    for folder in folders {
        for location in &folder.locations {
            locations.push(PathBuf::from(location));
        }
    }
    locations.sort_unstable();
    locations.dedup();
    Ok(locations)
}

/// Spawn a debounced watcher for the given locations.
fn spawn_watcher(
    mut locations: Vec<PathBuf>,
    tx: mpsc::Sender<Vec<FileChangeEvent>>,
) -> anyhow::Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    // Network mounts and very large directory trees can make both these checks and the
    // initial metadata walk expensive. This entire function runs on Tokio's blocking pool.
    locations.retain(|location| {
        if location.is_dir() {
            true
        } else {
            tracing::warn!(?location, "skipping unavailable watch directory");
            false
        }
    });

    // notify-debouncer-mini intentionally collapses the original notify EventKind into
    // DebouncedEventKind::Any. On Linux that also includes Access(Open) events, so merely
    // reading a media file used to be interpreted as a modification. Keep a lightweight
    // metadata snapshot to distinguish real creates/changes/removals after debouncing.
    //
    // Register a raw watcher before walking the tree. It records non-access events during
    // initialization, closing the otherwise large gap between snapshotting a path and
    // registering the final debounced watcher. The two watchers overlap briefly when the
    // snapshot is installed, so there is no hand-off window.
    let file_states = Arc::new(Mutex::new(WatcherFileStates::default()));
    let initializing_file_states = Arc::clone(&file_states);
    let mut initialization_watcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
            Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                let mut file_states = initializing_file_states
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if file_states.snapshot.is_none() {
                    file_states
                        .queued_paths
                        .extend(event.paths.into_iter().filter(|path| is_watched_file(path)));
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(?error, "file watcher initialization error"),
        })?;
    for location in &locations {
        if let Err(error) = initialization_watcher.watch(location, RecursiveMode::Recursive) {
            tracing::warn!(
                ?error,
                ?location,
                "failed to watch directory during initial snapshot"
            );
        }
    }

    let snapshot = initial_file_states(&locations);
    let debounced_file_states = Arc::clone(&file_states);
    let event_tx = tx.clone();
    let mut debouncer = new_debouncer(DEBOUNCE_DURATION, move |result: DebounceEventResult| {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                tracing::warn!(?errors, "file watcher error");
                return;
            }
        };
        let changes = events
            .into_iter()
            .filter_map(|event| {
                if event.kind != DebouncedEventKind::Any {
                    return None;
                }
                let path = event.path;
                if !is_watched_file(&path) {
                    return None;
                }
                let mut file_states = debounced_file_states
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match file_states.snapshot.as_mut() {
                    Some(snapshot) => classify_file_change(path, snapshot),
                    // The raw initialization watcher already records non-access events while
                    // the snapshot is being built. DebouncedEventKind::Any also represents
                    // Access(Open), so queuing it here would turn ordinary reads during startup
                    // into a large batch of false modifications.
                    None => None,
                }
            })
            .collect::<Vec<_>>();
        if !changes.is_empty() {
            let _ = event_tx.blocking_send(changes);
        }
    })?;

    for location in &locations {
        if let Err(error) = debouncer
            .watcher()
            .watch(location, RecursiveMode::Recursive)
        {
            tracing::warn!(?error, ?location, "failed to watch directory");
        } else {
            tracing::info!(?location, "watching directory for changes");
        }
    }

    std::thread::sleep(INITIAL_WATCHER_HANDOFF_SETTLE);
    let initialization_changes = install_initial_file_states(&file_states, snapshot);
    drop(initialization_watcher);
    if !initialization_changes.is_empty() {
        let _ = tx.blocking_send(initialization_changes);
    }

    Ok(debouncer)
}

#[derive(Debug, Default)]
struct WatcherFileStates {
    snapshot: Option<HashMap<PathBuf, WatchedFileState>>,
    queued_paths: HashSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchedFileState {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change_time_seconds: i64,
    #[cfg(unix)]
    change_time_nanoseconds: i64,
}

fn watched_file_state(path: &Path) -> Option<WatchedFileState> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(WatchedFileState {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        change_time_seconds: metadata.ctime(),
        #[cfg(unix)]
        change_time_nanoseconds: metadata.ctime_nsec(),
    })
}

fn classify_file_change(
    path: PathBuf,
    file_states: &mut HashMap<PathBuf, WatchedFileState>,
) -> Option<FileChangeEvent> {
    match (file_states.get(&path), watched_file_state(&path)) {
        (None, Some(current)) => {
            file_states.insert(path.clone(), current);
            Some(FileChangeEvent {
                path,
                change_type: FileChangeType::Created,
            })
        }
        (Some(previous), Some(current)) if previous != &current => {
            file_states.insert(path.clone(), current);
            Some(FileChangeEvent {
                path,
                change_type: FileChangeType::Modified,
            })
        }
        (Some(_), Some(_)) => None,
        (Some(_), None) => {
            file_states.remove(&path);
            Some(FileChangeEvent {
                path,
                change_type: FileChangeType::Deleted,
            })
        }
        (None, None) => None,
    }
}

fn install_initial_file_states(
    file_states: &Mutex<WatcherFileStates>,
    mut snapshot: HashMap<PathBuf, WatchedFileState>,
) -> Vec<FileChangeEvent> {
    let mut file_states = file_states
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let queued_paths = std::mem::take(&mut file_states.queued_paths);
    let changes = queued_paths
        .into_iter()
        .map(|path| classify_initialization_change(path, &mut snapshot))
        .collect();
    file_states.snapshot = Some(snapshot);
    changes
}

fn classify_initialization_change(
    path: PathBuf,
    file_states: &mut HashMap<PathBuf, WatchedFileState>,
) -> FileChangeEvent {
    let change_type = match watched_file_state(&path) {
        Some(current) => {
            let change_type = if file_states.contains_key(&path) {
                FileChangeType::Modified
            } else {
                FileChangeType::Created
            };
            file_states.insert(path.clone(), current);
            change_type
        }
        None => {
            file_states.remove(&path);
            FileChangeType::Deleted
        }
    };
    FileChangeEvent { path, change_type }
}

fn initial_file_states(locations: &[PathBuf]) -> HashMap<PathBuf, WatchedFileState> {
    let mut states = HashMap::new();
    let mut pending = locations.to_vec();
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(?error, ?directory, "failed to snapshot watched directory");
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if !is_ignored_path(&path) {
                    pending.push(path);
                }
            } else if is_watched_file(&path)
                && let Some(state) = watched_file_state(&path)
            {
                states.insert(path, state);
            }
        }
    }
    states
}

/// Check if a file path should be watched (media file or NFO, not in ignored directory).
fn is_watched_file(path: &Path) -> bool {
    if is_ignored_path(path) {
        return false;
    }

    // Check file extension
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| WATCHED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_ignored_path(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if IGNORED_DIR_PREFIXES
                .iter()
                .any(|prefix| name_str.starts_with(prefix))
            {
                return true;
            }
            if IGNORED_DIR_NAMES.iter().any(|&ignored| name_str == ignored) {
                return true;
            }
        }
    }
    false
}

/// Deduplicate changes: if the same path has multiple events, keep only the most recent.
pub fn deduplicate_changes(changes: Vec<FileChangeEvent>) -> Vec<FileChangeEvent> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for change in changes.into_iter().rev() {
        if seen.insert(change.path.clone()) {
            deduped.push(change);
        }
    }
    deduped.reverse();
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn is_watched_file_accepts_media_files() {
        assert!(is_watched_file(Path::new("/media/movie.mkv")));
        assert!(is_watched_file(Path::new("/media/song.mp3")));
        assert!(is_watched_file(Path::new("/media/photo.jpg")));
        assert!(is_watched_file(Path::new("/media/book.epub")));
        assert!(is_watched_file(Path::new("/media/movie.nfo")));
    }

    #[test]
    fn is_watched_file_rejects_non_media() {
        assert!(!is_watched_file(Path::new("/media/readme.txt")));
        assert!(!is_watched_file(Path::new("/media/script.sh")));
        assert!(!is_watched_file(Path::new("/media/data.json")));
    }

    #[test]
    fn is_watched_file_rejects_ignored_directories() {
        assert!(!is_watched_file(Path::new(
            "/media/.jellyrin-cache/temp.mkv"
        )));
        assert!(!is_watched_file(Path::new("/media/metadata/poster.jpg")));
        assert!(!is_watched_file(Path::new("/media/.hidden/movie.mkv")));
        assert!(!is_watched_file(Path::new(
            "/media/node_modules/package/video.mp4"
        )));
    }

    #[test]
    fn deduplicate_changes_merges_same_path() {
        let changes = vec![
            FileChangeEvent {
                path: PathBuf::from("/media/movie.mkv"),
                change_type: FileChangeType::Created,
            },
            FileChangeEvent {
                path: PathBuf::from("/media/movie.mkv"),
                change_type: FileChangeType::Modified,
            },
            FileChangeEvent {
                path: PathBuf::from("/media/other.mkv"),
                change_type: FileChangeType::Created,
            },
        ];
        let deduped = deduplicate_changes(changes);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].change_type, FileChangeType::Modified);
        assert_eq!(deduped[1].change_type, FileChangeType::Created);
    }

    #[test]
    fn access_without_metadata_change_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("movie.mkv");
        fs::write(&path, b"media").unwrap();
        let mut states = HashMap::from([(
            path.clone(),
            watched_file_state(&path).expect("test media file must have metadata"),
        )]);

        assert_eq!(fs::read(&path).unwrap(), b"media");

        assert!(classify_file_change(path, &mut states).is_none());
    }

    #[test]
    fn classifies_created_modified_and_deleted_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("movie.mkv");
        let mut states = initial_file_states(&[temp.path().to_path_buf()]);

        fs::write(&path, b"first").unwrap();
        let created = classify_file_change(path.clone(), &mut states).unwrap();
        assert_eq!(created.change_type, FileChangeType::Created);

        fs::write(&path, b"second version").unwrap();
        let modified = classify_file_change(path.clone(), &mut states).unwrap();
        assert_eq!(modified.change_type, FileChangeType::Modified);

        fs::remove_file(&path).unwrap();
        let deleted = classify_file_change(path, &mut states).unwrap();
        assert_eq!(deleted.change_type, FileChangeType::Deleted);
    }

    #[test]
    fn initialization_event_is_not_lost_when_snapshot_already_has_new_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("movie.mkv");
        fs::write(&path, b"new media state").unwrap();
        // Temp directories are intentionally hidden and therefore excluded by the real
        // traversal; seed the equivalent completed snapshot directly for this hand-off test.
        let snapshot = HashMap::from([(
            path.clone(),
            watched_file_state(&path).expect("test media file must have metadata"),
        )]);
        let file_states = Mutex::new(WatcherFileStates {
            snapshot: None,
            queued_paths: HashSet::from([path.clone()]),
        });

        let changes = install_initial_file_states(&file_states, snapshot);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, path);
        assert_eq!(changes[0].change_type, FileChangeType::Modified);
        assert!(file_states.lock().unwrap().snapshot.is_some());
    }

    #[test]
    fn debounced_event_is_ignored_during_snapshot_and_raw_event_is_reconciled() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("movie.mkv");
        fs::write(&path, b"media created during initialization").unwrap();
        let file_states = Mutex::new(WatcherFileStates::default());

        // This mirrors the debounced callback contract: DebouncedEventKind::Any is
        // intentionally ignored while the initial snapshot is absent. In particular, the
        // debounced path must not be added to queued_paths because Any can mean Access(Open).
        let debounced_change = {
            let mut file_states = file_states.lock().unwrap();
            match file_states.snapshot.as_mut() {
                Some(snapshot) => classify_file_change(path.clone(), snapshot),
                None => None,
            }
        };
        assert!(debounced_change.is_none());
        assert!(file_states.lock().unwrap().queued_paths.is_empty());

        // The raw initialization watcher retains the non-access event instead. Installing the
        // completed snapshot reconciles that path exactly once.
        file_states
            .lock()
            .unwrap()
            .queued_paths
            .insert(path.clone());
        let changes = install_initial_file_states(&file_states, HashMap::new());

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, path);
        assert_eq!(changes[0].change_type, FileChangeType::Created);
        let file_states = file_states.lock().unwrap();
        assert!(file_states.queued_paths.is_empty());
        assert!(file_states.snapshot.as_ref().unwrap().contains_key(&path));
    }
}
