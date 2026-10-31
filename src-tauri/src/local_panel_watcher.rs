// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Lightweight filesystem watcher for the AeroFile / local panels.
//!
//! Distinct from `file_watcher.rs` (which serves AeroSync with debouncing,
//! rename tracking, recursive walks, health heartbeats, etc.). This module
//! is intentionally minimal: it watches the folder each local panel shows,
//! non-recursively, and emits a single `local-fs-changed` Tauri event, naming
//! the panel and its folder, when the contents change. A flattened search
//! lists the folders below the panel's folder too; the frontend hands those
//! over, and each is watched the same way, so a change in one of them reloads
//! the panel. The list is bounded by the search's own entry cap: a recursive
//! watch would register every folder of the tree, the ones the search never
//! listed included (`/` holds hundreds of thousands).
//!
//! The frontend swaps the watched folders when the user navigates or the
//! search's folders change; the watcher is cheap to recreate, so a ready
//! replacement atomically takes the old one's slot.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

/// Coalescing interval for filesystem events. Prevents storms when an editor
/// (e.g. VS Code, vim) writes a file via temp-rename or when a transfer
/// completes a batch.
const COALESCE_MS: u64 = 250;

/// The most folders one panel watches: the panel's own and the folders a
/// flattened search lists, which lists at most 20,000 entries
/// (`flatten_local_descendants`' ceiling).
const MAX_WATCHED_DIRS: usize = 20_001;

/// The local panel a watch serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalPanel {
    Local,
    Local2,
}

/// Event payload sent to the frontend: the panel and the folder it showed
/// when the watch started, exactly as the frontend passed it.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct LocalFsChanged {
    panel: LocalPanel,
    path: String,
}

type Emit = Arc<dyn Fn(LocalFsChanged) + Send + Sync>;

struct WatcherSlot {
    /// Held for its `Drop` side effect: letting it fall out of scope tears
    /// down the inotify/FSEvents/RDCW handle. Reads are intentional no-ops.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    /// The folders watched, the panel's own first.
    dirs: Vec<PathBuf>,
    /// The request this watcher serves. Its callback emits while this is the
    /// panel's latest request; a later request for the same folders adopts
    /// the watcher instead of replacing it.
    serves: Arc<AtomicU64>,
}

/// One panel's watcher and the generation of its latest request.
///
/// The mutex is behind an `Arc` so a command can clone a `'static` handle out
/// of `State<'_, LocalPanelWatcherState>` and hand it to `spawn_blocking`;
/// `State` borrows the app and cannot cross that boundary itself.
#[derive(Clone)]
struct PanelWatch {
    inner: Arc<Mutex<Option<WatcherSlot>>>,
    generation: Arc<AtomicU64>,
}

impl PanelWatch {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn begin_request(&self) -> Result<u64, String> {
        // Serialize generation changes with the final watcher-slot install.
        // Otherwise a newer request can increment after the installer's last
        // generation check but before it replaces the slot.
        let _slot = self
            .inner
            .lock()
            .map_err(|_| "watcher state poisoned".to_string())?;
        Ok(self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1))
    }

    fn stop(&self) -> Result<(), String> {
        let mut slot = self
            .inner
            .lock()
            .map_err(|_| "watcher state poisoned".to_string())?;
        // Invalidation and clearing are one critical section shared with both
        // request creation and watcher installation.
        self.generation.fetch_add(1, Ordering::SeqCst);
        *slot = None;
        Ok(())
    }
}

/// Tauri-managed state. Holds at most one active watcher per local panel.
pub struct LocalPanelWatcherState {
    local: PanelWatch,
    local2: PanelWatch,
}

impl LocalPanelWatcherState {
    pub fn new() -> Self {
        Self {
            local: PanelWatch::new(),
            local2: PanelWatch::new(),
        }
    }

    fn panel(&self, panel: LocalPanel) -> &PanelWatch {
        match panel {
            LocalPanel::Local => &self.local,
            LocalPanel::Local2 => &self.local2,
        }
    }
}

