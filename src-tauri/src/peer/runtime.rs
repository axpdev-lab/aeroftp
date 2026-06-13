//! AeroShare Phase 1: the in-app registry of long-lived drive tasks.
//!
//! publish/replicate are PERSISTENT processes, not request/response calls, so
//! the GUI needs one owner for their lifecycle: `PeerRuntime`, registered as
//! Tauri managed state. Two registries live here:
//! - `subs` (read side): connecting a panel to a friend's drive
//!   (`provider_connect` with `protocol="peer"`) calls
//!   [`PeerRuntime::ensure_sub_for_config`], which idempotently guarantees a
//!   background replication task is converging the drive into its local
//!   replica folder; the `PeerProvider` then browses that folder.
//! - `shares` (write side, task 4): sharing a folder with a friend calls
//!   [`PeerRuntime::start_share`], which publishes the folder as an encrypted
//!   drive and keeps serving it; the command layer then seals the capability
//!   token for the recipient.
//!
//! Disconnecting a panel does NOT stop any task (design D-GUI-1: app open or
//! in tray = serving; Quit = process exit = stop).
//!
//! THREADING (memory `crash-tray-off-main-thread`): every task here runs on
//! the tokio pool via `tokio::spawn`; nothing touches the GTK main thread.
//! UI feedback goes EXCLUSIVELY through `app.emit("peer://...")` events
//! (precedent: `cross_profile_commands::emit_transfer_event`).
//!
//! The engine watch call returns when its live-update window elapses
//! (`run_docs_replicate` takes a deadline, engine stage 6/10), so the sub
//! task is a loop: each pass re-opens the persistent store, converges
//! differentially, then watches live for [`WATCH_WINDOW_SECS`]; errors back
//! off and retry. Stopping is prompt: a [`CancellationToken`] races the
//! engine future in `tokio::select!`, and dropping the future tears the iroh
//! endpoint down.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::peer::{PEER_EXTRA_LOCAL_FOLDER, PEER_EXTRA_NAMESPACE, PEER_EXTRA_TICKET};
use crate::providers::types::ProviderConfig;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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
/// How long `start_share` waits for the engine to report the published
/// drive's `(namespace, ticket)` before giving up (first publish walks +
/// encrypts the whole folder, so this scales with folder size; 90s covers
/// the P1 "share a folder of documents/photos" case).
const PUBLISH_READY_TIMEOUT_SECS: u64 = 90;

/// Payload for `peer://sync-status` events (read side). `state` is one of
/// `starting`, `syncing`, `error`, `stopped`; `detail` carries the error text.
#[derive(Clone, serde::Serialize)]
pub struct PeerSyncEvent {
    pub namespace: String,
    pub state: String,
    pub detail: Option<String>,
    pub at_ms: u64,
}

/// Payload for `peer://share-status` events (write side). `namespace` is
/// `None` until the engine reports the published drive. `state` is one of
/// `starting`, `serving`, `error`, `stopped`.
#[derive(Clone, serde::Serialize)]
pub struct PeerShareEvent {
    pub dir: String,
    pub namespace: Option<String>,
    pub state: String,
    pub detail: Option<String>,
    pub at_ms: u64,
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// How long the receive loop waits for the user's Accept/Decline before treating
/// the offer as declined. Kept BELOW the sender's decision window (peer-l0
/// `DECISION_WAIT_SECS` = 120) so a no-response resolves as a clean decline on the
/// sender rather than a connection timeout.
const INCOMING_RESPONSE_TIMEOUT_SECS: u64 = 110;

/// Payload for `peer://incoming-offer` (an incoming one-shot "Send file" offer
/// awaiting the user's Accept/Decline). `transfer_id` is echoed back by
/// `peer_incoming_respond`.
#[derive(Clone, serde::Serialize)]
pub struct PeerIncomingOfferEvent {
    pub transfer_id: String,
    pub sender_afid: String,
    pub name: String,
    pub size: u64,
    pub at_ms: u64,
}

/// Payload for `peer://incoming-status` (the outcome of an incoming transfer).
/// `state` is one of `completed`, `declined`, `failed`.
#[derive(Clone, serde::Serialize)]
pub struct PeerIncomingStatusEvent {
    pub state: String,
    pub sender_afid: String,
    pub name: String,
    pub path: Option<String>,
    pub error: Option<String>,
    pub at_ms: u64,
}

/// Payload for `peer://receiver-status` (the receive loop's lifecycle). `state`
/// is one of `listening`, `stopped`, `error`.
#[derive(Clone, serde::Serialize)]
pub struct PeerReceiverStatusEvent {
    pub state: String,
    pub detail: Option<String>,
    pub at_ms: u64,
}

/// Payload for `peer://knock` (an incoming predefined-code signal, no file).
/// `code` is one of the app's predefined message codes (the FE maps it to
/// localized text); `in_reply_to`, when set, is the code this knock answers.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerKnockEvent {
    pub sender_afid: String,
    pub code: String,
    pub in_reply_to: Option<String>,
    pub at_ms: u64,
}

