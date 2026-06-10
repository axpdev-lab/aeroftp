//! AeroShare Phase 1: the in-app registry of long-lived drive sync tasks.
//!
//! publish/replicate are PERSISTENT processes, not request/response calls, so
//! the GUI needs one owner for their lifecycle: `PeerRuntime`, registered as
//! Tauri managed state. Connecting a panel to a friend's drive
//! (`provider_connect` with `protocol="peer"`) calls
//! [`PeerRuntime::ensure_sub_for_config`], which idempotently guarantees a
//! background replication task is converging the drive into its local replica
//! folder; the `PeerProvider` then browses that folder. Disconnecting a panel
//! does NOT stop the task (design D-GUI-1: app open or in tray = serving;
//! Quit = process exit = stop).
//!
//! THREADING (memory `crash-tray-off-main-thread`): every task here runs on
//! the tokio pool via `tokio::spawn`; nothing touches the GTK main thread.
//! UI feedback goes EXCLUSIVELY through `app.emit("peer://sync-status", ...)`
//! events (precedent: `cross_profile_commands::emit_transfer_event`).
//!
//! The engine watch call returns when its live-update window elapses
//! (`run_docs_replicate` takes a deadline, engine stage 6/10), so the task is
//! a loop: each pass re-opens the persistent store, converges differentially,
//! then watches live for [`WATCH_WINDOW_SECS`]; errors back off and retry.
//! Stopping is prompt: a [`CancellationToken`] races the engine future in
//! `tokio::select!`, and dropping the future tears the iroh endpoint down.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::peer::{PEER_EXTRA_LOCAL_FOLDER, PEER_EXTRA_NAMESPACE, PEER_EXTRA_TICKET};
use crate::providers::types::ProviderConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// One engine pass = full differential converge + this many seconds of live
/// watching, then the loop re-enters (the persistent store makes the next
/// converge cheap). Big enough to keep endpoint churn negligible, small
/// enough that a wedged pass self-heals within the hour.
const WATCH_WINDOW_SECS: u64 = 3600;
/// Pause before retrying after an engine error (network down, relay
/// unreachable, friend offline on first sync, ...).
const ERROR_BACKOFF_SECS: u64 = 5;

/// Payload for `peer://sync-status` events. `state` is one of `starting`,
/// `syncing`, `error`, `stopped`; `detail` carries the error text.
#[derive(Clone, serde::Serialize)]
pub struct PeerSyncEvent {
    pub namespace: String,
    pub state: String,
    pub detail: Option<String>,
    pub at_ms: u64,
}

fn emit_sync_status(app: &AppHandle, namespace: &str, state: &str, detail: Option<String>) {
    let _ = app.emit(
        "peer://sync-status",
        PeerSyncEvent {
            namespace: namespace.to_string(),
            state: state.to_string(),
            detail,
            at_ms: chrono::Utc::now().timestamp_millis().max(0) as u64,
        },
    );
}

/// WI-5 parity lever: like the engine's `AEROFTP_PEER_DISCOVERY` /
/// `AEROFTP_PEER_TICKET_ADDRS`, the GUI sync tasks accept a comma-separated
/// relay override from `AEROFTP_PEER_RELAY` until the Connectivity Settings
/// surface (Phase 3) exists. `None` = the engine's research default.
fn relay_urls_from_env() -> Option<Vec<String>> {
    let raw = std::env::var("AEROFTP_PEER_RELAY").ok()?;
    let urls: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        None
    } else {
        Some(urls)
    }
}

struct SubHandle {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    replica_dir: PathBuf,
}

impl SubHandle {
    fn is_live(&self) -> bool {
        !self.cancel.is_cancelled() && !self.task.is_finished()
    }
}

/// Registry of running drive subscriptions, keyed by namespace id. One entry
/// per drive for the session's active user partition (the key material is
/// resolved from the active partition at ensure time).
#[derive(Default)]
pub struct PeerRuntime {
    subs: tokio::sync::RwLock<HashMap<String, SubHandle>>,
}