impl Default for LocalPanelWatcherState {
    fn default() -> Self {
        Self::new()
    }
}

/// The folders to watch for `root`: `root` first, then each of `below` that is
/// inside it, once. A folder outside `root` is left out: the rows of the last
/// folder can still be on screen while the panel moves to the next one.
fn watched_dirs(root: &Path, below: Vec<String>) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    let mut seen = std::collections::HashSet::new();
    for dir in below.into_iter().map(PathBuf::from) {
        if dir.starts_with(root) && dir != root && seen.insert(dir.clone()) {
            dirs.push(dir);
        }
    }
    if dirs.len() > MAX_WATCHED_DIRS {
        log::warn!(
            "local panel watcher: {} folders below {}, watching the first {}",
            dirs.len() - 1,
            root.display(),
            MAX_WATCHED_DIRS - 1
        );
        dirs.truncate(MAX_WATCHED_DIRS);
    }
    dirs
}

/// Start watching `path` for `panel` (panel 1 when omitted), non-recursively,
/// and each of `dirs` below it (a flattened search's folders). A successfully
/// prepared watcher replaces the panel's previous one. Idempotent: the same
/// folders again keep the watcher there.
///
/// `async`: `path` comes from the frontend, and `is_dir()` on it is a `stat(2)`
/// that blocks for the mount's own timeout when that mount is a network share
/// whose server has gone. Registering the inotify/FSEvents/RDCW watch is a
/// syscall against the same path. A synchronous command would do both on the
/// main thread, which on Linux is the GTK thread.
#[tauri::command]
pub async fn local_panel_watch(
    path: String,
    panel: Option<LocalPanel>,
    dirs: Option<Vec<String>>,
    app: AppHandle,
    state: State<'_, LocalPanelWatcherState>,
) -> Result<(), String> {
    let panel = panel.unwrap_or(LocalPanel::Local);
    let watch = state.panel(panel).clone();
    let request = watch.begin_request()?;
    let emit: Emit = Arc::new(move |payload| {
        // Best-effort emit; swallow errors (frontend may be unmounted).
        let _ = app.emit("local-fs-changed", payload);
    });
    tokio::task::spawn_blocking(move || {
        local_panel_watch_blocking(panel, path, dirs.unwrap_or_default(), emit, &watch, request)
    })
    .await
    .unwrap_or_else(|err| Err(format!("Watcher start task failed: {err}")))
}

fn local_panel_watch_blocking(
    panel: LocalPanel,
    path: String,
    below: Vec<String>,
    emit: Emit,
    watch: &PanelWatch,
    request: u64,
) -> Result<(), String> {
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("not a directory: {}", path));
    }
    let dirs = watched_dirs(&root, below);

    // The same folders: the watcher there serves this request from now on.
    // Building another and discarding it would leave the one there serving
    // a request that is no longer the latest, and it would emit nothing.
    {
        let slot = watch
            .inner
            .lock()
            .map_err(|_| "watcher state poisoned".to_string())?;
        if watch.generation.load(Ordering::SeqCst) != request {
            return Ok(());
        }
        if let Some(existing) = slot.as_ref().filter(|existing| existing.dirs == dirs) {
            existing.serves.store(request, Ordering::SeqCst);
            return Ok(());
        }
    }

    let callback_generation = Arc::clone(&watch.generation);
    let serves = Arc::new(AtomicU64::new(request));
    let callback_serves = Arc::clone(&serves);
    let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60)));

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let current = || {
                callback_generation.load(Ordering::SeqCst) == callback_serves.load(Ordering::SeqCst)
            };
            if !current() {
                return;
            }
            let event = match res {
                Ok(ev) => ev,
                Err(_) => return,
            };
            // Filter only events that actually affect the listing (skip Access).
            if matches!(event.kind, EventKind::Access(_) | EventKind::Other) {
                return;
            }
            // Coalesce bursts.
            let now = Instant::now();
            let mut last = match last_emit.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if now.duration_since(*last) < Duration::from_millis(COALESCE_MS) {
                return;
            }
            *last = now;
            if !current() {
                return;
            }
            emit(LocalFsChanged {
                panel,
                path: path.clone(),
            });
        })
        .map_err(|e| format!("watcher init failed: {}", e))?;

    watcher
        .watch(&root, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch start failed: {}", e))?;
    // A folder below that cannot be watched any more was removed or made
    // unreadable since the search listed it: the watch of the folder above it
    // reports that, and the search lists the folders again. Running out of
    // watches is different: a change there would go unnoticed, so it is
    // logged.
    let mut unwatched = 0usize;
    let mut last_error = None;
    for dir in &dirs[1..] {
        if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            unwatched += 1;
            last_error = Some(err);
        }
    }
    if let Some(err) = last_error {
        log::warn!(
            "local panel watcher: {unwatched} of {} folders below {} not watched (last error: {err})",
            dirs.len() - 1,
            root.display()
        );
    }

    // `stat` and watcher creation may block on a remote mount. A newer request
    // (or stop) must win even if this older call completes last.
    let mut slot = watch
        .inner
        .lock()
        .map_err(|_| "watcher state poisoned".to_string())?;
    if watch.generation.load(Ordering::SeqCst) != request {
        return Ok(());
    }
    *slot = Some(WatcherSlot {
        watcher,
        dirs,
        serves,
    });
    Ok(())
}