/// Short display form of an AeroFTP-ID, the default per-sender inbox subfolder
/// when the FE does not supply a friendly alias.
fn short_afid(afid: &str) -> String {
    if afid.len() > 13 {
        format!("{}_{}", &afid[..8], &afid[afid.len() - 4..])
    } else {
        afid.to_string()
    }
}

/// Sanitize a per-sender inbox subfolder name to a single safe path component.
fn sanitize_subfolder(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = safe.trim_matches(['.', ' ', '/']).to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

/// A pending incoming offer awaiting the user's decision. The receive loop parks
/// on `tx`'s receiver; `peer_incoming_respond` resolves it with the destination
/// directory (accept) or `None` (decline).
struct PendingOffer {
    tx: tokio::sync::oneshot::Sender<Option<PathBuf>>,
    sender_afid: String,
}

/// The standing receive loop's handle (the "Send file" toggle = ON state).
struct ReceiverHandle {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    /// Inbox root; received files land in `<inbox_root>/<sender subfolder>/`.
    inbox_root: PathBuf,
}

impl ReceiverHandle {
    fn is_live(&self) -> bool {
        !self.cancel.is_cancelled() && !self.task.is_finished()
    }
}

fn emit_sync_status(app: &AppHandle, namespace: &str, state: &str, detail: Option<String>) {
    let _ = app.emit(
        "peer://sync-status",
        PeerSyncEvent {
            namespace: namespace.to_string(),
            state: state.to_string(),
            detail,
            at_ms: now_ms(),
        },
    );
}

/// Record the drive's state in the runtime registry (so `peer_drives_list`
/// returns the REAL state, the F3 durability fix) AND emit the event. The
/// std Mutex is locked only for the insert, never across an await.
fn record_sync(
    app: &AppHandle,
    states: &std::sync::Mutex<HashMap<String, String>>,
    namespace: &str,
    state: &str,
    detail: Option<String>,
) {
    if let Ok(mut guard) = states.lock() {
        guard.insert(namespace.to_string(), state.to_string());
    }
    emit_sync_status(app, namespace, state, detail);
}

fn emit_share_status(
    app: &AppHandle,
    dir: &str,
    namespace: Option<&str>,
    state: &str,
    detail: Option<String>,
) {
    let _ = app.emit(
        "peer://share-status",
        PeerShareEvent {
            dir: dir.to_string(),
            namespace: namespace.map(str::to_string),
            state: state.to_string(),
            detail,
            at_ms: now_ms(),
        },
    );
}

/// WI-5 parity lever: like the engine's `AEROFTP_PEER_DISCOVERY` /
/// `AEROFTP_PEER_TICKET_ADDRS`, the GUI tasks accept a comma-separated
/// relay override from `AEROFTP_PEER_RELAY` until the Connectivity Settings
/// surface (Phase 3) exists. `None` = the engine's research default.
pub(crate) fn relay_urls_from_env() -> Option<Vec<String>> {
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

/// Stable per-folder slug for the publish store path. The slug pins the
/// persistent store, and the store pins the drive NAMESPACE (engine S8:
/// reopening the same store republishes the same namespace, so old grants
/// keep working). Inline FNV-1a so the value survives std/compiler upgrades
/// (`DefaultHasher` makes no such promise).
fn publish_store_slug(dir: &Path) -> String {
    let bytes = dir.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    format!("{safe}-{hash:016x}")
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

struct ShareHandle {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    /// The published local folder (reuse key: one serving task per folder).
    dir: PathBuf,
    /// The drive's DocTicket as reported at publish time, kept so a second
    /// grant on the same live folder can rebuild the share link.
    ticket: String,
}

impl ShareHandle {
    fn is_live(&self) -> bool {
        !self.cancel.is_cancelled() && !self.task.is_finished()
    }
}

/// Registry of running drive tasks, keyed by namespace id. One entry per
/// drive for the session's active user partition (key material is resolved
/// from the active partition at ensure/start time).
#[derive(Default)]
pub struct PeerRuntime {
    subs: tokio::sync::RwLock<HashMap<String, SubHandle>>,
    shares: tokio::sync::RwLock<HashMap<String, ShareHandle>>,
    /// Authoritative per-namespace state (`starting|syncing|live|error|
    /// stopped|standby`), recorded at every state emit. `peer_drives_list`
    /// returns THIS so the FE restores the real dot state on a re-pull instead
    /// of re-deriving `syncing` from the live-task boolean (the "green dot not
    /// durable across a remount" bug, F3). A std Mutex (never held across an
    /// await) so the sync engine reporter closure can record without async.
    states: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// The standing one-shot "Send file" receive loop (the Ricezione toggle).
    /// `None`/dead = not receiving. Toggle-driven today; an "always-on" future
    /// just calls `start_receiver` from boot (the loop hardcodes no policy).
    receiver: tokio::sync::Mutex<Option<ReceiverHandle>>,
    /// The single identity endpoint (`NodeId == AFID.ed`) shared by the receive
    /// loop AND outbound sends while receiving. `Some` only while the receiver is
    /// up. Reusing it for sends is the Finding 6b fix: binding a SECOND endpoint
    /// with the same identity (the old per-send endpoint) evicts the receiver on
    /// the relay ("Another endpoint connected with the same endpoint id"). Cloned
    /// out to send (the clone shares the magicsock); dropped on `stop_receiver`.
    identity_endpoint: tokio::sync::Mutex<Option<std::sync::Arc<aeroftp_peer_l0::PeerEndpoint>>>,
    /// Incoming offers awaiting the user's Accept/Decline, keyed by transfer id.
    /// Shared with the receive task (which inserts) and `peer_incoming_respond`
    /// (which resolves). A std Mutex, never held across an await.
    pending_offers: std::sync::Arc<std::sync::Mutex<HashMap<String, PendingOffer>>>,
    /// Monotonic source of transfer ids (paired with `now_ms` for readability).
    next_transfer_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl PeerRuntime {
    /// Idempotently ensure the replication task for the drive described by a
    /// peer `ProviderConfig` is running (the `provider_connect` seam).
    pub async fn ensure_sub_for_config(
        &self,
        app: &AppHandle,
        config: &ProviderConfig,
    ) -> Result<(), String> {
        let namespace = config
            .extra
            .get(PEER_EXTRA_NAMESPACE)
            .ok_or_else(|| "AeroShare config is missing the namespace".to_string())?;
        let ticket = config
            .extra
            .get(PEER_EXTRA_TICKET)
            .ok_or_else(|| "AeroShare config is missing the ticket".to_string())?;
        let replica_dir = config
            .extra
            .get(PEER_EXTRA_LOCAL_FOLDER)
            .map(PathBuf::from)
            .ok_or_else(|| "AeroShare config is missing the replica folder".to_string())?;
        self.ensure_sub(app, namespace, ticket, &replica_dir).await
    }

    /// Idempotently ensure a replication task converges `namespace` (dialed
    /// via `ticket`) into `replica_dir`. Resolves the drive's content key
    /// from the active user's partition vault (the import flow must have
    /// custodied it first); spawns only if no live task already serves this
    /// namespace into the same folder.
    pub async fn ensure_sub(
        &self,
        app: &AppHandle,
        namespace: &str,
        ticket: &str,
        replica_dir: &Path,
    ) -> Result<(), String> {
        // Fast path without touching the vault.
        {
            let subs = self.subs.read().await;
            if let Some(handle) = subs.get(namespace) {
                if handle.is_live() && handle.replica_dir == replica_dir {
                    return Ok(());
                }
            }
        }

        // The content key lives DEK-sealed in the active user's partition
        // (WI-4b custody model); a drive nobody imported is a clear error.
        let (user_id, content_key) = crate::user_partitions::gui_peer_drive_load(app, namespace)?
            .ok_or_else(|| {
            format!("No key for drive {namespace}: import its share token first (peer import)")
        })?;

        // Persistent blob/doc store, per user + namespace, beside the vault
        // (NOT inside the replica folder, which belongs to the user).
        let store_dir = crate::portable::app_config_dir(app)?
            .join("aeroshare")
            .join(user_id.to_string())
            .join(namespace)
            .join("store");
        std::fs::create_dir_all(&store_dir)
            .map_err(|e| format!("cannot create AeroShare store {}: {e}", store_dir.display()))?;
        std::fs::create_dir_all(replica_dir).map_err(|e| {
            format!(
                "cannot create AeroShare replica {}: {e}",
                replica_dir.display()
            )
        })?;

        let mut subs = self.subs.write().await;
        // Re-check under the write lock: a concurrent connect may have won.
        if let Some(handle) = subs.get(namespace) {
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
            namespace.to_string(),
            ticket.to_string(),
            content_key,
            replica_dir.to_path_buf(),
            store_dir,
            self.states.clone(),
        ));
        subs.insert(
            namespace.to_string(),
            SubHandle {
                cancel,
                task,
                replica_dir: replica_dir.to_path_buf(),
            },
        );
        Ok(())
    }

    /// Start (or reuse) the publish/serve task for `dir`. Returns the
    /// drive's `(namespace_id, ticket, content_key)`; the key is the one the
    /// served drive is ACTUALLY encrypted with, ready to seal into grants.
    ///
    /// One serving task per folder: sharing the SAME folder with a second
    /// friend reuses the live drive (same namespace + ticket + key; the
    /// command layer seals a fresh token per recipient).
    ///
    /// Key continuity across app restarts: the publish store path is derived
    /// from the folder ([`publish_store_slug`]) and a `namespace.txt` sidecar
    /// remembers the namespace the store pins (engine S8: reopening the store
    /// republishes the SAME namespace). When the sidecar names a namespace
    /// whose content key the vault still custodies, the drive is re-served
    /// under THAT key so earlier grants keep converging; a fresh key is
    /// minted only for a genuinely new drive (or when the vault lost the key,
    /// which degrades to a re-key: old tokens stop reading new versions,
    /// exactly the WI-4e semantics).
    pub async fn start_share(
        &self,
        app: &AppHandle,
        user_id: i64,
        dir: &Path,
        drive_name: &str,
        identity_secret: &[u8],
    ) -> Result<(String, String, zeroize::Zeroizing<Vec<u8>>), String> {
        // Reuse a live publish of the same folder.
        {
            let shares = self.shares.read().await;
            if let Some((ns, handle)) = shares.iter().find(|(_, h)| h.dir == dir) {
                if handle.is_live() {
                    let (_uid, key) = crate::user_partitions::gui_peer_drive_load(app, ns)?
                        .ok_or_else(|| {
                            format!("the live drive {ns} has no content key in the vault")
                        })?;
                    return Ok((ns.clone(), handle.ticket.clone(), key));
                }
            }
        }

        let publish_root = crate::portable::app_config_dir(app)?
            .join("aeroshare")
            .join(user_id.to_string())
            .join("publish")
            .join(publish_store_slug(dir));
        let store_dir = publish_root.join("store");
        let namespace_sidecar = publish_root.join("namespace.txt");
        std::fs::create_dir_all(&store_dir).map_err(|e| {
            format!(
                "cannot create AeroShare publish store {}: {e}",
                store_dir.display()
            )
        })?;

        // Resolve the content key BEFORE publishing: a reopened store keeps
        // its namespace, so publishing under a different key would silently
        // re-key the drive out from under earlier grants.
        let (content_key, key_is_fresh) = match std::fs::read_to_string(&namespace_sidecar)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .and_then(|prev_ns| {
                crate::user_partitions::gui_peer_drive_load(app, &prev_ns).transpose()
            })
            .transpose()?
        {
            Some((_uid, existing)) => (existing, false),
            None => (
                zeroize::Zeroizing::new(crate::peer::fresh_content_key().to_vec()),
                true,
            ),
        };

        let cancel = CancellationToken::new();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(share_loop(
            app.clone(),
            cancel.clone(),
            dir.to_string_lossy().to_string(),
            drive_name.to_string(),
            content_key.clone(),
            zeroize::Zeroizing::new(identity_secret.to_vec()),
            store_dir.to_string_lossy().to_string(),
            ready_tx,
        ));

        // Wait for the engine to report the namespace + ticket, or fail
        // closed (the task has already emitted the error on its event
        // channel; cancel keeps a half-started publish from serving).
        let published = match tokio::time::timeout(
            std::time::Duration::from_secs(PUBLISH_READY_TIMEOUT_SECS),
            ready_rx,
        )
        .await
        {
            Ok(Ok(published)) => published,
            Ok(Err(_)) => {
                cancel.cancel();
                return Err(
                    "publish ended before it was ready (see peer://share-status)".to_string(),
                );
            }
            Err(_) => {
                cancel.cancel();
                return Err(format!(
                    "publish not ready within {PUBLISH_READY_TIMEOUT_SECS}s (folder too large for P1?)"
                ));
            }
        };

        // Custody + continuity records (CLI publish precedent: custody after
        // ready). Failing to custody the key would strand the drive -> error.
        if key_is_fresh {
            crate::user_partitions::gui_peer_drive_store(
                app,
                &published.namespace_id,
                "publisher",
                &content_key,
            )?;
        }
        if let Err(e) = std::fs::write(&namespace_sidecar, &published.namespace_id) {
            tracing::warn!(
                "AeroShare: cannot write namespace sidecar {}: {e} (re-sharing this folder after \
                 a restart will re-key the drive)",
                namespace_sidecar.display()
            );
        }
        emit_share_status(
            app,
            &dir.to_string_lossy(),
            Some(&published.namespace_id),
            "serving",
            None,
        );

        let mut shares = self.shares.write().await;
        if let Some(previous) = shares.get(&published.namespace_id) {
            previous.cancel.cancel();
        }
        shares.insert(
            published.namespace_id.clone(),
            ShareHandle {
                cancel,
                task,
                dir: dir.to_path_buf(),
                ticket: published.ticket.clone(),
            },
        );
        Ok((published.namespace_id, published.ticket, content_key))
    }

    /// Namespaces with a LIVE replication task (read side).
    pub async fn live_sub_namespaces(&self) -> HashSet<String> {
        self.subs
            .read()
            .await
            .iter()
            .filter(|(_, h)| h.is_live())
            .map(|(ns, _)| ns.clone())
            .collect()
    }

    /// Namespaces with a LIVE publish/serve task (write side).
    pub async fn live_share_namespaces(&self) -> HashSet<String> {
        self.shares
            .read()
            .await
            .iter()
            .filter(|(_, h)| h.is_live())
            .map(|(ns, _)| ns.clone())
            .collect()
    }

    /// Snapshot of the authoritative per-namespace state registry (F3). Empty
    /// for a namespace whose task never started this session; `peer_drives_list`
    /// falls back to the live-task booleans there.
    pub fn states_snapshot(&self) -> HashMap<String, String> {
        self.states
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Stand a received drive DOWN to STANDBY: cancel its replication task
    /// (freeing CPU + relay) and record the `standby` state. Wired from
    /// `provider_disconnect`, so closing a peer connection tab pauses the drive
    /// instead of leaking the task (the orphan-task leak) and the friend dot
    /// shows idle (dark blue) instead of staying green. Resumable: a later
    /// `provider_connect` re-runs `ensure_sub` and the drive goes live again.
    /// No-op when no live sub serves `namespace`.
    pub async fn standby(&self, app: &AppHandle, namespace: &str) {
        let removed = {
            let mut subs = self.subs.write().await;
            subs.remove(namespace)
        };
        let handle = match removed {
            Some(handle) => handle,
            None => {
                // No live task: still mark the drive idle so the dot leaves
                // green even if the sub had already ended.
                record_sync(app, &self.states, namespace, "standby", None);
                return;
            }
        };
        handle.cancel.cancel();
        // Wait for the task to FULLY stop before recording standby, so a
        // trailing engine "live"/"syncing" callback (the reporter's
        // check-then-act gap) cannot overwrite it. The loop's select! breaks
        // promptly on cancel, so this returns fast; the timeout is a backstop.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle.task).await;
        record_sync(app, &self.states, namespace, "standby", None);
        tracing::info!("AeroShare: drive {namespace} moved to standby (tab closed)");
    }

    /// Stop SERVING a published drive (the write-side sibling of [`standby`]):
    /// cancel its publish/serve task, freeing the iroh endpoint + relay. The
    /// share metadata is kept by the caller (the "Shared by me" panel shows the
    /// folder as idle, re-servable), so this only tears the live task down.
    /// No-op when no live share serves `namespace`. Returns `true` if a live
    /// share was stopped.
    pub async fn stop_share(&self, namespace: &str) -> bool {
        let removed = {
            let mut shares = self.shares.write().await;
            shares.remove(namespace)
        };
        match removed {
            Some(handle) => {
                handle.cancel.cancel();
                // The share_loop emits "stopped" on its cancel arm; await it so
                // the task is fully down before the command returns and the FE
                // re-pulls the serving set.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle.task).await;
                tracing::info!("AeroShare: share {namespace} stopped serving");
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------------
    // One-shot "Send file to user" receive side (the Ricezione toggle).
    // ------------------------------------------------------------------

    /// Whether the standing receive loop is live.
    pub async fn is_receiving(&self) -> bool {
        self.receiver
            .lock()
            .await
            .as_ref()
            .map(ReceiverHandle::is_live)
            .unwrap_or(false)
    }

    /// Start the standing receive loop (idempotent). Binds an identity-seeded
    /// endpoint (`my_secret` -> `NodeId == my AFID.ed`) so friends can dial us by
    /// AFID, and writes accepted files under `inbox_root/<sender subfolder>/`.
    /// While the loop runs, each incoming offer is surfaced on
    /// `peer://incoming-offer` and parked until `respond_to_offer` resolves it.
    pub async fn start_receiver(
        &self,
        app: &AppHandle,
        my_secret: &[u8],
        inbox_root: PathBuf,
    ) -> Result<(), String> {
        let mut guard = self.receiver.lock().await;
        if let Some(handle) = guard.as_ref() {
            if handle.is_live() {
                return Ok(());
            }
        }
        // Build the single identity endpoint once and store it so outbound sends
        // reuse it (Finding 6b) instead of binding a second same-identity one.
        let endpoint = std::sync::Arc::new(
            crate::peer::build_identity_endpoint(my_secret, relay_urls_from_env())
                .await
                .map_err(|e| format!("could not bind the AeroShare receive endpoint: {e}"))?,
        );
        *self.identity_endpoint.lock().await = Some(endpoint.clone());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(receive_loop(
            app.clone(),
            cancel.clone(),
            my_secret.to_vec(),
            endpoint,
            self.pending_offers.clone(),
            self.next_transfer_id.clone(),
        ));
        *guard = Some(ReceiverHandle {
            cancel,
            task,
            inbox_root,
        });
        Ok(())
    }

    /// Stop the receive loop (toggle OFF). Cancels the accept loop (freeing the
    /// endpoint + relay) and drops any still-pending offers (they resolve as
    /// declines on the senders). No-op when not receiving.
    pub async fn stop_receiver(&self, app: &AppHandle) {
        let handle = { self.receiver.lock().await.take() };
        if let Some(handle) = handle {
            handle.cancel.cancel();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle.task).await;
        }
        // Drop the shared identity endpoint so we are no longer registered on the
        // relay (presence reflects "not receiving") and the next send rebinds a
        // clean transient endpoint.
        *self.identity_endpoint.lock().await = None;
        // Drop any parked offers: their senders get a clean decline.
        let drained: Vec<PendingOffer> = {
            let mut pending = self
                .pending_offers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            pending.drain().map(|(_, v)| v).collect()
        };
        for offer in drained {
            let _ = offer.tx.send(None);
        }
        emit_receiver_status(app, "stopped", None);
        tracing::info!("AeroShare: receive loop stopped");
    }

    /// Send a one-shot file to `recipient_afid`. REUSES the standing receiver's
    /// identity endpoint when one is up (Finding 6b: a second same-identity
    /// endpoint would evict the receiver on the relay); otherwise binds a clean
    /// transient endpoint (safe - there is no receiver to collide with). The
    /// shared endpoint is cloned out (sharing the magicsock) so the lock is not
    /// held across the transfer.
    pub async fn send_file(
        &self,
        recipient_afid: &str,
        my_secret: &[u8],
        file_path: &str,
    ) -> Result<(), String> {
        let shared = self.identity_endpoint.lock().await.clone();
        let result = match shared {
            Some(ep) => {
                crate::peer::send_on_endpoint(&ep, recipient_afid, my_secret, file_path).await
            }
            None => {
                crate::peer::send_file_oneshot(
                    recipient_afid,
                    my_secret,
                    file_path,
                    relay_urls_from_env(),
                )
                .await
            }
        };
        result.map_err(|e| format!("send failed: {e}"))
    }

    /// Send a knock (predefined-code signal, no file) to a recipient. Reuses the
    /// shared identity endpoint when the receiver is up (Finding 6b), else binds a
    /// fresh one. `in_reply_to` carries the code of the knock being answered, for a
    /// bounded predefined question/answer exchange.
    pub async fn send_knock(
        &self,
        recipient_afid: &str,
        my_secret: &[u8],
        code: &str,
        in_reply_to: Option<String>,
    ) -> Result<(), String> {
        let shared = self.identity_endpoint.lock().await.clone();
        let result = match shared {
            Some(ep) => {
                crate::peer::send_knock_on_endpoint(
                    &ep,
                    recipient_afid,
                    my_secret,
                    code,
                    in_reply_to,
                )
                .await
            }
            None => {
                crate::peer::send_knock_oneshot(
                    recipient_afid,
                    my_secret,
                    code,
                    in_reply_to,
                    relay_urls_from_env(),
                )
                .await
            }
        };
        result.map_err(|e| format!("knock failed: {e}"))
    }

    /// Resolve a pending incoming offer. `accept = true` writes the file into
    /// `inbox_root/<label or short-AFID>/`; `accept = false` declines. `label`
    /// is the FE's friendly per-sender folder name (the friend alias). Errors if
    /// the transfer id is unknown (already resolved or timed out).
    pub async fn respond_to_offer(
        &self,
        transfer_id: &str,
        accept: bool,
        label: Option<String>,
    ) -> Result<(), String> {
        let entry = {
            let mut pending = self
                .pending_offers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            pending.remove(transfer_id)
        };
        let entry = entry.ok_or_else(|| {
            "this transfer is no longer pending (already answered or timed out)".to_string()
        })?;
        if !accept {
            let _ = entry.tx.send(None);
            return Ok(());
        }
        let inbox_root = {
            self.receiver
                .lock()
                .await
                .as_ref()
                .map(|h| h.inbox_root.clone())
        }
        .ok_or_else(|| "the receive loop is not running".to_string())?;
        let sub = sanitize_subfolder(
            &label
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| short_afid(&entry.sender_afid)),
        );
        let dest = inbox_root.join(sub);
        let _ = entry.tx.send(Some(dest));
        Ok(())
    }
}

fn emit_incoming_status(
    app: &AppHandle,
    state: &str,
    sender_afid: &str,
    name: &str,
    path: Option<String>,
    error: Option<String>,
) {
    let _ = app.emit(
        "peer://incoming-status",
        PeerIncomingStatusEvent {
            state: state.to_string(),
            sender_afid: sender_afid.to_string(),
            name: name.to_string(),
            path,
            error,
            at_ms: now_ms(),
        },
    );
}

fn emit_receiver_status(app: &AppHandle, state: &str, detail: Option<String>) {
    let _ = app.emit(
        "peer://receiver-status",
        PeerReceiverStatusEvent {
            state: state.to_string(),
            detail,
            at_ms: now_ms(),
        },
    );
}

fn emit_knock(app: &AppHandle, sender_afid: &str, code: &str, in_reply_to: Option<&str>) {
    let _ = app.emit(
        "peer://knock",
        PeerKnockEvent {
            sender_afid: sender_afid.to_string(),
            code: code.to_string(),
            in_reply_to: in_reply_to.map(|s| s.to_string()),
            at_ms: now_ms(),
        },
    );
}

/// The standing receive loop. Runs `peer::receive_on_endpoint` on the shared
/// identity endpoint (so outbound sends reuse it - Finding 6b) with two callbacks:
/// `decide` surfaces each offer to the FE and parks until the user answers;
/// `notify` emits the per-transfer outcome. Wrapped in `select!` so a toggle-OFF
/// cancel tears the loop down promptly (and `stop_receiver` drops the endpoint).
async fn receive_loop(
    app: AppHandle,
    cancel: CancellationToken,
    my_secret: Vec<u8>,
    endpoint: std::sync::Arc<aeroftp_peer_l0::PeerEndpoint>,
    pending_offers: std::sync::Arc<std::sync::Mutex<HashMap<String, PendingOffer>>>,
    next_transfer_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    emit_receiver_status(&app, "listening", None);

    let decide = {
        let app = app.clone();
        let pending_offers = pending_offers.clone();
        let next_transfer_id = next_transfer_id.clone();
        move |offer: aeroftp_peer_l0::IncomingOffer| {
            let app = app.clone();
            let pending_offers = pending_offers.clone();
            let next_transfer_id = next_transfer_id.clone();
            async move {
                let seq = next_transfer_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let transfer_id = format!("{}-{}", now_ms(), seq);
                let (tx, rx) = tokio::sync::oneshot::channel();
                {
                    let mut pending = pending_offers.lock().unwrap_or_else(|e| e.into_inner());
                    pending.insert(
                        transfer_id.clone(),
                        PendingOffer {
                            tx,
                            sender_afid: offer.sender_afid.clone(),
                        },
                    );
                }
                let _ = app.emit(
                    "peer://incoming-offer",
                    PeerIncomingOfferEvent {
                        transfer_id: transfer_id.clone(),
                        sender_afid: offer.sender_afid.clone(),
                        name: offer.name.clone(),
                        size: offer.size,
                        at_ms: now_ms(),
                    },
                );
                let answered = tokio::time::timeout(
                    std::time::Duration::from_secs(INCOMING_RESPONSE_TIMEOUT_SECS),
                    rx,
                )
                .await;
                match answered {
                    Ok(Ok(dest)) => dest,
                    // Timed out or the responder dropped: clean up + decline.
                    _ => {
                        let mut pending = pending_offers.lock().unwrap_or_else(|e| e.into_inner());
                        pending.remove(&transfer_id);
                        None
                    }
                }
            }
        }
    };

    let notify = {
        let app = app.clone();
        move |ev: aeroftp_peer_l0::ReceiveEvent| match ev {
            aeroftp_peer_l0::ReceiveEvent::Completed {
                sender_afid,
                name,
                path,
            } => {
                tracing::info!(
                    "AeroShare received file name={name} from afid={sender_afid} -> {}",
                    path.display()
                );
                emit_incoming_status(
                    &app,
                    "completed",
                    &sender_afid,
                    &name,
                    Some(path.to_string_lossy().to_string()),
                    None,
                )
            }
            aeroftp_peer_l0::ReceiveEvent::Declined { sender_afid, name } => {
                emit_incoming_status(&app, "declined", &sender_afid, &name, None, None)
            }
            // Log the cause on the receive side too: before this the failure was
            // emitted only as the FE event, so the run log had no record of WHY an
            // incoming transfer failed (Finding 7).
            aeroftp_peer_l0::ReceiveEvent::Failed {
                sender_afid,
                name,
                error,
            } => {
                tracing::warn!(
                    "AeroShare receive FAILED name={name} from afid={sender_afid}: {error}"
                );
                emit_incoming_status(&app, "failed", &sender_afid, &name, None, Some(error))
            }
            // A knock: a predefined-code signal (no file). The FE maps the code to
            // localized text and surfaces it as a notification (+ optional quick reply).
            aeroftp_peer_l0::ReceiveEvent::Knock {
                sender_afid,
                code,
                in_reply_to,
            } => {
                tracing::info!(
                    "AeroShare knock code={code} from afid={sender_afid} in_reply_to={in_reply_to:?}"
                );
                emit_knock(&app, &sender_afid, &code, in_reply_to.as_deref())
            }
        }
    };

    let fut = crate::peer::receive_on_endpoint(&endpoint, &my_secret, decide, notify);
    tokio::select! {
        _ = cancel.cancelled() => {
            tracing::info!("AeroShare: receive loop cancelled");
        }
        result = fut => {
            // The loop now only returns on a CLOSED endpoint (Ok) or a fatal
            // setup error (Err); a per-connection accept failure no longer ends it.
            if let Err(e) = result {
                tracing::warn!("AeroShare receive loop ended with error: {e:#}");
                emit_receiver_status(&app, "error", Some(format!("{e:#}")));
            }
        }
    }
}

/// The long-lived replication task for one drive. Loops engine passes until
/// cancelled; every state change is emitted, never rendered directly.
#[allow(clippy::too_many_arguments)]
async fn sync_loop(
    app: AppHandle,
    cancel: CancellationToken,
    namespace: String,
    ticket: String,
    content_key: zeroize::Zeroizing<Vec<u8>>,
    replica_dir: PathBuf,
    store_dir: PathBuf,
    states: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
) {
    record_sync(&app, &states, &namespace, "starting", None);
    let out = replica_dir.to_string_lossy().to_string();
    let store = Some(store_dir.to_string_lossy().to_string());
    // Host-UI status reporter: the replicate worker calls this at phase
    // boundaries ("live" once a pass has converged and is just watching,
    // "syncing" when a delta lands), so the friend-card dot settles to green
    // instead of being pinned to "syncing" for the whole watch window. It
    // records into the same registry so a re-pull restores the real state (F3).
    let reporter: Option<crate::peer::StatusReporter> = {
        let app = app.clone();
        let namespace = namespace.clone();
        let states = states.clone();
        let cancel = cancel.clone();
        Some(std::sync::Arc::new(move |state: &str| {
            // Once cancelled (standby / replace), suppress late engine
            // callbacks so a trailing "live"/"syncing" cannot overwrite the
            // terminal state the canceller recorded (e.g. "standby").
            if cancel.is_cancelled() {
                return;
            }
            record_sync(&app, &states, &namespace, state, None);
        }))
    };
    loop {
        if cancel.is_cancelled() {
            break;
        }
        record_sync(&app, &states, &namespace, "syncing", None);
        let pass = crate::peer::replicate_drive_cap(
            ticket.clone(),
            out.clone(),
            &content_key,
            WATCH_WINDOW_SECS,
            store.clone(),
            relay_urls_from_env(),
            reporter.clone(),
        );
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = pass => {
                match result {
                    // Watch window elapsed: re-enter for the next converge+watch pass.
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!("AeroShare sync pass failed for {namespace}: {e}");
                        record_sync(&app, &states, &namespace, "error", Some(e.to_string()));
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(ERROR_BACKOFF_SECS)) => {}
                        }
                    }
                }
            }
        }
    }
    // No terminal emit here: the loop only exits on cancellation, and the
    // CANCELLER owns the terminal state (`standby` from PeerRuntime::standby,
    // or a fresh `starting` from the replacement task in ensure_sub). Emitting
    // "stopped" unconditionally would race-overwrite the canceller's state.
}

/// The long-lived publish/serve task for one shared folder. Publishes the
/// folder as an encrypted drive (reporting `(namespace, ticket)` on `ready`),
/// then serves it until cancelled. P1 serves the version captured at share
/// time (no auto-republish; folder edits flow with the P2 write direction).
#[allow(clippy::too_many_arguments)]
async fn share_loop(
    app: AppHandle,
    cancel: CancellationToken,
    dir: String,
    drive_name: String,
    content_key: zeroize::Zeroizing<Vec<u8>>,
    identity_secret: zeroize::Zeroizing<Vec<u8>>,
    store_dir: String,
    ready: tokio::sync::oneshot::Sender<crate::peer::DrivePublished>,
) {
    emit_share_status(&app, &dir, None, "starting", None);
    let fut = crate::peer::publish_drive_cap(
        dir.clone(),
        &content_key,
        &identity_secret,
        drive_name,
        Some(store_dir),
        0, // republish_after: none in P1 (serve the captured version)
        0, // republish_count
        relay_urls_from_env(),
        ready,
    );
    tokio::pin!(fut);
    tokio::select! {
        _ = cancel.cancelled() => {
            emit_share_status(&app, &dir, None, "stopped", None);
        }
        result = &mut fut => match result {
            Ok(()) => emit_share_status(&app, &dir, None, "stopped", None),
            Err(e) => {
                tracing::warn!("AeroShare publish failed for {dir}: {e}");
                emit_share_status(&app, &dir, None, "error", Some(e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_store_slug_is_stable_and_filename_safe() {
        let a = publish_store_slug(Path::new("/home/alice/Photos Estate 2026"));
        let b = publish_store_slug(Path::new("/home/alice/Photos Estate 2026"));
        assert_eq!(a, b, "same path, same slug (namespace stability)");
        assert!(a.starts_with("PhotosEstate2026-"));
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));

        let c = publish_store_slug(Path::new("/home/bob/Photos Estate 2026"));
        assert_ne!(a, c, "different absolute path, different slug");

        // FNV-1a regression pin: a silent algorithm change would orphan every
        // existing publish store (and with it the drive namespaces).
        assert_eq!(
            publish_store_slug(Path::new("/tmp/x")),
            "x-6cc122ddf274426c"
        );
    }
}
