// AeroSync Filesystem Watcher
// Dropbox-style real-time change detection using notify v6 + notify-debouncer-full 0.6
// Replaces dead watcher.rs (226 lines) with production-grade implementation

use notify::event::{ModifyKind, RenameMode};
#[cfg(test)]
use notify::event::{CreateKind, RemoveKind};
use notify::{Config, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Configuration constants
// ---------------------------------------------------------------------------

/// Debounce quiet period — events are batched until no new events arrive for this duration
const DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Tick rate for the debouncer's internal polling loop
const DEBOUNCE_TICK_RATE: Duration = Duration::from_millis(250);

/// Poll interval for PollWatcher fallback (used on network filesystems)
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// inotify subdirectory threshold — warn and consider PollWatcher fallback
const INOTIFY_SUBDIR_THRESHOLD: usize = 8000;

/// Health heartbeat timeout — if no events or heartbeats for this duration, consider unhealthy
#[allow(dead_code)] // Used by status() method
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Watcher operating mode
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WatcherMode {
    /// Native OS watcher (inotify/FSEvents/ReadDirectoryChangesW) with debouncer
    Native,
    /// Poll-based watcher (for network filesystems, NFS, CIFS)
    Poll,
    /// Automatic: try native first, fall back to poll if issues detected
    #[default]
    Auto,
}

/// Current watcher health status
#[allow(dead_code)] // Used by status() method and Tauri command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherStatus {
    /// Whether the watcher is currently running
    pub running: bool,
    /// Active watcher mode
    pub mode: WatcherMode,
    /// Path being watched
    pub watched_path: Option<String>,
    /// Number of events received since start
    pub events_received: u64,
    /// Last event timestamp (ISO 8601)
    pub last_event_at: Option<String>,
    /// Whether watcher is considered healthy
    pub healthy: bool,
    /// Warning message (e.g., inotify limit approaching)
    pub warning: Option<String>,
}

impl Default for WatcherStatus {
    fn default() -> Self {
        Self {
            running: false,
            mode: WatcherMode::Auto,
            watched_path: None,
            events_received: 0,
            last_event_at: None,
            healthy: true,
            warning: None,
        }
    }
}

/// A filesystem change event (our internal representation)
#[derive(Debug, Clone)]
pub struct WatcherEvent {
    /// Paths that changed
    pub paths: Vec<PathBuf>,
    /// Kind of change
    #[allow(dead_code)] // Populated but not yet consumed; will be used by sync engine
    pub kind: WatcherEventKind,
}

/// Simplified event kind for sync engine consumption
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WatcherEventKind {
    Created,
    Modified,
    Removed,
    Renamed,
    Other,
}

impl WatcherEventKind {
    fn from_notify(kind: &EventKind) -> Self {
        match kind {
            EventKind::Create(_) => Self::Created,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => Self::Renamed,
            EventKind::Modify(_) => Self::Modified,
            EventKind::Remove(_) => Self::Removed,
            _ => Self::Other,
        }
    }
}

// ---------------------------------------------------------------------------
// Exclude patterns — reusable filter for watcher events
// ---------------------------------------------------------------------------

/// Suffix patterns to exclude from watcher events (temp files, editor saves)
const EXCLUDE_SUFFIXES: &[&str] = &[
    "~", ".swp", ".swo", ".swn", ".tmp", ".bak", ".crdownload", ".part", ".partial",
];

/// Exact filename matches to exclude (OS metadata files)
const EXCLUDE_EXACT: &[&str] = &["Thumbs.db", "desktop.ini"];

/// Known non-content hidden files/directories to exclude.
/// User dotfiles like .env, .dockerignore, .editorconfig, .htaccess are NOT excluded.
const EXCLUDED_HIDDEN: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    ".bzr",                // VCS
    ".DS_Store",
    ".Spotlight-V100",     // macOS
    ".Trashes",
    ".fseventsd",          // macOS
    ".Trash-1000",         // Linux trash
    ".cache",
    ".local",              // XDG caches
    ".npm",
    ".yarn",
    ".pnpm-store",         // package managers
    "__pycache__",
    ".pytest_cache",       // Python
    ".aerosync-tmp",       // AeroSync temp
];

