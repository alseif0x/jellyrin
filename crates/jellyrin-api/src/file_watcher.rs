use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jellyrin_db::Database;
use notify::RecursiveMode;
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
    let debouncer = spawn_watcher(locations, tx)?;
    Ok((debouncer, rx))
}

/// Get all filesystem locations from all virtual folders.
async fn all_watch_locations(db: &Database) -> anyhow::Result<Vec<PathBuf>> {
    let folders = db.virtual_folders().await?;
    let mut locations = Vec::new();
    for folder in folders {
        for location in &folder.locations {
            let path = PathBuf::from(location);
            if path.exists() && path.is_dir() {
                locations.push(path);
            }
        }
    }
    Ok(locations)
}

/// Spawn a debounced watcher for the given locations.
fn spawn_watcher(
    locations: Vec<PathBuf>,
    tx: mpsc::Sender<Vec<FileChangeEvent>>,
) -> anyhow::Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
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
                let change_type = match event.kind {
                    DebouncedEventKind::Any => FileChangeType::Modified,
                    _ => return None,
                };
                let path = event.path;
                if !is_watched_file(&path) {
                    return None;
                }
                Some(FileChangeEvent { path, change_type })
            })
            .collect::<Vec<_>>();
        if !changes.is_empty() {
            let _ = tx.blocking_send(changes);
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

    Ok(debouncer)
}

/// Check if a file path should be watched (media file or NFO, not in ignored directory).
fn is_watched_file(path: &Path) -> bool {
    // Check if any component of the path starts with an ignored prefix
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if IGNORED_DIR_PREFIXES
                .iter()
                .any(|prefix| name_str.starts_with(prefix))
            {
                return false;
            }
            if IGNORED_DIR_NAMES.iter().any(|&ignored| name_str == ignored) {
                return false;
            }
        }
    }

    // Check file extension
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| WATCHED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
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
}