/// Stop the watcher of `panel`, or of both panels when omitted. Safe to call
/// when nothing is watching.
///
/// `async` for two reasons: dropping the slot tears down the platform watch
/// handle, and it takes the same lock `local_panel_watch` holds while it stats
/// a frontend-supplied path. Leaving this one synchronous would keep the freeze
/// reachable by navigating away from a hung mount.
#[tauri::command]
pub async fn local_panel_watch_stop(
    panel: Option<LocalPanel>,
    state: State<'_, LocalPanelWatcherState>,
) -> Result<(), String> {
    let watches: Vec<PanelWatch> = match panel {
        Some(panel) => vec![state.panel(panel).clone()],
        None => vec![state.local.clone(), state.local2.clone()],
    };
    tokio::task::spawn_blocking(move || watches.iter().try_for_each(PanelWatch::stop))
        .await
        .unwrap_or_else(|err| Err(format!("Watcher stop task failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_watch_generation_can_install() {
        let state = LocalPanelWatcherState::new();
        let watch = state.panel(LocalPanel::Local);
        let first = watch.begin_request().unwrap();
        let second = watch.begin_request().unwrap();

        assert_ne!(watch.generation.load(Ordering::SeqCst), first);
        assert_eq!(watch.generation.load(Ordering::SeqCst), second);

        {
            let _slot = watch.inner.lock().unwrap();
            watch.generation.fetch_add(1, Ordering::SeqCst);
        }
        assert_ne!(watch.generation.load(Ordering::SeqCst), second);
    }

    #[test]
    fn a_panel_s_request_leaves_the_other_panel_s_watch_alone() {
        let state = LocalPanelWatcherState::new();
        let local = state.panel(LocalPanel::Local).begin_request().unwrap();
        state.panel(LocalPanel::Local2).begin_request().unwrap();
        state.panel(LocalPanel::Local2).stop().unwrap();
        assert_eq!(
            state
                .panel(LocalPanel::Local)
                .generation
                .load(Ordering::SeqCst),
            local
        );
    }

    #[test]
    fn the_panel_names_are_the_frontend_s() {
        assert_eq!(
            serde_json::from_str::<LocalPanel>("\"local2\"").unwrap(),
            LocalPanel::Local2
        );
        assert_eq!(
            serde_json::to_value(LocalFsChanged {
                panel: LocalPanel::Local,
                path: "/a".into(),
            })
            .unwrap(),
            serde_json::json!({ "panel": "local", "path": "/a" })
        );
    }

    #[test]
    fn watches_the_folder_and_the_folders_below_it_once() {
        let root = Path::new("/data/photos");
        let dirs = watched_dirs(
            root,
            vec![
                "/data/photos/2026".into(),
                "/data/photos".into(),
                "/data/photos/2026".into(),
                "/data/photos-old".into(),
                "/data/other/2026".into(),
                "/data/photos/2026/summer".into(),
            ],
        );
        assert_eq!(
            dirs,
            [
                "/data/photos",
                "/data/photos/2026",
                "/data/photos/2026/summer"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn watches_a_bounded_number_of_folders_and_always_the_panel_s_own() {
        let root = Path::new("/r");
        let below = (0..MAX_WATCHED_DIRS + 5)
            .map(|i| format!("/r/{i}"))
            .collect();
        let dirs = watched_dirs(root, below);
        assert_eq!(dirs.len(), MAX_WATCHED_DIRS);
        assert_eq!(dirs[0], root);
    }

    /// The watch itself, on inotify: macOS and Windows deliver events with
    /// their own latency and are left to the logic tests above.
    #[cfg(target_os = "linux")]
    mod live {
        use super::*;
        use std::sync::mpsc;

        fn start(
            watch: &PanelWatch,
            panel: LocalPanel,
            root: &Path,
            below: &[&Path],
        ) -> mpsc::Receiver<LocalFsChanged> {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let emit: Emit = Arc::new(move |payload| {
                let _ = tx.lock().unwrap().send(payload);
            });
            let request = watch.begin_request().unwrap();
            local_panel_watch_blocking(
                panel,
                root.to_string_lossy().into_owned(),
                below
                    .iter()
                    .map(|d| d.to_string_lossy().into_owned())
                    .collect(),
                emit,
                watch,
                request,
            )
            .unwrap();
            rx
        }

        fn first_event(rx: &mpsc::Receiver<LocalFsChanged>) -> Option<LocalFsChanged> {
            rx.recv_timeout(Duration::from_secs(5)).ok()
        }

        #[test]
        fn a_change_in_a_folder_the_search_lists_reloads_the_panel() {
            let tmp = tempfile::tempdir().unwrap();
            let nested = tmp.path().join("2026").join("summer");
            std::fs::create_dir_all(&nested).unwrap();
            let watch = PanelWatch::new();
            let rx = start(&watch, LocalPanel::Local, tmp.path(), &[&nested]);

            std::fs::write(nested.join("new.jpg"), b"x").unwrap();
            assert_eq!(
                first_event(&rx),
                Some(LocalFsChanged {
                    panel: LocalPanel::Local,
                    path: tmp.path().to_string_lossy().into_owned(),
                })
            );
        }

        #[test]
        fn panel_2_s_watch_names_panel_2() {
            let tmp = tempfile::tempdir().unwrap();
            let state = LocalPanelWatcherState::new();
            let rx = start(
                state.panel(LocalPanel::Local2),
                LocalPanel::Local2,
                tmp.path(),
                &[],
            );

            std::fs::write(tmp.path().join("new.txt"), b"x").unwrap();
            assert_eq!(first_event(&rx).map(|e| e.panel), Some(LocalPanel::Local2));
        }

        #[test]
        fn the_same_folders_again_keep_the_watch_alive() {
            // React runs an effect twice in development, and the panel asks
            // again for the folders it watches. The watcher there kept serving
            // the first request, now stale, and emitted nothing more.
            let tmp = tempfile::tempdir().unwrap();
            let watch = PanelWatch::new();
            let first = start(&watch, LocalPanel::Local, tmp.path(), &[]);
            let second = start(&watch, LocalPanel::Local, tmp.path(), &[]);

            std::fs::write(tmp.path().join("new.txt"), b"x").unwrap();
            // The first request's watcher, adopted by the second, reports to
            // the first's channel; the second's watcher was never built.
            assert!(first_event(&first).is_some());
            assert!(second.try_recv().is_err());
        }
    }
}
