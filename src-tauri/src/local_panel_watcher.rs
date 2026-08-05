// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Lightweight non-recursive filesystem watcher for the AeroFile / local panel.
//!
//! Distinct from `file_watcher.rs` (which serves AeroSync with debouncing,
//! rename tracking, recursive walks, health heartbeats, etc.). This module
//! is intentionally minimal: it watches one directory non-recursively and
//! emits a single `local-fs-changed` Tauri event when its contents change.
//!
//! The frontend swaps the watched path when the user navigates to a new
//! folder; the watcher is cheap to recreate, so a ready replacement atomically
//! takes the old one's slot.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::path::PathBuf;
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

/// Event payload sent to the frontend.
#[derive(Clone, Debug, Serialize)]
struct LocalFsChanged {
    path: String,
}

struct WatcherSlot {
    /// Held for its `Drop` side effect: letting it fall out of scope tears
    /// down the inotify/FSEvents/RDCW handle. Reads are intentional no-ops.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    path: PathBuf,
}

/// Tauri-managed state. Holds at most one active watcher.
///
/// The mutex is behind an `Arc` so a command can clone a `'static` handle out
/// of `State<'_, LocalPanelWatcherState>` and hand it to `spawn_blocking`;
/// `State` borrows the app and cannot cross that boundary itself.
pub struct LocalPanelWatcherState {
    inner: Arc<Mutex<Option<WatcherSlot>>>,
    generation: Arc<AtomicU64>,
}

impl LocalPanelWatcherState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A `'static` handle to the slot, for use off the main thread.
    fn handle(&self) -> Arc<Mutex<Option<WatcherSlot>>> {
        Arc::clone(&self.inner)
    }

    fn generation_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.generation)
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
}

impl Default for LocalPanelWatcherState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start watching `path` non-recursively. A successfully prepared watcher
/// replaces the previous one. Idempotent: passing the same path again is a no-op.
///
/// `async`: `path` comes from the frontend, and `is_dir()` on it is a `stat(2)`
/// that blocks for the mount's own timeout when that mount is a network share
/// whose server has gone. Registering the inotify/FSEvents/RDCW watch is a
/// syscall against the same path. A synchronous command would do both on the
/// main thread, which on Linux is the GTK thread.
#[tauri::command]
pub async fn local_panel_watch(
    path: String,
    app: AppHandle,
    state: State<'_, LocalPanelWatcherState>,
) -> Result<(), String> {
    let slot = state.handle();
    let generation = state.generation_handle();
    let request = state.begin_request()?;
    tokio::task::spawn_blocking(move || {
        local_panel_watch_blocking(path, app, &slot, &generation, request)
    })
    .await
    .unwrap_or_else(|err| Err(format!("Watcher start task failed: {err}")))
}

fn local_panel_watch_blocking(
    path: String,
    app: AppHandle,
    state: &Mutex<Option<WatcherSlot>>,
    generation: &Arc<AtomicU64>,
    request: u64,
) -> Result<(), String> {
    let new_path = PathBuf::from(&path);
    if !new_path.is_dir() {
        return Err(format!("not a directory: {}", path));
    }

    let app_for_cb = app.clone();
    let watch_root = new_path.clone();
    let callback_generation = Arc::clone(generation);
    let last_emit = std::sync::Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60)));

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if callback_generation.load(Ordering::SeqCst) != request {
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
            let payload = LocalFsChanged {
                path: watch_root.display().to_string(),
            };
            if callback_generation.load(Ordering::SeqCst) != request {
                return;
            }
            // Best-effort emit; swallow errors (frontend may be unmounted).
            let _ = app_for_cb.emit("local-fs-changed", payload);
        })
        .map_err(|e| format!("watcher init failed: {}", e))?;

    watcher
        .watch(&new_path, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch start failed: {}", e))?;

    // `stat` and watcher creation may block on a remote mount. A newer request
    // (or stop) must win even if this older call completes last.
    if generation.load(Ordering::SeqCst) != request {
        return Ok(());
    }

    let mut slot = state
        .lock()
        .map_err(|_| "watcher state poisoned".to_string())?;
    if generation.load(Ordering::SeqCst) != request {
        return Ok(());
    }
    if slot
        .as_ref()
        .is_some_and(|existing| existing.path == new_path)
    {
        return Ok(());
    }

    *slot = Some(WatcherSlot {
        watcher,
        path: new_path,
    });
    Ok(())
}

/// Stop the active watcher (if any). Safe to call when nothing is watching.
///
/// `async` for two reasons: dropping the slot tears down the platform watch
/// handle, and it takes the same lock `local_panel_watch` holds while it stats
/// a frontend-supplied path. Leaving this one synchronous would keep the freeze
/// reachable by navigating away from a hung mount.
#[tauri::command]
pub async fn local_panel_watch_stop(
    state: State<'_, LocalPanelWatcherState>,
) -> Result<(), String> {
    let slot = state.handle();
    let generation = state.generation_handle();
    tokio::task::spawn_blocking(move || {
        let mut slot = slot
            .lock()
            .map_err(|_| "watcher state poisoned".to_string())?;
        // Invalidation and clearing are one critical section shared with both
        // request creation and watcher installation.
        generation.fetch_add(1, Ordering::SeqCst);
        *slot = None;
        Ok(())
    })
    .await
    .unwrap_or_else(|err| Err(format!("Watcher stop task failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_watch_generation_can_install() {
        let state = LocalPanelWatcherState::new();
        let first = state.begin_request().unwrap();
        let second = state.begin_request().unwrap();

        assert_ne!(state.generation.load(Ordering::SeqCst), first);
        assert_eq!(state.generation.load(Ordering::SeqCst), second);

        {
            let _slot = state.inner.lock().unwrap();
            state.generation.fetch_add(1, Ordering::SeqCst);
        }
        assert_ne!(state.generation.load(Ordering::SeqCst), second);
    }
}
