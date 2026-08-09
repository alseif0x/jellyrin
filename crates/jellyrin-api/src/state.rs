use std::path::PathBuf;

use crate::Database;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub web_dir: PathBuf,
    pub log_dir: PathBuf,
    pub local_address: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SystemLifecycleCommand {
    Restart,
    Shutdown,
}