/// Check if a path should be excluded from sync based on common patterns.
/// This is a quick pre-filter before the full sync exclude list is consulted.
///
/// Only excludes specific known non-content items. User dotfiles like `.env`,
/// `.dockerignore`, `.editorconfig`, `.htaccess` are preserved.
pub fn should_exclude_path(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        // Check suffix patterns (temp/editor files)
        for suffix in EXCLUDE_SUFFIXES {
            if name.ends_with(suffix) {
                return true;
            }
        }

        // Check exact filename matches (OS metadata)
        for exact in EXCLUDE_EXACT {
            if name == *exact {
                return true;
            }
        }

        // Check against known non-content hidden dirs/files (blacklist approach)
        for excluded in EXCLUDED_HIDDEN {
            if name == *excluded {
                return true;
            }
        }
    }

    // Check path components for VCS directories
    for component in path.components() {
        if let Some(s) = component.as_os_str().to_str() {
            if s == ".git" || s == ".svn" || s == ".hg" || s == ".bzr" {
                return true;
            }
        }
    }

    false
}

/// Deduplicate paths from multiple events into a unique set
fn deduplicate_paths(events: &[DebouncedEvent]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for event in events {
        for path in &event.event.paths {
            if !should_exclude_path(path) && seen.insert(path.clone()) {
                result.push(path.clone());
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// inotify limit detection (Linux only)
// ---------------------------------------------------------------------------

/// Count subdirectories under a path (non-recursive depth limit for safety)
fn count_subdirectories(path: &Path) -> usize {
    walkdir::WalkDir::new(path)
        .min_depth(1)
        .max_depth(20) // Safety limit
        .follow_links(false) // Never follow symlinks — prevents escape from sync root
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .take(20_000) // Early exit: stop counting after threshold to prevent DoS
        .count()
}

/// Check if the path has too many subdirectories for inotify
/// Returns (count, should_warn, should_fallback_to_poll)
pub fn check_inotify_capacity(path: &Path) -> (usize, bool, bool) {
    let count = count_subdirectories(path);
    let should_warn = count > INOTIFY_SUBDIR_THRESHOLD / 2; // Warn at 50%
    let should_fallback = count > INOTIFY_SUBDIR_THRESHOLD;
    (count, should_warn, should_fallback)
}

// ---------------------------------------------------------------------------
// FileWatcher
// ---------------------------------------------------------------------------

/// AeroSync filesystem watcher with debouncing and health monitoring.
///
/// Uses `notify-debouncer-full` for event batching and deduplication.
/// Supports automatic fallback from native OS watcher to PollWatcher
/// when network filesystems or inotify limits are detected.
pub struct FileWatcher {
    /// Sender to push watcher events to the async sync engine
    event_tx: mpsc::Sender<WatcherEvent>,
    /// Native debounced watcher (if using native mode)
    native_watcher: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
    /// Poll-based watcher (if using poll mode)
    poll_watcher: Option<PollWatcher>,
    /// Currently watched path
    watched_path: Option<PathBuf>,
    /// Active mode
    mode: WatcherMode,
    /// Event counter
    events_received: Arc<std::sync::atomic::AtomicU64>,
    /// Last event time
    last_event_at: Arc<std::sync::Mutex<Option<Instant>>>,
    /// Running flag
    running: Arc<AtomicBool>,
}

impl FileWatcher {
    /// Create a new FileWatcher with the given event channel sender.
    ///
    /// The receiver end should be consumed by the sync engine's
    /// `tokio::select!` loop to trigger incremental syncs.
    pub fn new(event_tx: mpsc::Sender<WatcherEvent>) -> Self {
        Self {
            event_tx,
            native_watcher: None,
            poll_watcher: None,
            watched_path: None,
            mode: WatcherMode::Auto,
            events_received: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_event_at: Arc::new(std::sync::Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start watching a directory.
    ///
    /// In `Auto` mode, checks inotify capacity on Linux and falls back
    /// to PollWatcher if the directory tree is too large.
    pub fn start(&mut self, path: &Path, mode: WatcherMode) -> Result<(), String> {
        // Stop any existing watcher
        self.stop();

        let resolved_mode = match mode {
            WatcherMode::Auto => {
                #[cfg(target_os = "linux")]
                {
                    let (count, should_warn, should_fallback) = check_inotify_capacity(path);
                    if should_fallback {
                        warn!(
                            "Directory has {} subdirectories (threshold: {}), using PollWatcher",
                            count, INOTIFY_SUBDIR_THRESHOLD
                        );
                        WatcherMode::Poll
                    } else {
                        if should_warn {
                            warn!(
                                "Directory has {} subdirectories, approaching inotify limit",
                                count
                            );
                        }
                        WatcherMode::Native
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    WatcherMode::Native
                }
            }
            other => other,
        };

        match resolved_mode {
            WatcherMode::Native | WatcherMode::Auto => self.start_native(path),
            WatcherMode::Poll => self.start_poll(path),
        }?;

        self.watched_path = Some(path.to_path_buf());
        self.mode = resolved_mode;
        self.running.store(true, Ordering::SeqCst);

        info!(
            "FileWatcher started on {:?} in {:?} mode",
            path, resolved_mode
        );
        Ok(())
    }

    /// Start native debounced watcher
    fn start_native(&mut self, path: &Path) -> Result<(), String> {
        let tx = self.event_tx.clone();
        let events_received = self.events_received.clone();
        let last_event_at = self.last_event_at.clone();

        let mut debouncer = new_debouncer(
            DEBOUNCE_TIMEOUT,
            Some(DEBOUNCE_TICK_RATE),
            move |result: DebounceEventResult| {
                match result {
                    Ok(events) => {
                        let paths = deduplicate_paths(&events);
                        if paths.is_empty() {
                            return;
                        }

                        // Determine primary event kind from first event
                        let kind = events
                            .first()
                            .map(|e| WatcherEventKind::from_notify(&e.event.kind))
                            .unwrap_or(WatcherEventKind::Other);

                        let event = WatcherEvent {
                            paths,
                            kind,
                        };

                        events_received.fetch_add(1, Ordering::Relaxed);
                        if let Ok(mut guard) = last_event_at.lock() {
                            *guard = Some(Instant::now());
                        }

                        // Non-blocking send — if channel is full, log and drop
                        if let Err(e) = tx.try_send(event) {
                            warn!("Watcher event channel full, dropping event: {}", e);
                        }
                    }
                    Err(errors) => {
                        for e in errors {
                            error!("Watcher error: {:?}", e);
                        }
                    }
                }
            },
        )
        .map_err(|e| format!("Failed to create debounced watcher: {}", e))?;

        debouncer
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch path {:?}: {}", path, e))?;

        self.native_watcher = Some(debouncer);
        Ok(())
    }

    /// Start poll-based watcher (fallback for network filesystems)
    fn start_poll(&mut self, path: &Path) -> Result<(), String> {
        let tx = self.event_tx.clone();
        let events_received = self.events_received.clone();
        let last_event_at = self.last_event_at.clone();

        let config = Config::default().with_poll_interval(POLL_INTERVAL);

        let mut watcher = PollWatcher::new(
            move |result: Result<notify::Event, notify::Error>| match result {
                Ok(event) => {
                    let paths: Vec<PathBuf> = event
                        .paths
                        .into_iter()
                        .filter(|p| !should_exclude_path(p))
                        .collect();

                    if paths.is_empty() {
                        return;
                    }

                    let kind = WatcherEventKind::from_notify(&event.kind);
                    let watcher_event = WatcherEvent {
                        paths,
                        kind,
                    };

                    events_received.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut guard) = last_event_at.lock() {
                        *guard = Some(Instant::now());
                    }

                    if let Err(e) = tx.try_send(watcher_event) {
                        warn!("Poll watcher event channel full, dropping: {}", e);
                    }
                }
                Err(e) => {
                    error!("Poll watcher error: {:?}", e);
                }
            },
            config,
        )
        .map_err(|e| format!("Failed to create poll watcher: {}", e))?;

        watcher
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to poll-watch path {:?}: {}", path, e))?;

        self.poll_watcher = Some(watcher);
        Ok(())
    }

    /// Stop watching
    pub fn stop(&mut self) {
        if let Some(debouncer) = self.native_watcher.take() {
            debouncer.stop_nonblocking();
            info!("Native watcher stopped");
        }

        if let Some(mut watcher) = self.poll_watcher.take() {
            if let Some(path) = &self.watched_path {
                let _ = watcher.unwatch(path);
            }
            info!("Poll watcher stopped");
        }

        self.running.store(false, Ordering::SeqCst);
        self.watched_path = None;
    }

    /// Get current watcher status for UI display
    #[allow(dead_code)] // Used in unit tests and Tauri command
    pub fn status(&self) -> WatcherStatus {
        let last_event_str = self
            .last_event_at
            .lock()
            .ok()
            .and_then(|guard| {
                guard.map(|instant| {
                    let elapsed = instant.elapsed();
                    format!("{}s ago", elapsed.as_secs())
                })
            });

        let healthy = if self.running.load(Ordering::SeqCst) {
            // Healthy if we've received events recently or just started
            self.last_event_at
                .lock()
                .ok()
                .and_then(|guard| guard.map(|i| i.elapsed() < HEALTH_TIMEOUT))
                .unwrap_or(true) // Healthy if never received events (just started)
        } else {
            false
        };

        WatcherStatus {
            running: self.running.load(Ordering::SeqCst),
            mode: self.mode,
            watched_path: self.watched_path.as_ref().map(|p| p.display().to_string()),
            events_received: self.events_received.load(Ordering::Relaxed),
            last_event_at: last_event_str,
            healthy,
            warning: None,
        }
    }

    /// Check if the watcher is currently running
    #[allow(dead_code)] // Used in unit tests
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_exclude_git() {
        assert!(should_exclude_path(Path::new("/project/.git/objects/abc")));
        assert!(should_exclude_path(Path::new(".git")));
    }

    #[test]
    fn test_should_exclude_temp_files() {
        assert!(should_exclude_path(Path::new("document.swp")));
        assert!(should_exclude_path(Path::new("file.tmp")));
        assert!(should_exclude_path(Path::new("backup.bak")));
        assert!(should_exclude_path(Path::new("download.crdownload")));
        assert!(should_exclude_path(Path::new("file.partial")));
    }

    #[test]
    fn test_should_exclude_os_files() {
        assert!(should_exclude_path(Path::new(".DS_Store")));
        assert!(should_exclude_path(Path::new("Thumbs.db")));
        assert!(should_exclude_path(Path::new("desktop.ini")));
    }

    #[test]
    fn test_should_not_exclude_normal_files() {
        assert!(!should_exclude_path(Path::new("document.txt")));
        assert!(!should_exclude_path(Path::new("photo.jpg")));
        assert!(!should_exclude_path(Path::new("src/main.rs")));
        assert!(!should_exclude_path(Path::new("Cargo.toml")));
    }

    #[test]
    fn test_should_not_exclude_aerosync_files() {
        assert!(!should_exclude_path(Path::new(".aerosync")));
        assert!(!should_exclude_path(Path::new(".aeroagent")));
    }

    #[test]
    fn test_should_exclude_known_hidden_dirs() {
        // Known non-content hidden items are excluded
        assert!(should_exclude_path(Path::new(".cache")));
        assert!(should_exclude_path(Path::new(".npm")));
        assert!(should_exclude_path(Path::new(".Trash-1000")));
        assert!(should_exclude_path(Path::new("__pycache__")));
        assert!(should_exclude_path(Path::new(".bzr")));
    }

    #[test]
    fn test_should_not_exclude_user_dotfiles() {
        // User dotfiles must NOT be excluded
        assert!(!should_exclude_path(Path::new(".env")));
        assert!(!should_exclude_path(Path::new(".dockerignore")));
        assert!(!should_exclude_path(Path::new(".editorconfig")));
        assert!(!should_exclude_path(Path::new(".htaccess")));
        assert!(!should_exclude_path(Path::new(".eslintrc")));
        assert!(!should_exclude_path(Path::new(".prettierrc")));
        assert!(!should_exclude_path(Path::new(".gitignore")));
    }

    #[test]
    fn test_watcher_event_kind_from_notify() {
        assert_eq!(
            WatcherEventKind::from_notify(&EventKind::Create(CreateKind::File)),
            WatcherEventKind::Created
        );
        assert_eq!(
            WatcherEventKind::from_notify(&EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            WatcherEventKind::Modified
        );
        assert_eq!(
            WatcherEventKind::from_notify(&EventKind::Remove(RemoveKind::File)),
            WatcherEventKind::Removed
        );
        assert_eq!(
            WatcherEventKind::from_notify(&EventKind::Modify(ModifyKind::Name(
                RenameMode::Both
            ))),
            WatcherEventKind::Renamed
        );
    }

    #[test]
    fn test_watcher_status_default() {
        let status = WatcherStatus::default();
        assert!(!status.running);
        assert_eq!(status.mode, WatcherMode::Auto);
        assert!(status.watched_path.is_none());
        assert_eq!(status.events_received, 0);
        assert!(status.healthy);
        assert!(status.warning.is_none());
    }

    #[test]
    fn test_watcher_mode_serde() {
        let modes = vec![WatcherMode::Native, WatcherMode::Poll, WatcherMode::Auto];
        for mode in modes {
            let json = serde_json::to_string(&mode).unwrap();
            let parsed: WatcherMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn test_inotify_capacity_check() {
        // Test with current directory (should not panic)
        let (count, _, _) = check_inotify_capacity(Path::new("."));
        assert!(count < 1_000_000); // Sanity check
    }

    #[test]
    fn test_file_watcher_lifecycle() {
        let (tx, _rx) = mpsc::channel(100);
        let mut watcher = FileWatcher::new(tx);

        assert!(!watcher.is_running());

        let status = watcher.status();
        assert!(!status.running);
        assert_eq!(status.events_received, 0);

        watcher.stop(); // Should not panic on double-stop
    }
}