impl PeerRuntime {
    /// Idempotently ensure the replication task for the drive described by a
    /// peer `ProviderConfig` is running. Resolves the drive's content key
    /// from the active user's partition vault (the `peer import` flow must
    /// have custodied it first), then spawns the sync loop if no live task
    /// already converges this namespace into the same replica folder.
    pub async fn ensure_sub_for_config(
        &self,
        app: &AppHandle,
        config: &ProviderConfig,
    ) -> Result<(), String> {
        let namespace = config
            .extra
            .get(PEER_EXTRA_NAMESPACE)
            .ok_or_else(|| "AeroShare config is missing the namespace".to_string())?
            .clone();
        let ticket = config
            .extra
            .get(PEER_EXTRA_TICKET)
            .ok_or_else(|| "AeroShare config is missing the ticket".to_string())?
            .clone();
        let replica_dir = config
            .extra
            .get(PEER_EXTRA_LOCAL_FOLDER)
            .map(PathBuf::from)
            .ok_or_else(|| "AeroShare config is missing the replica folder".to_string())?;

        // Fast path without touching the vault.
        {
            let subs = self.subs.read().await;
            if let Some(handle) = subs.get(&namespace) {
                if handle.is_live() && handle.replica_dir == replica_dir {
                    return Ok(());
                }
            }
        }

        // The content key lives DEK-sealed in the active user's partition
        // (WI-4b custody model); a drive nobody imported is a clear error.
        let (user_id, content_key) = crate::user_partitions::gui_peer_drive_load(app, &namespace)?
            .ok_or_else(|| {
                format!("No key for drive {namespace}: import its share token first (peer import)")
            })?;

        // Persistent blob/doc store, per user + namespace, beside the vault
        // (NOT inside the replica folder, which belongs to the user).
        let store_dir = crate::portable::app_config_dir(app)?
            .join("aeroshare")
            .join(user_id.to_string())
            .join(&namespace)
            .join("store");
        std::fs::create_dir_all(&store_dir)
            .map_err(|e| format!("cannot create AeroShare store {}: {e}", store_dir.display()))?;
        std::fs::create_dir_all(&replica_dir).map_err(|e| {
            format!(
                "cannot create AeroShare replica {}: {e}",
                replica_dir.display()
            )
        })?;

        let mut subs = self.subs.write().await;
        // Re-check under the write lock: a concurrent connect may have won.
        if let Some(handle) = subs.get(&namespace) {
            if handle.is_live() && handle.replica_dir == replica_dir {
                return Ok(());
            }
            // Same drive, stale task or a different target folder: replace.
            handle.cancel.cancel();
        }

        let cancel = CancellationToken::new();
        let task = tokio::spawn(sync_loop(
            app.clone(),
            cancel.clone(),
            namespace.clone(),
            ticket,
            content_key,
            replica_dir.clone(),
            store_dir,
        ));
        subs.insert(
            namespace,
            SubHandle {
                cancel,
                task,
                replica_dir,
            },
        );
        Ok(())
    }
}

/// The long-lived replication task for one drive. Loops engine passes until
/// cancelled; every state change is emitted, never rendered directly.
async fn sync_loop(
    app: AppHandle,
    cancel: CancellationToken,
    namespace: String,
    ticket: String,
    content_key: zeroize::Zeroizing<Vec<u8>>,
    replica_dir: PathBuf,
    store_dir: PathBuf,
) {
    emit_sync_status(&app, &namespace, "starting", None);
    let out = replica_dir.to_string_lossy().to_string();
    let store = Some(store_dir.to_string_lossy().to_string());
    loop {
        if cancel.is_cancelled() {
            break;
        }
        emit_sync_status(&app, &namespace, "syncing", None);
        let pass = crate::peer::replicate_drive_cap(
            ticket.clone(),
            out.clone(),
            &content_key,
            WATCH_WINDOW_SECS,
            store.clone(),
            relay_urls_from_env(),
        );
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = pass => {
                match result {
                    // Watch window elapsed: re-enter for the next converge+watch pass.
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!("AeroShare sync pass failed for {namespace}: {e}");
                        emit_sync_status(&app, &namespace, "error", Some(e.to_string()));
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(ERROR_BACKOFF_SECS)) => {}
                        }
                    }
                }
            }
        }
    }
    emit_sync_status(&app, &namespace, "stopped", None);
}
