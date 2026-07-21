//! Provider Commands - Tauri commands for multi-protocol cloud storage
//!
//! This module provides Tauri commands that route operations through
//! the StorageProvider abstraction, enabling support for FTP, WebDAV, S3, etc.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::provider_transfer_executor::{
    resolve_provider_list_session_model, resolve_provider_transfer_runtime,
    ProviderDownloadExecutor, ProviderUploadExecutor,
};
use crate::providers::{
    FileVersion, LockInfo, ProviderConfig, ProviderError, ProviderFactory, ProviderType,
    RemoteEntry, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult, SharePermission,
    StorageInfo, StorageProvider,
};
use crate::transfer_dag::{DagObserver, TransferDagBuilder};
use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
use crate::transfer_event_sink::{AppHandleSink, GuiDagObserver, TransferEventSink};
use crate::transfer_orchestrator::{execute_batch, ProgressObserver, TransferBatch};
use crate::transfer_settings::TransferSettingsInput;
use crate::util::AbortOnDrop;

/// Global flag: when true, filesystem watcher should suppress sync triggers.
/// Set during folder download/upload to prevent AeroCloud interference.
pub static TRANSFER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn is_plain_github_provider(provider: &mut dyn StorageProvider) -> bool {
    provider.provider_type() == ProviderType::GitHub
        && !crate::crypt_overlay_provider::is_crypt_overlay_provider(provider)
}

/// RAII guard that clears `TRANSFER_IN_PROGRESS` on drop. Covers normal
/// returns AND panic-unwind: without this, a panic in the folder transfer
/// pipeline left the watcher suppressed forever until app restart.
struct TransferInProgressGuard(());

impl TransferInProgressGuard {
    fn acquire() -> Self {
        TRANSFER_IN_PROGRESS.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for TransferInProgressGuard {
    fn drop(&mut self) {
        TRANSFER_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// State for managing the active storage provider
/// Connection-scoped cache of the unlocked overlay keys, so a view-only lock
/// (toggle the crypt overlay off then on on the SAME connection) can re-arm
/// instantly without re-running the KDF (Argon2id for AeroCrypt, scrypt for
/// rclone-crypt). Bound to a `generation`: any connect/disconnect/swap or an
/// explicit hard lock bumps the generation and drops this (zeroizing the keys),
/// so a cached key can NEVER be applied to a different connection.
struct CachedCryptOverlay {
    keys: crate::crypt_overlay_provider::OverlayKeys,
    /// Normalized plaintext scope the overlay was armed at.
    scope: String,
    /// Wire tag: `"rclone-crypt"` | `"aerocrypt"`.
    kind: String,
    /// Connection generation this cache belongs to.
    generation: u64,
}

pub struct ProviderState {
    /// Currently active provider (if connected)
    pub provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    /// Connection-scoped unlocked-overlay-key cache for instant re-arm after a
    /// view-only lock. `std::sync::Mutex`: the critical sections are tiny and
    /// never await. Wiped (keys zeroized) on connect/disconnect and hard lock.
    cached_overlay: std::sync::Mutex<Option<CachedCryptOverlay>>,
    /// Bumped on every connection change (connect swap, disconnect, hard lock)
    /// so a stale cached key can never be re-armed onto a new connection.
    connection_generation: Arc<AtomicU64>,
    /// Current provider configuration
    pub config: Arc<Mutex<Option<ProviderConfig>>>,
    /// Cancel flag for aborting folder transfers
    pub cancel_flag: Arc<AtomicBool>,
    /// Cancellation token cloned into async retry waits so user cancel wakes them immediately.
    cancel_token: Mutex<CancellationToken>,
    /// Held GitHub App installation token: never crosses IPC.
    /// Set by `github_app_token_from_pem`/`_from_vault`, consumed by `provider_connect`.
    pub held_github_app_token: Mutex<Option<String>>,
    /// Counter of transfer operations that hold a `SharedProvider` clone
    /// from a spawned task without keeping the provider mutex locked.
    /// `provider_disconnect` and the `provider_connect` swap path drain
    /// this before mutating the slot so an active DAG transfer cannot see
    /// the provider box yanked from under it (issue #233).
    pub in_flight_transfers: Arc<AtomicUsize>,
    /// Wakes drain waiters when an in-flight transfer guard drops.
    in_flight_notify: Arc<Notify>,
    /// Crypt-overlay CAPABILITY flag (sticky for the session): true once a crypt
    /// overlay has been applied to this connection (`provider_apply_crypt_overlay`),
    /// cleared on connect/disconnect and on a full overlay removal. Combined with
    /// [`Self::overlay_wrapped`] it gates the AeroAgent `gui_tools` raw-provider
    /// paths: a raw write is refused while the session is crypt-capable but the
    /// live provider is currently UNWRAPPED (badge locked / outside the encrypted
    /// scope), which would otherwise corrupt the crypt store with plaintext.
    pub active_crypt_overlay: Arc<AtomicBool>,
    /// True while the live `provider` box is currently a `CryptOverlayProvider`
    /// (Phase 3 on-demand model): set by `provider_apply_crypt_overlay`, cleared
    /// by `provider_clear_crypt_overlay` and on connect/disconnect. When wrapped,
    /// every surface that touches `provider` (browser, agent `gui_tools`, speed
    /// test, preview) is transparently crypt-aware; when unwrapped the raw
    /// connection is exposed (plaintext outside the scope, ciphertext while
    /// locked).
    pub overlay_wrapped: Arc<AtomicBool>,
}

impl ProviderState {
    pub fn new() -> Self {
        Self {
            provider: Arc::new(Mutex::new(None)),
            cached_overlay: std::sync::Mutex::new(None),
            connection_generation: Arc::new(AtomicU64::new(0)),
            config: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            cancel_token: Mutex::new(CancellationToken::new()),
            held_github_app_token: Mutex::new(None),
            in_flight_transfers: Arc::new(AtomicUsize::new(0)),
            in_flight_notify: Arc::new(Notify::new()),
            active_crypt_overlay: Arc::new(AtomicBool::new(false)),
            overlay_wrapped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn reset_cancel_state(&self) -> CancellationToken {
        self.cancel_flag.store(false, Ordering::Relaxed);
        let token = CancellationToken::new();
        *self.cancel_token.lock().await = token.clone();
        token
    }

    pub async fn current_cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().await.clone()
    }

    pub async fn request_cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.cancel_token.lock().await.cancel();
    }

    /// Fail-closed guard against a raw write into a crypt-bound session.
    ///
    /// When the session is crypt-CAPABLE ([`Self::active_crypt_overlay`]) but the
    /// live `provider` box is currently UNWRAPPED ([`Self::overlay_wrapped`] false:
    /// the badge is locked or the browser stepped outside the encrypted scope),
    /// any write that reaches `provider` directly hits the RAW backend and would
    /// inject plaintext content or a cleartext name into the encrypted store,
    /// silently corrupting it. Every command that mutates the remote through the
    /// raw `ProviderState::provider` (uploads, mkdir, delete, rename, server-copy,
    /// and the AeroAgent `gui_tools` paths) calls this first and refuses in exactly
    /// that window. When the overlay is wrapped the write goes through the crypt
    /// decorator transparently, and when the session is not crypt-capable at all
    /// this is a no-op.
    pub fn guard_no_raw_crypt_write(&self, op: &str) -> Result<(), String> {
        let crypt_capable = self.active_crypt_overlay.load(Ordering::SeqCst);
        let wrapped = self.overlay_wrapped.load(Ordering::SeqCst);
        if crypt_capable && !wrapped {
            return Err(format!(
                "{op} is blocked: this session has a crypt overlay that is currently locked or \
                 out of its encrypted scope, so a direct provider write would inject plaintext \
                 into the encrypted store. Re-enter the encrypted scope (or unlock the overlay) \
                 before writing."
            ));
        }
        Ok(())
    }

    pub fn arm_crypt_capability(&self) {
        self.active_crypt_overlay.store(true, Ordering::SeqCst);
        self.overlay_wrapped.store(false, Ordering::SeqCst);
    }

    /// Store (a clone of) the unlocked overlay keys for the current connection so
    /// a later view-only lock can re-arm without re-deriving. Stamped with the
    /// live generation; a re-arm only proceeds when the generation still matches.
    fn store_overlay_key_cache(
        &self,
        keys: crate::crypt_overlay_provider::OverlayKeys,
        scope: String,
        kind: String,
    ) {
        let generation = self.connection_generation.load(Ordering::SeqCst);
        if let Ok(mut slot) = self.cached_overlay.lock() {
            *slot = Some(CachedCryptOverlay {
                keys,
                scope,
                kind,
                generation,
            });
        }
    }

    /// Hard-invalidate the key cache: bump the connection generation and drop the
    /// cached keys (zeroized). Called on connect/disconnect/swap and on an
    /// explicit hard lock. After this, a re-arm falls back to a full re-derive.
    fn invalidate_overlay_key_cache(&self) {
        self.connection_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut slot) = self.cached_overlay.lock() {
            *slot = None;
        }
    }

    /// Take the cached keys for an instant re-arm IF one exists AND still belongs
    /// to the live connection generation. Returns `(keys, scope, kind)`. A stale
    /// entry (generation moved on) is dropped and `None` returned so the caller
    /// re-derives. The clone is cheap key material; it zeroizes on drop.
    fn cached_overlay_for_rearm(
        &self,
    ) -> Option<(crate::crypt_overlay_provider::OverlayKeys, String, String)> {
        let live = self.connection_generation.load(Ordering::SeqCst);
        let mut slot = self.cached_overlay.lock().ok()?;
        match slot.as_ref() {
            Some(c) if c.generation == live => {
                Some((c.keys.clone(), c.scope.clone(), c.kind.clone()))
            }
            Some(_) => {
                // Stale (connection changed under it): drop so it can never re-arm.
                *slot = None;
                None
            }
            None => None,
        }
    }
}

impl Default for ProviderState {
    fn default() -> Self {
        Self::new()
    }
}

/// Error string returned by a connect command when the user aborts an
/// in-progress connection (Esc / "still connecting" Cancel, Ehud wishlist
/// W3.1 #270.5). The frontend matches on this marker to surface a calm
/// "connection cancelled" toast instead of a "connection failed" error, and
/// to skip stamping a connect-failure marker on the saved-server card.
pub const CONNECT_CANCELLED: &str = "CONNECT_CANCELLED";

/// Registry of cancellation tokens for in-progress connection attempts,
/// keyed by a frontend-generated connection token string. It lets the
/// `cancel_connection` command abort a slow `provider_connect` / `connect_ftp`
/// running under `tokio::select!` without coupling to either command's
/// provider/ftp state. A token is registered when the connect starts and
/// removed by a drop guard when the connect resolves (success, error, or
/// cancel), so the map never accumulates stale entries.
#[derive(Default)]
pub struct ConnectionCancelRegistry {
    tokens: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
}

impl ConnectionCancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh cancellation token under `key` and return it for the
    /// connect command to `select!` on. If a token is already registered for
    /// the same key (a retried attempt that reused the id), it is cancelled
    /// and replaced so a stale entry can never shadow the live attempt.
    pub fn register(&self, key: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let mut map = self.lock();
        if let Some(previous) = map.insert(key.to_string(), token.clone()) {
            previous.cancel();
        }
        token
    }

    /// Remove the token for `key`. Called by the drop guard once the connect
    /// resolves. A no-op when the key is absent.
    pub fn unregister(&self, key: &str) {
        self.lock().remove(key);
    }

    /// Cancel the in-progress connection registered under `key`. Idempotent:
    /// returns `true` if a live token was found and signalled, `false` if the
    /// key was already gone (the connect resolved before the cancel landed, or
    /// it was never registered).
    pub fn cancel(&self, key: &str) -> bool {
        match self.lock().get(key) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, CancellationToken>> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub fn active_count(&self) -> usize {
        self.lock().len()
    }
}

/// RAII guard that de-registers a connection cancellation token from the
/// [`ConnectionCancelRegistry`] on every exit path of a connect command,
/// including early `?` returns and the cancel branch. Without it a token that
/// outlived its connect would let a late `cancel_connection` cancel a fresh,
/// unrelated attempt that happened to reuse the id.
pub(crate) struct ConnectTokenGuard<'a> {
    registry: &'a ConnectionCancelRegistry,
    key: String,
}

impl<'a> ConnectTokenGuard<'a> {
    pub(crate) fn new(registry: &'a ConnectionCancelRegistry, key: String) -> Self {
        Self { registry, key }
    }
}

impl Drop for ConnectTokenGuard<'_> {
    fn drop(&mut self) {
        self.registry.unregister(&self.key);
    }
}

/// Run a connect future, abortable via the shared [`ConnectionCancelRegistry`]
/// under `connect_token`, returning `CONNECT_CANCELLED` if an Esc / "still
/// connecting" Cancel fires first. Registers the token for the future's
/// lifetime (de-registered by a drop guard on every exit path) and races it
/// against `token.cancelled()` with `tokio::select!`; dropping the future on
/// cancel tears down the in-flight work (HTTP connect, subprocess) cleanly.
///
/// When `connect_token` is `None` the future simply runs to completion with no
/// cancellation point. This is the same plumbing `provider_connect` does inline
/// (#270.5), factored out so the OAuth connect path and the MEGAcmd WebDAV URL
/// preflight, which run outside `provider_connect`, become cancellable too
/// (#360).
pub(crate) async fn run_cancellable_connect<T, F>(
    cancel_registry: &ConnectionCancelRegistry,
    connect_token: Option<&str>,
    fut: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let cancel_token = connect_token.map(|key| cancel_registry.register(key));
    let _cancel_guard =
        connect_token.map(|key| ConnectTokenGuard::new(cancel_registry, key.to_string()));
    match cancel_token.as_ref() {
        Some(token) => tokio::select! {
            res = fut => res,
            _ = token.cancelled() => Err(CONNECT_CANCELLED.to_string()),
        },
        None => fut.await,
    }
}

/// Marker returned by a foreground listing (`provider_list_files`,
/// `provider_change_dir`) that the user aborted with the remote panel's Cancel
/// button. The frontend matches on it to stay silent: an abort the user asked
/// for is not an error, so no "listing failed" toast, and the connect flow that
/// issued the listing must bail out instead of painting the late result.
pub const LISTING_CANCELLED: &str = "LISTING_CANCELLED";

/// Cancellation handle for the foreground remote listing currently in flight.
///
/// The panel spinner's Cancel button used to be cosmetic: it hid the spinner
/// while `provider_list_files` kept running, and the connect flow then painted
/// the late result anyway. A cancel cannot be honoured from the frontend alone,
/// because the listing holds `ProviderState::provider` for its whole duration:
/// a `provider_disconnect` issued to force the point would simply queue behind
/// the stuck listing on that very mutex. So the abort has to happen inside the
/// listing command, and the cancel signal has to reach it without touching any
/// provider lock. This state is exactly that signal, and `cancel_remote_listing`
/// is deliberately the one command that takes it and nothing else.
///
/// Aborting is safe because every provider's `list()` is pure async (no
/// `spawn_blocking`, verified across `providers/`), so dropping the future on
/// the cancel branch of `tokio::select!` tears down the in-flight request and
/// releases the provider mutex, which is what lets the follow-up disconnect run
/// immediately rather than inheriting the stall.
///
/// The slot holds the latest armed listing plus a generation id, so a guard can
/// disarm only its own token and never clear a newer listing's. A replaced token
/// is left uncancelled on purpose: a superseding listing is not a user cancel,
/// and the frontend already discards the stale response by generation.
#[derive(Default)]
pub struct ListingCancelState {
    slot: std::sync::Mutex<Option<(u64, CancellationToken)>>,
    next_id: AtomicU64,
}

impl ListingCancelState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a fresh token for a listing that is about to start, superseding any
    /// token left in the slot. Returns the generation id (for the drop guard)
    /// and the token to `select!` on.
    fn arm(&self) -> (u64, CancellationToken) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let token = CancellationToken::new();
        *self.lock() = Some((id, token.clone()));
        (id, token)
    }

    /// Clear the slot when it still holds generation `id`. A no-op once a newer
    /// listing has taken it over, so a resolving listing can never disarm the
    /// cancel of the one that replaced it.
    fn disarm(&self, id: u64) {
        let mut slot = self.lock();
        if slot.as_ref().is_some_and(|(current, _)| *current == id) {
            *slot = None;
        }
    }

    /// Signal the in-flight foreground listing. Idempotent: `false` means there
    /// was nothing to cancel (it resolved before the click landed), which the UI
    /// treats as success, since the outcome the user asked for already holds.
    pub fn cancel(&self) -> bool {
        match self.lock().as_ref() {
            Some((_, token)) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u64, CancellationToken)>> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub fn is_armed(&self) -> bool {
        self.lock().is_some()
    }
}

/// RAII guard that disarms a listing token on every exit path of the listing
/// command, including early `?` returns and the cancel branch, so a later Cancel
/// click can never signal a token whose listing already resolved.
struct ListingTokenGuard<'a> {
    state: &'a ListingCancelState,
    id: u64,
}

impl<'a> ListingTokenGuard<'a> {
    fn new(state: &'a ListingCancelState, id: u64) -> Self {
        Self { state, id }
    }
}

impl Drop for ListingTokenGuard<'_> {
    fn drop(&mut self) {
        self.state.disarm(self.id);
    }
}

/// Run a foreground listing future, abortable from the panel spinner's Cancel
/// button, returning [`LISTING_CANCELLED`] when the user aborts first.
///
/// `fut` must include the `state.provider.lock().await` acquisition, not just
/// the listing call: a listing queued behind another stuck provider operation
/// looks identical to the user (a frozen panel) and must be just as cancellable.
pub(crate) async fn run_cancellable_listing<T, F>(
    listing_cancel: &ListingCancelState,
    fut: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let (id, token) = listing_cancel.arm();
    let _guard = ListingTokenGuard::new(listing_cancel, id);
    tokio::select! {
        res = fut => res,
        _ = token.cancelled() => Err(LISTING_CANCELLED.to_string()),
    }
}

/// Abort the foreground remote listing in flight (panel spinner Cancel). Takes
/// only [`ListingCancelState`], never `ProviderState`, so it cannot itself
/// block on the provider mutex the stuck listing is holding. Idempotent, and
/// reports whether a live listing was actually signalled.
#[tauri::command]
pub async fn cancel_remote_listing(
    listing_cancel: State<'_, ListingCancelState>,
) -> Result<bool, String> {
    let signalled = listing_cancel.cancel();
    if signalled {
        info!("cancel_remote_listing: signalled cancel for in-flight listing");
    }
    Ok(signalled)
}

/// RAII guard around `ProviderState::in_flight_transfers`. Acquired by the
/// command-level entry points before they hand a `SharedProvider` clone to
/// a spawned DAG transfer task and dropped only when that task returns,
/// so `provider_disconnect` / `provider_connect` (swap) can drain the
/// counter to zero before mutating the provider slot. See [issue #233].
pub struct TransferOperationGuard {
    counter: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl TransferOperationGuard {
    fn acquire(state: &ProviderState) -> Self {
        state.in_flight_transfers.fetch_add(1, Ordering::SeqCst);
        Self {
            counter: Arc::clone(&state.in_flight_transfers),
            notify: Arc::clone(&state.in_flight_notify),
        }
    }
}

impl Drop for TransferOperationGuard {
    fn drop(&mut self) {
        if self.counter.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
        }
    }
}

/// Best-effort wait until `state.in_flight_transfers` reaches zero, bounded
/// by `total_timeout` so a hung transfer cannot indefinitely block a
/// disconnect or a session swap. On timeout the function logs a warning
/// and returns, restoring the pre-fix behaviour (the active transfer will
/// surface `NotConnected` once the slot is mutated).
/// Public so session-only providers (MTP PLACES open) can reuse the same
/// swap-safe drain before replacing `ProviderState::provider`.
pub(crate) async fn drain_in_flight_transfers(state: &ProviderState, total_timeout: Duration) {
    let deadline = Instant::now() + total_timeout;
    while state.in_flight_transfers.load(Ordering::SeqCst) > 0 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!(
                "drain_in_flight_transfers: {} transfers still in flight after {:?}; \
                 proceeding (active transfers will surface NotConnected)",
                state.in_flight_transfers.load(Ordering::SeqCst),
                total_timeout
            );
            return;
        }
        // Register interest BEFORE re-checking the counter so a notify from
        // the last drop between check and wait is not lost. Cap the
        // individual wait at 1s so a missed notification cannot stall the
        // drain forever.
        let notified = state.in_flight_notify.notified();
        let _ = tokio::time::timeout(remaining.min(Duration::from_secs(1)), notified).await;
    }
}

// ============ Auto-reconnect on idle disconnect (T-AUTO-RECONNECT-IDLE) ============

/// Lifecycle phases of a silent reconnect attempt. Mirrored to the
/// frontend via the `provider-session` Tauri event so the UI can
/// surface a transient toast (e.g. "Session expired, reconnecting...")
/// without forcing the user to disconnect and reconnect manually.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SessionEventKind {
    /// The previous operation hit a dead session.
    Lost,
    /// Reconnect attempt is in flight.
    Reconnecting,
    /// Reconnect succeeded and the original op was replayed.
    Reconnected,
    /// Reconnect itself failed: the original error stands.
    ReconnectFailed,
}

#[derive(Debug, Clone, Serialize)]
struct SessionEvent {
    kind: SessionEventKind,
    /// Free-form detail (server reply, network error, ...). Empty for
    /// success transitions where there is nothing meaningful to show.
    detail: String,
}

fn emit_session_event(app: &AppHandle, kind: SessionEventKind, detail: impl Into<String>) {
    let _ = app.emit(
        "provider-session",
        SessionEvent {
            kind,
            detail: detail.into(),
        },
    );
}

/// Drive a single silent reconnect attempt against the live provider
/// instance, reusing the credentials it captured at original
/// connect-time. The provider's internal `current_dir` is saved and
/// best-effort restored after the new session is established, so a
/// subsequent retry of the user's operation sees the same path
/// context as before the disconnect.
///
/// Returns `Ok(restored_pwd)` on success. The caller is responsible
/// for replaying the failed operation against the freshly-reconnected
/// provider.
async fn try_silent_reconnect(
    app: &AppHandle,
    provider: &mut Box<dyn StorageProvider>,
) -> Result<String, ProviderError> {
    let prev_dir = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    emit_session_event(app, SessionEventKind::Reconnecting, "");
    tracing::warn!("Provider session lost; attempting silent reconnect");

    provider.connect().await.inspect_err(|e| {
        emit_session_event(app, SessionEventKind::ReconnectFailed, e.to_string());
    })?;

    // Best-effort cwd restore. Failure here is not fatal: the caller's
    // retry will hit the right error if the path is genuinely gone.
    if prev_dir != "/" {
        if let Err(e) = provider.cd(&prev_dir).await {
            tracing::warn!(
                "Reconnect succeeded but failed to restore previous dir {}: {}",
                prev_dir,
                e
            );
        }
    }
    Ok(prev_dir)
}

// ============ Request/Response Types ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConnectionParams {
    /// Protocol type: "ftp", "ftps", "sftp", "webdav", "s3", "mega", "opendrive"
    pub protocol: String,
    /// Optional saved-provider preset id (`nextcloud`, `koofr`, `custom-webdav`, `megacmd`, ...).
    #[serde(default, alias = "providerId")]
    pub provider_id: Option<String>,
    /// Host/URL (FTP server, WebDAV URL, or S3 endpoint)
    pub server: String,
    /// Port (optional, defaults based on protocol)
    pub port: Option<u16>,
    /// Username or Access Key ID
    pub username: String,
    /// Password or Secret Access Key
    pub password: String,
    /// Initial remote path to navigate to
    pub initial_path: Option<String>,
    /// S3 bucket name
    pub bucket: Option<String>,
    /// S3/cloud region
    pub region: Option<String>,
    /// Custom endpoint for S3-compatible services
    pub endpoint: Option<String>,
    /// Use path-style URLs for S3
    pub path_style: Option<bool>,
    /// Skip WebDAV Authorization headers for anonymous local bridges
    pub anonymous: Option<bool>,
    /// S3: Default storage class for uploads (STANDARD, STANDARD_IA, GLACIER, etc.)
    pub storage_class: Option<String>,
    /// S3: Server-side encryption mode (AES256 or aws:kms)
    pub sse_mode: Option<String>,
    /// S3: KMS key ID for SSE-KMS encryption
    pub sse_kms_key_id: Option<String>,
    /// S3: AWS STS session token for temporary credentials (AssumeRole / SSO).
    /// AWS-only; ignored by S3-compatible backends without STS.
    pub session_token: Option<String>,
    /// S3: ARN of an IAM role to assume via STS `AssumeRole` (issue #301).
    /// When set, `connect()` exchanges the access key/secret for temporary
    /// credentials. AWS-only.
    pub role_arn: Option<String>,
    /// S3: `ExternalId` for the AssumeRole call (cross-account protection).
    pub role_external_id: Option<String>,
    /// S3: caller-chosen `RoleSessionName`; defaults to `aeroftp-session`.
    pub role_session_name: Option<String>,
    /// S3: requested credential lifetime in seconds (900..=43200).
    pub role_duration_seconds: Option<u32>,
    /// S3: MFA device serial/ARN required by the role's trust policy.
    pub role_mfa_serial: Option<String>,
    /// S3: one-time MFA token code for the first AssumeRole. Single-use, never
    /// persisted.
    pub role_mfa_token_code: Option<String>,
    /// Save session keys (MEGA)
    pub save_session: Option<bool>,
    /// Backend selection for MEGA: "native" or "megacmd"
    pub mega_mode: Option<String>,
    /// Session expiry timestamp (MEGA)
    pub session_expires_at: Option<i64>,
    /// MEGA: whether to logout/clear session on disconnect
    pub logout_on_disconnect: Option<bool>,
    /// SFTP: Path to private key file
    pub private_key_path: Option<String>,
    /// SFTP: Passphrase for encrypted private key
    pub key_passphrase: Option<String>,
    /// SFTP: Connection timeout in seconds
    pub timeout: Option<u64>,
    /// FTP/FTPS: TLS mode ("none", "explicit", "implicit", "explicit_if_available")
    pub tls_mode: Option<String>,
    /// FTP/FTPS: Accept invalid/self-signed certificates
    pub verify_cert: Option<bool>,
    /// Filen: Optional TOTP 2FA code
    pub two_factor_code: Option<String>,
    /// Filen/MEGA/Internxt: Optional persisted base32 TOTP secret. When set,
    /// the backend derives the 6-digit code on every connect (no prompt).
    pub totp_secret: Option<String>,
    /// Filen: Optional CLI API key. When set, the provider authenticates with
    /// it and skips the `/v3/login` call (and therefore the 2FA TOTP window).
    /// The password is still required: it derives the E2E master key.
    pub filen_api_key: Option<String>,
    /// GitHub: auth mode used to obtain the token
    pub github_auth_mode: Option<String>,
    /// GitHub: App ID for installation-token mode
    pub github_app_id: Option<String>,
    /// GitHub: Installation ID for installation-token mode
    pub github_installation_id: Option<String>,
    /// GitHub: Local PEM path for installation-token refresh
    pub github_pem_path: Option<String>,
    /// GitHub: Installation token expiry (ISO 8601)
    pub github_token_expires_at: Option<String>,
    /// GitHub: optional branch override
    pub github_branch: Option<String>,
    /// AeroShare (`protocol="peer"`): the drive's iroh-docs namespace id.
    /// For a peer connection `server` carries the friend's AeroFTP-ID and
    /// `username` the friend's display alias.
    #[serde(default, alias = "peerNamespace")]
    pub peer_namespace: Option<String>,
    /// AeroShare: the publisher's DocTicket (dial addresses + namespace).
    #[serde(default, alias = "peerTicket")]
    pub peer_ticket: Option<String>,
    /// AeroShare: absolute LOCAL folder the drive replicates into.
    #[serde(default, alias = "peerLocalFolder")]
    pub peer_local_folder: Option<String>,
    /// AeroShare: my role on the drive (`replicator` default; `publisher`
    /// arrives with the Phase 2 write direction).
    #[serde(default, alias = "peerRole")]
    pub peer_role: Option<String>,
    /// W3.1 (#270.5): frontend-generated token identifying this connection
    /// attempt. When present, `provider_connect` registers a cancellation
    /// token under it so an Esc / "still connecting" Cancel can abort the
    /// connect via `cancel_connection`. Absent for callers that opt out.
    #[serde(default, alias = "connectToken")]
    pub connect_token: Option<String>,
    /// OpenDrive (#252): per-account default privacy (`private`/`public`/
    /// `hidden`) applied to newly created folders and uploaded files. Stored
    /// in `ProviderConfig.extra["default_privacy"]`.
    #[serde(default, alias = "opendriveDefaultPrivacy")]
    pub opendrive_default_privacy: Option<String>,
}

impl ProviderConnectionParams {
    /// Convert to provider configuration
    pub fn to_provider_config(&self) -> Result<ProviderConfig, String> {
        let provider_type = match self.protocol.to_lowercase().as_str() {
            "ftp" => ProviderType::Ftp,
            "ftps" => ProviderType::Ftps,
            "sftp" => ProviderType::Sftp,
            "webdav" => ProviderType::WebDav,
            "s3" => ProviderType::S3,
            "mega" => ProviderType::Mega,
            "box" => ProviderType::Box,
            "pcloud" => ProviderType::PCloud,
            "azure" => ProviderType::Azure,
            "filen" => ProviderType::Filen,
            "internxt" => ProviderType::Internxt,
            "kdrive" => ProviderType::KDrive,
            "jottacloud" => ProviderType::Jottacloud,
            "drime" => ProviderType::DrimeCloud,
            "filelu" => ProviderType::FileLu,
            "koofr" => ProviderType::Koofr,
            "opendrive" => ProviderType::OpenDrive,
            "yandexdisk" => ProviderType::YandexDisk,
            "github" => ProviderType::GitHub,
            "gitlab" => ProviderType::GitLab,
            "swift" => ProviderType::Swift,
            "googlephotos" | "google_photos" => ProviderType::GooglePhotos,
            "immich" => ProviderType::Immich,
            "imagekit" | "image_kit" => ProviderType::ImageKit,
            "uploadcare" | "upload_care" => ProviderType::Uploadcare,
            "cloudinary" => ProviderType::Cloudinary,
            "b2" | "backblaze" | "backblazeb2" => ProviderType::Backblaze,
            "peer" | "aeroshare" => ProviderType::Peer,
            other => return Err(format!("Unknown protocol: {}", other)),
        };

        let mut extra = std::collections::HashMap::new();
        if let Some(provider_id) = self
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            extra.insert("provider_id".to_string(), provider_id.to_string());
        }

        if let Some(ref provider_id) = self.provider_id {
            if !provider_id.trim().is_empty() {
                extra.insert(
                    crate::providers::mega_df::PROVIDER_ID_META_KEY.to_string(),
                    provider_id.trim().to_string(),
                );
            }
        }

        // OpenDrive (#252): persist the per-account default privacy, ignoring
        // anything that isn't a valid private/public/hidden token.
        if provider_type == ProviderType::OpenDrive {
            if let Some(token) = self
                .opendrive_default_privacy
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase)
                .filter(|s| {
                    crate::providers::opendrive::OpenDriveAccessLevel::from_token(s).is_some()
                })
            {
                extra.insert("default_privacy".to_string(), token);
            }
        }

        // Add S3-specific options
        if provider_type == ProviderType::S3 {
            if let Some(ref bucket) = self.bucket {
                extra.insert("bucket".to_string(), bucket.clone());
            } else {
                return Err("S3 requires a bucket name".to_string());
            }
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us-east-1".to_string());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            if let Some(path_style) = self.path_style {
                extra.insert("path_style".to_string(), path_style.to_string());
            }
            // S3 enterprise: storage class, SSE mode, KMS key
            if let Some(ref sc) = self.storage_class {
                if !sc.is_empty() {
                    extra.insert("storage_class".to_string(), sc.clone());
                }
            }
            if let Some(ref sse) = self.sse_mode {
                if !sse.is_empty() {
                    extra.insert("sse_mode".to_string(), sse.clone());
                }
            }
            if let Some(ref kms) = self.sse_kms_key_id {
                if !kms.is_empty() {
                    extra.insert("sse_kms_key_id".to_string(), kms.clone());
                }
            }
            // AWS STS temporary credentials (AssumeRole / SSO). AWS-only.
            if let Some(token) = self
                .session_token
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                extra.insert("session_token".to_string(), token.to_string());
            }
            // STS AssumeRole config (issue #301, Fase 2). AWS-only.
            for (key, value) in [
                ("role_arn", self.role_arn.as_ref()),
                ("role_external_id", self.role_external_id.as_ref()),
                ("role_session_name", self.role_session_name.as_ref()),
                ("role_mfa_serial", self.role_mfa_serial.as_ref()),
                ("role_mfa_token_code", self.role_mfa_token_code.as_ref()),
            ] {
                if let Some(v) = value.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    extra.insert(key.to_string(), v.to_string());
                }
            }
            if let Some(d) = self.role_duration_seconds {
                extra.insert("role_duration_seconds".to_string(), d.to_string());
            }
        }

        if provider_type == ProviderType::Backblaze {
            if let Some(ref bucket) = self.bucket {
                extra.insert("bucket".to_string(), bucket.clone());
            } else {
                return Err("Backblaze B2 requires a bucket name".to_string());
            }
        }

        if provider_type == ProviderType::ImageKit {
            if self.username.trim().is_empty() {
                return Err("ImageKit requires the URL endpoint ID".to_string());
            }
            extra.insert("imagekit_id".to_string(), self.username.trim().to_string());
        }

        if provider_type == ProviderType::Uploadcare {
            if self.username.trim().is_empty() {
                return Err("Uploadcare requires the public API key".to_string());
            }
            extra.insert("public_key".to_string(), self.username.trim().to_string());
        }

        if provider_type == ProviderType::Cloudinary {
            // Cloudinary requires cloud_name (account identifier) + api_key
            // (username) + api_secret (password). The cloud_name is shipped
            // either via the dedicated `cloud_name` extra or via the host
            // field as a courtesy fallback.
            if self.username.trim().is_empty() {
                return Err("Cloudinary requires the API key".to_string());
            }
            let cloud_name = self
                .bucket
                .as_ref()
                .map(|b| b.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let h = self.server.trim();
                    if h.is_empty() || h == "api.cloudinary.com" {
                        None
                    } else {
                        Some(h.to_string())
                    }
                })
                .ok_or_else(|| "Cloudinary requires the cloud name".to_string())?;
            extra.insert("cloud_name".to_string(), cloud_name);
        }

        if provider_type == ProviderType::WebDav && self.anonymous.unwrap_or(false) {
            extra.insert("anonymous".to_string(), "true".to_string());
        }

        // AeroShare peer drive: `server` carries the friend's AeroFTP-ID;
        // namespace + ticket + local replica folder arrive in the dedicated
        // params and ride in `extra` (consumed by `PeerProviderConfig` and the
        // PeerRuntime sync task).
        if provider_type == ProviderType::Peer {
            crate::peer::validate_aeroftp_id(self.server.trim())
                .map_err(|e| format!("AeroShare: invalid friend AeroFTP-ID: {e}"))?;
            let namespace = self
                .peer_namespace
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "AeroShare requires the drive namespace".to_string())?;
            let ticket = self
                .peer_ticket
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "AeroShare requires the drive ticket".to_string())?;
            let local_folder = self
                .peer_local_folder
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "AeroShare requires the local replica folder".to_string())?;
            if !std::path::Path::new(local_folder).is_absolute() {
                return Err("AeroShare replica folder must be an absolute path".to_string());
            }
            extra.insert(
                crate::providers::peer::PEER_EXTRA_NAMESPACE.to_string(),
                namespace.to_string(),
            );
            extra.insert(
                crate::providers::peer::PEER_EXTRA_TICKET.to_string(),
                ticket.to_string(),
            );
            extra.insert(
                crate::providers::peer::PEER_EXTRA_LOCAL_FOLDER.to_string(),
                local_folder.to_string(),
            );
            extra.insert(
                crate::providers::peer::PEER_EXTRA_ROLE.to_string(),
                self.peer_role
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("replicator")
                    .to_string(),
            );
        }

        // Add FTP/FTPS-specific options
        if provider_type == ProviderType::Ftp || provider_type == ProviderType::Ftps {
            if let Some(ref tls_mode) = self.tls_mode {
                extra.insert("tls_mode".to_string(), tls_mode.clone());
            }
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        // WebDAV scheme override + self-signed cert opt-out. tls_mode accepts
        // "http", "https", or "auto" (default). Required for local WebDAV
        // bridges such as Filen Desktop (port 1900, HTTP) and any custom-
        // port HTTP server where the auto-detection would otherwise pick
        // HTTPS.
        if provider_type == ProviderType::WebDav {
            if let Some(ref tls_mode) = self.tls_mode {
                if !tls_mode.is_empty() {
                    extra.insert("tls_mode".to_string(), tls_mode.clone());
                }
            }
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        // S3 self-signed cert opt-out for local-loopback bridges (Filen Desktop S3
        // over HTTPS at local.s3.filen.io:1800 uses a self-signed certificate).
        if provider_type == ProviderType::S3 {
            tracing::info!(
                "[S3] provider_commands: self.verify_cert={:?} self.endpoint={:?} self.bucket={:?}",
                self.verify_cert,
                self.endpoint,
                self.bucket
            );
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        // Add MEGA-specific options
        if provider_type == ProviderType::Mega {
            if self.save_session.unwrap_or(true) {
                extra.insert("save_session".to_string(), "true".to_string());
            }
            if let Some(ref mega_mode) = self.mega_mode {
                if !mega_mode.is_empty() {
                    extra.insert("mega_mode".to_string(), mega_mode.clone());
                }
            }
            if let Some(ts) = self.session_expires_at {
                extra.insert("session_expires_at".to_string(), ts.to_string());
            }
            if let Some(logout) = self.logout_on_disconnect {
                extra.insert("logout_on_disconnect".to_string(), logout.to_string());
            }
        }

        // Add Azure-specific options
        if provider_type == ProviderType::Azure {
            if let Some(ref bucket) = self.bucket {
                extra.insert("container".to_string(), bucket.clone());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            // account_name comes from username field
        }

        // 2FA TOTP forwarding for E2E providers + MEGA. The frontend ships
        // the 6-digit code on connectionParams.options.two_factor_code; we
        // only insert it into extra when actually present so that profiles
        // without 2FA enabled don't send empty fields to the API.
        if provider_type == ProviderType::Filen
            || provider_type == ProviderType::Internxt
            || provider_type == ProviderType::Mega
        {
            if let Some(ref code) = self.two_factor_code {
                if !code.is_empty() {
                    extra.insert("two_factor_code".to_string(), code.clone());
                }
            }
            // Persisted base32 TOTP secret: forwarded so the provider derives
            // the 6-digit code itself on every connect and never prompts
            // (issue #128). The frontend persists this on the profile; without
            // this line it never reached ProviderConfig and the field was
            // effectively dead.
            if let Some(ref secret) = self.totp_secret {
                if !secret.trim().is_empty() {
                    extra.insert("totp_secret".to_string(), secret.trim().to_string());
                }
            }
            // Filen CLI API key: authenticates API transport without the
            // /v3/login call, so reconnects skip the 2FA TOTP window.
            if provider_type == ProviderType::Filen {
                if let Some(ref api_key) = self.filen_api_key {
                    if !api_key.trim().is_empty() {
                        extra.insert("filen_api_key".to_string(), api_key.trim().to_string());
                    }
                }
            }
        }

        if provider_type == ProviderType::GitHub || provider_type == ProviderType::GitLab {
            // Branch override: shared by both GitHub and GitLab
            if let Some(ref branch) = self.github_branch {
                if !branch.is_empty() {
                    extra.insert("branch".to_string(), branch.clone());
                }
            }
        }

        // GitLab: accept_invalid_certs for self-hosted instances
        if provider_type == ProviderType::GitLab {
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        if provider_type == ProviderType::GitHub {
            if let Some(ref auth_mode) = self.github_auth_mode {
                if !auth_mode.is_empty() {
                    extra.insert("github_auth_mode".to_string(), auth_mode.clone());
                }
            }
            if let Some(ref app_id) = self.github_app_id {
                if !app_id.is_empty() {
                    extra.insert("github_app_id".to_string(), app_id.clone());
                }
            }
            if let Some(ref installation_id) = self.github_installation_id {
                if !installation_id.is_empty() {
                    extra.insert(
                        "github_installation_id".to_string(),
                        installation_id.clone(),
                    );
                }
            }
            if let Some(ref pem_path) = self.github_pem_path {
                if !pem_path.is_empty() {
                    extra.insert("github_pem_path".to_string(), pem_path.clone());
                }
            }
            if let Some(ref expires_at) = self.github_token_expires_at {
                if !expires_at.is_empty() {
                    extra.insert("github_token_expires_at".to_string(), expires_at.clone());
                }
            }
        }

        // Add pCloud-specific options
        if provider_type == ProviderType::PCloud {
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us".to_string());
            }
        }

        // Add kDrive-specific options
        if provider_type == ProviderType::KDrive {
            if let Some(ref bucket) = self.bucket {
                // Reuse bucket field for drive_id
                extra.insert("drive_id".to_string(), bucket.clone());
            } else {
                return Err("kDrive requires a Drive ID".to_string());
            }
        }

        // Add SFTP-specific options
        if provider_type == ProviderType::Sftp {
            if let Some(ref key_path) = self.private_key_path {
                if !key_path.is_empty() {
                    extra.insert("private_key_path".to_string(), key_path.clone());
                }
            }
            if let Some(ref passphrase) = self.key_passphrase {
                if !passphrase.is_empty() {
                    extra.insert("key_passphrase".to_string(), passphrase.clone());
                }
            }
            if let Some(timeout) = self.timeout {
                extra.insert("timeout".to_string(), timeout.to_string());
            }
        }

        let host = if provider_type == ProviderType::Mega {
            "mega.nz".to_string()
        } else if provider_type == ProviderType::Internxt {
            "gateway.internxt.com".to_string()
        } else if provider_type == ProviderType::KDrive {
            "api.infomaniak.com".to_string()
        } else if provider_type == ProviderType::Jottacloud {
            "jfs.jottacloud.com".to_string()
        } else if provider_type == ProviderType::DrimeCloud {
            "app.drime.cloud".to_string()
        } else if provider_type == ProviderType::FileLu {
            "filelu.com".to_string()
        } else if provider_type == ProviderType::Koofr {
            "app.koofr.net".to_string()
        } else if provider_type == ProviderType::OpenDrive {
            "dev.opendrive.com".to_string()
        } else if provider_type == ProviderType::YandexDisk {
            "cloud-api.yandex.net".to_string()
        } else if provider_type == ProviderType::ImageKit {
            "api.imagekit.io".to_string()
        } else if provider_type == ProviderType::Uploadcare {
            "api.uploadcare.com".to_string()
        } else if provider_type == ProviderType::Cloudinary {
            "api.cloudinary.com".to_string()
        } else if provider_type == ProviderType::Azure {
            // Azure constructs endpoint from account_name if server is empty
            if self.server.is_empty() {
                String::new()
            } else {
                self.server.clone()
            }
        } else {
            self.server.clone()
        };

        // Strip port suffix from host if present (e.g. "127.0.0.1:2121" → "127.0.0.1")
        // Users sometimes enter host:port in the server field, but port is a separate param
        let host = if let Some(colon_idx) = host.rfind(':') {
            let after = &host[colon_idx + 1..];
            if after.parse::<u16>().is_ok() {
                host[..colon_idx].to_string()
            } else {
                host
            }
        } else {
            host
        };

        Ok(ProviderConfig {
            name: format!("{}@{}", self.username, host),
            provider_type,
            host,
            port: self.port,
            username: Some(self.username.clone()),
            password: Some(self.password.clone()),
            initial_path: self.initial_path.clone(),
            extra,
        })
    }
}

#[derive(Serialize)]
pub struct ProviderListResponse {
    pub files: Vec<RemoteEntry>,
    pub current_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDriveTrashActionItem {
    pub item_id: String,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct ProviderConnectionInfo {
    pub connected: bool,
    pub protocol: Option<String>,
    pub display_name: Option<String>,
    pub server_info: Option<String>,
}

// ============ Tauri Commands ============

/// Connect to a storage provider using the specified protocol.
///
/// IPC panic safety net: the real work runs in [`provider_connect_inner`],
/// wrapped by [`crate::panic_safe::catch`] so a panic in the connect path (a
/// misconfigured crypto provider, a provider factory bug, ...) becomes an `Err`
/// the UI can render instead of a promise that hangs forever. See `panic_safe`.
#[tauri::command]
pub async fn provider_connect(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    peer_runtime: State<'_, crate::peer::runtime::PeerRuntime>,
    params: ProviderConnectionParams,
) -> Result<String, String> {
    crate::panic_safe::catch(
        "provider_connect",
        provider_connect_inner(app, state, cancel_registry, peer_runtime, params),
    )
    .await
}

async fn provider_connect_inner(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    peer_runtime: State<'_, crate::peer::runtime::PeerRuntime>,
    params: ProviderConnectionParams,
) -> Result<String, String> {
    info!(
        "Connecting to {} provider: {}",
        params.protocol, params.server
    );

    let mut config = params.to_provider_config()?;

    // AeroShare: a peer drive browses a LOCAL replica that the PeerRuntime
    // keeps converging in the background. Ensure its sync task is up BEFORE
    // the provider connects, so a first connect creates the replica folder
    // and starts pulling (the panel can browse the last-synced state even
    // while the friend is offline).
    if config.provider_type == ProviderType::Peer {
        peer_runtime.ensure_sub_for_config(&app, &config).await?;
    }

    // SEC-GH-001: For GitHub App mode, inject the held installation token
    // so the token never crosses the IPC boundary.
    // Only inject when password is empty/missing (App mode sends empty password).
    // PAT and Device Flow provide their own password: never overwrite.
    // Uses clone() instead of take() so the token survives connection retries.
    if config.provider_type == ProviderType::GitHub {
        let password_empty = config.password.as_ref().is_none_or(|p| p.is_empty());
        if password_empty {
            let held = state.held_github_app_token.lock().await;
            if let Some(ref token) = *held {
                config.password = Some(token.clone());
            }
        }
    }

    // Create provider using factory
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    // A3-05: Zeroize password after it has been consumed by the provider
    config.zeroize_password();

    // W3.1 (#270.5): register a cancellation token under the frontend-supplied
    // connect token so an Esc / "still connecting" Cancel can abort this
    // connect. The guard de-registers it on every exit path.
    let connect_key = params.connect_token.clone();
    let cancel_token = connect_key
        .as_deref()
        .map(|key| cancel_registry.register(key));
    let _cancel_guard = connect_key
        .as_deref()
        .map(|key| ConnectTokenGuard::new(&cancel_registry, key.to_string()));

    // Connect, cancellable via the registered token. Dropping the
    // `provider.connect()` future on cancel tears down the in-flight
    // TCP/TLS/HTTP connect cleanly for async transports (reqwest, russh,
    // suppaftp). An `ssh2` SFTP handshake on `spawn_blocking` is NOT abortable
    // mid-syscall: it runs to completion in the background and is dropped right
    // after, which is acceptable for a first cut (documented in the PR).
    let connect_outcome: Result<(), String> = match cancel_token.as_ref() {
        Some(token) => tokio::select! {
            res = provider.connect() => res.map_err(|e| format!("Connection failed: {}", e)),
            _ = token.cancelled() => Err(CONNECT_CANCELLED.to_string()),
        },
        None => provider
            .connect()
            .await
            .map_err(|e| format!("Connection failed: {}", e)),
    };
    connect_outcome?;

    let display_name = provider.display_name();
    let protocol = format!("{:?}", provider.provider_type());

    // Store provider and config. If a previous provider is still held here
    // (reconnect-without-disconnect, user swapping servers from the UI, etc.),
    // gracefully disconnect it first; synchronously dropping a connected
    // `Box<dyn StorageProvider>` does not run async disconnect, which leaks
    // server-side sessions, socket handles, and OAuth refresh tokens.
    //
    // Issue #233: drain in-flight DAG transfers BEFORE taking the slot, so
    // an active download/upload cannot see the box yanked from under it.
    drain_in_flight_transfers(&state, Duration::from_secs(30)).await;
    {
        let mut prov_lock = state.provider.lock().await;
        if let Some(mut previous) = prov_lock.take() {
            if let Err(err) = previous.disconnect().await {
                warn!(
                    "provider_connect: previous provider disconnect failed: {}",
                    err
                );
            }
        }
        *prov_lock = Some(provider);
        // A new connection occupies the slot: the previous connection's cached
        // overlay keys must never re-arm onto it. Bump generation + zeroize while
        // still holding the provider lock, so a concurrent re-arm is serialized out.
        state.invalidate_overlay_key_cache();
    }
    // A fresh connection carries no crypt overlay: reset both the sticky
    // capability flag and the wrapped flag. The GUI re-applies the overlay via
    // `provider_apply_crypt_overlay` once it has connected and resolved the
    // profile binding (auto-unlock) or the user activates one ad-hoc.
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    {
        let mut config_lock = state.config.lock().await;
        *config_lock = Some(config);
    }

    info!("Connected successfully: {}", display_name);
    Ok(format!("Connected to {} via {}", display_name, protocol))
}

/// Cancel an in-progress connection attempt identified by the frontend-
/// generated `token` (Ehud wishlist W3.1 #270.5). Looks the token up in the
/// shared [`ConnectionCancelRegistry`] and signals its cancellation, which
/// wakes the `tokio::select!` in `provider_connect` / `connect_ftp` and makes
/// the connect return `CONNECT_CANCELLED`. Idempotent: returns `Ok(())` even
/// when the token is already gone (the connect resolved before the cancel
/// landed), so the UI never has to special-case the race.
#[tauri::command]
pub async fn cancel_connection(
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    token: String,
) -> Result<(), String> {
    if cancel_registry.cancel(&token) {
        info!("cancel_connection: signalled cancel for in-progress connect");
    }
    Ok(())
}

/// Disconnect from the current provider
#[tauri::command]
pub async fn provider_disconnect(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    peer_runtime: State<'_, crate::peer::runtime::PeerRuntime>,
) -> Result<(), String> {
    // LT1: this command runs on the async runtime (off the GTK main thread).
    // The teardown below is pure provider/session state + peer emit: it must
    // NOT call tray/window/menu/notification APIs. Any UI update triggered by
    // disconnect (window title, toast) is owned by the frontend and goes
    // through Tauri's main-thread message path. See tray_badge /
    // open_extract_window / rebuild_menu / aeroshare_notify for the
    // run_on_main_thread discipline on the remaining GTK sites.
    //
    // Issue #233: wait for any in-flight DAG transfer to drain before
    // mutating the provider slot. Without this, an active download/upload
    // sees the box yanked and surfaces a spurious `NotConnected` instead
    // of completing or failing on its real I/O error.
    drain_in_flight_transfers(&state, Duration::from_secs(30)).await;

    // AeroShare: closing a peer connection tab stands the received drive down
    // to STANDBY - cancel the replication task (frees CPU/relay and fixes the
    // orphan-task leak) and mark it idle (dark-blue dot), resumable on the next
    // connect. Done while the config is still present so we know the namespace.
    {
        let config_lock = state.config.lock().await;
        if let Some(cfg) = config_lock.as_ref() {
            if cfg.provider_type == ProviderType::Peer {
                if let Some(ns) = cfg.extra.get(crate::providers::peer::PEER_EXTRA_NAMESPACE) {
                    peer_runtime.standby(&app, ns).await;
                }
            }
        }
    }

    let mut provider_lock = state.provider.lock().await;

    if let Some(ref mut provider) = *provider_lock {
        info!("Disconnecting from provider: {}", provider.display_name());
        provider
            .disconnect()
            .await
            .map_err(|e| format!("Disconnect failed: {}", e))?;
    }

    *provider_lock = None;

    let mut config_lock = state.config.lock().await;
    *config_lock = None;

    // The provider (raw or wrapped) is gone: clear both crypt-overlay flags.
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    // Connection torn down: zeroize any cached overlay keys and bump generation.
    state.invalidate_overlay_key_cache();

    Ok(())
}

/// Arm crypt capability before a saved-profile auto-unlock attempt.
///
/// If the unlock succeeds, `provider_apply_crypt_overlay` marks the session
/// wrapped. If it fails, the raw-write guard remains active and refuses direct
/// writes into the still-raw encrypted store until disconnect or retry.
#[tauri::command]
pub fn provider_arm_crypt_capability(state: State<'_, ProviderState>) -> Result<(), String> {
    state.arm_crypt_capability();
    Ok(())
}

/// Parameters for [`provider_apply_crypt_overlay`]: the overlay binding plus the
/// already-resolved unlock secrets. `password`/`salt` come from the per-profile
/// vault (`aerocrypt_overlay_pw_<id>` / `_salt_<id>`) for an auto-unlock, or from
/// the ad-hoc unlock modal.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyCryptOverlayParams {
    /// "rclone-crypt" or "aerocrypt".
    pub kind: String,
    /// Plaintext anchor where the overlay is rooted (`""`/`"/"` = whole remote).
    #[serde(default)]
    pub remote_scope: String,
    /// rclone-crypt filename encryption mode ("standard"/"obfuscate"/"off").
    pub filename_encryption: Option<String>,
    /// rclone-crypt directory-name encryption (default true).
    pub directory_name_encryption: Option<bool>,
    /// Unlock password.
    pub password: String,
    /// rclone-crypt salt (ignored by aerocrypt, which reads its remote config).
    pub salt: Option<String>,
    /// Optional AeroCrypt keyfile path (Tier 1 second factor). Resolved to its
    /// digest here, fail-closed: an unreadable file is an error, never a silent
    /// password-only unlock.
    #[serde(default)]
    pub keyfile_path: Option<String>,
    /// Profile ID, used to load headerless vault config from the keystore.
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub with_header: Option<bool>,
    /// Opt-in default-salt mode for native AeroCrypt (D1). When true, the
    /// create path uses the public constant instead of a random per-vault salt.
    #[serde(default)]
    pub use_default_salt: Option<bool>,
}

/// Apply a crypt overlay (rclone-crypt or AeroCrypt) to the live connection in
/// place (Phase 3 on-demand model). Wraps the raw `ProviderState` provider with
/// the [`crate::crypt_overlay_provider::CryptOverlayProvider`] decorator so the
/// browser, agent `gui_tools`, speed test, and preview all become transparently
/// crypt-aware (plaintext paths in, ciphertext on the wire). FAIL-CLOSED: an
/// unlock failure leaves the raw connection untouched and returns the error.
/// Idempotent: re-applying re-anchors (the prior overlay is reverted first).
/// Returns scope plus optional headed-marker heal info (tracker #421 item #7).
#[tauri::command]
pub async fn provider_apply_crypt_overlay(
    state: State<'_, ProviderState>,
    params: ApplyCryptOverlayParams,
) -> Result<crate::crypt_overlay_provider::ApplyOverlayResult, String> {
    let (local_config_json, local_config_salt) = if let Some(id) = &params.profile_id {
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            let json = crate::user_partitions::resolve_active_credential(
                &store,
                &format!("aerocrypt_overlay_config_{}", id),
            )
            .ok()
            .flatten()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

            let salt = crate::user_partitions::resolve_active_credential(
                &store,
                &format!("aerocrypt_overlay_salt_{}", id),
            )
            .ok()
            .flatten()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

            (json, salt)
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    let with_header = params.with_header.unwrap_or(false);
    let binding = crate::crypt_compare::OverlayUnlockParams {
        kind: params.kind,
        remote_scope: params.remote_scope,
        filename_encryption: params
            .filename_encryption
            .unwrap_or_else(|| "standard".to_string()),
        directory_name_encryption: params.directory_name_encryption.unwrap_or(true),
        off_suffix: None,
        profile_id: params.profile_id,
        local_config_json,
        local_config_salt,
        with_header,
    };
    let salt = params.salt.unwrap_or_default();
    let use_default_salt = params.use_default_salt;
    // Keyfile second factor: resolve the picked path to its digest before
    // touching the connection; a keyfile vault with no keyfile fails closed
    // inside the unlock with a clear "requires a keyfile" error.
    let keyfile_digest = match params.keyfile_path.as_deref().filter(|s| !s.is_empty()) {
        Some(p) => Some(crate::crypt_overlay_provider::keyfile_digest_from_path(p)?),
        None => None,
    };
    let result = {
        let mut guard = state.provider.lock().await;
        match crate::crypt_overlay_provider::apply_overlay_in_place(
            &mut guard,
            &binding,
            &params.password,
            &salt,
            keyfile_digest.as_ref(),
            with_header,
            use_default_salt,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                // The unlock reason is otherwise swallowed by the generic frontend
                // "could not be unlocked" message. Log it (kind only, no path or
                // secrets) so a support session can tell a wrong password from a
                // config gap from a missing profile id.
                warn!("crypt overlay unlock failed (kind={}): {}", binding.kind, e);
                return Err(e);
            }
        }
    };
    // Sticky capability + currently-wrapped: the agent raw-write guard is now
    // satisfied (writes route through the decorator).
    state.active_crypt_overlay.store(true, Ordering::SeqCst);
    state.overlay_wrapped.store(true, Ordering::SeqCst);
    // Cache the just-derived keys for this connection so a later view-only lock
    // can re-arm instantly (no KDF re-run). Clone them off the live decorator;
    // the cache is bound to the current generation and wiped on any connection
    // change or hard lock.
    {
        let mut guard = state.provider.lock().await;
        if let Some(dec) = guard.as_mut().and_then(|p| {
            p.as_any_mut()
                .downcast_mut::<crate::crypt_overlay_provider::CryptOverlayProvider>()
        }) {
            state.store_overlay_key_cache(
                dec.keys().clone(),
                dec.scope().to_string(),
                dec.keys().kind_str().to_string(),
            );
        }
    }
    info!(
        "Crypt overlay applied to live provider (scope: {:?}, marker_restored={})",
        result.scope, result.marker_restored
    );
    Ok(result)
}

/// Build the OPTIONAL recovery kit for a saved headerless AeroCrypt profile, on
/// demand and WITHOUT connecting: reads the public config persisted in the local
/// keystore (`aerocrypt_overlay_config_<id>` + its salt of record) and returns
/// the public-only kit (vault_id, salt, KDF params, never secrets). This backs
/// the "Recovery kit" action in the saved-server context menu, so a user can
/// re-view and re-save the kit any time. Errors clearly when the profile has no
/// headerless vault yet (never created, or it uses an on-remote header).
#[tauri::command]
pub fn aerocrypt_profile_recovery_kit(
    profile_id: String,
) -> Result<crate::aerocrypt::emergency_kit::EmergencyKit, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "The local keystore is unavailable.".to_string())?;
    let config_json = crate::user_partitions::resolve_active_credential(
        &store,
        &format!("aerocrypt_overlay_config_{profile_id}"),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty())
    .ok_or_else(|| {
        "No recovery kit cached for this profile yet. Connect to its vault once so its \
         public configuration is stored locally (this happens automatically on connect \
         for headerless, headed and keyfile vaults), then reopen the recovery kit."
            .to_string()
    })?;
    let salt = crate::user_partitions::resolve_active_credential(
        &store,
        &format!("aerocrypt_overlay_salt_{profile_id}"),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string());
    // Same salt-of-record integrity check the unlock path uses: a mismatch means
    // a partial restore or local tampering, so fail closed rather than hand back
    // a kit that would not actually reconstruct the vault.
    crate::crypt_overlay_provider::validate_headerless_config_salt(
        &profile_id,
        &config_json,
        salt.as_deref(),
    )?;
    crate::aerocrypt::emergency_kit::build_from_config_json(&config_json)
}

/// Re-parse a saved Emergency Kit text, QR payload, or headed marker and confirm
/// its public fields (vault_id, salt, version, KDF) still match the active
/// profile's keystore config. Offline, no password, no network. Surfaces the
/// internal `build_from_config_json` / kit-text parser path so a user can check
/// a printed kit occasionally without a full recovery drill (tracker #421 #6).
#[tauri::command]
pub fn aerocrypt_verify_recovery_kit(
    profile_id: String,
    kit_or_marker_text: String,
) -> Result<crate::aerocrypt::emergency_kit::KitVerifyReport, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "The local keystore is unavailable.".to_string())?;
    let config_json = crate::user_partitions::resolve_active_credential(
        &store,
        &format!("aerocrypt_overlay_config_{profile_id}"),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string())
    .filter(|s| !s.is_empty())
    .ok_or_else(|| {
        "No recovery kit cached for this profile yet. Connect to its vault once so its \
         public configuration is stored locally, then reopen Verify."
            .to_string()
    })?;
    let salt = crate::user_partitions::resolve_active_credential(
        &store,
        &format!("aerocrypt_overlay_salt_{profile_id}"),
    )
    .ok()
    .flatten()
    .map(|s| s.to_string());
    crate::crypt_overlay_provider::validate_headerless_config_salt(
        &profile_id,
        &config_json,
        salt.as_deref(),
    )?;
    crate::aerocrypt::emergency_kit::verify_against_active(&config_json, &kit_or_marker_text)
}

/// Generate a fresh transfer-safe AeroCrypt keyfile (AEROFTP-KEYFILE-V1) at
/// `path`. Refuses to overwrite an existing file (a keyfile is a factor: losing
/// the original makes its vaults unopenable, so clobbering one is never OK).
/// Mode 0600 on unix. Mirrors the CLI `crypt init --keyfile-gen` path.
#[tauri::command]
pub async fn crypt_generate_keyfile(path: String) -> Result<(), String> {
    // Exclusive create with mode 0600 at open time: no world-readable window, no
    // symlink redirect, no exists()+write TOCTOU (see write_keyfile_new).
    let content = crate::aerocrypt::generate_keyfile_v1();
    crate::aerocrypt::write_keyfile_new(std::path::Path::new(&path), content.as_bytes())
}

/// Revert the live connection to its raw provider, removing any crypt overlay
/// (Phase 3 on-demand model). Used when the badge is locked or the user steps
/// outside the encrypted scope, so the browser shows the raw remote (ciphertext
/// names while locked, plaintext names outside the scope) exactly like the
/// retired command layer did. `full = true` also drops the sticky capability
/// flag (a complete overlay removal, not a transient lock / scope-out), which
/// re-opens the agent `gui_tools` raw paths, AND hard-invalidates the cached
/// overlay keys (zeroize): this is the "hard lock" / teardown path. A view-only
/// lock that wants an instant re-arm uses [`provider_lock_crypt_overlay`]
/// instead, which keeps the cache. Idempotent.
#[tauri::command]
pub async fn provider_clear_crypt_overlay(
    state: State<'_, ProviderState>,
    full: Option<bool>,
) -> Result<bool, String> {
    let removed = {
        let mut guard = state.provider.lock().await;
        crate::crypt_overlay_provider::clear_overlay_in_place(&mut guard)
    };
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    if full.unwrap_or(false) {
        state.active_crypt_overlay.store(false, Ordering::SeqCst);
        // Hard lock / teardown: the cached keys must not survive.
        state.invalidate_overlay_key_cache();
    }
    Ok(removed)
}

/// View-only lock: unwrap the live provider to raw (ciphertext names) exactly
/// like a locked overlay, but KEEP the cached overlay keys so a following
/// [`provider_rearm_cached_crypt_overlay`] can re-arm instantly without
/// re-running the KDF. Backs the fast crypt toggle (off then on on the same
/// connection). Mirrors the flag effect of a full clear (capability dropped so
/// the raw view is honest) but never touches the key cache. Idempotent.
#[tauri::command]
pub async fn provider_lock_crypt_overlay(state: State<'_, ProviderState>) -> Result<bool, String> {
    let removed = {
        let mut guard = state.provider.lock().await;
        crate::crypt_overlay_provider::clear_overlay_in_place(&mut guard)
    };
    state.overlay_wrapped.store(false, Ordering::SeqCst);
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    Ok(removed)
}

/// Result of an instant cached re-arm.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RearmCryptOverlayResult {
    /// Normalized plaintext scope the overlay is anchored at.
    pub scope: String,
    /// Wire tag: `"rclone-crypt"` | `"aerocrypt"`.
    pub kind: String,
}

/// Instant re-arm from the connection-scoped key cache: re-wrap the live raw
/// provider with the cached overlay keys, skipping the KDF entirely. Returns the
/// scope + kind on success. Fails (so the caller falls back to a full re-derive)
/// when there is no cached key for the live connection generation, or the slot
/// is empty. The cache is only ever populated by a successful
/// `provider_apply_crypt_overlay` on THIS connection and wiped on any connection
/// change, so a cached key can never re-arm onto a different server.
#[tauri::command]
pub async fn provider_rearm_cached_crypt_overlay(
    state: State<'_, ProviderState>,
) -> Result<RearmCryptOverlayResult, String> {
    // Hold the provider lock across the whole re-arm so a concurrent
    // connect/disconnect (which bumps the generation under the same lock) cannot
    // interleave between the generation check and the re-wrap.
    let mut guard = state.provider.lock().await;
    let (keys, scope, kind) = state
        .cached_overlay_for_rearm()
        .ok_or_else(|| "no cached overlay key for this connection".to_string())?;
    // Revert any existing overlay first so we never stack two decorators, then
    // take the raw inner, wrap it with the cached keys, and put it back.
    crate::crypt_overlay_provider::clear_overlay_in_place(&mut guard);
    let raw = guard
        .take()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let wrapped = crate::crypt_overlay_provider::CryptOverlayProvider::new(raw, keys, &scope);
    *guard = Some(Box::new(wrapped));
    drop(guard);
    state.active_crypt_overlay.store(true, Ordering::SeqCst);
    state.overlay_wrapped.store(true, Ordering::SeqCst);
    Ok(RearmCryptOverlayResult { scope, kind })
}

/// #390 smart re-anchor probe. After arming the overlay, report whether the
/// current on-wire cwd is a valid location inside the encrypted view (so the UI
/// keeps the user in place) or a hidden/outside location (so the UI re-anchors to
/// the scope/root). Returns `true` when nothing is wrapped, so a raw session is
/// never yanked.
#[tauri::command]
pub async fn provider_crypt_cwd_in_view(state: State<'_, ProviderState>) -> Result<bool, String> {
    let mut guard = state.provider.lock().await;
    let Some(provider) = guard.as_mut() else {
        return Ok(true);
    };
    match provider
        .as_any_mut()
        .downcast_mut::<crate::crypt_overlay_provider::CryptOverlayProvider>()
    {
        Some(overlay) => overlay
            .cwd_in_encrypted_view()
            .await
            .map_err(|e| e.to_string()),
        None => Ok(true),
    }
}

/// Check if connected to a provider
#[tauri::command]
pub async fn provider_check_connection(
    state: State<'_, ProviderState>,
) -> Result<ProviderConnectionInfo, String> {
    let provider_lock = state.provider.lock().await;

    match &*provider_lock {
        Some(provider) => Ok(ProviderConnectionInfo {
            connected: provider.is_connected(),
            protocol: Some(format!("{:?}", provider.provider_type())),
            display_name: Some(provider.display_name()),
            server_info: None,
        }),
        None => Ok(ProviderConnectionInfo {
            connected: false,
            protocol: None,
            display_name: None,
            server_info: None,
        }),
    }
}

/// Lightweight liveness probe for the currently connected provider (#128-C).
///
/// Runs a bare `list(".")` on the active session and reports whether it
/// succeeds, WITHOUT the silent-reconnect retry that `provider_list_files`
/// performs. That reconnect would re-run the provider login (for Filen / MEGA /
/// Internxt a fresh TOTP window), which is exactly what the caller wants to
/// avoid: re-entering an already-connected 2FA account through the 🏠 Home
/// button must not force a new 2FA code. `Ok(false)` (no provider, not
/// connected, or the list failed) tells the UI to fall back to the normal
/// disconnect + reconnect flow.
#[tauri::command]
pub async fn provider_probe_alive(
    state: State<'_, ProviderState>,
    protocol: Option<String>,
    username: Option<String>,
) -> Result<bool, String> {
    // The backend keeps only the most-recently-connected provider (single slot).
    // Confirm that slot still holds THIS account before probing, so a probe can
    // never make the UI reuse a different account's live session.
    {
        let config_lock = state.config.lock().await;
        let Some(config) = config_lock.as_ref() else {
            return Ok(false);
        };
        // Normalize to lowercase alphanumerics so `filen`/`Filen`, `s3`/`S3`,
        // `googledrive`/`GoogleDrive` all compare equal across the IPC boundary.
        let norm = |s: &str| -> String {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect()
        };
        if let Some(expected) = protocol.as_deref() {
            let actual = format!("{:?}", config.provider_type);
            if norm(&actual) != norm(expected) {
                return Ok(false);
            }
        }
        if let (Some(expected), Some(actual)) = (username.as_deref(), config.username.as_deref()) {
            if !expected.is_empty() && !expected.eq_ignore_ascii_case(actual) {
                return Ok(false);
            }
        }
    }
    let mut provider_lock = state.provider.lock().await;
    let Some(provider) = provider_lock.as_mut() else {
        return Ok(false);
    };
    if !provider.is_connected() {
        return Ok(false);
    }
    Ok(provider.list(".").await.is_ok())
}

/// List files in the specified path.
///
/// Abortable from the remote panel's Cancel button: the whole body, provider
/// mutex acquisition included, runs under [`run_cancellable_listing`].
#[tauri::command]
pub async fn provider_list_files(
    app: AppHandle,
    state: State<'_, ProviderState>,
    listing_cancel: State<'_, ListingCancelState>,
    path: Option<String>,
) -> Result<ProviderListResponse, String> {
    crate::panic_safe::catch(
        "provider_list_files",
        run_cancellable_listing(
            &listing_cancel,
            provider_list_files_inner(&app, &state, path),
        ),
    )
    .await
}

async fn provider_list_files_inner(
    app: &AppHandle,
    state: &ProviderState,
    path: Option<String>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    let list_path = path.as_deref().unwrap_or(".");

    // Retry once on a transport-level disconnect (server idle reaper,
    // NAT eviction). Pre-existing successful sessions can become dead
    // between user actions; surfacing the raw `session closed` as a
    // hard error forces a manual reconnect, which Tom (issue #161)
    // flagged as the workflow killer.
    let files = match provider.list(list_path).await {
        Ok(files) => files,
        Err(e) if e.is_connection_lost() => {
            emit_session_event(app, SessionEventKind::Lost, e.to_string());
            try_silent_reconnect(app, provider)
                .await
                .map_err(|err| format!("Failed to reconnect: {}", err))?;
            let files = provider
                .list(list_path)
                .await
                .map_err(|err| format!("Failed to list files after reconnect: {}", err))?;
            emit_session_event(app, SessionEventKind::Reconnected, "");
            files
        }
        Err(e) => return Err(format!("Failed to list files: {}", e)),
    };

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Change to the specified directory.
///
/// Abortable from the remote panel's Cancel button, exactly like
/// `provider_list_files`: a drill-in that stalls on a slow server is the other
/// half of the same freeze.
#[tauri::command]
pub async fn provider_change_dir(
    app: AppHandle,
    state: State<'_, ProviderState>,
    listing_cancel: State<'_, ListingCancelState>,
    path: String,
) -> Result<ProviderListResponse, String> {
    run_cancellable_listing(
        &listing_cancel,
        provider_change_dir_inner(&app, &state, path),
    )
    .await
}

async fn provider_change_dir_inner(
    app: &AppHandle,
    state: &ProviderState,
    path: String,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    // Run the cd op, and on a transport-level disconnect retry it
    // exactly once after a silent reconnect. `try_silent_reconnect`
    // restores the pre-disconnect cwd, so a `cd("..")` request is
    // resolved relative to where the user actually was (not the
    // post-reconnect home dir).
    let nav_result = if path == ".." {
        provider.cd_up().await
    } else {
        provider.cd(&path).await
    };

    if let Err(e) = nav_result {
        if !e.is_connection_lost() {
            return Err(if path == ".." {
                format!("Failed to go up: {}", e)
            } else {
                format!("Failed to change directory: {}", e)
            });
        }
        emit_session_event(app, SessionEventKind::Lost, e.to_string());
        try_silent_reconnect(app, provider)
            .await
            .map_err(|err| format!("Failed to reconnect: {}", err))?;
        let retry = if path == ".." {
            provider.cd_up().await
        } else {
            provider.cd(&path).await
        };
        retry.map_err(|err| {
            if path == ".." {
                format!("Failed to go up after reconnect: {}", err)
            } else {
                format!("Failed to change directory after reconnect: {}", err)
            }
        })?;
        emit_session_event(app, SessionEventKind::Reconnected, "");
    }

    let files = provider
        .list(".")
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = provider.pwd().await.unwrap_or_else(|_| path.clone());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Navigate to parent directory
#[tauri::command]
pub async fn provider_go_up(
    app: AppHandle,
    state: State<'_, ProviderState>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    if let Err(e) = provider.cd_up().await {
        if !e.is_connection_lost() {
            return Err(format!("Failed to go up: {}", e));
        }
        emit_session_event(&app, SessionEventKind::Lost, e.to_string());
        try_silent_reconnect(&app, provider)
            .await
            .map_err(|err| format!("Failed to reconnect: {}", err))?;
        provider
            .cd_up()
            .await
            .map_err(|err| format!("Failed to go up after reconnect: {}", err))?;
        emit_session_event(&app, SessionEventKind::Reconnected, "");
    }

    let files = provider
        .list(".")
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Get current working directory
#[tauri::command]
pub async fn provider_pwd(state: State<'_, ProviderState>) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .pwd()
        .await
        .map_err(|e| format!("Failed to get working directory: {}", e))
}

/// Run a plain single-file download through the graph engine and emit the
/// GUI completion / error events.
///
/// Reached for the plain classic leaf (no delta consumed the transfer, no
/// segmented engine ran, no resume offset). The `"start"` and `"progress"`
/// events were already emitted by the shared pre-DAG code and the per-byte
/// callback; this function owns only the terminal `"complete"` (via
/// [`GuiDagObserver`]) and `"error"` (emitted here, where the typed error
/// is in hand) events. The emitted events are byte-identical with the
/// pre-convergence single-file path.
#[allow(clippy::too_many_arguments)]
async fn run_dag_download_leaf(
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    // Holds the in-flight counter for the lifetime of this leaf so
    // `provider_disconnect` cannot drain past us. Held by name (not `_`)
    // to make the lifetime explicit at the call site.
    _op_guard: TransferOperationGuard,
    transfer_id: String,
    filename: String,
    remote_path: String,
    local_path: String,
    modified: Option<String>,
    progress_cb: Option<Box<dyn Fn(u64, u64) + Send>>,
    file_size: u64,
    delta_fallback_reason: Option<String>,
    // FINDING-4 Part B: the live session cancel token so an in-flight Stop
    // aborts the current file (see `execute_single_file_dag`).
    cancel_token: Option<CancellationToken>,
) -> Result<String, String> {
    // Capability snapshot drives the shaped-graph shape (single transfer
    // core vs multipart fan-out). For downloads the shape collapses to one
    // transfer node regardless, but we still resolve caps here so the
    // builder picks up `rate_limited_api` / `resume_download` correctly.
    let (caps, route_hint) = {
        let guard = provider.lock().await;
        let caps = guard
            .as_ref()
            .map(|p| p.transfer_capabilities())
            .unwrap_or_default();
        let hint = guard
            .as_ref()
            .map(|p| p.router_hint())
            .unwrap_or(crate::transfer_router::ProviderHint::OAuthCloud);
        (caps, hint)
    };

    // Phase B routing decision: only env override is honoured in GUI
    // commands (no flag surface). Default keeps the shaped DAG path
    // exactly as it was before the router was wired in.
    let route_ctx = crate::transfer_router::RouteContext::new(
        route_hint,
        crate::transfer_router::Operation::Download,
        file_size,
    )
    .with_override(crate::transfer_router::Override::from_env());
    let decision = crate::transfer_router::Router::new().pick(route_ctx);

    if decision.engine == crate::transfer_router::Engine::Legacy {
        info!(
            "Download routed to Legacy engine: {} ({})",
            filename, decision.reason
        );
        let result = {
            let mut guard = provider.lock().await;
            match guard.as_mut() {
                Some(p) => {
                    let dl = async { p.download(&remote_path, &local_path, progress_cb).await };
                    match &cancel_token {
                        Some(tok) => tokio::select! {
                            biased;
                            _ = tok.cancelled() => Err(ProviderError::TransferFailed(
                                "Transfer cancelled by user".to_string(),
                            )),
                            r = dl => r,
                        },
                        None => dl.await,
                    }
                }
                None => Err(ProviderError::NotConnected),
            }
        };
        return match result {
            Ok(()) => {
                // Parity with the DAG `PreserveMetadata` node: restore the
                // remote mtime and emit the GUI completion event. Without this
                // the Legacy download route silently dropped the remote mtime
                // (breaking overwrite-if-newer sync) and never finished the
                // progress bar (audit W1.2, the prerequisite for any future
                // download->Legacy routing carve-out).
                crate::preserve_remote_mtime(&local_path, modified.as_deref());
                let actual_size = tokio::fs::metadata(&local_path)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(file_size);
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "complete".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "download".to_string(),
                        message: Some(format!(
                            "({} in 0s)",
                            if actual_size > 1_048_576 {
                                format!("{:.1} MB", actual_size as f64 / 1_048_576.0)
                            } else {
                                format!("{:.1} KB", actual_size as f64 / 1024.0)
                            }
                        )),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: delta_fallback_reason,
                    },
                );
                info!("Download completed: {}", filename);
                Ok(format!("Downloaded: {}", filename))
            }
            Err(e) => {
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "error".to_string(),
                        transfer_id,
                        filename: filename.clone(),
                        direction: "download".to_string(),
                        message: Some(format!("Download failed: {}", e)),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Err(format!("Download failed: {}", e))
            }
        };
    }

    let built = TransferDagBuilder::shaped_file(
        crate::transfer_dag::TransferDirection::Download,
        &caps,
        file_size,
    );
    // Seed with the discovered remote size; the transfer node overwrites it
    // with the real on-disk size once the download succeeds.
    let report_size = Arc::new(AtomicU64::new(file_size));
    let sink: Arc<dyn TransferEventSink> = Arc::new(AppHandleSink::new(app.clone()));
    let observer: Arc<dyn DagObserver> = Arc::new(GuiDagObserver::new(
        sink,
        transfer_id.clone(),
        filename.clone(),
        "download".to_string(),
        built.emit_progress,
        Arc::clone(&report_size),
        delta_fallback_reason,
    ));

    match crate::transfer_dag_single_file::execute_single_file_dag(
        &built,
        provider,
        remote_path,
        local_path,
        modified,
        progress_cb,
        observer,
        report_size,
        file_size,
        cancel_token,
    )
    .await
    {
        Ok(()) => {
            info!("Download completed: {}", filename);
            Ok(format!("Downloaded: {}", filename))
        }
        Err(e) => {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Download failed: {}", e)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(format!("Download failed: {}", e))
        }
    }
}

/// DAG-ENGINE phase 1: run a plain single-file upload through the graph
/// engine and emit the GUI completion / error events. The upload mirror of
/// [`run_dag_download_leaf`]; GitHub keeps its dedicated commit-based path.
#[allow(clippy::too_many_arguments)]
async fn run_dag_upload_leaf(
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    // Same role as the download leaf: pin the in-flight counter against
    // `provider_disconnect` (issue #233).
    _op_guard: TransferOperationGuard,
    transfer_id: String,
    filename: String,
    local_path: String,
    remote_path: String,
    progress_cb: Option<Box<dyn Fn(u64, u64) + Send>>,
    file_size: u64,
    delta_fallback_reason: Option<String>,
    // FINDING-4 Part B: the live session cancel token so an in-flight Stop
    // aborts the current file (see `execute_single_file_dag`).
    cancel_token: Option<CancellationToken>,
) -> Result<String, String> {
    // Capability snapshot drives the shaped-graph shape. On an upload, this
    // is the gate between the legacy single-`UploadFile` core and a native
    // multipart fan-out (`UploadPart` x N) when the provider advertises
    // `multipart_upload`.
    let (caps, route_hint) = {
        let guard = provider.lock().await;
        let caps = guard
            .as_ref()
            .map(|p| p.transfer_capabilities())
            .unwrap_or_default();
        let hint = guard
            .as_ref()
            .map(|p| p.router_hint())
            .unwrap_or(crate::transfer_router::ProviderHint::OAuthCloud);
        (caps, hint)
    };

    // Phase B routing decision: same pattern as the download leaf, env
    // override only. Default path stays on the shaped DAG.
    let route_ctx = crate::transfer_router::RouteContext::new(
        route_hint,
        crate::transfer_router::Operation::Upload,
        file_size,
    )
    .with_override(crate::transfer_router::Override::from_env());
    let decision = crate::transfer_router::Router::new().pick(route_ctx);

    if decision.engine == crate::transfer_router::Engine::Legacy {
        info!(
            "Upload routed to Legacy engine: {} ({})",
            filename, decision.reason
        );
        let mut guard = provider.lock().await;
        let result = match guard.as_mut() {
            Some(p) => {
                let ul = async { p.upload(&local_path, &remote_path, progress_cb).await };
                match &cancel_token {
                    Some(tok) => tokio::select! {
                        biased;
                        _ = tok.cancelled() => Err(ProviderError::TransferFailed(
                            "Transfer cancelled by user".to_string(),
                        )),
                        r = ul => r,
                    },
                    None => ul.await,
                }
            }
            None => Err(ProviderError::NotConnected),
        };
        return match result {
            Ok(()) => Ok(format!("Uploaded: {}", filename)),
            Err(e) => {
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "error".to_string(),
                        transfer_id,
                        filename: filename.clone(),
                        direction: "upload".to_string(),
                        message: Some(format!("Upload failed: {}", e)),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Err(format!("Upload failed: {}", e))
            }
        };
    }

    let built = TransferDagBuilder::shaped_file(
        crate::transfer_dag::TransferDirection::Upload,
        &caps,
        file_size,
    );
    // The upload node does not touch the report size: the local file size is
    // the value the legacy completion event reports.
    let report_size = Arc::new(AtomicU64::new(file_size));
    let sink: Arc<dyn TransferEventSink> = Arc::new(AppHandleSink::new(app.clone()));
    let observer: Arc<dyn DagObserver> = Arc::new(GuiDagObserver::new(
        sink,
        transfer_id.clone(),
        filename.clone(),
        "upload".to_string(),
        built.emit_progress,
        Arc::clone(&report_size),
        delta_fallback_reason,
    ));

    match crate::transfer_dag_single_file::execute_single_file_dag(
        &built,
        provider,
        remote_path,
        local_path,
        None,
        progress_cb,
        observer,
        report_size,
        file_size,
        cancel_token,
    )
    .await
    {
        Ok(()) => {
            info!("Upload completed: {}", filename);
            Ok(format!("Uploaded: {}", filename))
        }
        Err(e) => {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Upload failed: {}", e)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(format!("Upload failed: {}", e))
        }
    }
}

/// Result of a remote AeroVault-family header sniff: `container` is `"vault"`
/// (encrypted AES-256) / `"zip"` (plaintext .aerozip lane) / `None`, and
/// `version` is the AeroVault generation `"v2"`/`"v3"`/`"v4"`/`None`. Mirrors what
/// the local `detect_aero_container` + `detect_aero_vault_version` return so the
/// frontend maps it exactly like a local file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroRemoteMeta {
    pub container: Option<String>,
    pub version: Option<String>,
}

/// Header window fetched to sniff our own container formats on a remote. Their
/// magic/version live in the first bytes, so this stays tiny relative to the file;
/// it is generous enough to also cover a typical AeroVault v3 header plus its
/// embedded Error-Correction extension directory so the `v4` chip still resolves.
/// If the EC directory sits beyond this window the version simply degrades to
/// `v3` on remote (the encryption badge only needs the first 12 bytes).
const AERO_REMOTE_HEADER_BYTES: u64 = 256 * 1024;

/// Remote counterpart of `detect_aero_container` + `detect_aero_vault_version`.
///
/// Fetches only the file header via the provider's ranged read (`read_range`,
/// offset 0), writes it to a temp copy, and reuses the existing, tested path-based
/// detectors, so an `.aerovault` / `.aerozip` / renamed AeroVault on a
/// range-capable remote (SFTP, FTP/FTPS, S3, WebDAV, Backblaze B2, Koofr) shows the
/// same padlock + generation chip as a local file, with no whole-file download.
///
/// Providers that cannot range-read (E2EE like MEGA/Filen/Internxt, the crypt
/// overlay, or a not-yet-wired HTTP backend) return `NotSupported` from
/// `read_range`; this surfaces as an `Err` that the caller catches and degrades to
/// "no badge". Never downloads the whole file.
#[tauri::command]
pub async fn provider_detect_aero_remote(
    state: State<'_, ProviderState>,
    remote_path: String,
) -> Result<AeroRemoteMeta, String> {
    let bytes = {
        let mut provider_lock = state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;
        // Cap the header read at the real file size so we never over-ask on a tiny
        // container; size(0) (unknown) falls back to the full header window.
        let size = provider.size(&remote_path).await.unwrap_or(0);
        let want = if size > 0 {
            size.min(AERO_REMOTE_HEADER_BYTES)
        } else {
            AERO_REMOTE_HEADER_BYTES
        };
        provider
            .read_range(&remote_path, 0, want)
            .await
            .map_err(|e| format!("remote header read: {e}"))?
    };

    // Reuse the path-based detectors verbatim on a temp copy of the header.
    let tmp = std::env::temp_dir().join(format!(
        "aeroftp-hdr-{}-{}.bin",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| format!("temp header write: {e}"))?;
    let tmp_str = tmp.to_string_lossy().to_string();

    let container = crate::aerovault_v3::detect_aero_container(tmp_str.clone())
        .await
        .unwrap_or(None);
    let version = if container.as_deref() == Some("vault") {
        crate::aerovault_v3::detect_aero_vault_version(tmp_str.clone())
            .await
            .unwrap_or(None)
    } else {
        None
    };
    let _ = tokio::fs::remove_file(&tmp).await;

    Ok(AeroRemoteMeta { container, version })
}

/// Widest tail window a ZIP End-Of-Central-Directory record can sit in: the 22-byte
/// EOCD plus the maximum 64 KiB archive comment. Reading this tail is enough to
/// locate the EOCD (and, for a normal archive, the ZIP64 EOCD record + locator that
/// precede it) without a whole-file download.
const ZIP_EOCD_WINDOW: u64 = 65_557;

/// Hard cap on how much central directory / 7z next-header we will range-read. The
/// central directory is metadata (tiny next to the payload) but a hostile or
/// corrupt archive could claim a huge size; refuse rather than stream it.
const REMOTE_ARCHIVE_INDEX_CAP: u64 = 64 * 1024 * 1024;

/// Remote counterpart of `detect_archive_meta` for third-party **ZIP** and **7z**.
///
/// A ZIP's central directory lives at the file TAIL and a 7z's next-header at an
/// offset near the tail, so a head-only read (unlike our own container formats)
/// cannot classify them. This fetches only the byte ranges the format's index
/// occupies via the provider's ranged read (`read_range`) and feeds them to the
/// same byte-slice parsers the local detector uses (`zip_find_eocd`,
/// `parse_zip_central_dir`, and the 7z AES-coder scan), so a password-protected
/// `.zip` / `.7z` on a range-capable remote (SFTP, FTP/FTPS, S3, WebDAV, Backblaze
/// B2, Koofr) shows the same padlock + cipher badge as a local file, with no
/// whole-file download.
///
/// **Encryption only.** The compression method needs the whole archive on remote
/// (the ZIP per-entry method is in the central directory we do read, but 7z / RAR
/// method resolution wants the full container), so `compression` stays `None` here
/// and the remote Compression column stays blank by design. RAR is `unrar`
/// path-only and is not handled; it degrades to no badge on remote.
///
/// Providers that cannot range-read (E2EE like MEGA/Filen/Internxt, the crypt
/// overlay, a not-yet-wired HTTP backend) return `NotSupported` from `read_range`,
/// which surfaces as an `Err` the caller catches and degrades to "no badge".
#[tauri::command]
pub async fn provider_detect_archive_meta_remote(
    state: State<'_, ProviderState>,
    remote_path: String,
    kind: String,
) -> Result<crate::ArchiveMeta, String> {
    let mut provider_lock = state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;
    let size = provider.size(&remote_path).await.unwrap_or(0);
    match kind.as_str() {
        "zip" => detect_zip_meta_remote(provider.as_mut(), &remote_path, size).await,
        "sevenz" => detect_7z_meta_remote(provider.as_mut(), &remote_path, size).await,
        other => Err(format!("unsupported remote archive kind: {other}")),
    }
}

/// Fetch a ZIP's tail (EOCD window) and central directory via ranged reads and
/// reuse the local byte-slice parser. Handles the ZIP64 `0xFFFF_FFFF` sentinel by
/// resolving the real 64-bit central-directory offset from the ZIP64 EOCD record
/// that sits in the same tail window; if that record precedes the window (a
/// pathologically large comment) we degrade to no badge rather than widen into a
/// whole-file read.
async fn detect_zip_meta_remote(
    provider: &mut dyn StorageProvider,
    path: &str,
    size: u64,
) -> Result<crate::ArchiveMeta, String> {
    if size < 22 {
        return Err("remote size too small / unknown for ZIP".to_string());
    }
    let want = size.min(ZIP_EOCD_WINDOW);
    let tail_start = size - want;
    let tail = provider
        .read_range(path, tail_start, want)
        .await
        .map_err(|e| format!("remote zip tail read: {e}"))?;
    let eocd = crate::zip_find_eocd(&tail).ok_or("no end-of-central-directory record")?;
    if eocd + 20 > tail.len() {
        return Err("truncated EOCD".to_string());
    }
    let entries16 = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]);
    let cd_size32 = u32::from_le_bytes([
        tail[eocd + 12],
        tail[eocd + 13],
        tail[eocd + 14],
        tail[eocd + 15],
    ]);
    let cd_off32 = u32::from_le_bytes([
        tail[eocd + 16],
        tail[eocd + 17],
        tail[eocd + 18],
        tail[eocd + 19],
    ]);

    // Resolve the true (entries, cd offset, cd size), widening to 64-bit for ZIP64.
    let (entries, cd_off, cd_size) =
        if cd_off32 == 0xFFFF_FFFF || cd_size32 == 0xFFFF_FFFF || entries16 == 0xFFFF {
            // The ZIP64 EOCD locator (PK\x06\x07, 20 bytes) precedes the EOCD; its
            // bytes 8..16 hold the absolute offset of the ZIP64 EOCD record
            // (PK\x06\x06), which carries the real 64-bit entry count, cd size and
            // cd offset. Both records normally sit within a few dozen bytes of the
            // EOCD, i.e. inside our tail window.
            let loc = tail[..eocd]
                .windows(4)
                .rposition(|w| w == [0x50, 0x4B, 0x06, 0x07])
                .ok_or("no zip64 eocd locator")?;
            if loc + 16 > tail.len() {
                return Err("truncated zip64 locator".to_string());
            }
            let z64_off = u64::from_le_bytes(tail[loc + 8..loc + 16].try_into().unwrap());
            if z64_off < tail_start {
                // Record precedes our tail window; resolving it would need another
                // ranged read of an unknown span. Degrade to no badge instead.
                return Err("zip64 eocd record outside tail window".to_string());
            }
            let rec = (z64_off - tail_start) as usize;
            // `rec` derives from an attacker-controlled 64-bit offset; guard the 56-byte
            // window with checked_add so it cannot wrap (overflow-checks are off in
            // release) and slip a huge index past the bound into an OOB slice panic.
            if rec.checked_add(56).is_none_or(|e| e > tail.len())
                || tail[rec..rec + 4] != [0x50, 0x4B, 0x06, 0x06]
            {
                return Err("bad zip64 eocd record".to_string());
            }
            let entries = u64::from_le_bytes(tail[rec + 32..rec + 40].try_into().unwrap()) as usize;
            let cd_size = u64::from_le_bytes(tail[rec + 40..rec + 48].try_into().unwrap());
            let cd_off = u64::from_le_bytes(tail[rec + 48..rec + 56].try_into().unwrap());
            (entries, cd_off, cd_size)
        } else {
            (entries16 as usize, cd_off32 as u64, cd_size32 as u64)
        };

    if cd_size == 0 {
        return Err("empty central directory".to_string());
    }
    if cd_size > REMOTE_ARCHIVE_INDEX_CAP {
        return Err("central directory too large to range-read".to_string());
    }
    // The central directory must land inside the file. `cd_off`/`cd_size` are
    // attacker-controlled; if `cd_off + cd_size` overflows or runs past `size`
    // we degrade (an Err here yields no badge upstream, like the other Errs)
    // rather than issue a ranged read of a bogus window. Covers both the in-tail
    // and the ranged branch below (mirrors the 7z path and local detect twins).
    if cd_off.checked_add(cd_size).is_none_or(|end| end > size) {
        return Err("central directory outside the file".to_string());
    }

    // The central directory may already be inside the tail we fetched (small
    // archives, or one whose whole tail we read): parse in place. Otherwise a
    // single extra ranged read of exactly the central directory.
    let mut meta = if cd_off >= tail_start {
        let start = (cd_off - tail_start) as usize;
        let end = (start + cd_size as usize).min(tail.len());
        let cd = tail.get(start..end).ok_or("cd offset outside tail")?;
        crate::parse_zip_central_dir(cd, entries)
    } else {
        let cd = provider
            .read_range(path, cd_off, cd_size)
            .await
            .map_err(|e| format!("remote zip central-dir read: {e}"))?;
        crate::parse_zip_central_dir(&cd, entries)
    };
    // Remote surfacing is encryption-only, matching the 7z remote path and the
    // frontend contract: the per-entry compression method, though present in the
    // ZIP central directory, is not surfaced on remote (the Compression column
    // stays blank there; it is a local-only column).
    meta.compression = None;
    Ok(meta)
}

/// Fetch a 7z's 32-byte start header and its next-header region via ranged reads
/// and scan for the AES coder id, mirroring the local `detect_7z_meta`. Encryption
/// only: the compression method needs the whole archive, so `compression` is
/// `None`.
async fn detect_7z_meta_remote(
    provider: &mut dyn StorageProvider,
    path: &str,
    size: u64,
) -> Result<crate::ArchiveMeta, String> {
    if size < 32 {
        return Err("remote size too small / unknown for 7z".to_string());
    }
    let start = provider
        .read_range(path, 0, 32)
        .await
        .map_err(|e| format!("remote 7z start-header read: {e}"))?;
    if start.len() < 32 {
        return Err("short 7z start header".to_string());
    }
    const SIG: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
    if start[..6] != SIG {
        return Err("not a 7z archive".to_string());
    }
    let nh_off = u64::from_le_bytes(start[12..20].try_into().unwrap());
    let nh_size = u64::from_le_bytes(start[20..28].try_into().unwrap());
    // `nh_off` is attacker-controlled; if the header start (32 + nh_off) would wrap
    // or the size is out of range, degrade to no badge rather than range-read a
    // bogus window (never a wrong badge). The window must also land inside the
    // file: `nh_start > size`, or `nh_start + nh_size` overflowing or exceeding
    // `size`, degrades the same way (mirrors the local `detect_7z_meta`,
    // CLAUDE-AV-B1-02).
    let nh_start = 32u64.checked_add(nh_off);
    let within_file =
        nh_start.is_some_and(|s| s <= size && s.checked_add(nh_size).is_some_and(|e| e <= size));
    if nh_size == 0 || nh_size > REMOTE_ARCHIVE_INDEX_CAP || !within_file {
        return Ok(crate::ArchiveMeta {
            encrypted: false,
            cipher: None,
            compression: None,
        });
    }
    let hdr = provider
        .read_range(path, nh_start.unwrap(), nh_size)
        .await
        .map_err(|e| format!("remote 7z next-header read: {e}"))?;
    const AES_CODER_ID: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];
    let encrypted = hdr.windows(4).any(|w| w == AES_CODER_ID);
    Ok(crate::ArchiveMeta {
        encrypted,
        cipher: encrypted.then(|| "AES-256".to_string()),
        compression: None,
    })
}

/// Download a file from the remote server
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_download_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    modified: Option<String>,
    use_delta: Option<bool>,
    download_segments: Option<u32>,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    let filename = std::path::Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("pdl-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Downloading via provider: {} -> {}",
        remote_path, local_path
    );

    // Emit start event
    crate::transfer_event_sink::emit_gui_transfer_event(
        &app,
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting download: {}", filename)),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    // Create parent directory if needed
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let file_size = provider.size(&remote_path).await.unwrap_or(0);
    let app_progress = app.clone();
    let tid_progress = transfer_id.clone();
    let fname_progress = filename.clone();

    let dl_start_time = std::time::Instant::now();
    let mut progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if file_size > 0 {
        Some(Box::new(move |transferred: u64, total: u64| {
            let pct = if total > 0 {
                ((transferred as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };
            let elapsed = dl_start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.1 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let eta = if speed > 0 && transferred < total {
                ((total - transferred) as f64 / speed as f64) as u64
            } else {
                0
            };
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app_progress,
                crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: tid_progress.clone(),
                    filename: fname_progress.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: tid_progress.clone(),
                        filename: fname_progress.clone(),
                        direction: "download".to_string(),
                        percentage: pct,
                        transferred,
                        total,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }))
    } else {
        None
    };

    // Delta path (SFTP + key-auth + rsync on remote): tried before the
    // classic download. Self-gated inside `try_delta_transfer`: returns
    // `None` for non-SFTP providers, password-only auth, or when the SSH
    // handle is not available. A `hard_error` (host-key mismatch, permission
    // denied) surfaces as a transfer error without the silent classic
    // fallback: security failures must not be masked. Same contract as
    // `sync::perform_download` in the sync_tree path.
    let mut delta_fallback_reason: Option<String> = None;
    {
        if crate::delta_sync_rsync::gui_delta_enabled() && use_delta.unwrap_or(true) {
            let local_path_buf = std::path::PathBuf::from(&local_path);
            // FINDING-4 Part B: the delta (native rsync) transport runs on its
            // OWN separate SSH connection and never reached the classic cancel
            // path, so a Stop during a delta transfer ran to completion. Race it
            // against the live session cancel token; on cancel we drop the delta
            // future (tearing down its SSH connection, leaving the main session
            // intact) and return a cancellation error WITHOUT falling through to
            // the classic path (which would restart the transfer).
            let cancel_token = state.current_cancel_token().await;
            let delta_fut = crate::delta_sync_rsync::try_delta_transfer_with_progress(
                provider.as_mut(),
                crate::delta_sync_rsync::SyncDirection::Download,
                &local_path_buf,
                &remote_path,
                Some(crate::make_delta_progress_sink(
                    app.clone(),
                    transfer_id.clone(),
                    filename.clone(),
                    "download",
                )),
            );
            let delta_cancelled;
            let delta_outcome = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    delta_cancelled = true;
                    None
                }
                r = delta_fut => {
                    delta_cancelled = false;
                    r
                }
            };
            if delta_cancelled {
                let err_msg = format!("Download cancelled by user: {}", filename);
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "error".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "download".to_string(),
                        message: Some("Download cancelled by user".to_string()),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                return Err(err_msg);
            }
            if let Some(result) = delta_outcome {
                if result.used_delta {
                    let delta_stats = result
                        .stats
                        .as_ref()
                        .map(crate::sync::DeltaTransferStats::from_rsync);
                    crate::preserve_remote_mtime(&local_path, modified.as_deref());
                    let actual_size = tokio::fs::metadata(&local_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(file_size);
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "complete".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "download".to_string(),
                            message: Some(format!(
                                "({} via delta)",
                                if actual_size > 1_048_576 {
                                    format!("{:.1} MB", actual_size as f64 / 1_048_576.0)
                                } else {
                                    format!("{:.1} KB", actual_size as f64 / 1024.0)
                                }
                            )),
                            progress: None,
                            path: None,
                            delta_stats,
                            fallback_reason: None,
                        },
                    );
                    info!("Download completed via delta path: {}", filename);
                    return Ok(format!("Downloaded: {}", filename));
                }
                if let Some(hard_err) = result.hard_error {
                    let err_msg = format!("delta hard rejection: {}", hard_err);
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "error".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "download".to_string(),
                            message: Some(err_msg.clone()),
                            progress: None,
                            path: None,
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                    return Err(err_msg);
                }
                // Silent fallback: result.fallback_reason populated but we continue
                // with the classic provider path below.
                delta_fallback_reason = result.fallback_reason;
            }
        }
    }

    // Resume-aware download: if provider supports resume and a partial .aerotmp exists,
    // use resume_download to continue from where we left off. This avoids re-downloading
    // data on S3/Azure (pay-per-GB) and all other HTTP-based providers.
    let tmp_path = format!("{}.aerotmp", local_path);
    let partial_offset = if provider.supports_resume() {
        tokio::fs::metadata(&tmp_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    // GTC-2: opportunistic intra-file range parallelism on the single-file
    // path. Gated on no-resume (`partial_offset == 0`) because the segmented
    // engine pre-allocates and overwrites its own `.aerotmp`; a partial
    // legacy resume must not be silently dropped. On hard failure we fall
    // through to the legacy single-stream branch below.
    let mut segmented_result: Option<Result<(), String>> = None;
    if partial_offset == 0 {
        if let Some(requested) = download_segments {
            if let Some(segments) =
                crate::provider_transfer_executor::provider_segmented_download_eligible(
                    provider.as_ref(),
                    file_size,
                    requested,
                    requested as usize,
                )
            {
                info!(
                    "Segmented download: {} segments on {} ({} bytes)",
                    segments, filename, file_size
                );
                let cancel = tokio_util::sync::CancellationToken::new();
                let outcome = crate::provider_transfer_executor::run_provider_segmented_download(
                    provider.as_ref(),
                    &remote_path,
                    &local_path,
                    file_size,
                    segments,
                    progress_cb.take(),
                    cancel,
                )
                .await;
                if let Err(ref e) = outcome {
                    warn!(
                        "Segmented download failed, falling back to single-stream: {}",
                        e
                    );
                }
                segmented_result = Some(outcome);
            }
        }
    }

    // If segmented ran (success or hard error) the original progress_cb has
    // been moved into it. Build a fresh callback for the legacy fallback so
    // the user still sees per-byte progress.
    if segmented_result.is_some() && progress_cb.is_none() && file_size > 0 {
        let app_progress_fb = app.clone();
        let tid_fb = transfer_id.clone();
        let fname_fb = filename.clone();
        let start_fb = std::time::Instant::now();
        progress_cb = Some(Box::new(move |transferred: u64, total: u64| {
            let pct = if total > 0 {
                ((transferred as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };
            let elapsed = start_fb.elapsed().as_secs_f64();
            let speed = if elapsed > 0.1 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let eta = if speed > 0 && transferred < total {
                ((total - transferred) as f64 / speed as f64) as u64
            } else {
                0
            };
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app_progress_fb,
                crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: tid_fb.clone(),
                    filename: fname_fb.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: tid_fb.clone(),
                        filename: fname_fb.clone(),
                        direction: "download".to_string(),
                        percentage: pct,
                        transferred,
                        total,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }));
    }

    // DAG-ENGINE: route the plain classic single-file leaf through the
    // graph engine. The segmented engine and a resume offset keep their
    // legacy code: they are not the plain leaf. The shaped runner handles
    // both the single-`DownloadFile` core and the multipart fan-out shape.
    if segmented_result.is_none() && partial_offset == 0 {
        let provider_arc = Arc::clone(&state.provider);
        // Acquire the operation guard BEFORE releasing the mutex: this
        // closes the window where `provider_disconnect` could `.take()`
        // the provider box between our drop and the DAG node's re-lock
        // (issue #233). The guard lives for the entire DAG leaf call.
        let op_guard = TransferOperationGuard::acquire(&state);
        // FINDING-4 Part B: grab the LIVE session cancel token (never reset here,
        // so a queue-wide cancel that fired mid-batch still targets this leaf)
        // so an in-flight Stop drops the current transfer.
        let cancel_token = state.current_cancel_token().await;
        // Release the command-level guard so the DAG transfer node can lock
        // the same provider mutex from its spawned task.
        drop(provider_lock);
        return run_dag_download_leaf(
            app,
            provider_arc,
            op_guard,
            transfer_id,
            filename,
            remote_path,
            local_path,
            modified,
            progress_cb,
            file_size,
            delta_fallback_reason,
            Some(cancel_token),
        )
        .await;
    }

    // Issue #332: make an in-flight download interruptible. Reset the shared
    // provider cancel flag for this transfer, then race the download future
    // against it. A user Cancel raises the flag via `cancel_transfer`; when it
    // flips we stop polling and drop the download future, which tears down the
    // connection and returns promptly instead of running the transfer to
    // completion. The steady-state (no-cancel) path is unchanged apart from a
    // 200ms wakeup. The leftover `.aerotmp` is reclaimed by `cleanup`.
    state
        .cancel_flag
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let dl_cancel = state.cancel_flag.clone();
    let result = {
        let work = async {
            if let Some(Ok(())) = &segmented_result {
                Ok(())
            } else if partial_offset > 0 {
                info!(
                    "Resuming download from offset {} bytes: {}",
                    partial_offset, filename
                );
                provider
                    .resume_download(&remote_path, &local_path, partial_offset, progress_cb)
                    .await
            } else {
                provider
                    .download(&remote_path, &local_path, progress_cb)
                    .await
            }
        };
        tokio::pin!(work);
        loop {
            tokio::select! {
                biased;
                r = &mut work => break r,
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                    if dl_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        crate::transfer_event_sink::emit_gui_transfer_event(&app, crate::TransferEvent {
                                event_type: "error".to_string(),
                                transfer_id: transfer_id.clone(),
                                filename: filename.clone(),
                                direction: "download".to_string(),
                                message: Some("Download cancelled by user".to_string()),
                                progress: None,
                                path: None,
                                delta_stats: None,
                                fallback_reason: None,
                            },);
                        // Break so MTP (and others) can drop a partial local file.
                        break Err(crate::providers::types::ProviderError::TransferFailed(
                            format!("Download cancelled by user: {}", filename),
                        ));
                    }
                }
            }
        }
    };

    // Partial local target is not a valid whole file after cancel (MTP has no resume).
    if let Err(ref err) = result {
        if err.to_string().contains("cancelled by user") {
            let _ = tokio::fs::remove_file(&local_path).await;
        }
    }

    match &result {
        Ok(()) => {
            // Preserve remote mtime on the local file
            crate::preserve_remote_mtime(&local_path, modified.as_deref());
            let actual_size = tokio::fs::metadata(&local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(file_size);
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "({} in 0s)",
                        if actual_size > 1_048_576 {
                            format!("{:.1} MB", actual_size as f64 / 1_048_576.0)
                        } else {
                            format!("{:.1} KB", actual_size as f64 / 1024.0)
                        }
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: delta_fallback_reason,
                },
            );
            info!("Download completed: {}", filename);
            Ok(format!("Downloaded: {}", filename))
        }
        Err(e) if e.to_string().contains("cancelled by user") => {
            // Cancel path already emitted a transfer_event above.
            Err(format!("Download cancelled by user: {}", filename))
        }
        Err(e) => {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Download failed: {}", e)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(format!("Download failed: {}", e))
        }
    }
}

/// Download a folder recursively from the remote server (OAuth providers)
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_download_folder(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    file_exists_action: Option<String>,
    max_concurrent: Option<u32>,
    retry_count: Option<u32>,
    timeout_seconds: Option<u64>,
    download_segments: Option<u32>,
) -> Result<String, String> {
    let transfer_settings = TransferSettingsInput {
        max_concurrent,
        retry_count,
        timeout_seconds,
        download_segments,
    };

    // Capture current pwd so we can restore it after folder scan changes it
    let original_pwd = {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            p.pwd().await.unwrap_or_default()
        } else {
            String::new()
        }
    };

    // RAII guard: clears TRANSFER_IN_PROGRESS on every exit path including panic.
    let _transfer_guard = TransferInProgressGuard::acquire();
    let result = provider_download_folder_inner(
        &app,
        &state,
        &remote_path,
        &local_path,
        file_exists_action,
        transfer_settings,
    )
    .await;

    // Restore provider pwd (folder scan traverses subdirectories via cd)
    if !original_pwd.is_empty() {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            let _ = p.cd(&original_pwd).await;
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_upload_folder(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    file_exists_action: Option<String>,
    max_concurrent: Option<u32>,
    retry_count: Option<u32>,
    timeout_seconds: Option<u64>,
    commit_message: Option<String>,
) -> Result<String, String> {
    // Fail-closed: never write plaintext into a crypt store whose overlay is
    // currently unwrapped (badge locked / outside the encrypted scope).
    state.guard_no_raw_crypt_write("Upload")?;

    let transfer_settings = TransferSettingsInput {
        max_concurrent,
        retry_count,
        timeout_seconds,
        // Upload-side intra-file parallelism is a separate slice (out
        // of scope for GTC-1); upload paths keep single-stream legacy
        // behaviour regardless of the requested segments knob.
        download_segments: None,
    };

    // Capture current pwd so we can restore it after upload
    let original_pwd = {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            p.pwd().await.unwrap_or_default()
        } else {
            String::new()
        }
    };

    let _transfer_guard = TransferInProgressGuard::acquire();
    let result = provider_upload_folder_inner(
        &app,
        &state,
        &local_path,
        &remote_path,
        file_exists_action,
        transfer_settings,
        commit_message,
    )
    .await;

    // Restore provider pwd (upload may change it via mkdir/cd)
    if !original_pwd.is_empty() {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            let _ = p.cd(&original_pwd).await;
        }
    }

    result
}

/// Collected file entry for 2-phase download
struct DownloadEntry {
    remote_path: String,
    local_path: String,
    name: String,
    size: u64,
    modified: Option<String>,
}

fn provider_transfer_cancelled(state: &State<'_, ProviderState>) -> bool {
    state.cancel_flag.load(Ordering::Relaxed)
}

/// Sanitize a remote filename to prevent path traversal attacks.
/// Strips path separators, `..` components, null bytes, and drive letters.
/// Returns the sanitized filename, or an error if the name is empty or entirely unsafe.
pub(crate) fn sanitize_remote_filename(name: &str) -> Result<String, String> {
    // Split on both Unix and Windows path separators, filter out dangerous components
    let sanitized: Vec<&str> = name
        .split(&['/', '\\'][..])
        .filter(|component| {
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && !component.contains('\0')
        })
        .collect();

    if sanitized.is_empty() {
        return Err(format!("Unsafe remote filename rejected: {:?}", name));
    }

    // Take only the last component (the actual filename)
    let filename = sanitized
        .last()
        .ok_or_else(|| "Internal error: sanitized filename unexpectedly empty".to_string())?
        .to_string();

    // Reject Windows drive letters (e.g. "C:")
    if filename.len() >= 2
        && filename.as_bytes()[1] == b':'
        && filename.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(format!(
            "Unsafe remote filename with drive letter rejected: {:?}",
            name
        ));
    }

    Ok(filename)
}

/// Verify that a resolved path is safely contained within the expected base directory.
pub(crate) fn verify_path_containment(
    base: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    // Use canonicalize on the base (which must already exist)
    let canonical_base = base
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize base path: {}", e))?;

    // For target, canonicalize the parent (which should exist after create_dir_all)
    // and then append the filename
    let canonical_target = if target.exists() {
        target
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize target path: {}", e))?
    } else if let Some(parent) = target.parent() {
        if parent.exists() {
            let canonical_parent = parent
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize parent path: {}", e))?;
            canonical_parent.join(target.file_name().unwrap_or_default())
        } else {
            target.to_path_buf()
        }
    } else {
        target.to_path_buf()
    };

    if !canonical_target.starts_with(&canonical_base) {
        return Err(format!(
            "Path traversal detected: {:?} escapes base directory {:?}",
            canonical_target, canonical_base
        ));
    }
    Ok(())
}

/// Inner implementation: 2-phase approach with per-file lock release and retry
async fn provider_download_folder_inner(
    app: &AppHandle,
    state: &State<'_, ProviderState>,
    remote_path: &str,
    local_path: &str,
    file_exists_action: Option<String>,
    transfer_settings: TransferSettingsInput,
) -> Result<String, String> {
    let file_exists_action = file_exists_action.unwrap_or_default();
    let (runtime_settings, session_model, capabilities) =
        resolve_provider_transfer_runtime(&state.provider, transfer_settings).await;

    let cancel_token = state.reset_cancel_state().await;

    let folder_name = std::path::Path::new(remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("dl-folder-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Downloading folder via provider: {} -> {} (requested_concurrency={}, effective_concurrency={}, retries={}, timeout={}s)",
        remote_path,
        local_path,
        runtime_settings.requested_max_concurrent,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    // Emit start event
    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting folder download: {}", folder_name)),
            progress: None,
            path: Some(remote_path.to_string()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    // Create local folder
    tokio::fs::create_dir_all(local_path)
        .await
        .map_err(|e| format!("Failed to create local folder: {}", e))?;

    // ── Streaming scan + transfer: directory-by-directory interleaving ──
    //
    // Instead of scanning ALL files first, then downloading ALL files,
    // we scan one directory at a time and download its files immediately.
    // This means the first file starts downloading after scanning just the
    // root directory, not after the entire recursive scan completes.
    //
    // Pattern (like an audio player buffer):
    //   scan dir A → transfer files from A
    //   scan dir B → transfer files from B
    //   ...until all directories are exhausted.

    let mut folders_to_scan: Vec<(String, String)> =
        vec![(remote_path.to_string(), local_path.to_string())];
    let mut files_downloaded = 0u32;
    let mut files_skipped = 0u32;
    let mut total_files_discovered = 0u32;
    let mut dirs_scanned = 0u32;
    let mut file_global_index = 0u32;
    let mut last_scan_emit = std::time::Instant::now();
    let base_local = std::path::Path::new(local_path);
    let mut transfer_entries: Vec<TransferEntry> = Vec::new();

    while let Some((remote_folder, local_folder)) = folders_to_scan.pop() {
        // ── Check cancel before scanning next directory ──
        if provider_transfer_cancelled(state) {
            info!(
                "Provider folder download cancelled by user after {} files",
                files_downloaded
            );
            crate::transfer_event_sink::emit_gui_transfer_event(
                app,
                crate::TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "Download cancelled after {} files",
                        files_downloaded
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(format!(
                "Download cancelled after {} files",
                files_downloaded
            ));
        }

        // ── Scan this directory (acquire lock, list, release) ──
        let mut dir_files: Vec<DownloadEntry> = Vec::new();
        {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;

            provider
                .cd(&remote_folder)
                .await
                .map_err(|e| format!("Failed to change to folder {}: {}", remote_folder, e))?;

            let files = provider
                .list(".")
                .await
                .map_err(|e| format!("Failed to list files in {}: {}", remote_folder, e))?;

            for file in files {
                let safe_name = match sanitize_remote_filename(&file.name) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!("Skipping unsafe remote entry: {}", e);
                        continue;
                    }
                };

                let remote_file_path = if remote_folder.ends_with('/') {
                    format!("{}{}", remote_folder, file.name)
                } else {
                    format!("{}/{}", remote_folder, file.name)
                };
                let local_file_path_buf = std::path::Path::new(&local_folder).join(&safe_name);
                let local_file_path = local_file_path_buf.to_string_lossy().to_string();

                if file.is_dir {
                    tokio::fs::create_dir_all(&local_file_path)
                        .await
                        .map_err(|e| {
                            format!("Failed to create folder {}: {}", local_file_path, e)
                        })?;
                    verify_path_containment(base_local, &local_file_path_buf)?;
                    folders_to_scan.push((remote_file_path, local_file_path));
                } else {
                    if let Some(parent) = local_file_path_buf.parent() {
                        if parent.exists() {
                            verify_path_containment(base_local, &local_file_path_buf)?;
                        }
                    }
                    dir_files.push(DownloadEntry {
                        remote_path: remote_file_path,
                        local_path: local_file_path,
                        name: safe_name,
                        size: file.size,
                        modified: file.modified.clone(),
                    });
                }
            }
        } // ← provider lock released: ready to transfer this batch

        dirs_scanned += 1;
        total_files_discovered += dir_files.len() as u32;

        // Emit scanning progress
        if last_scan_emit.elapsed().as_millis() > 500 || dirs_scanned <= 1 {
            crate::transfer_event_sink::emit_gui_transfer_event(
                app,
                crate::TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "Scanning... {} files found, {} downloaded ({} dirs queued)",
                        total_files_discovered,
                        files_downloaded,
                        folders_to_scan.len()
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            last_scan_emit = std::time::Instant::now();
        }

        // ── Transfer files from this directory immediately ──
        for entry in &dir_files {
            // Check cancel before each file
            if provider_transfer_cancelled(state) {
                info!(
                    "Provider folder download cancelled by user after {} files",
                    files_downloaded
                );
                crate::transfer_event_sink::emit_gui_transfer_event(
                    app,
                    crate::TransferEvent {
                        event_type: "cancelled".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: folder_name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!(
                            "Download cancelled after {} files",
                            files_downloaded
                        )),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                return Ok(format!(
                    "Download cancelled after {} files",
                    files_downloaded
                ));
            }

            file_global_index += 1;

            // Check if local file exists and should be skipped
            if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
                let local_p = std::path::Path::new(&entry.local_path);
                if let Ok(local_meta) = std::fs::metadata(local_p) {
                    if local_meta.is_file() {
                        let remote_modified = entry.modified.as_ref().and_then(|s| {
                            let clean = s.strip_suffix('Z').unwrap_or(s);
                            chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M:%S")
                                .or_else(|_| {
                                    chrono::NaiveDateTime::parse_from_str(
                                        clean,
                                        "%Y-%m-%dT%H:%M:%S",
                                    )
                                })
                                .ok()
                                .map(|ndt| ndt.and_utc())
                        });
                        if crate::should_skip_file_download(
                            &file_exists_action,
                            remote_modified,
                            entry.size,
                            &local_meta,
                        ) {
                            files_skipped += 1;
                            crate::transfer_event_sink::emit_gui_transfer_event(
                                app,
                                crate::TransferEvent {
                                    event_type: "file_skip".to_string(),
                                    transfer_id: format!("{}-{}", transfer_id, file_global_index),
                                    filename: entry.name.clone(),
                                    direction: "download".to_string(),
                                    message: Some(format!("Skipped (identical): {}", entry.name)),
                                    progress: None,
                                    path: Some(entry.remote_path.clone()),
                                    delta_stats: None,
                                    fallback_reason: None,
                                },
                            );
                            continue;
                        }
                    }
                }
            }

            let file_transfer_id = format!("{}-{}", transfer_id, file_global_index);

            transfer_entries.push(TransferEntry {
                id: file_transfer_id,
                display_name: entry.name.clone(),
                remote_path: entry.remote_path.clone(),
                local_path: entry.local_path.clone(),
                size: entry.size,
                modified: entry.modified.clone(),
            });
        }
    }

    let batch = TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: TransferDirection::Download,
        config: TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
            max_backlog: crate::transfer_domain::default_transfer_max_backlog(),
            schedule: Default::default(),
        },
        entries: transfer_entries,
    };

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = remote_path.to_string();
    let total_files_for_progress = total_files_discovered;
    let initial_skipped = files_skipped;
    let progress_observer: ProgressObserver = Arc::new(move |snapshot| {
        let processed = initial_skipped + snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        crate::transfer_event_sink::emit_gui_transfer_event(
            &progress_app,
            crate::TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "download".to_string(),
                message: Some(format!(
                    "Downloaded {} / {} files ({} skipped, {} errors)",
                    snapshot.completed, total_files_for_progress, initial_skipped, snapshot.failed
                )),
                progress: Some(crate::TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_folder_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "download".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_remote_path.clone()),
                }),
                path: Some(progress_remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let sink: Arc<dyn TransferEventSink> = Arc::new(AppHandleSink::new(app.clone()));
    let executor = Arc::new(ProviderDownloadExecutor::new(
        sink.clone(),
        state.provider.clone(),
        runtime_settings,
        cancel_token,
        session_model,
        capabilities,
    ));

    let batch_result = execute_batch(
        sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    files_downloaded = batch_result.completed;
    let files_errored = batch_result.failed;

    info!(
        "Provider folder download completed via orchestrator: {} ({} downloaded, {} skipped, {} errors)",
        folder_name, files_downloaded, files_skipped, files_errored
    );

    let event_type = if batch_result.cancelled {
        "cancelled".to_string()
    } else {
        "complete".to_string()
    };
    let result_message = if batch_result.cancelled {
        format!(
            "Download cancelled after {} files",
            files_downloaded + files_skipped + files_errored
        )
    } else {
        format!(
            "Downloaded {} files, {} skipped, {} errors",
            files_downloaded, files_skipped, files_errored
        )
    };

    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type,
            transfer_id,
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

async fn provider_upload_folder_inner(
    app: &AppHandle,
    state: &State<'_, ProviderState>,
    local_path: &str,
    remote_path: &str,
    file_exists_action: Option<String>,
    transfer_settings: TransferSettingsInput,
    commit_message: Option<String>,
) -> Result<String, String> {
    let file_exists_action = file_exists_action.unwrap_or_default();
    let (runtime_settings, session_model, capabilities) =
        resolve_provider_transfer_runtime(&state.provider, transfer_settings).await;

    let cancel_token = state.reset_cancel_state().await;

    let folder_name = std::path::Path::new(local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("ul-folder-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Uploading folder via provider: {} -> {} (requested_concurrency={}, effective_concurrency={}, retries={}, timeout={}s)",
        local_path,
        remote_path,
        runtime_settings.requested_max_concurrent,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Starting folder upload: {}", folder_name)),
            progress: None,
            path: Some(remote_path.to_string()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let local_base = std::path::Path::new(local_path);
    if !local_base.is_dir() {
        return Err("Source is not a directory".to_string());
    }

    {
        let mut provider_lock = state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;

        if is_plain_github_provider(provider.as_mut()) {
            let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            if let Err(e) = github
                .create_directory(remote_path, commit_message.as_deref())
                .await
            {
                let err_str = e.to_string().to_lowercase();
                if !err_str.contains("exist")
                    && !err_str.contains("409")
                    && !err_str.contains("already")
                {
                    return Err(format!("Failed to create directory: {}", e));
                }
            }
        } else if let Err(e) = provider.mkdir(remote_path).await {
            let err_str = e.to_string().to_lowercase();
            if !err_str.contains("exist") && !err_str.contains("409") && !err_str.contains("550") {
                return Err(format!("Failed to create directory: {}", e));
            }
        }
    }

    let mut dirs_to_scan: Vec<(std::path::PathBuf, String)> =
        vec![(local_base.to_path_buf(), remote_path.to_string())];
    let mut dirs_to_create: Vec<String> = Vec::new();
    let mut transfer_entries: Vec<TransferEntry> = Vec::new();
    let mut total_files_discovered = 0u32;
    let mut dirs_scanned = 0u32;
    let mut file_global_index = 0u32;
    let mut last_scan_emit = std::time::Instant::now();

    while let Some((current_local_dir, current_remote_dir)) = dirs_to_scan.pop() {
        if provider_transfer_cancelled(state) {
            crate::transfer_event_sink::emit_gui_transfer_event(
                app,
                crate::TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "Upload cancelled after {} files",
                        transfer_entries.len()
                    )),
                    progress: None,
                    path: Some(remote_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(format!(
                "Upload cancelled after {} files",
                transfer_entries.len()
            ));
        }

        let mut read_dir = tokio::fs::read_dir(&current_local_dir)
            .await
            .map_err(|e| format!("Failed to read directory {:?}: {}", current_local_dir, e))?;

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let local_entry_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_entry_path =
                format!("{}/{}", current_remote_dir.trim_end_matches('/'), name);
            let file_type = match entry.file_type().await {
                Ok(file_type) => file_type,
                Err(error) => {
                    warn!(
                        "Failed to read provider upload entry type {:?}: {}",
                        local_entry_path, error
                    );
                    continue;
                }
            };

            if file_type.is_symlink() {
                crate::transfer_event_sink::emit_gui_transfer_event(
                    app,
                    crate::TransferEvent {
                        event_type: "file_skip".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: name.clone(),
                        direction: "upload".to_string(),
                        message: Some(format!("Skipped symlink: {}", name)),
                        progress: None,
                        path: Some(remote_entry_path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                continue;
            }

            if file_type.is_dir() {
                dirs_to_scan.push((local_entry_path.clone(), remote_entry_path.clone()));
                dirs_to_create.push(remote_entry_path);
            } else if file_type.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                file_global_index += 1;
                total_files_discovered += 1;
                transfer_entries.push(TransferEntry {
                    id: format!("{}-{}", transfer_id, file_global_index),
                    display_name: name.clone(),
                    remote_path: remote_entry_path,
                    local_path: local_entry_path.to_string_lossy().to_string(),
                    size,
                    modified: None,
                });
            }
        }

        dirs_scanned += 1;
        if last_scan_emit.elapsed().as_millis() > 500 || dirs_scanned <= 1 {
            crate::transfer_event_sink::emit_gui_transfer_event(
                app,
                crate::TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "Scanning... {} files found ({} dirs queued)",
                        total_files_discovered,
                        dirs_to_scan.len()
                    )),
                    progress: None,
                    path: Some(remote_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            last_scan_emit = std::time::Instant::now();
        }
    }

    dirs_to_create.sort_by_key(|a| a.matches('/').count());
    for remote_dir in &dirs_to_create {
        let mut provider_lock = state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;

        let mkdir_result = if is_plain_github_provider(provider.as_mut()) {
            let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            github
                .create_directory(remote_dir, commit_message.as_deref())
                .await
                .map_err(|e| e.to_string())
        } else {
            provider.mkdir(remote_dir).await.map_err(|e| e.to_string())
        };

        if let Err(error) = mkdir_result {
            let lowered = error.to_lowercase();
            if !lowered.contains("exist") && !lowered.contains("409") {
                warn!(
                    "Failed to create provider directory {}: {}",
                    remote_dir, error
                );
            }
        }
    }

    let mut files_skipped = 0u32;
    if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
        let mut remote_index: std::collections::HashMap<
            String,
            (u64, Option<chrono::DateTime<chrono::Utc>>),
        > = std::collections::HashMap::new();
        let mut remote_dirs = Vec::with_capacity(dirs_to_create.len() + 1);
        remote_dirs.push(remote_path.to_string());
        remote_dirs.extend(dirs_to_create.iter().cloned());
        remote_dirs.sort();
        remote_dirs.dedup();

        {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;

            for remote_dir in &remote_dirs {
                if provider_transfer_cancelled(state) {
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        app,
                        crate::TransferEvent {
                            event_type: "cancelled".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: folder_name.clone(),
                            direction: "upload".to_string(),
                            message: Some(
                                "Upload cancelled before remote conflict scan".to_string(),
                            ),
                            progress: None,
                            path: Some(remote_path.to_string()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                    return Ok("Upload cancelled before remote conflict scan".to_string());
                }

                match provider.list(remote_dir).await {
                    Ok(entries) => {
                        for entry in entries.into_iter().filter(|entry| !entry.is_dir) {
                            let fallback_path =
                                format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
                            let modified =
                                crate::parse_remote_modified_datetime(entry.modified.as_deref());
                            remote_index.insert(fallback_path.clone(), (entry.size, modified));
                            if entry.path != fallback_path {
                                remote_index.insert(entry.path, (entry.size, modified));
                            }
                        }
                    }
                    Err(error) => warn!(
                        "Failed to list provider directory {} before conflict scan: {}",
                        remote_dir, error
                    ),
                }
            }
        }

        transfer_entries.retain(|entry| {
            let Some(&(remote_size, remote_modified)) = remote_index.get(&entry.remote_path) else {
                return true;
            };
            let Ok(local_meta) = std::fs::metadata(&entry.local_path) else {
                return true;
            };
            if !crate::should_skip_file_upload(
                &file_exists_action,
                &local_meta,
                remote_size,
                remote_modified,
            ) {
                return true;
            }

            files_skipped += 1;
            crate::transfer_event_sink::emit_gui_transfer_event(
                app,
                crate::TransferEvent {
                    event_type: "file_skip".to_string(),
                    transfer_id: entry.id.clone(),
                    filename: entry.display_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Skipped (identical): {}", entry.display_name)),
                    progress: None,
                    path: Some(entry.remote_path.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            false
        });
    }

    let total_files_for_progress = transfer_entries.len() as u32;

    let batch = TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: TransferDirection::Upload,
        config: TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
            max_backlog: crate::transfer_domain::default_transfer_max_backlog(),
            schedule: Default::default(),
        },
        entries: transfer_entries,
    };

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = remote_path.to_string();
    let progress_observer: ProgressObserver = Arc::new(move |snapshot| {
        let processed = snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        crate::transfer_event_sink::emit_gui_transfer_event(
            &progress_app,
            crate::TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!(
                    "Uploaded {} / {} files ({} errors)",
                    snapshot.completed, total_files_for_progress, snapshot.failed
                )),
                progress: Some(crate::TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_folder_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "upload".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_remote_path.clone()),
                }),
                path: Some(progress_remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let sink: Arc<dyn TransferEventSink> = Arc::new(AppHandleSink::new(app.clone()));
    let executor = Arc::new(ProviderUploadExecutor::new(
        sink.clone(),
        state.provider.clone(),
        runtime_settings,
        commit_message,
        cancel_token,
        session_model,
        capabilities,
    ));

    let batch_result = execute_batch(
        sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    let files_uploaded = batch_result.completed;
    let files_errored = batch_result.failed;
    let event_type = if batch_result.cancelled {
        "cancelled".to_string()
    } else {
        "complete".to_string()
    };
    let result_message = if batch_result.cancelled {
        format!(
            "Upload cancelled after {} files",
            files_uploaded + files_errored
        )
    } else {
        format!(
            "Uploaded {} files, {} skipped, {} errors",
            files_uploaded, files_skipped, files_errored
        )
    };

    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type,
            transfer_id,
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

/// Upload a file to the remote server
#[tauri::command]
pub async fn provider_upload_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    commit_message: Option<String>,
    use_delta: Option<bool>,
    resume: Option<bool>,
) -> Result<String, String> {
    // Fail-closed: never write plaintext into a crypt store whose overlay is
    // currently unwrapped (badge locked / outside the encrypted scope).
    state.guard_no_raw_crypt_write("Upload")?;

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    // Reject upload targets with characters this provider's backend forbids
    // before any transfer work starts (delta / DAG / GitHub branches below).
    crate::restricted_chars::validate_path(provider.provider_type(), &remote_path)
        .map_err(|e| e.to_string())?;

    let filename = std::path::Path::new(&local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("pul-{}", chrono::Utc::now().timestamp_millis());
    let file_size = tokio::fs::metadata(&local_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    info!("Uploading via provider: {} -> {}", local_path, remote_path);

    // Emit start event
    crate::transfer_event_sink::emit_gui_transfer_event(
        &app,
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Starting upload: {}", filename)),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let app_progress = app.clone();
    let tid_progress = transfer_id.clone();
    let fname_progress = filename.clone();

    let ul_start_time = std::time::Instant::now();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if file_size > 0 {
        Some(Box::new(move |transferred: u64, total: u64| {
            let pct = if total > 0 {
                ((transferred as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };
            let elapsed = ul_start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.1 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let eta = if speed > 0 && transferred < total {
                ((total - transferred) as f64 / speed as f64) as u64
            } else {
                0
            };
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app_progress,
                crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: tid_progress.clone(),
                    filename: fname_progress.clone(),
                    direction: "upload".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: tid_progress.clone(),
                        filename: fname_progress.clone(),
                        direction: "upload".to_string(),
                        percentage: pct,
                        transferred,
                        total,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }))
    } else {
        None
    };

    // Delta path (SFTP + key-auth + rsync on remote): same contract as
    // `sync::perform_upload`. Skipped automatically for GitHub / non-SFTP
    // / password-only auth (self-gated inside `try_delta_transfer`).
    // `hard_error` must not silently fall back to the classic path.
    let mut delta_fallback_reason: Option<String> = None;
    {
        if crate::delta_sync_rsync::gui_delta_enabled() && use_delta.unwrap_or(true) {
            let local_path_buf = std::path::PathBuf::from(&local_path);
            // FINDING-4 Part B: race the delta (native rsync) upload against the
            // live cancel token. The delta transport runs on its own SSH
            // connection and bypasses the classic cancel path, so without this a
            // Stop during a delta upload ran to completion. On cancel we drop the
            // future and return, without falling through to the classic path.
            let cancel_token = state.current_cancel_token().await;
            let delta_fut = crate::delta_sync_rsync::try_delta_transfer_with_progress(
                provider.as_mut(),
                crate::delta_sync_rsync::SyncDirection::Upload,
                &local_path_buf,
                &remote_path,
                Some(crate::make_delta_progress_sink(
                    app.clone(),
                    transfer_id.clone(),
                    filename.clone(),
                    "upload",
                )),
            );
            let delta_cancelled;
            let delta_outcome = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    delta_cancelled = true;
                    None
                }
                r = delta_fut => {
                    delta_cancelled = false;
                    r
                }
            };
            if delta_cancelled {
                let err_msg = format!("Upload cancelled by user: {}", filename);
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "error".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "upload".to_string(),
                        message: Some("Upload cancelled by user".to_string()),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                return Err(err_msg);
            }
            if let Some(delta_result) = delta_outcome {
                if delta_result.used_delta {
                    let delta_stats = delta_result
                        .stats
                        .as_ref()
                        .map(crate::sync::DeltaTransferStats::from_rsync);
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "complete".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: Some(format!(
                                "({} via delta)",
                                if file_size > 1_048_576 {
                                    format!("{:.1} MB", file_size as f64 / 1_048_576.0)
                                } else {
                                    format!("{:.1} KB", file_size as f64 / 1024.0)
                                }
                            )),
                            progress: None,
                            path: None,
                            delta_stats,
                            fallback_reason: None,
                        },
                    );
                    info!("Upload completed via delta path: {}", filename);
                    return Ok(format!("Uploaded: {}", filename));
                }
                if let Some(hard_err) = delta_result.hard_error {
                    let err_msg = format!("delta hard rejection: {}", hard_err);
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "error".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: Some(err_msg.clone()),
                            progress: None,
                            path: None,
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                    return Err(err_msg);
                }
                // Silent fallback to classic provider upload below.
                delta_fallback_reason = delta_result.fallback_reason;
            }
        }
    }

    // Resume-aware classic upload. When the caller asked to resume an
    // interrupted transfer and the provider can append from a byte offset
    // (currently SFTP), continue from the remote's current size instead of
    // re-sending from zero. Reached only when the delta path did not already
    // handle the transfer (delta itself resumes efficiently for key-auth SFTP).
    // Crypt overlay providers report supports_resume_upload_append()=false, so a
    // crypt-bound upload always falls through to a full re-encrypt (fail-safe),
    // and guard_no_raw_crypt_write above already blocks an unwrapped crypt store.
    if resume.unwrap_or(false)
        && !is_plain_github_provider(provider.as_mut())
        && provider.supports_resume_upload_append()
    {
        let remote_size = provider.size(&remote_path).await.unwrap_or(0);
        // Only resume a genuine partial: a smaller-but-nonzero remote file.
        if remote_size > 0 && remote_size < file_size {
            info!(
                "Resuming upload from offset {} bytes: {}",
                remote_size, filename
            );
            // FINDING-4 Part B parity: race the append against the live cancel
            // token so an in-flight Stop drops the resume future (the russh
            // write stream tears down on drop) instead of running to completion.
            let cancel_token = state.current_cancel_token().await;
            let up_fut =
                provider.resume_upload(&local_path, &remote_path, remote_size, progress_cb);
            tokio::pin!(up_fut);
            let mut resume_outcome: Option<Result<(), ProviderError>> = None;
            let cancelled = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => true,
                r = &mut up_fut => { resume_outcome = Some(r); false }
            };
            if cancelled {
                let err_msg = format!("Upload cancelled by user: {}", filename);
                crate::transfer_event_sink::emit_gui_transfer_event(
                    &app,
                    crate::TransferEvent {
                        event_type: "error".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "upload".to_string(),
                        message: Some("Upload cancelled by user".to_string()),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                return Err(err_msg);
            }
            return match resume_outcome.expect("resume outcome set when not cancelled") {
                Ok(()) => {
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "complete".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: Some(format!(
                                "({} via resume)",
                                if file_size > 1_048_576 {
                                    format!("{:.1} MB", file_size as f64 / 1_048_576.0)
                                } else {
                                    format!("{:.1} KB", file_size as f64 / 1024.0)
                                }
                            )),
                            progress: None,
                            path: None,
                            delta_stats: None,
                            fallback_reason: delta_fallback_reason,
                        },
                    );
                    info!("Upload resumed to completion: {}", filename);
                    Ok(format!("Uploaded: {}", filename))
                }
                Err(e) => {
                    let err_msg = format!("Upload failed: {}", e);
                    crate::transfer_event_sink::emit_gui_transfer_event(
                        &app,
                        crate::TransferEvent {
                            event_type: "error".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: Some(err_msg.clone()),
                            progress: None,
                            path: None,
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                    Err(err_msg)
                }
            };
        }
        // remote_size == 0 or >= file_size: nothing to resume; fall through to
        // the normal DAG upload below (progress_cb is untouched here).
    }

    // DAG-ENGINE: route the plain classic single-file upload through the
    // graph engine. GitHub keeps its dedicated commit-based upload (a
    // different API shape, not the plain leaf). The shaped runner handles
    // both the single-`UploadFile` core and the multipart fan-out shape.
    if !is_plain_github_provider(provider.as_mut()) {
        let provider_arc = Arc::clone(&state.provider);
        // Issue #233: acquire the in-flight guard before releasing the
        // mutex, so the swap or disconnect path waits for the DAG leaf
        // to return instead of yanking the provider box from under it.
        let op_guard = TransferOperationGuard::acquire(&state);
        // FINDING-4 Part B: grab the LIVE session cancel token (never reset here)
        // so an in-flight Stop drops the current upload.
        let cancel_token = state.current_cancel_token().await;
        drop(provider_lock);
        return run_dag_upload_leaf(
            app,
            provider_arc,
            op_guard,
            transfer_id,
            filename,
            local_path,
            remote_path,
            progress_cb,
            file_size,
            delta_fallback_reason,
            Some(cancel_token),
        )
        .await;
    }

    // Issue #332: make an in-flight upload interruptible, mirroring
    // provider_download_file. Reset the cancel flag for this transfer, then race
    // the upload future against it so a user Cancel drops the future and returns
    // promptly. The steady-state path is unchanged apart from a 200ms wakeup.
    state
        .cancel_flag
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let ul_cancel = state.cancel_flag.clone();
    let result = {
        let work = async {
            if is_plain_github_provider(provider.as_mut()) {
                let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
                    .as_any_mut()
                    .downcast_mut::<crate::providers::github::GitHubProvider>()
                    .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
                github
                    .upload_file(&local_path, &remote_path, commit_message.as_deref())
                    .await
                    .map_err(|e| format!("Upload failed: {}", e))
            } else {
                provider
                    .upload(&local_path, &remote_path, progress_cb)
                    .await
                    .map_err(|e| format!("Upload failed: {}", e))
            }
        };
        tokio::pin!(work);
        loop {
            tokio::select! {
                biased;
                r = &mut work => break r,
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                    if ul_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        crate::transfer_event_sink::emit_gui_transfer_event(&app, crate::TransferEvent {
                                event_type: "error".to_string(),
                                transfer_id: transfer_id.clone(),
                                filename: filename.clone(),
                                direction: "upload".to_string(),
                                message: Some("Upload cancelled by user".to_string()),
                                progress: None,
                                path: None,
                                delta_stats: None,
                                fallback_reason: None,
                            },);
                        // Break (do not return) so MTP can best-effort delete a
                        // partial object below. MTP has no honest resume: a
                        // half-sent object is invalid on many devices.
                        break Err(format!("Upload cancelled by user: {}", filename));
                    }
                }
            }
        }
    };

    // MTP honesty: cancelled whole-file upload may leave a partial object.
    // Delete it when the backend allows; never claim resume.
    if let Err(ref err) = result {
        if err.contains("cancelled by user")
            && provider.provider_type() == crate::providers::types::ProviderType::Mtp
        {
            if let Err(del_err) = provider.delete(&remote_path).await {
                warn!(
                    "MTP cancelled upload cleanup delete failed for {}: {}",
                    remote_path, del_err
                );
            }
        }
    }

    match &result {
        Ok(()) => {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "({} in 0s)",
                        if file_size > 1_048_576 {
                            format!("{:.1} MB", file_size as f64 / 1_048_576.0)
                        } else {
                            format!("{:.1} KB", file_size as f64 / 1024.0)
                        }
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: delta_fallback_reason,
                },
            );
            info!("Upload completed: {}", filename);
            Ok(format!("Uploaded: {}", filename))
        }
        Err(e) if e.contains("cancelled by user") => {
            // Cancel path already emitted a transfer_event above.
            Err(e.clone())
        }
        Err(e) => {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &app,
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(e.clone()),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(e.clone())
        }
    }
}

/// Create a directory
#[tauri::command]
pub async fn provider_mkdir(
    state: State<'_, ProviderState>,
    path: String,
    commit_message: Option<String>,
) -> Result<(), String> {
    // Fail-closed: refuse a cleartext mkdir name into an unwrapped crypt store.
    state.guard_no_raw_crypt_write("Create directory")?;

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Creating directory: {}", path);

    // Reject folder names with characters this provider's backend forbids.
    crate::restricted_chars::validate_path(provider.provider_type(), &path)
        .map_err(|e| e.to_string())?;

    if is_plain_github_provider(provider.as_mut()) {
        let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .create_directory(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    } else {
        provider
            .mkdir(&path)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    Ok(())
}

/// Delete a file
#[tauri::command]
pub async fn provider_delete_file(
    state: State<'_, ProviderState>,
    path: String,
    commit_message: Option<String>,
) -> Result<(), String> {
    // Fail-closed: a plaintext path against an unwrapped crypt store targets the
    // wrong (or no) object; refuse instead of acting on the raw backend.
    state.guard_no_raw_crypt_write("Delete file")?;

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Deleting file: {}", path);

    if is_plain_github_provider(provider.as_mut()) {
        let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .delete_file(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to delete file: {}", e))?;
    } else {
        provider
            .delete(&path)
            .await
            .map_err(|e| format!("Failed to delete file: {}", e))?;
    }

    Ok(())
}

/// Delete a directory
#[tauri::command]
pub async fn provider_delete_dir(
    app: AppHandle,
    state: State<'_, ProviderState>,
    path: String,
    recursive: bool,
    commit_message: Option<String>,
) -> Result<(), String> {
    // Fail-closed: a plaintext path against an unwrapped crypt store targets the
    // wrong (or no) object; refuse instead of acting on the raw backend.
    state.guard_no_raw_crypt_write("Delete directory")?;

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Deleting directory: {} (recursive: {})", path, recursive);

    // Emit scanning event for folder deletes so the ScanningToast appears
    if recursive {
        let folder_name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        crate::transfer_event_sink::emit_gui_transfer_event(
            &app,
            crate::TransferEvent {
                event_type: "scanning".to_string(),
                transfer_id: format!("del-dir-{}", chrono::Utc::now().timestamp_millis()),
                filename: folder_name,
                direction: "delete".to_string(),
                message: Some("Scanning folder for deletion...".to_string()),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    if is_plain_github_provider(provider.as_mut()) {
        // QA-GH-006: GitHub always needs recursive delete (no empty dirs in git)
        let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .delete_directory_recursive(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    } else if recursive {
        provider
            .rmdir_recursive(&path)
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    } else {
        provider
            .rmdir(&path)
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    }

    // Emit delete_complete so ScanningToast dismisses
    if recursive {
        let folder_name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        crate::transfer_event_sink::emit_gui_transfer_event(
            &app,
            crate::TransferEvent {
                event_type: "delete_complete".to_string(),
                transfer_id: format!("del-dir-done-{}", chrono::Utc::now().timestamp_millis()),
                filename: folder_name,
                direction: "delete".to_string(),
                message: Some("Directory deleted".to_string()),
                progress: None,
                path: Some(path),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    Ok(())
}

/// Rename a file or directory
#[tauri::command]
pub async fn provider_rename(
    state: State<'_, ProviderState>,
    from: String,
    to: String,
) -> Result<(), String> {
    // Fail-closed: renaming through the raw backend while the overlay is
    // unwrapped would create a cleartext name in the encrypted store.
    state.guard_no_raw_crypt_write("Rename")?;

    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Renaming: {} -> {}", from, to);

    // Reject names with characters this provider's backend forbids before the
    // rename hits the API, so the user sees a clear message instead of a silent
    // or opaque failure (discussion #272).
    crate::restricted_chars::validate_path(provider.provider_type(), &to)
        .map_err(|e| e.to_string())?;

    provider
        .rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

/// Capability-shaped copy through the production transfer DAG.
///
/// The graph exposes either one native `ServerSideCopy` core or an explicit
/// `DownloadFile` then `UploadFile` core. Recoverable native-copy rejection
/// is observed at the node boundary before the fallback graph runs.
#[tauri::command]
pub async fn provider_server_copy(
    state: State<'_, ProviderState>,
    from: String,
    to: String,
) -> Result<(), String> {
    // Fail-closed: a raw server-side copy while the overlay is unwrapped would
    // land a cleartext-named object in the encrypted store (the content stays
    // ciphertext but the name never gets encrypted), orphaning it from the
    // overlay's decrypted listing.
    state.guard_no_raw_crypt_write("Server copy")?;

    let provider_lock = state.provider.lock().await;
    if provider_lock.is_none() {
        return Err("Not connected to any provider".to_string());
    }

    info!("Server copy: {} -> {}", from, to);

    let provider = Arc::clone(&state.provider);
    let _op_guard = TransferOperationGuard::acquire(&state);
    drop(provider_lock);

    match crate::transfer_dag_single_file::execute_copy_dag(
        crate::transfer_dag_single_file::CopyProviderHandle::optional(provider),
        from,
        to,
        Arc::new(crate::transfer_dag::NoopDagObserver),
    )
    .await
    {
        Ok(outcome) => {
            info!(
                decision = ?outcome.decision,
                logical_bytes = outcome.metrics.logical_bytes,
                wire_bytes = outcome.metrics.wire_bytes,
                local_payload_bytes = outcome.metrics.local_payload_bytes,
                "Copy DAG completed"
            );
            Ok(())
        }
        Err(e) => Err(format!("Failed to copy: {}", e)),
    }
}

/// Check if provider supports server-side copy
#[tauri::command]
pub async fn provider_supports_server_copy(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_lock = state.provider.lock().await;
    let provider = provider_lock
        .as_ref()
        .ok_or("Not connected to any provider")?;
    Ok(provider.supports_server_copy())
}

/// Get file/directory information
#[tauri::command]
pub async fn provider_stat(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<RemoteEntry, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .stat(&path)
        .await
        .map_err(|e| format!("Failed to get file info: {}", e))
}

/// Server-side content hash(es) of a remote object WITHOUT downloading it.
///
/// Returns an `algo -> hex` map populated only from what the backend
/// exposes cheaply (S3 ETag md5, B2 contentSha1, pCloud, SFTP
/// `sha256sum`, Drive/OneDrive/Box API digests, Dropbox `content_hash`).
/// An empty map means the provider has no cheap server-side hash for
/// this object; the caller should say so rather than download it.
#[tauri::command]
pub async fn provider_checksum(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    if !provider.supports_checksum() {
        return Ok(std::collections::HashMap::new());
    }

    provider
        .checksum(&path)
        .await
        .map_err(|e| format!("Failed to get server-side checksum: {}", e))
}

/// Keep connection alive (NOOP equivalent)
#[tauri::command]
pub async fn provider_keep_alive(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    if let Some(ref mut provider) = *provider_lock {
        provider
            .keep_alive()
            .await
            .map_err(|e| format!("Keep alive failed: {}", e))?;
    }

    Ok(())
}

/// Get server information
#[tauri::command]
pub async fn provider_server_info(state: State<'_, ProviderState>) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .server_info()
        .await
        .map_err(|e| format!("Failed to get server info: {}", e))
}

/// Get file size
#[tauri::command]
pub async fn provider_file_size(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .size(&path)
        .await
        .map_err(|e| format!("Failed to get file size: {}", e))
}

/// Check if a file/directory exists
#[tauri::command]
pub async fn provider_exists(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<bool, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .exists(&path)
        .await
        .map_err(|e| format!("Failed to check existence: {}", e))
}

// ============ OAuth2 Commands ============

/// OAuth2 connection parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConnectionParams {
    /// Provider: "google_drive", "dropbox", "onedrive", "zoho_workdrive", etc.
    pub provider: String,
    /// OAuth2 client ID (from app registration)
    pub client_id: String,
    /// OAuth2 client secret (from app registration)
    pub client_secret: String,
    /// Region for multi-region providers (Zoho: "us", "eu", "in", "au", "jp", "ca", "sa")
    #[serde(default = "default_region")]
    pub region: String,
    /// Server profile identifier owning these tokens. When supplied the vault
    /// stores OAuth tokens under `oauth_<provider>_<profile_id>`, so two
    /// profiles backed by distinct cloud accounts coexist on the same device.
    /// When omitted the legacy singleton key is used. Issue #214.
    #[serde(default)]
    pub profile_id: String,
    /// #360: frontend-generated token identifying this connect attempt. When
    /// present, `oauth2_full_auth` / `oauth2_connect` register a cancellation
    /// token under it so an Esc / "still connecting" Cancel can abort them via
    /// `cancel_connection`. Absent for callers that opt out.
    #[serde(default, alias = "connectToken")]
    pub connect_token: Option<String>,
}

fn default_region() -> String {
    "us".to_string()
}

/// OAuth2 flow state
#[derive(Debug, Clone, Serialize)]
pub struct OAuthFlowStarted {
    /// URL to open in browser
    pub auth_url: String,
    /// State parameter for verification
    pub state: String,
}

/// Start OAuth2 authentication flow
/// Returns the authorization URL to open in browser
#[tauri::command]
pub async fn oauth2_start_auth(params: OAuthConnectionParams) -> Result<OAuthFlowStarted, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};

    info!("Starting OAuth2 flow for {}", params.provider);

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos(&params.client_id, &params.client_secret)
        }
        "dropbox" => OAuthConfig::dropbox(&params.client_id, &params.client_secret),
        "onedrive" | "microsoft" => OAuthConfig::onedrive(&params.client_id, &params.client_secret),
        "box" => OAuthConfig::box_cloud(&params.client_id, &params.client_secret),
        "pcloud" => OAuthConfig::pcloud(&params.client_id, &params.client_secret, &params.region),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
            OAuthConfig::zoho(&params.client_id, &params.client_secret, &params.region)
        }
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    }
    .with_profile_id(&params.profile_id);

    let manager = OAuth2Manager::new();
    let (auth_url, state) = manager
        .start_auth_flow(&config)
        .await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
    }

    Ok(OAuthFlowStarted { auth_url, state })
}

/// Complete OAuth2 authentication with the authorization code
#[tauri::command]
pub async fn oauth2_complete_auth(
    params: OAuthConnectionParams,
    code: String,
    state: String,
) -> Result<String, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};

    info!("Completing OAuth2 flow for {}", params.provider);

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos(&params.client_id, &params.client_secret)
        }
        "dropbox" => OAuthConfig::dropbox(&params.client_id, &params.client_secret),
        "onedrive" | "microsoft" => OAuthConfig::onedrive(&params.client_id, &params.client_secret),
        "box" => OAuthConfig::box_cloud(&params.client_id, &params.client_secret),
        "pcloud" => OAuthConfig::pcloud(&params.client_id, &params.client_secret, &params.region),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
            OAuthConfig::zoho(&params.client_id, &params.client_secret, &params.region)
        }
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    }
    .with_profile_id(&params.profile_id);

    let manager = OAuth2Manager::new();
    manager
        .complete_auth_flow(&config, &code, &state)
        .await
        .map_err(|e| format!("Failed to complete OAuth flow: {}", e))?;

    Ok("Authentication successful".to_string())
}

/// OAuth2 connection result with display name and account email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2ConnectResult {
    pub display_name: String,
    pub account_email: Option<String>,
}

/// Connect to an OAuth2-based cloud provider (after authentication)
#[tauri::command]
pub async fn oauth2_connect(
    state: State<'_, ProviderState>,
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    params: OAuthConnectionParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{
        dropbox::DropboxConfig, google_drive::GoogleDriveConfig, google_photos::GooglePhotosConfig,
        onedrive::OneDriveConfig, types::BoxConfig, types::PCloudConfig,
        zoho_workdrive::ZohoWorkdriveConfig, BoxProvider, DropboxProvider, GoogleDriveProvider,
        GooglePhotosProvider, OneDriveProvider, PCloudProvider, ZohoWorkdriveProvider,
    };

    info!("Connecting to OAuth2 provider: {}", params.provider);

    // #360: build + connect the provider under the connect token so an Esc /
    // "still connecting" Cancel aborts a slow OAuth API connect (the My Servers
    // OAuth path previously could not be cancelled at all).
    let build_provider = async {
        let provider: Box<dyn StorageProvider> = match params.provider.to_lowercase().as_str() {
            "google_drive" | "googledrive" | "google" => {
                let config = GoogleDriveConfig::new(&params.client_id, &params.client_secret);
                let mut p = GoogleDriveProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("Google Drive connection failed: {}", e))?;
                Box::new(p)
            }
            "googlephotos" | "google_photos" => {
                let config = GooglePhotosConfig::new(&params.client_id, &params.client_secret);
                let mut p = GooglePhotosProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("Google Photos connection failed: {}", e))?;
                Box::new(p)
            }
            "dropbox" => {
                let config = DropboxConfig::new(&params.client_id, &params.client_secret);
                let mut p = DropboxProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("Dropbox connection failed: {}", e))?;
                Box::new(p)
            }
            "onedrive" | "microsoft" => {
                let config = OneDriveConfig::new(&params.client_id, &params.client_secret);
                let mut p = OneDriveProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("OneDrive connection failed: {}", e))?;
                Box::new(p)
            }
            "box" => {
                let config = BoxConfig {
                    client_id: params.client_id.clone(),
                    client_secret: params.client_secret.clone(),
                };
                let mut p = BoxProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("Box connection failed: {}", e))?;
                Box::new(p)
            }
            "pcloud" => {
                // pCloud tokens are region-locked: always prefer vault-stored region
                // (detected during token exchange) over serde default "us"
                let region = crate::credential_store::CredentialStore::from_cache()
                    .and_then(|store| store.get("oauth_pcloud_region").ok())
                    .unwrap_or(params.region.clone());
                let config = PCloudConfig {
                    client_id: params.client_id.clone(),
                    client_secret: params.client_secret.clone(),
                    region,
                };
                let mut p = PCloudProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("pCloud connection failed: {}", e))?;
                Box::new(p)
            }
            "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
                let config = ZohoWorkdriveConfig::new(
                    &params.client_id,
                    &params.client_secret,
                    &params.region,
                );
                let mut p = ZohoWorkdriveProvider::new(config).with_profile_id(&params.profile_id);
                p.connect()
                    .await
                    .map_err(|e| format!("Zoho WorkDrive connection failed: {}", e))?;
                Box::new(p)
            }
            "yandexdisk" | "yandex_disk" | "yandex" => {
                // Yandex Disk OAuth: retrieve token from stored OAuth tokens
                use crate::providers::{OAuth2Manager, OAuthProvider};
                let manager = OAuth2Manager::new();
                let tokens = manager
                    .load_tokens(OAuthProvider::YandexDisk, &params.profile_id)
                    .map_err(|e| format!("No Yandex Disk tokens found: {}", e))?;
                let mut p =
                    crate::providers::YandexDiskProvider::new(tokens.access_token.clone(), None);
                p.connect()
                    .await
                    .map_err(|e| format!("Yandex Disk connection failed: {}", e))?;
                Box::new(p)
            }
            other => return Err(format!("Unknown OAuth2 provider: {}", other)),
        };
        Ok::<Box<dyn StorageProvider>, String>(provider)
    };

    let provider = run_cancellable_connect(
        &cancel_registry,
        params.connect_token.as_deref(),
        build_provider,
    )
    .await?;

    let display_name = provider.display_name();
    let account_email = provider.account_email();

    // Store provider
    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(provider);
    // Fresh connection carries no crypt overlay: reset both flags.
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);

    info!(
        "Connected to {} ({})",
        display_name,
        account_email.as_deref().unwrap_or("no email")
    );
    Ok(OAuth2ConnectResult {
        display_name,
        account_email,
    })
}

/// Full OAuth2 authentication flow - starts server, opens browser, waits for callback, completes auth
#[tauri::command]
pub async fn oauth2_full_auth(
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    params: OAuthConnectionParams,
) -> Result<String, String> {
    use crate::providers::{
        oauth2::{bind_callback_listener, bind_callback_listener_on_port, wait_for_callback},
        OAuth2Manager, OAuthConfig,
    };

    info!("Starting full OAuth2 flow for {}", params.provider);

    // #360: register the connect token so an Esc / "still connecting" Cancel can
    // abort the browser-callback wait below (the longest phase of the flow).
    // The guard de-registers it on every exit path.
    let cancel_token = params
        .connect_token
        .as_deref()
        .map(|key| cancel_registry.register(key));
    let _cancel_guard = params
        .connect_token
        .as_deref()
        .map(|key| ConnectTokenGuard::new(&cancel_registry, key.to_string()));

    // Some providers require exact redirect_uri matching, so use a fixed port
    let fixed_port: u16 = match params.provider.to_lowercase().as_str() {
        "box" => 9484,
        "dropbox" => 17548,
        "onedrive" | "microsoft" => 27154,
        "pcloud" => 17384,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => 18765,
        "yandexdisk" | "yandex_disk" | "yandex" => 19847,
        _ => 0,
    };

    // Bind callback listener (fixed port for Box, ephemeral for others)
    let (listener, port) = if fixed_port > 0 {
        bind_callback_listener_on_port(fixed_port).await
    } else {
        bind_callback_listener().await
    }
    .map_err(|e| format!("Failed to bind callback listener: {}", e))?;

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google_with_port(&params.client_id, &params.client_secret, port)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos_with_port(&params.client_id, &params.client_secret, port)
        }
        "dropbox" => OAuthConfig::dropbox_with_port(&params.client_id, &params.client_secret, port),
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive_with_port(&params.client_id, &params.client_secret, port)
        }
        "box" => OAuthConfig::box_cloud_with_port(&params.client_id, &params.client_secret, port),
        "pcloud" => OAuthConfig::pcloud_with_port(
            &params.client_id,
            &params.client_secret,
            port,
            &params.region,
        ),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthConfig::zoho_with_port(
            &params.client_id,
            &params.client_secret,
            port,
            &params.region,
        ),
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk_with_port(&params.client_id, &params.client_secret, port)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    }
    .with_profile_id(&params.profile_id);

    // Create manager ONCE and keep it for the entire flow
    let manager = OAuth2Manager::new();

    // Generate auth URL with the dynamic port in redirect_uri
    let (auth_url, expected_state) = manager
        .start_auth_flow(&config)
        .await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Start waiting for callback in background. AbortOnDrop ensures the task
    // (and the bound TCP listener) is aborted on ANY early-return path below -
    // raw tokio::spawn would detach the handle and leak the port until process
    // restart if `open::that` fails or the 5-minute timeout fires.
    let mut callback_task = AbortOnDrop::spawn(async move { wait_for_callback(listener).await });

    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
        return Err(format!(
            "Could not open browser: {}. Please open this URL manually: {}",
            e, auth_url
        ));
    }

    info!("Browser opened, waiting for callback...");

    // Wait for callback (with timeout). tokio::select! keeps ownership of the
    // guard inside the macro; on timeout the guard drops at function exit and
    // aborts the task, releasing the port.
    let callback_result = tokio::select! {
        res = callback_task.wait() => res
            .map_err(|e| format!("Callback server error: {}", e))?
            .map_err(|e| format!("Callback error: {}", e))?,
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(300)) => {
            return Err("OAuth timeout: no response within 5 minutes".to_string());
        }
        // #360: Esc / Cancel aborts the wait; dropping callback_task (AbortOnDrop)
        // releases the bound listener port.
        _ = async {
            match cancel_token.as_ref() {
                Some(token) => token.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            return Err(CONNECT_CANCELLED.to_string());
        }
    };

    let (code, state) = callback_result;

    // Verify state matches
    if state != expected_state {
        return Err("OAuth state mismatch - possible CSRF attack".to_string());
    }

    info!("Callback received, completing authentication...");

    // pCloud uses non-standard token exchange (GET, no PKCE, no expiry)
    if params.provider.to_lowercase() == "pcloud" {
        pcloud_exchange_code(&config, &code)
            .await
            .map_err(|e| format!("Failed to exchange code for tokens: {}", e))?;
    } else {
        // Standard OAuth2 flow using the SAME manager instance (which has the PKCE verifier stored)
        manager
            .complete_auth_flow(&config, &code, &expected_state)
            .await
            .map_err(|e| format!("Failed to exchange code for tokens: {}", e))?;
    }

    info!(
        "OAuth2 authentication completed successfully for {}",
        params.provider
    );
    Ok("Authentication successful! You can now connect.".to_string())
}

/// pCloud uses a non-standard OAuth2 token exchange:
/// - GET request (not POST)
/// - No PKCE support
/// - Tokens never expire (no refresh_token or expires_in)
/// - Response: {"access_token": "...", "userid": ..., "token_type": "bearer", "result": 0}
/// - Region-aware: tries configured endpoint first, then fallback (US↔EU)
async fn pcloud_exchange_code(
    config: &crate::providers::OAuthConfig,
    code: &str,
) -> Result<(), crate::providers::ProviderError> {
    use crate::providers::{oauth2::StoredTokens, OAuth2Manager, ProviderError};

    let client_secret = config.client_secret.as_deref().ok_or_else(|| {
        ProviderError::InvalidConfig("Missing client_secret for pCloud".to_string())
    })?;

    // pCloud accounts are region-locked (US=api.pcloud.com, EU=eapi.pcloud.com).
    // The auth code is only valid on the account's region endpoint.
    // Try configured endpoint first, fallback to the other region.
    let endpoints = if config.token_url.contains("eapi.pcloud.com") {
        vec![
            "https://eapi.pcloud.com/oauth2_token",
            "https://api.pcloud.com/oauth2_token",
        ]
    } else {
        vec![
            "https://api.pcloud.com/oauth2_token",
            "https://eapi.pcloud.com/oauth2_token",
        ]
    };

    let http = reqwest::Client::new();
    let mut last_error = String::new();

    for endpoint in &endpoints {
        let url = format!(
            "{}?client_id={}&client_secret={}&code={}",
            endpoint,
            urlencoding::encode(&config.client_id),
            urlencoding::encode(client_secret),
            urlencoding::encode(code),
        );

        let resp = match http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("HTTP error on {}: {}", endpoint, e);
                continue;
            }
        };

        if !resp.status().is_success() {
            last_error = format!("HTTP {} from {}", resp.status(), endpoint);
            continue;
        }

        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_error = format!("Read error from {}: {}", endpoint, e);
                continue;
            }
        };

        let body: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                last_error = format!("Parse error from {}: {}", endpoint, e);
                continue;
            }
        };

        // Check for pCloud error response: error 2012 means wrong region, try next
        if let Some(result) = body["result"].as_i64() {
            if result != 0 {
                let error_msg = body["error"].as_str().unwrap_or("Unknown error");
                last_error = format!("pCloud error {} ({}): {}", result, endpoint, error_msg);
                continue;
            }
        }

        let access_token = body["access_token"].as_str().ok_or_else(|| {
            ProviderError::AuthenticationFailed("pCloud: missing access_token".to_string())
        })?;

        let tokens = StoredTokens {
            access_token: access_token.to_string(),
            refresh_token: None, // pCloud tokens don't expire
            expires_at: None,
            token_type: "Bearer".to_string(),
            scopes: vec![],
        };

        let manager = OAuth2Manager::new();
        manager.store_tokens(config.provider, config.profile_id(), &tokens)?;

        // Persist detected region so oauth2_connect uses the correct API endpoint
        let region = if endpoint.contains("eapi") {
            "eu"
        } else {
            "us"
        };
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            let _ = store.store("oauth_pcloud_region", region);
        }

        info!(
            "pCloud OAuth tokens obtained via {} ({}, permanent, no expiry)",
            endpoint,
            region.to_uppercase()
        );
        return Ok(());
    }

    Err(ProviderError::AuthenticationFailed(format!(
        "pCloud token exchange failed on all endpoints: {}",
        last_error
    )))
}

/// Check if OAuth2 tokens exist for a provider. When `profile_id` is supplied
/// the per-profile vault key is consulted (with legacy fallback honoured by
/// `OAuth2Manager::load_tokens`), otherwise the historic singleton key is
/// used. Issue #214.
#[tauri::command]
pub async fn oauth2_has_tokens(
    provider: String,
    profile_id: Option<String>,
) -> Result<bool, String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};

    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "googlephotos" | "google_photos" => OAuthProvider::GooglePhotos,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthProvider::ZohoWorkdrive,
        "yandexdisk" | "yandex_disk" | "yandex" => OAuthProvider::YandexDisk,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let pid = profile_id.unwrap_or_default();
    let manager = OAuth2Manager::new();
    Ok(manager.has_tokens(oauth_provider, &pid))
}

/// Clear OAuth2 tokens for a provider (logout). When `profile_id` is supplied
/// only that profile's tokens are removed; otherwise the legacy singleton key
/// is targeted. Issue #214.
#[tauri::command]
pub async fn oauth2_logout(provider: String, profile_id: Option<String>) -> Result<(), String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};

    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "googlephotos" | "google_photos" => OAuthProvider::GooglePhotos,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthProvider::ZohoWorkdrive,
        "yandexdisk" | "yandex_disk" | "yandex" => OAuthProvider::YandexDisk,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let pid = profile_id.unwrap_or_default();
    let manager = OAuth2Manager::new();
    manager
        .clear_tokens(oauth_provider, &pid)
        .map_err(|e| format!("Failed to clear tokens: {}", e))?;

    info!("Logged out from {}", provider);
    Ok(())
}

/// Create a shareable link for a file using the OAuth provider's native sharing API
#[tauri::command]
pub async fn provider_create_share_link(
    state: State<'_, ProviderState>,
    path: String,
    expires_in_secs: Option<u64>,
    password: Option<String>,
    permissions: Option<String>,
) -> Result<ShareLinkResult, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_share_links() {
        return Err(format!(
            "{} does not support native share links",
            provider.provider_type()
        ));
    }

    let options = ShareLinkOptions {
        expires_in_secs,
        password,
        permissions,
    };

    let result = provider
        .create_share_link(&path, options)
        .await
        .map_err(|e| format!("Failed to create share link: {}", e))?;

    info!("Created share link for {}: {}", path, result.url);
    Ok(result)
}

/// Query share link capabilities for the current provider
#[tauri::command]
pub async fn provider_share_link_capabilities(
    state: State<'_, ProviderState>,
) -> Result<ShareLinkCapabilities, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    Ok(provider.share_link_capabilities())
}

/// Remove a share/export link for a file or folder
#[tauri::command]
pub async fn provider_remove_share_link(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .remove_share_link(&path)
        .await
        .map_err(|e| format!("Failed to remove share link: {}", e))?;

    info!("Removed share link for {}", path);
    Ok(())
}

/// List existing share links for a file or folder
#[tauri::command]
pub async fn provider_list_share_links(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<crate::providers::ShareLinkInfo>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .list_share_links(&path)
        .await
        .map_err(|e| format!("Failed to list share links: {}", e))
}

/// Import a file/folder from a public link into the account
#[tauri::command]
pub async fn provider_import_link(
    state: State<'_, ProviderState>,
    link: String,
    dest: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_import_link() {
        return Err(format!(
            "{} does not support importing from links",
            provider.provider_type()
        ));
    }

    provider
        .import_link(&link, &dest)
        .await
        .map_err(|e| format!("Failed to import link: {}", e))?;

    info!("Imported link to {}", dest);
    Ok(())
}

/// Get storage quota information (used/total/free bytes)
#[tauri::command]
pub async fn provider_storage_info(state: State<'_, ProviderState>) -> Result<StorageInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .storage_info()
        .await
        .map_err(|e| format!("Failed to get storage info: {}", e))
}

/// Query MEGAcmd quota directly through `mega-df`.
#[tauri::command]
pub async fn mega_df_query(profile_id: String) -> Result<(u64, u64), String> {
    let _ = profile_id;
    crate::providers::mega_df::mega_df_query()
        .await
        .map(|(used, total, _versioning)| (used, total))
        .map_err(|e| format!("Failed to query mega-df: {}", e))
}

/// Resolve the local MEGAcmd WebDAV bridge URL via `mega-webdav /` so the
/// Endpoint URL field can auto-fill instead of the user copying it from the
/// MEGAcmd terminal (#215).
#[tauri::command]
pub async fn mega_webdav_url(
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    connect_token: Option<String>,
) -> Result<String, String> {
    // #360: this `mega-webdav /` blocks (up to its 30s timeout) when the MEGAcmd
    // daemon is not running. The My Servers connect runs it as a preflight, so
    // make it cancellable: an Esc / "still connecting" Cancel drops the future
    // (which, with kill_on_drop, also tears down the blocked subprocess).
    run_cancellable_connect(&cancel_registry, connect_token.as_deref(), async {
        crate::providers::mega_df::mega_webdav_url_query()
            .await
            .map_err(|e| format!("{}", e))
    })
    .await
}

/// Get disk usage for a path in bytes
#[tauri::command]
pub async fn provider_disk_usage(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .disk_usage(&path)
        .await
        .map_err(|e| format!("Failed to get disk usage: {}", e))
}

/// Search for files matching a pattern under the given path
#[tauri::command]
pub async fn provider_find(
    state: State<'_, ProviderState>,
    path: String,
    pattern: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_find() {
        return Err(format!(
            "{} does not support remote search",
            provider.provider_type()
        ));
    }

    provider
        .find(&path, &pattern)
        .await
        .map_err(|e| format!("Search failed: {}", e))
}

/// Set transfer speed limits (KB/s, 0 = unlimited)
#[tauri::command]
pub async fn provider_set_speed_limit(
    state: State<'_, ProviderState>,
    upload_kb: u64,
    download_kb: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .set_speed_limit(upload_kb, download_kb)
        .await
        .map_err(|e| format!("Failed to set speed limit: {}", e))
}

/// Get current transfer speed limits (upload_kb, download_kb) in KB/s
#[tauri::command]
pub async fn provider_get_speed_limit(
    state: State<'_, ProviderState>,
) -> Result<(u64, u64), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .get_speed_limit()
        .await
        .map_err(|e| format!("Failed to get speed limit: {}", e))
}

/// Check if the current provider supports resume transfers
#[tauri::command]
pub async fn provider_supports_resume(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_resume())
}

/// Resume a download from a given byte offset
#[tauri::command]
pub async fn provider_resume_download(
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    provider
        .resume_download(&remote_path, &local_path, offset, None)
        .await
        .map_err(|e| format!("Resume download failed: {}", e))?;

    Ok(format!("Resume download completed: {}", remote_path))
}

/// Resume an upload from a given byte offset
#[tauri::command]
pub async fn provider_resume_upload(
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .resume_upload(&local_path, &remote_path, offset, None)
        .await
        .map_err(|e| format!("Resume upload failed: {}", e))?;

    Ok(format!("Resume upload completed: {}", remote_path))
}

// --- File Versions ---

#[tauri::command]
pub async fn provider_supports_versions(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_versions())
}

#[tauri::command]
pub async fn provider_list_versions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<FileVersion>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .list_versions(&path)
        .await
        .map_err(|e| format!("List versions failed: {}", e))
}

#[tauri::command]
pub async fn provider_download_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
    local_path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .download_version(&path, &version_id, &local_path)
        .await
        .map_err(|e| format!("Download version failed: {}", e))?;
    Ok(format!("Downloaded version {} of {}", version_id, path))
}

#[tauri::command]
pub async fn provider_restore_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .restore_version(&path, &version_id)
        .await
        .map_err(|e| format!("Restore version failed: {}", e))
}

/// Permanently delete (purge) one version or delete marker of a file. Routes
/// through the trait, so a live crypt overlay maps the plaintext path to
/// ciphertext (mirroring `provider_restore_version`). For a soft-deleted S3
/// object, purging its delete marker's `version_id` "undeletes" it; purging a
/// content version removes that version for good. Irreversible.
#[tauri::command]
pub async fn provider_delete_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .delete_version(&path, &version_id)
        .await
        .map_err(|e| format!("Delete version failed: {}", e))
}

// --- File Locking ---

#[tauri::command]
pub async fn provider_supports_locking(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_locking())
}

#[tauri::command]
pub async fn provider_lock_file(
    state: State<'_, ProviderState>,
    path: String,
    timeout: u64,
) -> Result<LockInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .lock_file(&path, timeout)
        .await
        .map_err(|e| format!("Lock failed: {}", e))
}

#[tauri::command]
pub async fn provider_unlock_file(
    state: State<'_, ProviderState>,
    path: String,
    lock_token: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .unlock_file(&path, &lock_token)
        .await
        .map_err(|e| format!("Unlock failed: {}", e))
}

// --- Thumbnails ---

#[tauri::command]
pub async fn provider_supports_thumbnails(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_thumbnails())
}

#[tauri::command]
pub async fn provider_get_thumbnail(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .get_thumbnail(&path)
        .await
        .map_err(|e| format!("Get thumbnail failed: {}", e))
}

// --- Permissions / Advanced Sharing ---

#[tauri::command]
pub async fn provider_supports_permissions(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_permissions())
}

#[tauri::command]
pub async fn provider_list_permissions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<SharePermission>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .list_permissions(&path)
        .await
        .map_err(|e| format!("List permissions failed: {}", e))
}

#[tauri::command]
pub async fn provider_add_permission(
    state: State<'_, ProviderState>,
    path: String,
    role: String,
    target_type: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let perm = SharePermission {
        role,
        target_type,
        target,
    };
    provider
        .add_permission(&path, &perm)
        .await
        .map_err(|e| format!("Add permission failed: {}", e))
}

#[tauri::command]
pub async fn provider_remove_permission(
    state: State<'_, ProviderState>,
    path: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .remove_permission(&path, &target)
        .await
        .map_err(|e| format!("Remove permission failed: {}", e))
}

/// GUI adapter that maps the `AppHandle`-free scan observer 1:1 onto the
/// existing `sync_scan_progress` event. The clone-pool / locked scan paths
/// hold only `&dyn DagObserver`; this adapter (which does own the
/// `AppHandle`) restores the moving "remote" counter without threading the
/// `AppHandle` into the scan module. The payload shape is byte-identical to
/// the manual emits in `provider_compare_directories`, so the frontend is
/// unchanged.
struct ScanProgressEmitter {
    app: AppHandle,
}

impl crate::transfer_dag::DagObserver for ScanProgressEmitter {
    fn on_scan_progress(&self, scanned: usize, _in_flight: usize) {
        let _ = self.app.emit(
            "sync_scan_progress",
            serde_json::json!({
                "phase": "remote", "files_found": scanned,
            }),
        );
    }
}

async fn decrypt_remote_file_map_for_compare(
    kind: Option<&str>,
    vault_id: &str,
    rclone_state: &crate::rclone_crypt::RcloneCryptState,
    aerocrypt_state: &crate::aerocrypt_provider::AeroCryptState,
    entries: HashMap<String, crate::sync::FileInfo>,
) -> Result<HashMap<String, crate::sync::FileInfo>, String> {
    match kind {
        Some("rclone-crypt") => {
            let vaults = rclone_state.vaults.lock().await;
            let keys = vaults
                .get(vault_id)
                .ok_or_else(|| "Crypt vault is not unlocked".to_string())?;
            Ok(normalize_rclone_remote_files_for_compare(keys, entries))
        }
        Some("aerocrypt") => {
            let vaults = aerocrypt_state.vaults.lock().await;
            let keys = vaults
                .get(vault_id)
                .ok_or_else(|| "Crypt vault is not unlocked".to_string())?;
            Ok(normalize_aerocrypt_remote_files_for_compare(
                &keys.master_key,
                entries,
            ))
        }
        Some(_) => Err("Unsupported crypt overlay kind for compare".to_string()),
        None => Err("Missing crypt overlay kind for compare".to_string()),
    }
}

fn normalize_rclone_remote_files_for_compare(
    keys: &crate::rclone_crypt::RcloneCryptKeys,
    entries: HashMap<String, crate::sync::FileInfo>,
) -> HashMap<String, crate::sync::FileInfo> {
    let mut out = HashMap::with_capacity(entries.len());
    for (rel_path, mut info) in entries {
        let Some(plain_rel_path) = decrypt_rel_rclone(keys, &rel_path) else {
            continue;
        };
        info.name = basename_from_rel_path(&plain_rel_path);
        if !info.is_dir {
            info.size = rclone_decrypted_size(info.size);
        }
        info.checksum = None;
        info.checksum_alg = None;
        out.insert(plain_rel_path, info);
    }
    out
}

fn normalize_aerocrypt_remote_files_for_compare(
    master_key: &[u8; 32],
    entries: HashMap<String, crate::sync::FileInfo>,
) -> HashMap<String, crate::sync::FileInfo> {
    let mut out = HashMap::with_capacity(entries.len());
    for (rel_path, mut info) in entries {
        let Some(plain_rel_path) = decrypt_rel_aerocrypt(master_key, &rel_path) else {
            continue;
        };
        info.name = basename_from_rel_path(&plain_rel_path);
        // AeroCrypt content-size mapping is deliberately deferred: the native
        // overlay has a versioned container format, so Compare is name-aware
        // here but size-policy matches may still need the follow-up decoder.
        info.checksum = None;
        info.checksum_alg = None;
        out.insert(plain_rel_path, info);
    }
    out
}

// Single source of truth for the crypt-compare crypto lives in
// `crate::crypt_compare`; the GUI's FileInfo-map normalizers above reuse the
// pure rel-path decrypt and size mapping so the CLI / MCP `RemoteEntry` path and
// this GUI path can never drift.
use crate::crypt_compare::{decrypt_rel_aerocrypt, decrypt_rel_rclone, rclone_decrypted_size};

fn basename_from_rel_path(rel_path: &str) -> String {
    rel_path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(rel_path)
        .to_string()
}

/// Compare local and remote directories using the StorageProvider trait.
/// Works with all protocols (SFTP, WebDAV, S3, Google Drive, etc.)
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_compare_directories(
    app: AppHandle,
    state: State<'_, ProviderState>,
    rclone_state: State<'_, crate::rclone_crypt::RcloneCryptState>,
    aerocrypt_state: State<'_, crate::aerocrypt_provider::AeroCryptState>,
    local_path: String,
    remote_path: String,
    crypt_vault_id: Option<String>,
    crypt_kind: Option<String>,
    options: Option<crate::sync::CompareOptions>,
) -> Result<Vec<crate::sync::FileComparison>, String> {
    use crate::sync::{
        build_comparison_results_with_index, load_sync_index, should_exclude, FileInfo,
    };

    let mut options = options.unwrap_or_default();
    crate::sync::apply_error_correction_excludes(&mut options);
    let crypt_vault_id = crypt_vault_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    let crypt_kind = crypt_kind
        .map(|kind| kind.trim().to_string())
        .filter(|kind| !kind.is_empty());
    let crypt_compare_active = crypt_vault_id.is_some();
    if crypt_compare_active {
        options.compare_checksum = false;
        options.strict_checksum = false;
    }

    info!(
        "Provider compare: local={}, remote={}",
        local_path, remote_path
    );

    // Reset the provider cancel flag: takes ownership for this compare run.
    // The user's next Cancel click flips it back to true and the scan stops.
    state
        .cancel_flag
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "local", "files_found": 0,
        }),
    );

    // Get local files (reuse the same logic from lib.rs).
    // Pass the AppHandle so the scan emits throttled progress events -
    // otherwise large trees (e.g. a home directory) look like a stall.
    let (local_files, local_scan) = crate::get_local_files_recursive_checked(
        &local_path,
        &local_path,
        &options.exclude_patterns,
        options.compare_checksum,
        Some(&state.cancel_flag),
        Some(&app),
    )
    .await
    .map_err(|e| format!("Failed to scan local directory: {}", e))?;

    // CLAUDE-AV-B3-13: same guard as `compare_directories`. This path shares the
    // walker, so a half-read local tree would reach the Compare tab as a set of
    // "remote only" rows that a Mirror preset deletes on the provider.
    crate::ensure_scan_complete("local", &local_path, &local_scan)?;

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "remote", "files_found": local_files.len(),
        }),
    );

    // Get remote files via provider. Clone-backed providers use explicit
    // checker/list leases; legacy providers keep the per-directory lock path.
    let mut remote_files: HashMap<String, FileInfo> = HashMap::new();

    // First check we're connected
    {
        let provider_lock = state.provider.lock().await;
        if provider_lock.is_none() {
            return Err("Not connected to any provider".to_string());
        }
    }

    let list_model = resolve_provider_list_session_model(&state.provider, 8).await;
    if list_model.is_clone_pool() {
        use crate::sync_core::scan::{scan_remote_tree_with_provider_lock_checked, ScanOptions};

        if state.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "{}: compare cancelled by user before the remote tree was fully listed.",
                crate::SCAN_INCOMPLETE_MARKER
            ));
        }

        let scan_options = ScanOptions {
            exclude_patterns: options.exclude_patterns.clone(),
            compute_remote_checksum: options.compare_checksum && !crypt_compare_active,
            disable_recursive_fastpath: crypt_compare_active,
            ..Default::default()
        };
        let scan_observer = ScanProgressEmitter { app: app.clone() };
        let (remote_entries, remote_scan) = scan_remote_tree_with_provider_lock_checked(
            state.provider.clone(),
            &remote_path,
            &scan_options,
            &list_model,
            Some(state.cancel_flag.clone()),
            Some(&scan_observer),
        )
        .await;

        // CLAUDE-AV-B3-13: a directory the provider refused to list contributes
        // no rows, which is indistinguishable from the user having deleted its
        // contents. Running the preset remote-to-local then deletes the local
        // copies of files that are still sitting on the provider.
        crate::ensure_scan_complete("remote", &remote_path, &remote_scan)?;

        // The parallel scan stops early when the cancel flag is raised but
        // returns the partial results it gathered. Surface the same
        // user-facing error the per-directory legacy path returned so the UI
        // does not treat a cancelled compare as a completed one.
        if state.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "{}: compare cancelled by user before the remote tree was fully listed.",
                crate::SCAN_INCOMPLETE_MARKER
            ));
        }

        for entry in remote_entries {
            let relative_path = entry.rel_path;
            let name = std::path::Path::new(&relative_path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| relative_path.clone());
            let remote_abs_path = if remote_path.ends_with('/') {
                format!("{}{}", remote_path, relative_path.trim_start_matches('/'))
            } else {
                format!("{}/{}", remote_path, relative_path.trim_start_matches('/'))
            };
            let modified = entry.mtime.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok()
                    .or_else(|| {
                        let clean = s.strip_suffix('Z').unwrap_or(&s);
                        chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M")
                            .or_else(|_| {
                                chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M:%S")
                            })
                            .ok()
                            .map(|dt| {
                                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                    dt,
                                    chrono::Utc,
                                )
                            })
                    })
            });

            let file_info = FileInfo {
                name,
                path: remote_abs_path,
                size: entry.size,
                modified,
                is_dir: false,
                checksum_alg: entry.checksum_alg,
                checksum: entry.checksum_hex,
            };

            remote_files.insert(relative_path, file_info);
        }

        let _ = app.emit(
            "sync_scan_progress",
            serde_json::json!({
                "phase": "remote",
                "files_found": local_files.len() + remote_files.len(),
            }),
        );
    } else {
        let mut dirs_to_process = vec![remote_path.clone()];
        while let Some(current_dir) = dirs_to_process.pop() {
            // Abort the remote scan if the user cancelled from the UI.
            // Without this, the walk keeps listing directories until the tree is
            // exhausted, which can look like a runaway scan on large providers.
            if state.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(format!(
                    "{}: compare cancelled by user before the remote tree was fully listed.",
                    crate::SCAN_INCOMPLETE_MARKER
                ));
            }

            // Lock provider only for this single list operation, then release
            let entries = {
                let mut provider_lock = state.provider.lock().await;
                let provider = provider_lock
                    .as_mut()
                    .ok_or("Not connected to any provider")?;
                provider.list(&current_dir).await.map_err(|e| {
                    // CLAUDE-AV-B3-13: a directory that would not list leaves
                    // its files out of the compare, where they read as deleted.
                    // Mark it so the UI fails closed instead of falling back to
                    // a flat plan built from the panel listings.
                    format!(
                        "{}: failed to list {}: {}",
                        crate::SCAN_INCOMPLETE_MARKER,
                        current_dir,
                        e
                    )
                })?
            };

            for entry in entries {
                if entry.name == "." || entry.name == ".." {
                    continue;
                }

                let relative_path = if current_dir == remote_path {
                    entry.name.clone()
                } else {
                    let rel_dir = current_dir
                        .strip_prefix(&remote_path)
                        .unwrap_or(&current_dir);
                    let rel_dir = rel_dir.trim_start_matches('/');
                    if rel_dir.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}/{}", rel_dir, entry.name)
                    }
                };

                if should_exclude(&relative_path, &options.exclude_patterns) {
                    continue;
                }

                let modified = entry.modified.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .ok()
                        .or_else(|| {
                            let clean = s.strip_suffix('Z').unwrap_or(&s);
                            chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M")
                                .or_else(|_| {
                                    chrono::NaiveDateTime::parse_from_str(
                                        clean,
                                        "%Y-%m-%d %H:%M:%S",
                                    )
                                })
                                .ok()
                                .map(|dt| {
                                    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                        dt,
                                        chrono::Utc,
                                    )
                                })
                        })
                });

                let file_info = FileInfo {
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                    size: entry.size,
                    modified,
                    is_dir: entry.is_dir,
                    checksum_alg: None,
                    checksum: None,
                };

                remote_files.insert(relative_path, file_info);

                if entry.is_dir {
                    let sub_path = if current_dir.ends_with('/') {
                        format!("{}{}", current_dir, entry.name)
                    } else {
                        format!("{}/{}", current_dir, entry.name)
                    };
                    dirs_to_process.push(sub_path);
                }
            }

            let _ = app.emit(
                "sync_scan_progress",
                serde_json::json!({
                    "phase": "remote",
                    "files_found": local_files.len() + remote_files.len(),
                }),
            );
        }
    }

    if let Some(vault_id) = crypt_vault_id.as_deref() {
        let raw_len = remote_files.len();
        remote_files = decrypt_remote_file_map_for_compare(
            crypt_kind.as_deref(),
            vault_id,
            &rclone_state,
            &aerocrypt_state,
            remote_files,
        )
        .await?;
        // rclone-crypt has no config MAC: a wrong overlay password derives
        // valid-shaped keys that decrypt nothing, so a non-empty remote that
        // normalizes to zero rows is a wrong-key signal. Fail closed rather
        // than re-flag the whole tree as missing (the #364 symptom).
        if crypt_kind.as_deref() == Some("rclone-crypt") && raw_len > 0 && remote_files.is_empty() {
            return Err(
                "Crypt overlay decrypted no remote entries: wrong overlay password or non-crypt remote."
                    .to_string(),
            );
        }
    }

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "comparing",
            "files_found": local_files.len() + remote_files.len(),
        }),
    );

    let index = load_sync_index(&local_path, &remote_path).ok().flatten();
    let results =
        build_comparison_results_with_index(local_files, remote_files, &options, index.as_ref());
    info!(
        "Provider compare complete: {} differences found (index: {})",
        results.len(),
        if index.is_some() { "used" } else { "none" }
    );

    Ok(results)
}

// ============ 4shared OAuth 1.0 Commands ============

/// Parameters for 4shared OAuth 1.0 authentication
#[derive(Debug, Clone, Deserialize)]
pub struct FourSharedAuthParams {
    pub consumer_key: String,
    pub consumer_secret: String,
    /// #360: connect token for Esc / "still connecting" Cancel (see
    /// `OAuthConnectionParams::connect_token`).
    #[serde(default, alias = "connectToken")]
    pub connect_token: Option<String>,
}

/// Result from starting 4shared OAuth flow
#[derive(Debug, Clone, Serialize)]
pub struct FourSharedAuthStarted {
    pub auth_url: String,
    pub request_token: String,
    pub request_token_secret: String,
}

/// Vault key for 4shared OAuth tokens
const FOURSHARED_TOKEN_KEY: &str = "oauth_fourshared";

/// Store 4shared tokens in credential vault (same pattern as OAuth2)
fn store_fourshared_tokens(access_token: &str, access_token_secret: &str) -> Result<(), String> {
    let token_data = format!("{}:{}", access_token, access_token_secret);

    // Try vault first
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        store
            .store(FOURSHARED_TOKEN_KEY, &token_data)
            .map_err(|e| format!("Failed to store tokens: {}", e))?;
        return Ok(());
    }

    // Try auto-init vault
    if crate::credential_store::CredentialStore::init().is_ok() {
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            store
                .store(FOURSHARED_TOKEN_KEY, &token_data)
                .map_err(|e| format!("Failed to store tokens: {}", e))?;
            return Ok(());
        }
    }

    Err("Credential vault not available. Please unlock the vault first.".to_string())
}

/// Load 4shared tokens from credential vault
fn load_fourshared_tokens() -> Result<(String, String), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        if let Ok(data) = store.get(FOURSHARED_TOKEN_KEY) {
            let parts: Vec<&str> = data.splitn(2, ':').collect();
            if parts.len() == 2 {
                return Ok((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }
    Err("No 4shared tokens found. Please authenticate first.".to_string())
}

/// Start 4shared OAuth 1.0 flow: obtain request token, return auth URL
#[tauri::command]
pub async fn fourshared_start_auth(
    params: FourSharedAuthParams,
) -> Result<FourSharedAuthStarted, String> {
    use crate::providers::oauth1;

    info!("Starting 4shared OAuth 1.0 flow");

    // Bind a local callback listener to get a port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?
        .port();
    drop(listener);

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    )
    .await?;

    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser: {}", e);
    }

    Ok(FourSharedAuthStarted {
        auth_url,
        request_token,
        request_token_secret,
    })
}

/// Complete 4shared OAuth 1.0 flow: exchange request token + verifier for access token
#[tauri::command]
pub async fn fourshared_complete_auth(
    params: FourSharedAuthParams,
    request_token: String,
    request_token_secret: String,
    verifier: String,
) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Completing 4shared OAuth 1.0 flow");

    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    )
    .await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 authentication completed successfully");
    Ok("Authentication successful".to_string())
}

/// Full 4shared OAuth 1.0 flow: start server, open browser, wait for callback, exchange tokens
#[tauri::command]
pub async fn fourshared_full_auth(
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    params: FourSharedAuthParams,
) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Starting full 4shared OAuth 1.0 flow");

    // #360: register the connect token so Esc / Cancel can abort the callback wait.
    let cancel_token = params
        .connect_token
        .as_deref()
        .map(|key| cancel_registry.register(key));
    let _cancel_guard = params
        .connect_token
        .as_deref()
        .map(|key| ConnectTokenGuard::new(&cancel_registry, key.to_string()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?
        .port();

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    // Step 1: Request token
    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    )
    .await?;

    // Step 2: Open authorization URL
    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        return Err(format!(
            "Could not open browser: {}. Open manually: {}",
            e, auth_url
        ));
    }

    info!(
        "Browser opened, waiting for OAuth 1.0 callback on port {}...",
        port
    );

    // Step 3: Wait for callback (cancellable via Esc / "still connecting", #360)
    let (token, verifier) = tokio::select! {
        res = tokio::time::timeout(
            tokio::time::Duration::from_secs(300),
            wait_for_oauth1_callback(listener),
        ) => res
            .map_err(|_| "OAuth timeout: no response within 5 minutes".to_string())?
            .map_err(|e| format!("Callback error: {}", e))?,
        _ = async {
            match cancel_token.as_ref() {
                Some(token) => token.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            return Err(CONNECT_CANCELLED.to_string());
        }
    };

    if token != request_token {
        return Err("OAuth token mismatch: possible CSRF attack".to_string());
    }

    // Step 4: Exchange for access token
    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    )
    .await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 full auth completed successfully");
    Ok("Authentication successful! You can now connect.".to_string())
}

/// Wait for OAuth 1.0 callback (returns oauth_token, oauth_verifier).
/// oauth_verifier is optional: 4shared uses OAuth 1.0 (not 1.0a) and does NOT send a verifier.
/// Accepts connections in a loop to handle browser prefetch/favicon requests.
async fn wait_for_oauth1_callback(
    listener: tokio::net::TcpListener,
) -> Result<(String, String), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Accept connections in a loop: browsers may send favicon or prefetch requests first
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("Accept error: {}", e))?;

        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| format!("Read error: {}", e))?;

        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line: GET /callback?oauth_token=xxx HTTP/1.1
        let request_path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");

        // Ignore non-callback requests (favicon, prefetch, etc.)
        if !request_path.starts_with("/callback") {
            let response_404 = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(response_404.as_bytes()).await;
            let _ = stream.shutdown().await;
            continue;
        }

        let query = request_path.split('?').nth(1).unwrap_or("");

        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                Some((parts.next()?, parts.next()?))
            })
            .collect();

        let oauth_token = params
            .get("oauth_token")
            .ok_or("Missing oauth_token in callback")?
            .to_string();
        // oauth_verifier is optional: 4shared (OAuth 1.0, not 1.0a) doesn't send it
        let oauth_verifier = params
            .get("oauth_verifier")
            .map(|v| v.to_string())
            .unwrap_or_default();

        let response = r#"HTTP/1.1 200 OK
Content-Type: text/html; charset=utf-8
Connection: close

<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>AeroFTP - Authorization Complete</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            min-height: 100vh;
            display: flex;
            justify-content: center;
            align-items: center;
            background: linear-gradient(135deg, #0f0f1a 0%, #1a1a2e 50%, #16213e 100%);
            color: #fff;
            overflow: hidden;
        }
        .bg-particles {
            position: fixed; top: 0; left: 0; width: 100%; height: 100%;
            pointer-events: none; overflow: hidden; z-index: 0;
        }
        .particle {
            position: absolute; width: 4px; height: 4px;
            background: rgba(0, 212, 255, 0.3); border-radius: 50%;
            animation: float 15s infinite;
        }
        .particle:nth-child(1) { left: 10%; animation-delay: 0s; }
        .particle:nth-child(2) { left: 20%; animation-delay: 2s; }
        .particle:nth-child(3) { left: 30%; animation-delay: 4s; }
        .particle:nth-child(4) { left: 40%; animation-delay: 6s; }
        .particle:nth-child(5) { left: 50%; animation-delay: 8s; }
        .particle:nth-child(6) { left: 60%; animation-delay: 10s; }
        .particle:nth-child(7) { left: 70%; animation-delay: 12s; }
        .particle:nth-child(8) { left: 80%; animation-delay: 14s; }
        .particle:nth-child(9) { left: 90%; animation-delay: 1s; }
        .particle:nth-child(10) { left: 95%; animation-delay: 3s; }
        @keyframes float {
            0%, 100% { transform: translateY(100vh) scale(0); opacity: 0; }
            10% { opacity: 1; } 90% { opacity: 1; }
            100% { transform: translateY(-100vh) scale(1); opacity: 0; }
        }
        .container {
            position: relative; z-index: 1; text-align: center;
            padding: 60px 30px;
            background: rgba(22, 33, 62, 0.8);
            backdrop-filter: blur(20px); border-radius: 24px;
            box-shadow: 0 25px 80px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.1);
            max-width: 440px; animation: slideUp 0.6s ease-out;
        }
        @keyframes slideUp {
            from { opacity: 0; transform: translateY(30px); }
            to { opacity: 1; transform: translateY(0); }
        }
        .logo { margin-bottom: 30px; }
        .app-name {
            font-size: 28px; font-weight: 700;
            background: linear-gradient(135deg, #00d4ff, #0099ff);
            -webkit-background-clip: text; -webkit-text-fill-color: transparent;
            background-clip: text; margin-top: 12px; letter-spacing: -0.5px;
        }
        .success-icon {
            width: 90px; height: 90px; margin: 20px auto 30px;
            background: linear-gradient(135deg, #00d4ff, #00ff88);
            border-radius: 50%; display: flex;
            justify-content: center; align-items: center;
            animation: pulse 2s infinite;
            box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3);
        }
        @keyframes pulse {
            0%, 100% { box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3); }
            50% { box-shadow: 0 10px 60px rgba(0, 212, 255, 0.5); }
        }
        .success-icon svg {
            width: 45px; height: 45px; stroke: #fff;
            stroke-width: 3; fill: none;
            animation: checkmark 0.8s ease-out 0.3s both;
        }
        @keyframes checkmark {
            from { stroke-dashoffset: 50; }
            to { stroke-dashoffset: 0; }
        }
        .success-icon svg path { stroke-dasharray: 50; stroke-dashoffset: 0; }
        h1 { font-size: 26px; font-weight: 600; color: #fff; margin-bottom: 12px; }
        .subtitle {
            font-size: 16px; color: rgba(255, 255, 255, 0.7);
            line-height: 1.6; margin-bottom: 30px;
        }
        .provider-badge {
            display: inline-flex; align-items: center; gap: 8px;
            padding: 10px 20px; background: rgba(255, 255, 255, 0.1);
            border-radius: 30px; font-size: 14px;
            color: rgba(255, 255, 255, 0.9); margin-bottom: 30px;
        }
        .provider-badge svg { width: 20px; height: 20px; }
        .close-hint {
            font-size: 13px; color: rgba(255, 255, 255, 0.5);
            padding-top: 20px; border-top: 1px solid rgba(255, 255, 255, 0.1);
        }
        .close-hint kbd {
            display: inline-block; padding: 2px 8px;
            background: rgba(255, 255, 255, 0.1); border-radius: 4px;
            font-family: monospace; font-size: 12px; margin: 0 2px;
        }
    </style>
</head>
<body>
    <div class="bg-particles">
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
    </div>
    <div class="container">
        <div class="logo">
            <div class="app-name">AeroFTP</div>
        </div>
        <div class="success-icon">
            <svg viewBox="0 0 24 24">
                <path d="M5 13l4 4L19 7" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
        <h1>Authorization Successful</h1>
        <p class="subtitle">Your 4shared account has been connected securely.<br>You're all set to access your files!</p>
        <div class="provider-badge">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"/>
            </svg>
            4shared Connected
        </div>
        <p class="close-hint">You can close this tab and return to AeroFTP<br>or press <kbd>Ctrl</kbd> + <kbd>W</kbd></p>
    </div>
</body>
</html>"#;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        return Ok((oauth_token, oauth_verifier));
    }
}

/// Connect to 4shared after authentication
#[tauri::command]
pub async fn fourshared_connect(
    state: State<'_, ProviderState>,
    cancel_registry: State<'_, ConnectionCancelRegistry>,
    params: FourSharedAuthParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{types::FourSharedConfig, FourSharedProvider};

    info!("Connecting to 4shared...");

    let connect_token = params.connect_token.clone();
    let (access_token, access_token_secret) = load_fourshared_tokens()?;

    let config = FourSharedConfig {
        consumer_key: params.consumer_key,
        consumer_secret: params.consumer_secret.into(),
        access_token: access_token.into(),
        access_token_secret: access_token_secret.into(),
    };

    let mut provider = FourSharedProvider::new(config);
    // #360: cancellable connect (Esc / "still connecting" Cancel).
    run_cancellable_connect(&cancel_registry, connect_token.as_deref(), async {
        provider
            .connect()
            .await
            .map_err(|e| format!("4shared connection failed: {}", e))
    })
    .await?;

    let display_name = provider.display_name();
    let account_email = provider.account_email();

    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(Box::new(provider));
    // Fresh connection carries no crypt overlay: reset both flags.
    state.active_crypt_overlay.store(false, Ordering::SeqCst);
    state.overlay_wrapped.store(false, Ordering::SeqCst);

    info!(
        "Connected to 4shared ({})",
        account_email.as_deref().unwrap_or("no email")
    );
    Ok(OAuth2ConnectResult {
        display_name,
        account_email,
    })
}

// ── Zoho WorkDrive Trash Operations ────────────────────────────────────

/// List trashed files/folders in Zoho WorkDrive (privatespace + team folders)
#[tauri::command]
pub async fn zoho_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    // Downcast to ZohoWorkdriveProvider
    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let mut entries = zoho
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Permanently delete files/folders from Zoho WorkDrive trash
#[tauri::command]
pub async fn zoho_permanent_delete(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    if file_ids.len() == 1 {
        zoho.permanent_delete(&file_ids[0])
            .await
            .map_err(|e| format!("Permanent delete failed: {}", e))
    } else {
        zoho.permanent_delete_batch(&file_ids)
            .await
            .map_err(|e| format!("Permanent delete batch failed: {}", e))
    }
}

/// Restore files/folders from Zoho WorkDrive trash to their original location
#[tauri::command]
pub async fn zoho_restore_from_trash(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    if file_ids.len() == 1 {
        zoho.restore_from_trash(&file_ids[0])
            .await
            .map_err(|e| format!("Restore failed: {}", e))
    } else {
        zoho.restore_from_trash_batch(&file_ids)
            .await
            .map_err(|e| format!("Restore batch failed: {}", e))
    }
}

// ── Zoho WorkDrive Label Operations ───────────────────────────────────

/// List all labels available in the Zoho WorkDrive team
#[tauri::command]
pub async fn zoho_list_team_labels(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let labels = zoho
        .list_team_labels()
        .await
        .map_err(|e| format!("Failed to list team labels: {}", e))?;

    Ok(labels
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// List labels applied to a specific file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_get_file_labels(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let labels = zoho
        .get_file_labels(&path)
        .await
        .map_err(|e| format!("Failed to get file labels: {}", e))?;

    Ok(labels
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// Add a label to a file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_add_file_label(
    state: State<'_, ProviderState>,
    path: String,
    label_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.add_file_label(&path, &label_id)
        .await
        .map_err(|e| format!("Failed to add label: {}", e))
}

/// Create a new label in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_create_label(
    state: State<'_, ProviderState>,
    name: String,
    color: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let label = zoho
        .create_label(&name, &color)
        .await
        .map_err(|e| format!("Failed to create label: {}", e))?;

    serde_json::to_value(label).map_err(|e| format!("Serialize error: {}", e))
}

/// Remove a label from a file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_remove_file_label(
    state: State<'_, ProviderState>,
    path: String,
    label_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.remove_file_label(&path, &label_id)
        .await
        .map_err(|e| format!("Failed to remove label: {}", e))
}

// ── Zoho WorkDrive MCP-parity Operations ──────────────────────────────

/// Get authenticated user info (MCP parity: getUserInfo)
#[tauri::command]
pub async fn zoho_get_user_info(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let info = zoho
        .get_user_info()
        .await
        .map_err(|e| format!("Failed to get user info: {}", e))?;

    serde_json::to_value(info).map_err(|e| format!("Serialize error: {}", e))
}

/// List all share links for a file/folder (MCP parity: getFileShareLinks)
#[tauri::command]
pub async fn zoho_get_file_share_links(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let links = zoho
        .get_file_share_links(&path)
        .await
        .map_err(|e| format!("Failed to get share links: {}", e))?;

    Ok(links
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// Delete an external share link (MCP parity: deleteExternalShareLink)
#[tauri::command]
pub async fn zoho_delete_share_link(
    state: State<'_, ProviderState>,
    link_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.delete_share_link(&link_id)
        .await
        .map_err(|e| format!("Failed to delete share link: {}", e))
}

/// Create a native Zoho document (MCP parity: createNativeDocument)
/// doc_type: "writer"/"zw", "sheet"/"zs", "show"/"presentation"/"zp"
#[tauri::command]
pub async fn zoho_create_native_document(
    state: State<'_, ProviderState>,
    name: String,
    doc_type: String,
    folder_path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.create_native_document(&name, &doc_type, &folder_path)
        .await
        .map_err(|e| format!("Failed to create native document: {}", e))
}

// ── Jottacloud Trash Operations ───────────────────────────────────────

/// Move files to Jottacloud Trash (soft delete)
#[tauri::command]
pub async fn jottacloud_move_to_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .move_to_trash(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in Jottacloud Trash
#[tauri::command]
pub async fn jottacloud_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    let mut entries = jotta
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore files from Jottacloud Trash to their original location
#[tauri::command]
pub async fn jottacloud_restore_from_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .restore_from_trash(path)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Permanently delete files from Jottacloud Trash
#[tauri::command]
pub async fn jottacloud_permanent_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

// ── MEGA Trash Operations ────────────────────────────────────────────

/// Move files to MEGA Rubbish Bin (soft delete)
#[tauri::command]
pub async fn mega_move_to_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    // Try native provider first, then MEGAcmd
    if let Some(native) = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        for path in &paths {
            native
                .move_to_trash(path)
                .await
                .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
        }
        return Ok(());
    }

    let mega = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    for path in &paths {
        mega.move_to_trash(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in MEGA Rubbish Bin
#[tauri::command]
pub async fn mega_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        return native
            .list_trash()
            .await
            .map_err(|e| format!("Failed to list trash: {}", e));
    }

    let mega = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    mega.list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore files from MEGA Rubbish Bin to cloud root
#[tauri::command]
pub async fn mega_restore_from_trash(
    state: State<'_, ProviderState>,
    filenames: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        let cwd = native.pwd().await.unwrap_or_else(|_| "/".to_string());
        for filename in &filenames {
            native
                .restore_from_trash(filename, &cwd)
                .await
                .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }

    let mega = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    let cwd = mega.pwd().await.unwrap_or_else(|_| "/".to_string());
    for filename in &filenames {
        mega.restore_from_trash(filename, &cwd)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
    }
    Ok(())
}

/// Permanently delete files from MEGA Rubbish Bin
#[tauri::command]
pub async fn mega_permanent_delete(
    state: State<'_, ProviderState>,
    filenames: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        for filename in &filenames {
            native
                .permanent_delete_from_trash(filename)
                .await
                .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }

    let mega = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    for filename in &filenames {
        mega.permanent_delete_from_trash(filename)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
    }
    Ok(())
}

// ── Google Drive Trash Operations ────────────────────────────────────

/// Move files to Google Drive Trash (soft delete)
#[tauri::command]
pub async fn google_drive_trash_file(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for path in &paths {
        gdrive
            .trash_file(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in Google Drive Trash
#[tauri::command]
pub async fn google_drive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    let mut entries = gdrive
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore files from Google Drive Trash
#[tauri::command]
pub async fn google_drive_restore_from_trash(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for file_id in &file_ids {
        gdrive
            .restore_from_trash(file_id)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

/// Permanently delete files from Google Drive Trash
#[tauri::command]
pub async fn google_drive_permanent_delete(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for file_id in &file_ids {
        gdrive
            .permanent_delete(file_id)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

// ── OpenDrive Trash Operations ──────────────────────────────────────

/// List items in OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    let mut entries = opendrive
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore items from OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_restore_from_trash(
    state: State<'_, ProviderState>,
    items: Vec<OpenDriveTrashActionItem>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    for item in &items {
        opendrive
            .restore_from_trash(&item.item_id, item.is_dir)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", item.item_id, e))?;
    }
    Ok(())
}

/// Permanently delete items from OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_permanent_delete(
    state: State<'_, ProviderState>,
    items: Vec<OpenDriveTrashActionItem>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    for item in &items {
        opendrive
            .permanent_delete_from_trash(&item.item_id, item.is_dir)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", item.item_id, e))?;
    }
    Ok(())
}

/// Empty OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    opendrive
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e))
}

/// Set OpenDrive privacy for a file or folder.
/// is_public=false => private, is_public=true => public.
#[tauri::command]
pub async fn opendrive_set_path_privacy(
    state: State<'_, ProviderState>,
    path: String,
    is_public: bool,
    is_dir: bool,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    if is_dir {
        opendrive
            .set_folder_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set folder privacy failed for {}: {}", path, e))
    } else {
        opendrive
            .set_file_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set file privacy failed for {}: {}", path, e))
    }
}

/// Set OpenDrive three-level access for a file or folder. Accepts the
/// AeroFTP-canonical tokens `private`, `public`, or `hidden`.
#[tauri::command]
pub async fn opendrive_set_path_access(
    state: State<'_, ProviderState>,
    path: String,
    access_level: String,
    is_dir: bool,
) -> Result<(), String> {
    let level = crate::providers::opendrive::OpenDriveAccessLevel::from_token(&access_level)
        .ok_or_else(|| {
            format!(
                "Unknown OpenDrive access level: '{}' (expected private, public, or hidden)",
                access_level
            )
        })?;

    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    if is_dir {
        opendrive
            .set_folder_access(&path, level)
            .await
            .map_err(|e| format!("Set folder access failed for {}: {}", path, e))
    } else {
        opendrive
            .set_file_access(&path, level)
            .await
            .map_err(|e| format!("Set file access failed for {}: {}", path, e))
    }
}

/// Set FourShared privacy for a file or folder.
/// is_public=false => private, is_public=true => public.
#[tauri::command]
pub async fn fourshared_set_path_privacy(
    state: State<'_, ProviderState>,
    path: String,
    is_public: bool,
    is_dir: bool,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::FourShared {
        return Err("This operation is only available for FourShared".to_string());
    }

    let fourshared = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::fourshared::FourSharedProvider>()
        .ok_or_else(|| "Failed to access FourShared provider".to_string())?;

    if is_dir {
        fourshared
            .set_folder_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set folder privacy failed for {}: {}", path, e))
    } else {
        fourshared
            .set_file_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set file privacy failed for {}: {}", path, e))
    }
}

// ─── Yandex Disk-Specific Commands ────────────────────────────────────────

/// List items in Yandex Disk trash
#[tauri::command]
pub async fn yandex_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    let mut entries = yandex
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore items from Yandex Disk trash
#[tauri::command]
pub async fn yandex_restore_from_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    for path in &paths {
        yandex
            .restore_from_trash(path)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Permanently delete items from Yandex Disk trash
#[tauri::command]
pub async fn yandex_permanent_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    for path in &paths {
        yandex
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Empty Yandex Disk trash
#[tauri::command]
pub async fn yandex_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    yandex
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e))
}

// ─── Box-Specific Commands ────────────────────────────────────────────────

/// List items in Box trash
#[tauri::command]
pub async fn box_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let mut entries = bx
        .list_trash()
        .await
        .map_err(|e| format!("List trash failed: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Move files/folders to Box trash (soft delete)
#[tauri::command]
pub async fn box_trash_files(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.trash_files(&paths)
        .await
        .map_err(|e| format!("Trash failed: {}", e))
}

/// Restore an item from Box trash
#[tauri::command]
pub async fn box_restore_from_trash(
    state: State<'_, ProviderState>,
    item_id: String,
    item_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.restore_from_trash(&item_id, &item_type)
        .await
        .map_err(|e| format!("Restore failed: {}", e))
}

/// Permanently delete an item from Box trash
#[tauri::command]
pub async fn box_permanent_delete(
    state: State<'_, ProviderState>,
    item_id: String,
    item_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.permanent_delete_from_trash(&item_id, &item_type)
        .await
        .map_err(|e| format!("Permanent delete failed: {}", e))
}

/// Move a file or folder to a different parent folder on Box
#[tauri::command]
pub async fn box_move_file(
    state: State<'_, ProviderState>,
    from_path: String,
    to_folder: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.move_item(&from_path, &to_folder)
        .await
        .map_err(|e| format!("Move failed: {}", e))
}

/// List comments on a Box file
#[tauri::command]
pub async fn box_list_comments(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let comments = bx
        .list_comments(&path)
        .await
        .map_err(|e| format!("List comments failed: {}", e))?;
    serde_json::to_value(&comments)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// Add a comment to a Box file
#[tauri::command]
pub async fn box_add_comment(
    state: State<'_, ProviderState>,
    path: String,
    message: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.add_comment(&path, &message)
        .await
        .map_err(|e| format!("Add comment failed: {}", e))
}

/// Delete a comment on Box
#[tauri::command]
pub async fn box_delete_comment(
    state: State<'_, ProviderState>,
    comment_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.delete_comment(&comment_id)
        .await
        .map_err(|e| format!("Delete comment failed: {}", e))
}

/// Add a collaboration on a Box file or folder
#[tauri::command]
pub async fn box_add_collaboration(
    state: State<'_, ProviderState>,
    path: String,
    email: String,
    role: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.add_collaboration(&path, &email, &role)
        .await
        .map_err(|e| format!("Add collaboration failed: {}", e))
}

/// Remove a collaboration from Box
#[tauri::command]
pub async fn box_remove_collaboration(
    state: State<'_, ProviderState>,
    collab_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.remove_collaboration(&collab_id)
        .await
        .map_err(|e| format!("Remove collaboration failed: {}", e))
}

/// Apply watermark to a Box file
#[tauri::command]
pub async fn box_set_watermark(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.set_watermark(&path)
        .await
        .map_err(|e| format!("Set watermark failed: {}", e))
}

/// Remove watermark from a Box file
#[tauri::command]
pub async fn box_remove_watermark(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.remove_watermark(&path)
        .await
        .map_err(|e| format!("Remove watermark failed: {}", e))
}

/// Set tags on a Box file or folder
#[tauri::command]
pub async fn box_set_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.set_tags(&path, &tags)
        .await
        .map_err(|e| format!("Set tags failed: {}", e))
}

/// Lock a Box folder
#[tauri::command]
pub async fn box_lock_folder(state: State<'_, ProviderState>, path: String) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.lock_folder(&path)
        .await
        .map_err(|e| format!("Lock folder failed: {}", e))
}

/// Unlock a Box folder by lock ID
#[tauri::command]
pub async fn box_unlock_folder(
    state: State<'_, ProviderState>,
    lock_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.unlock_folder(&lock_id)
        .await
        .map_err(|e| format!("Unlock folder failed: {}", e))
}

/// List collaborations on a Box file or folder
#[tauri::command]
pub async fn box_list_collaborations(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let collabs = bx
        .list_collaborations(&path)
        .await
        .map_err(|e| format!("List collaborations failed: {}", e))?;
    serde_json::to_value(&collabs)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// List folder locks on a Box folder
#[tauri::command]
pub async fn box_list_folder_locks(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let locks = bx
        .list_folder_locks(&path)
        .await
        .map_err(|e| format!("List folder locks failed: {}", e))?;
    serde_json::to_value(&locks)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// Check if 4shared tokens exist
#[tauri::command]
pub async fn fourshared_has_tokens() -> Result<bool, String> {
    Ok(load_fourshared_tokens().is_ok())
}

/// Clear 4shared tokens (logout)
#[tauri::command]
pub async fn fourshared_logout() -> Result<(), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        let _ = store.delete(FOURSHARED_TOKEN_KEY);
    }
    info!("Logged out from 4shared");
    Ok(())
}

// ─── FileLu-Specific Commands ─────────────────────────────────────────────

/// Set or unset a file password on FileLu.
/// Pass empty string to remove the password.
#[tauri::command]
pub async fn filelu_set_file_password(
    state: State<'_, ProviderState>,
    path: String,
    password: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_file_password(&path, &password)
        .await
        .map_err(|e| e.to_string())
}

/// Toggle a FileLu file between private (only_me=true) and public.
#[tauri::command]
pub async fn filelu_set_file_privacy(
    state: State<'_, ProviderState>,
    path: String,
    only_me: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_file_privacy(&path, only_me)
        .await
        .map_err(|e| e.to_string())
}

/// Clone a FileLu file server-side. Returns the URL of the cloned file.
#[tauri::command]
pub async fn filelu_clone_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.clone_file(&path).await.map_err(|e| e.to_string())
}

/// Set or unset a FileLu folder password (requires folder sharing enabled).
#[tauri::command]
pub async fn filelu_set_folder_password(
    state: State<'_, ProviderState>,
    path: String,
    password: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_folder_password(&path, &password)
        .await
        .map_err(|e| e.to_string())
}

/// Configure FileLu folder settings: filedrop and public visibility.
#[tauri::command]
pub async fn filelu_set_folder_settings(
    state: State<'_, ProviderState>,
    path: String,
    filedrop: bool,
    is_public: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_folder_settings(&path, filedrop, is_public)
        .await
        .map_err(|e| e.to_string())
}

/// List all deleted files in FileLu trash.
#[tauri::command]
pub async fn filelu_list_deleted(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filelu::DeletedFileEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.list_deleted_files().await.map_err(|e| e.to_string())
}

/// Restore a deleted file from FileLu trash by file_code.
#[tauri::command]
pub async fn filelu_restore_file(
    state: State<'_, ProviderState>,
    file_code: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.restore_deleted_file(&file_code)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete a FileLu file from trash by file_code.
#[tauri::command]
pub async fn filelu_permanent_delete(
    state: State<'_, ProviderState>,
    file_code: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.permanent_delete_file(&file_code)
        .await
        .map_err(|e| e.to_string())
}

/// Upload a file from a remote URL to a FileLu folder. Returns file_code.
#[tauri::command]
pub async fn filelu_remote_url_upload(
    state: State<'_, ProviderState>,
    remote_url: String,
    dest_path: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.remote_url_upload(&remote_url, &dest_path)
        .await
        .map_err(|e| e.to_string())
}

/// Restore a deleted folder from FileLu trash by fld_id.
#[tauri::command]
pub async fn filelu_restore_folder(
    state: State<'_, ProviderState>,
    fld_id: u64,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.restore_deleted_folder(fld_id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Google Drive Extended Commands ───────────────────────────────────────

/// Star or unstar files on Google Drive
#[tauri::command]
pub async fn google_drive_set_starred(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    starred: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    for path in &paths {
        gd.set_starred(path, starred)
            .await
            .map_err(|e| format!("Star failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List comments on a Google Drive file
#[tauri::command]
pub async fn google_drive_list_comments(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.list_comments(&path).await.map_err(|e| e.to_string())
}

/// Add a comment to a Google Drive file
#[tauri::command]
pub async fn google_drive_add_comment(
    state: State<'_, ProviderState>,
    path: String,
    message: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.add_comment(&path, &message)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a comment from a Google Drive file
#[tauri::command]
pub async fn google_drive_delete_comment(
    state: State<'_, ProviderState>,
    path: String,
    comment_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.delete_comment(&path, &comment_id)
        .await
        .map_err(|e| e.to_string())
}

/// Set custom properties on a Google Drive file
#[tauri::command]
pub async fn google_drive_set_properties(
    state: State<'_, ProviderState>,
    path: String,
    properties: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.set_properties(&path, &properties)
        .await
        .map_err(|e| e.to_string())
}

/// Set description on a Google Drive file
#[tauri::command]
pub async fn google_drive_set_description(
    state: State<'_, ProviderState>,
    path: String,
    description: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.set_description(&path, &description)
        .await
        .map_err(|e| e.to_string())
}

// ─── Dropbox Extended Commands ────────────────────────────────────────────

/// List items in Dropbox trash (deleted files)
#[tauri::command]
pub async fn dropbox_list_trash(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    let mut entries = db.list_deleted(&path).await.map_err(|e| e.to_string())?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore a file from Dropbox trash
#[tauri::command]
pub async fn dropbox_restore_from_trash(
    state: State<'_, ProviderState>,
    path: String,
    rev: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.restore_file(&path, &rev)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete a file from Dropbox
#[tauri::command]
pub async fn dropbox_permanent_delete(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.permanent_delete(&path).await.map_err(|e| e.to_string())
}

/// Return the Dropbox account tier ("basic" | "pro" | "business"). The GUI
/// gates Permanent Delete / Empty Trash on "business" because
/// `files/permanently_delete` is a Dropbox Business only endpoint.
#[tauri::command]
pub async fn dropbox_account_type(state: State<'_, ProviderState>) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.account_type().await.map_err(|e| e.to_string())
}

/// Set tags on a Dropbox file (replaces existing tags)
#[tauri::command]
pub async fn dropbox_set_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.set_tags(&path, &tags).await.map_err(|e| e.to_string())
}

/// Get tags for Dropbox files
#[tauri::command]
pub async fn dropbox_get_tags(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.get_tags(&paths).await.map_err(|e| e.to_string())
}

// ─── OneDrive Extended Commands ───────────────────────────────────────────

/// List items in OneDrive recycle bin
#[tauri::command]
pub async fn onedrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    let mut entries = od.list_trash().await.map_err(|e| e.to_string())?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Move files to OneDrive recycle bin (soft delete)
#[tauri::command]
pub async fn onedrive_trash_files(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    for path in &paths {
        od.trash_file(path)
            .await
            .map_err(|e| format!("Trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Restore an item from OneDrive recycle bin
#[tauri::command]
pub async fn onedrive_restore_from_trash(
    state: State<'_, ProviderState>,
    item_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    od.restore_from_trash(&item_id)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete an item from OneDrive
#[tauri::command]
pub async fn onedrive_permanent_delete(
    state: State<'_, ProviderState>,
    item_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    od.permanent_delete(&item_id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Folder Size Calculation ───

/// Global cancellation flag for folder size scan
static FOLDER_SIZE_CANCEL: AtomicBool = AtomicBool::new(false);

/// Progress payload emitted during folder size scan
#[derive(Clone, Serialize)]
pub struct FolderSizeProgress {
    total_bytes: u64,
    file_count: u64,
    dir_count: u64,
    scanning: bool,
}

/// Recursively calculate folder size via provider list(): BFS with progress events.
/// Safety: max 50,000 entries, max depth 50.
#[tauri::command]
pub async fn provider_calculate_folder_size(
    state: State<'_, ProviderState>,
    app: AppHandle,
    path: String,
) -> Result<FolderSizeProgress, String> {
    FOLDER_SIZE_CANCEL.store(false, Ordering::Relaxed);

    let mut total_bytes: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    let mut entries_scanned: u64 = 0;

    const MAX_ENTRIES: u64 = 50_000;
    const MAX_DEPTH: usize = 50;

    // BFS queue: (path, depth)
    let mut queue: Vec<(String, usize)> = vec![(path, 0)];

    while let Some((current_path, depth)) = queue.pop() {
        if FOLDER_SIZE_CANCEL.load(Ordering::Relaxed) {
            // Cancelled: return partial results
            let result = FolderSizeProgress {
                total_bytes,
                file_count,
                dir_count,
                scanning: false,
            };
            let _ = app.emit("folder-size-progress", &result);
            return Ok(result);
        }

        if depth > MAX_DEPTH || entries_scanned > MAX_ENTRIES {
            break;
        }

        // List directory contents
        let entries = {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;
            provider
                .list(&current_path)
                .await
                .map_err(|e| format!("Failed to list {}: {}", current_path, e))?
        };

        for entry in &entries {
            entries_scanned += 1;
            if entry.is_dir {
                dir_count += 1;
                let subpath = if current_path == "/" || current_path.is_empty() {
                    format!("/{}", entry.name)
                } else if current_path.ends_with('/') {
                    format!("{}{}", current_path, entry.name)
                } else {
                    format!("{}/{}", current_path, entry.name)
                };
                queue.push((subpath, depth + 1));
            } else {
                file_count += 1;
                total_bytes += entry.size;
            }
        }

        // Emit progress every directory listing
        let progress = FolderSizeProgress {
            total_bytes,
            file_count,
            dir_count,
            scanning: true,
        };
        let _ = app.emit("folder-size-progress", &progress);
    }

    let result = FolderSizeProgress {
        total_bytes,
        file_count,
        dir_count,
        scanning: false,
    };
    let _ = app.emit("folder-size-progress", &result);
    Ok(result)
}

/// Cancel an in-progress folder size calculation
#[tauri::command]
pub async fn provider_cancel_folder_size() -> Result<(), String> {
    FOLDER_SIZE_CANCEL.store(true, Ordering::Relaxed);
    Ok(())
}

static USED_SCAN_CANCEL: AtomicBool = AtomicBool::new(false);

/// Result of the explicit "used storage" scan (item 4b).
#[derive(Clone, Serialize)]
pub struct UsedScanResult {
    pub used: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub truncated: bool,
    pub method: String,
}

/// Progress payload emitted on `used-scan-progress` while the scan runs.
#[derive(Clone, Serialize)]
pub struct UsedScanProgress {
    pub used: u64,
    pub file_count: u64,
    pub scanning: bool,
}

/// Explicit recursive "used storage" scan for the connected provider
/// (item 4b). Shares the S3/WebDAV fast-path fallback semantics with
/// `used_scan`, but keeps the GUI BFS inline so it can re-lock the
/// provider per directory. NEVER called automatically: the GUI "Calculate
/// used storage" action invokes it. Persisting the figure into the
/// profile's lastQuota is done frontend-side (same path as the cached API
/// quota).
#[tauri::command]
pub async fn provider_scan_used(
    state: State<'_, ProviderState>,
    app: AppHandle,
    path: String,
) -> Result<UsedScanResult, String> {
    USED_SCAN_CANCEL.store(false, Ordering::Relaxed);

    const MAX_DEPTH: usize = 100;
    const MAX_ENTRIES: u64 = 500_000;
    let root = if path.trim().is_empty() {
        "/".to_string()
    } else {
        path
    };

    let emit_progress = |files: u64, bytes: u64, scanning: bool| {
        let _ = app.emit(
            "used-scan-progress",
            UsedScanProgress {
                used: bytes,
                file_count: files,
                scanning,
            },
        );
    };

    // --- Single-shot specializations (one short lock each) -------------
    // S3: flat ListObjectsV2; WebDAV: PROPFIND Depth:infinity. The shared
    // helper treats any fast-path failure, and empty/non-recursed WebDAV
    // responses, as a miss so we fall through to the per-directory BFS.
    {
        let mut guard = state.provider.lock().await;
        let provider = guard
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;

        if let Some(fast) =
            crate::used_scan::provider_list_recursive_fastpath(provider, &root).await
        {
            let mut used = 0u64;
            let mut files = 0u64;
            let mut dirs = 0u64;
            let mut truncated = false;
            for e in fast.entries {
                if e.is_dir {
                    dirs += 1;
                    continue;
                }
                if files >= MAX_ENTRIES {
                    truncated = true;
                    break;
                }
                used = used.saturating_add(e.size);
                files += 1;
            }
            emit_progress(files, used, false);
            return Ok(UsedScanResult {
                used,
                file_count: files,
                dir_count: dirs,
                truncated,
                method: fast.method.to_string(),
            });
        }
    }

    // --- Generic BFS: re-lock the provider per directory ---------------
    // Mirrors provider_calculate_folder_size so cancel stays responsive
    // and other remote operations on this session can interleave instead
    // of blocking for the whole (possibly huge) walk.
    let mut used = 0u64;
    let mut file_count = 0u64;
    let mut dir_count = 0u64;
    let mut truncated = false;
    let mut queue: Vec<(String, usize)> = vec![(root.clone(), 0)];
    let mut last_emit = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(400))
        .unwrap_or_else(std::time::Instant::now);

    while let Some((dir, depth)) = queue.pop() {
        if USED_SCAN_CANCEL.load(Ordering::Relaxed) {
            truncated = true;
            break;
        }
        if depth >= MAX_DEPTH || (file_count + dir_count) >= MAX_ENTRIES {
            truncated = true;
            continue;
        }
        let entries = {
            let mut guard = state.provider.lock().await;
            let provider = guard
                .as_mut()
                .ok_or_else(|| "Not connected to any provider".to_string())?;
            match provider.list(&dir).await {
                Ok(e) => e,
                Err(e) => {
                    // A single unreadable directory must not abort the
                    // whole figure: the result is a lower bound.
                    tracing::warn!("[provider_scan_used] failed to list {}: {}", dir, e);
                    truncated = true;
                    continue;
                }
            }
        };
        for entry in entries {
            // Skip symlinks: a symlink-to-dir is reported with both is_dir
            // and is_symlink (sftp.rs), so following it lets cur->. / up->..
            // cycles inflate the figure and exhaust the budget. Matches the
            // MCP scan, used_scan.rs and the sftp rmdir_recursive precedent.
            if entry.is_symlink {
                continue;
            }
            // Cap inside the loop (not only between directories) so one
            // hostile listing cannot grow the queue past MAX_ENTRIES.
            if (file_count + dir_count) >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            if entry.is_dir {
                dir_count += 1;
                queue.push((entry.path.clone(), depth + 1));
            } else {
                used = used.saturating_add(entry.size);
                file_count += 1;
            }
        }
        if last_emit.elapsed() >= std::time::Duration::from_millis(250) {
            emit_progress(file_count, used, true);
            last_emit = std::time::Instant::now();
        }
    }

    emit_progress(file_count, used, false);
    Ok(UsedScanResult {
        used,
        file_count,
        dir_count,
        truncated,
        method: "bfs".to_string(),
    })
}

/// Cancel an in-progress used-storage scan.
#[tauri::command]
pub async fn provider_cancel_used_scan() -> Result<(), String> {
    USED_SCAN_CANCEL.store(true, Ordering::Relaxed);
    Ok(())
}

// ── GitHub-specific commands ──────────────────────────────────────

/// List all branches of the connected GitHub repository
#[tauri::command]
pub async fn github_list_branches(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .list_branches()
        .await
        .map_err(|e| format!("Failed to list branches: {}", e))
}

/// Get info about the connected GitHub repository
#[tauri::command]
pub async fn github_get_info(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    Ok(serde_json::json!({
        "owner": github.owner(),
        "repo": github.repo(),
        "branch": github.active_branch(),
        "writeMode": format!("{:?}", github.write_mode()),
        "writeModeKind": match github.write_mode() {
            crate::providers::github::GitHubWriteMode::Unknown => "unknown",
            crate::providers::github::GitHubWriteMode::DirectWrite => "direct",
            crate::providers::github::GitHubWriteMode::DirectWriteProtected { .. } => "direct",
            crate::providers::github::GitHubWriteMode::BranchWorkflow { .. } => "branch",
            crate::providers::github::GitHubWriteMode::ReadOnly { .. } => "readonly",
        },
        "workingBranch": github.working_branch(),
        "repoPrivate": github.is_private(),
    }))
}

// ── GitLab-specific commands ──────────────────────────────────────

/// List all branches of the connected GitLab repository
#[tauri::command]
pub async fn gitlab_list_branches(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let branches = gitlab
        .list_branches()
        .await
        .map_err(|e| format!("Failed to list branches: {}", e))?;

    Ok(branches
        .iter()
        .map(|b| {
            serde_json::json!({
                "name": b.name,
                "protected": b.is_protected,
                "default": b.is_default,
                "canPush": b.can_push,
            })
        })
        .collect())
}

/// Get info about the connected GitLab repository
#[tauri::command]
pub async fn gitlab_get_info(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let on_non_default = gitlab.active_branch_name() != gitlab.default_branch_name();
    let (write_mode, write_mode_kind, working_branch) = if !gitlab.can_push() {
        ("ReadOnly", "readonly", serde_json::Value::Null)
    } else if on_non_default {
        // On a non-default branch with push access → branch mode (MR available)
        (
            "Branch",
            "branch",
            serde_json::Value::String(gitlab.active_branch_name().to_string()),
        )
    } else {
        ("Direct", "direct", serde_json::Value::Null)
    };

    Ok(serde_json::json!({
        "owner": gitlab.project_path(),
        "repo": gitlab.project_path(),
        "branch": gitlab.active_branch_name(),
        "writeMode": write_mode,
        "writeModeKind": write_mode_kind,
        "workingBranch": working_branch,
        "repoPrivate": gitlab.is_private(),
    }))
}

/// Switch branch on the connected GitLab repository
#[tauri::command]
pub async fn gitlab_switch_branch(
    state: State<'_, ProviderState>,
    branch: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .switch_branch(&branch)
        .await
        .map_err(|e| format!("Failed to switch branch: {}", e))
}

/// Atomic batch upload of files to GitLab via REST commits API.
#[tauri::command]
pub async fn gitlab_batch_upload(
    state: State<'_, ProviderState>,
    files: Vec<serde_json::Value>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let mut actions = Vec::with_capacity(files.len());
    for file_val in &files {
        let local_path = file_val
            .get("localPath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'localPath'")?;
        let remote_path = file_val
            .get("remotePath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'remotePath'")?;
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {}", local_path, e))?;
        let content_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
        let clean_path = remote_path.trim_start_matches('/');
        // Check if file exists to determine create vs update
        let action = if gitlab.exists(clean_path).await.unwrap_or(false) {
            "update"
        } else {
            "create"
        };
        actions.push(serde_json::json!({
            "action": action,
            "file_path": clean_path,
            "content": content_b64,
            "encoding": "base64",
        }));
    }

    let commit = gitlab
        .commit_actions_pub(&message, actions)
        .await
        .map_err(|e| format!("Batch upload failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": commit.id,
        "files_count": files.len(),
    }))
}

/// Atomic batch delete of files on GitLab via REST commits API.
#[tauri::command]
pub async fn gitlab_batch_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let actions: Vec<serde_json::Value> = paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "action": "delete",
                "file_path": p.trim_start_matches('/'),
            })
        })
        .collect();

    let commit = gitlab
        .commit_actions_pub(&message, actions)
        .await
        .map_err(|e| format!("Batch delete failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": commit.id,
        "deletions_count": paths.len(),
    }))
}

// ── GitLab: Releases ───────────────────────────────────────────────

/// List all releases of the connected GitLab repository
#[tauri::command]
pub async fn gitlab_list_releases(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let releases = gitlab
        .list_releases()
        .await
        .map_err(|e| format!("Failed to list releases: {}", e))?;

    Ok(releases
        .iter()
        .map(|r| {
            serde_json::json!({
                "tag_name": r.tag_name,
                "name": r.name,
                "description": r.description,
                "created_at": r.created_at,
                "released_at": r.released_at,
                "author": r.author.username,
                "assets_count": r.assets.count,
                "sources": r.assets.sources.iter().map(|s| serde_json::json!({
                    "format": s.format,
                    "url": s.url,
                })).collect::<Vec<_>>(),
            })
        })
        .collect())
}

/// List asset links for a GitLab release
#[tauri::command]
pub async fn gitlab_list_release_assets(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let links = gitlab
        .list_release_links(&tag)
        .await
        .map_err(|e| format!("Failed to list release assets: {}", e))?;

    Ok(links
        .iter()
        .map(|l| {
            serde_json::json!({
                "id": l.id,
                "name": l.name,
                "url": l.url,
                "direct_asset_url": l.direct_asset_url,
                "link_type": l.link_type,
                "external": l.external,
            })
        })
        .collect())
}

/// Create a new GitLab release
#[tauri::command]
pub async fn gitlab_create_release(
    state: State<'_, ProviderState>,
    tag: String,
    name: String,
    description: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let release = gitlab
        .create_release(&tag, &name, &description)
        .await
        .map_err(|e| format!("Failed to create release: {}", e))?;

    Ok(serde_json::json!({
        "tag_name": release.tag_name,
        "name": release.name,
    }))
}

/// Delete a GitLab release
#[tauri::command]
pub async fn gitlab_delete_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .delete_release(&tag)
        .await
        .map_err(|e| format!("Failed to delete release: {}", e))
}

/// Upload a file as release asset on GitLab.
/// `link_type`: "other" (default), "package", "image", "runbook".
#[tauri::command]
pub async fn gitlab_upload_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    local_path: String,
    asset_name: String,
    link_type: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let link = gitlab
        .upload_release_asset(&tag, &local_path, &asset_name, link_type.as_deref())
        .await
        .map_err(|e| format!("Failed to upload release asset: {}", e))?;

    Ok(serde_json::json!({
        "id": link.id,
        "name": link.name,
        "url": link.url,
        "link_type": link.link_type,
    }))
}

/// Delete a release asset link on GitLab
#[tauri::command]
pub async fn gitlab_delete_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    link_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .delete_release_link(&tag, link_id)
        .await
        .map_err(|e| format!("Failed to delete release asset: {}", e))
}

/// Read a file from the connected GitLab repository as UTF-8 text
#[tauri::command]
pub async fn gitlab_read_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .read_file_content(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))
}

/// Download a release asset via authenticated backend (works for private repos)
#[tauri::command]
pub async fn gitlab_download_release_asset(
    state: State<'_, ProviderState>,
    url: String,
    local_path: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .download_release_asset(&url, &local_path)
        .await
        .map_err(|e| format!("Failed to download asset: {}", e))
}

// ── GitLab: Merge Requests ─────────────────────────────────────────

/// Create a merge request on the connected GitLab repository
#[tauri::command]
pub async fn gitlab_create_merge_request(
    state: State<'_, ProviderState>,
    title: String,
    body: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .create_merge_request(&title, &body)
        .await
        .map_err(|e| format!("Failed to create merge request: {}", e))
}

// ── GitLab: Web URLs ───────────────────────────────────────────────

/// Get web URL for a file or directory on GitLab
#[tauri::command]
pub async fn gitlab_get_web_url(
    state: State<'_, ProviderState>,
    path: String,
    is_dir: bool,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    Ok(gitlab.web_url(&path, is_dir))
}

/// Create a pull request on the connected GitHub repository
#[tauri::command]
pub async fn github_create_pr(
    state: State<'_, ProviderState>,
    title: String,
    body: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let pr = github
        .ensure_pull_request(&title, Some(&body), false)
        .await
        .map_err(|e| format!("Failed to create PR: {}", e))?;

    Ok(pr.html_url)
}

/// GitHub Device Flow: Step 1: Request device code
/// Returns user_code and verification_uri for the user to authorize in browser
#[tauri::command]
pub async fn github_device_flow_start() -> Result<serde_json::Value, String> {
    let response = crate::providers::github::auth::request_device_code().await?;

    // Try to open browser automatically
    let _ = open::that(&response.verification_uri);

    Ok(serde_json::json!({
        "user_code": response.user_code,
        "verification_uri": response.verification_uri,
        "device_code": response.device_code,
        "expires_in": response.expires_in,
        "interval": response.interval,
    }))
}

/// GitHub Device Flow: Step 2: Poll for token.
/// SEC-GH-001: Token held backend-side, never returned to frontend.
#[tauri::command]
pub async fn github_device_flow_complete(
    state: State<'_, ProviderState>,
    device_code: String,
    interval: u64,
) -> Result<serde_json::Value, String> {
    let token = crate::providers::github::auth::poll_for_token(&device_code, interval).await?;
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Vault key for a GitHub App PEM, keyed by app_id + installation_id
fn github_pem_vault_key(app_id: &str, installation_id: &str) -> String {
    format!("github_pem_{}_{}", app_id, installation_id)
}

/// Validate PEM contents: non-empty, correct RSA header
fn validate_pem_contents(pem_contents: &str) -> Result<(), String> {
    if pem_contents.trim().is_empty() {
        return Err(
            "PEM file is empty. Please download a new private key from GitHub App settings."
                .to_string(),
        );
    }
    if !pem_contents.contains("-----BEGIN RSA PRIVATE KEY-----")
        && !pem_contents.contains("-----BEGIN PRIVATE KEY-----")
    {
        return Err(
            "Invalid PEM format: file does not contain an RSA private key. \
             Download a fresh .pem from GitHub > Settings > Developer settings > GitHub Apps > Private keys."
                .to_string(),
        );
    }
    Ok(())
}

/// GitHub App Bot Mode: Read .pem from disk, store in vault, and get installation token.
/// SEC-GH-001: The installation token is held backend-side and never crosses IPC.
/// The frontend receives only success status and expiry metadata.
#[tauri::command]
pub async fn github_app_token_from_pem(
    state: State<'_, ProviderState>,
    pem_path: String,
    app_id: String,
    installation_id: String,
) -> Result<serde_json::Value, String> {
    log::info!("GitHub App token: reading PEM from {}", pem_path);

    // Check file exists before reading: provide actionable error
    let path = std::path::Path::new(&pem_path);
    if !path.exists() {
        return Err(format!(
            "PEM file not found: '{}'. The .pem file may have been moved or deleted. Please re-import it.",
            pem_path
        ));
    }

    // Read PEM securely in backend: key never crosses IPC
    let pem_contents = std::fs::read_to_string(&pem_path)
        .map_err(|e| format!("Cannot read .pem file '{}': {}", pem_path, e))?;

    log::info!("GitHub App token: PEM read OK, validating...");

    validate_pem_contents(&pem_contents)?;

    // Validate PEM by attempting JWT generation
    crate::providers::github::auth::validate_pem(&pem_contents, &app_id)?;

    // Store PEM content + App credentials in vault (encrypted AES-256-GCM)
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        if let Err(e) = store.store(&vault_key, &pem_contents) {
            log::warn!("Could not store PEM in vault (non-fatal): {}", e);
        } else {
            log::info!("GitHub App PEM stored in vault as '{}'", vault_key);
        }
        // Store App ID + Installation ID so the form can pre-populate on new connections
        let creds = serde_json::json!({
            "app_id": app_id,
            "installation_id": installation_id,
        });
        let _ = store.store("github_app_credentials", &creds.to_string());
    }

    log::info!("GitHub App token: PEM valid, requesting installation token...");

    // Get installation token
    let token_resp = crate::providers::github::auth::get_installation_token(
        &pem_contents,
        &app_id,
        &installation_id,
    )
    .await?;

    // SEC-GH-001: Hold the token backend-side: never return it to the frontend
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token_resp.token);
    }

    Ok(serde_json::json!({
        "success": true,
        "expires_at": token_resp.expires_at,
    }))
}

/// GitHub App Bot Mode: Read PEM from vault (previously imported) and refresh installation token.
/// SEC-GH-001: The installation token is held backend-side and never crosses IPC.
#[tauri::command]
pub async fn github_app_token_from_vault(
    state: State<'_, ProviderState>,
    app_id: String,
    installation_id: String,
) -> Result<serde_json::Value, String> {
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    log::info!(
        "GitHub App token: reading PEM from vault key '{}'",
        vault_key
    );

    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready: cannot retrieve stored PEM".to_string())?;

    let pem_contents = store
        .get(&vault_key)
        .map_err(|_| "PEM not found in vault. Please re-import the .pem file.".to_string())?;

    validate_pem_contents(&pem_contents)?;
    crate::providers::github::auth::validate_pem(&pem_contents, &app_id)?;

    // Ensure App credentials are saved in vault for form pre-population
    let creds = serde_json::json!({
        "app_id": app_id,
        "installation_id": installation_id,
    });
    let _ = store.store("github_app_credentials", &creds.to_string());

    log::info!("GitHub App token: vault PEM valid, requesting installation token...");

    let token_resp = crate::providers::github::auth::get_installation_token(
        &pem_contents,
        &app_id,
        &installation_id,
    )
    .await?;

    // SEC-GH-001: Hold the token backend-side: never return it to the frontend
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token_resp.token);
    }

    Ok(serde_json::json!({
        "success": true,
        "expires_at": token_resp.expires_at,
    }))
}

/// Get stored GitHub App credentials (App ID + Installation ID) from vault
#[tauri::command]
pub async fn github_get_app_credentials() -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    match store.get("github_app_credentials") {
        Ok(json_str) => serde_json::from_str(&json_str)
            .map_err(|e| format!("Invalid credentials format: {}", e)),
        Err(_) => Ok(serde_json::Value::Null),
    }
}

/// Store GitHub PAT in vault (encrypted)
#[tauri::command]
pub async fn github_store_pat(pat: String) -> Result<(), String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    // MUV-5: per-user dual-write. The vault stays source of truth + fallback;
    // the token is mirrored into the active user's partition with an explicit
    // "github" type (the prefix classifier deliberately excludes `github_*` so
    // it never touches the machine-global `github_pem_*` / app credentials).
    crate::user_partitions::store_active_credential_typed_dual(
        &store,
        "github_pat",
        "github",
        &pat,
    )
    .map_err(|e| format!("Failed to store PAT: {}", e))?;
    log::info!("GitHub PAT stored in vault");
    Ok(())
}

/// Store the held GitHub token into vault as PAT (for Device Flow persistence).
/// SEC-GH-001: Takes from held_github_app_token and stores in vault without IPC exposure.
#[tauri::command]
pub async fn github_store_pat_from_held(state: State<'_, ProviderState>) -> Result<(), String> {
    let token = {
        let held = state.held_github_app_token.lock().await;
        held.clone()
    };
    if let Some(token) = token {
        let store = crate::credential_store::CredentialStore::from_cache()
            .ok_or_else(|| "Vault not ready".to_string())?;
        // MUV-5: per-user dual-write with an explicit "github" type (see
        // github_store_pat).
        crate::user_partitions::store_active_credential_typed_dual(
            &store,
            "github_oauth_token",
            "github",
            &token,
        )
        .map_err(|e| format!("Failed to store OAuth token: {}", e))?;
        log::info!("GitHub Device Flow token stored in vault as OAuth token");
    }
    Ok(())
}

/// Load stored GitHub OAuth token from vault into held token.
/// Used on app restart when OAuth mode reconnects.
#[tauri::command]
pub async fn github_load_oauth_token(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    // MUV-5: resolve from the active user's partition, falling back to the
    // dual-written vault copy (a not-yet-migrated key or a non-admin active user
    // still resolves the shared singleton).
    let token = crate::user_partitions::resolve_active_credential(&store, "github_oauth_token")
        .ok()
        .flatten()
        .ok_or_else(|| "No OAuth token stored in vault".to_string())?
        .to_string();
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Get stored GitHub PAT from vault.
/// SEC-GH-001: Token held backend-side for connect, returns only success status.
#[tauri::command]
pub async fn github_get_pat(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    // MUV-5: resolve from the active user's partition, falling back to the
    // dual-written vault copy (see github_load_oauth_token).
    let pat = crate::user_partitions::resolve_active_credential(&store, "github_pat")
        .ok()
        .flatten()
        .ok_or_else(|| "No PAT stored in vault".to_string())?
        .to_string();
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(pat);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Check if a GitHub App PEM is stored in the vault
#[tauri::command]
pub async fn github_has_vault_pem(app_id: String, installation_id: String) -> Result<bool, String> {
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    Ok(store.get(&vault_key).is_ok())
}

/// List all releases for the connected GitHub repository
#[tauri::command]
pub async fn github_list_releases(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let releases = github
        .list_all_releases()
        .await
        .map_err(|e| format!("Failed to list releases: {}", e))?;

    let result: Vec<serde_json::Value> = releases
        .iter()
        .map(|r| {
            serde_json::json!({
                "tag": &r.name,
                "path": &r.path,
                "published_at": &r.modified,
                "draft": r.metadata.get("draft").map(|v| v == "true").unwrap_or(false),
                "prerelease": r.metadata.get("prerelease").map(|v| v == "true").unwrap_or(false),
                "body": r.metadata.get("body").cloned().unwrap_or_default(),
                "release_id": r.metadata.get("release_id").cloned().unwrap_or_default(),
            })
        })
        .collect();

    Ok(serde_json::json!({ "releases": result, "count": result.len() }))
}

/// List assets for a specific release tag
#[tauri::command]
pub async fn github_list_release_assets(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let assets = github
        .list_assets_for_release(&tag)
        .await
        .map_err(|e| format!("Failed to list release assets: {}", e))?;

    let result: Vec<serde_json::Value> = assets
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": &a.name,
                "size": a.size,
                "content_type": a.mime_type,
                "download_count": a.metadata.get("download_count").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0),
                "browser_download_url": a.metadata.get("browser_download_url").cloned().unwrap_or_default(),
                "updated_at": &a.modified,
            })
        })
        .collect();

    Ok(serde_json::json!({ "assets": result, "count": result.len(), "tag": tag }))
}

/// Create a new release on the connected GitHub repository
#[tauri::command]
pub async fn github_create_release(
    state: State<'_, ProviderState>,
    tag: String,
    name: String,
    body: String,
    draft: bool,
    prerelease: bool,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let release = github
        .create_new_release(&tag, &name, &body, draft, prerelease)
        .await
        .map_err(|e| format!("Failed to create release: {}", e))?;

    Ok(serde_json::json!({
        "id": release.id,
        "tag_name": release.tag_name,
        "name": release.name,
        "draft": release.draft,
        "prerelease": release.prerelease,
        "created_at": release.created_at,
    }))
}

/// Read a text file from the connected GitHub repository (always from repo root).
#[tauri::command]
pub async fn github_read_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    // Prefix with "/" to force resolve_path to treat as absolute (from root),
    // regardless of the user's current navigation directory.
    let root_path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    let bytes = provider
        .download_to_bytes(&root_path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    String::from_utf8(bytes).map_err(|e| format!("File is not valid UTF-8: {}", e))
}

// ── GitHub Pages ──────────────────────────────────────────────────

/// Get GitHub Pages site info (returns null if not enabled)
#[tauri::command]
pub async fn github_get_pages(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    match github.get_pages_info().await {
        Ok(Some(site)) => Ok(serde_json::to_value(site).unwrap_or_default()),
        Ok(None) => Ok(serde_json::Value::Null),
        Err(e) => Err(format!("Failed to get Pages info: {}", e)),
    }
}

/// List GitHub Pages builds
#[tauri::command]
pub async fn github_list_pages_builds(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let builds = github
        .list_pages_builds()
        .await
        .map_err(|e| format!("Failed to list Pages builds: {}", e))?;
    Ok(serde_json::to_value(builds).unwrap_or_default())
}

/// Trigger a GitHub Pages rebuild (legacy build_type only)
#[tauri::command]
pub async fn github_trigger_pages_build(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let status = github
        .trigger_pages_build()
        .await
        .map_err(|e| format!("Failed to trigger Pages build: {}", e))?;
    Ok(serde_json::to_value(status).unwrap_or_default())
}

/// Update GitHub Pages configuration (CNAME, HTTPS, source)
#[tauri::command]
pub async fn github_update_pages(
    state: State<'_, ProviderState>,
    cname: Option<String>,
    https_enforced: Option<bool>,
    source_branch: Option<String>,
    source_path: Option<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .update_pages_config(
            cname.as_deref(),
            https_enforced,
            source_branch.as_deref(),
            source_path.as_deref(),
        )
        .await
        .map_err(|e| format!("Failed to update Pages config: {}", e))
}

/// Check DNS health for GitHub Pages custom domain
#[tauri::command]
pub async fn github_pages_health(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let health = github
        .pages_health_check()
        .await
        .map_err(|e| format!("Failed to check Pages DNS health: {}", e))?;
    Ok(serde_json::to_value(health).unwrap_or_default())
}

/// Upload a file as a release asset
#[tauri::command]
pub async fn github_upload_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    local_path: String,
    asset_name: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .upload_asset(&tag, &local_path, &asset_name)
        .await
        .map_err(|e| format!("Failed to upload release asset: {}", e))?;

    Ok(serde_json::json!({
        "tag": tag,
        "asset": asset_name,
        "status": "uploaded",
    }))
}

/// Delete an entire release by tag
#[tauri::command]
pub async fn github_delete_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .delete_release_by_tag(&tag)
        .await
        .map_err(|e| format!("Failed to delete release: {}", e))?;

    Ok(serde_json::json!({ "tag": tag, "status": "deleted" }))
}

/// Delete a specific asset from a release
#[tauri::command]
pub async fn github_delete_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    asset_name: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .delete_asset(&tag, &asset_name)
        .await
        .map_err(|e| format!("Failed to delete release asset: {}", e))?;

    Ok(serde_json::json!({ "tag": tag, "asset": asset_name, "status": "deleted" }))
}

/// Download a release asset to a local file
#[tauri::command]
pub async fn github_download_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    asset_name: String,
    local_path: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .download_asset(&tag, &asset_name, &local_path)
        .await
        .map_err(|e| format!("Failed to download release asset: {}", e))?;

    Ok(
        serde_json::json!({ "tag": tag, "asset": asset_name, "path": local_path, "status": "downloaded" }),
    )
}

/// Get detailed release information by tag
#[tauri::command]
pub async fn github_get_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let release = github
        .get_release(&tag)
        .await
        .map_err(|e| format!("Failed to get release info: {}", e))?;

    let assets: Vec<serde_json::Value> = release
        .assets
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "size": a.size,
                "download_count": a.download_count,
                "content_type": a.content_type,
                "browser_download_url": a.browser_download_url,
                "created_at": a.created_at,
                "updated_at": a.updated_at,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "id": release.id,
        "tag_name": release.tag_name,
        "name": release.name,
        "body": release.body,
        "draft": release.draft,
        "prerelease": release.prerelease,
        "created_at": release.created_at,
        "published_at": release.published_at,
        "assets": assets,
        "asset_count": assets.len(),
    }))
}

/// Atomic multi-file commit via GraphQL createCommitOnBranch
#[tauri::command]
pub async fn github_batch_commit(
    state: State<'_, ProviderState>,
    branch: String,
    message: String,
    additions: Vec<serde_json::Value>,
    deletions: Vec<String>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    // Parse additions: [{path: String, content: String}]
    let parsed_additions: Vec<(String, String)> = additions
        .iter()
        .map(|v| {
            let path = v
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| "Each addition must have a 'path' string field".to_string())?;
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .ok_or_else(|| "Each addition must have a 'content' string field".to_string())?;
            Ok((path.to_string(), content.to_string()))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let oid = github
        .batch_commit(&branch, &message, &parsed_additions, &deletions)
        .await
        .map_err(|e| format!("Batch commit failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "branch": branch,
        "additions_count": parsed_additions.len(),
        "deletions_count": deletions.len(),
    }))
}

/// Atomic batch upload of binary files to GitHub via GraphQL createCommitOnBranch.
/// Unlike github_batch_commit (text-only), this reads files from disk as binary.
#[tauri::command]
pub async fn github_batch_upload(
    state: State<'_, ProviderState>,
    files: Vec<serde_json::Value>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let mut additions: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
    for file_val in &files {
        let local_path = file_val
            .get("localPath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'localPath'")?;
        let remote_path = file_val
            .get("remotePath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'remotePath'")?;
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {}", local_path, e))?;
        additions.push((remote_path.trim_start_matches('/').to_string(), data));
    }

    let oid = github
        .batch_upload(&message, &additions, &[])
        .await
        .map_err(|e| format!("Batch upload failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "files_count": additions.len(),
    }))
}

/// Atomic batch delete of files on GitHub via GraphQL createCommitOnBranch.
#[tauri::command]
pub async fn github_batch_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let deletions: Vec<String> = paths
        .iter()
        .map(|p| p.trim_start_matches('/').to_string())
        .collect();

    let oid = github
        .batch_upload(&message, &[], &deletions)
        .await
        .map_err(|e| format!("Batch delete failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "files_count": deletions.len(),
    }))
}

// ── GitHub Local Sync Detection ──────────────────────────────────

/// Check if a git remote line matches a specific owner/repo exactly.
/// Prevents partial matches like `repo-old` matching `repo`.
fn remote_matches_repo(line: &str, owner: &str, repo: &str) -> bool {
    let lower = line.to_lowercase();
    let ssh = format!("github.com:{}/{}", owner, repo).to_lowercase();
    let https = format!("github.com/{}/{}", owner, repo).to_lowercase();

    for pattern in [&ssh, &https] {
        if let Some(idx) = lower.find(pattern) {
            let after = idx + pattern.len();
            // Must be followed by `.git`, whitespace, or end of string
            let rest = &lower[after..];
            if rest.is_empty() || rest.starts_with(".git") || rest.starts_with(char::is_whitespace)
            {
                return true;
            }
        }
    }
    false
}

/// SEC-GH-002/003: Validate and canonicalize a local path for git operations.
/// Returns the canonical path only if it is a real directory containing a `.git` folder.
fn validate_local_git_path(local_path: &str) -> Result<std::path::PathBuf, String> {
    let canonical = std::fs::canonicalize(local_path)
        .map_err(|e| format!("Invalid local path '{}': {}", local_path, e))?;
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| format!("Cannot access '{}': {}", canonical.display(), e))?;
    if !meta.is_dir() {
        return Err(format!("'{}' is not a directory", canonical.display()));
    }
    if !canonical.join(".git").exists() {
        return Err(format!("'{}' is not a git repository", canonical.display()));
    }
    Ok(canonical)
}

/// Helper: run an async git command with non-interactive environment guards.
async fn git_command(args: &[&str], dir: &std::path::Path) -> Result<std::process::Output, String> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .output()
        .await
        .map_err(|e| format!("Failed to run git {}: {}", args.first().unwrap_or(&""), e))
}

/// Check if the local working directory has unpushed commits for the connected GitHub repo.
/// SEC-GH-002/003: Path is canonicalized, validated as git repo, and all commands are async.
#[tauri::command]
pub async fn github_check_local_sync(
    state: State<'_, ProviderState>,
    local_path: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Ok(serde_json::json!({"is_local_repo": false}));
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let owner = github.owner().to_string();
    let repo = github.repo().to_string();

    // Validate and canonicalize the local path
    let canonical = match validate_local_git_path(&local_path) {
        Ok(p) => p,
        Err(_) => return Ok(serde_json::json!({"is_local_repo": false})),
    };

    // Check if local repo's remote matches this GitHub repo
    let remote_out = git_command(&["remote", "-v"], &canonical).await?;
    if !remote_out.status.success() {
        return Ok(serde_json::json!({"is_local_repo": false}));
    }
    let remote_output = String::from_utf8_lossy(&remote_out.stdout).to_string();

    let matches = remote_output
        .lines()
        .any(|line| remote_matches_repo(line, &owner, &repo));

    if !matches {
        return Ok(serde_json::json!({"is_local_repo": true, "repo_matches": false}));
    }

    // Get local HEAD
    let head_out = git_command(&["rev-parse", "HEAD"], &canonical).await?;
    let local_head = if head_out.status.success() {
        String::from_utf8_lossy(&head_out.stdout).trim().to_string()
    } else {
        return Ok(
            serde_json::json!({"is_local_repo": true, "repo_matches": true, "error": "Cannot read local HEAD"}),
        );
    };

    // Get remote HEAD via GitHub API
    let branch = github.active_branch().to_string();
    let remote_head = {
        match github
            .client_mut()
            .get_json::<serde_json::Value>(&format!(
                "/repos/{}/{}/git/ref/heads/{}",
                owner,
                repo,
                urlencoding::encode(&branch)
            ))
            .await
        {
            Ok(val) => {
                match val
                    .get("object")
                    .and_then(|o| o.get("sha"))
                    .and_then(|s| s.as_str())
                {
                    Some(sha) => sha.to_string(),
                    None => {
                        return Ok(serde_json::json!({
                            "is_local_repo": true, "repo_matches": true,
                            "error": "Cannot parse remote HEAD SHA"
                        }))
                    }
                }
            }
            Err(e) => {
                return Ok(serde_json::json!({
                    "is_local_repo": true, "repo_matches": true,
                    "error": format!("Cannot fetch remote HEAD: {}", e)
                }))
            }
        }
    };

    // Count unpushed commits
    let count_out = git_command(
        &["rev-list", &format!("{}..HEAD", remote_head), "--count"],
        &canonical,
    )
    .await?;
    let unpushed_count = if count_out.status.success() {
        String::from_utf8_lossy(&count_out.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
    } else {
        0
    };

    Ok(serde_json::json!({
        "is_local_repo": true,
        "repo_matches": true,
        "local_head": local_head,
        "remote_head": remote_head,
        "unpushed_count": unpushed_count,
        "branch": branch,
    }))
}

/// Push local commits to the remote GitHub repository.
/// SEC-GH-002: Path validated and verified to match the connected repo before executing push.
#[tauri::command]
pub async fn github_push_local(
    state: State<'_, ProviderState>,
    local_path: String,
) -> Result<serde_json::Value, String> {
    // Validate the local path
    let canonical = validate_local_git_path(&local_path)?;

    // Verify the repo remote matches the connected GitHub repo
    {
        let mut provider_guard = state.provider.lock().await;
        let provider = provider_guard
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        if is_plain_github_provider(provider.as_mut()) {
            let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            let owner = github.owner().to_string();
            let repo = github.repo().to_string();

            let remote_out = git_command(&["remote", "-v"], &canonical).await?;
            let remote_output = String::from_utf8_lossy(&remote_out.stdout).to_string();
            let matches = remote_output
                .lines()
                .any(|line| remote_matches_repo(line, &owner, &repo));
            if !matches {
                return Err(format!(
                    "Local repo remote does not match connected GitHub repo {}/{}",
                    owner, repo
                ));
            }
        }
    }

    let output = git_command(&["push"], &canonical).await?;

    if output.status.success() {
        Ok(serde_json::json!({
            "status": "ok",
            "message": String::from_utf8_lossy(&output.stdout).trim().to_string(),
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git push failed: {}", stderr))
    }
}

// ── GitHub Actions ────────────────────────────────────────────────

/// List recent GitHub Actions workflow runs
#[tauri::command]
pub async fn github_list_actions_runs(
    state: State<'_, ProviderState>,
    branch: Option<String>,
    per_page: Option<u8>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let runs = github
        .list_actions_runs(branch.as_deref(), per_page.unwrap_or(20))
        .await
        .map_err(|e| format!("Failed to list Actions runs: {}", e))?;
    Ok(serde_json::to_value(runs).unwrap_or_default())
}

/// Re-run a GitHub Actions workflow
#[tauri::command]
pub async fn github_rerun_workflow(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .rerun_actions_workflow(run_id)
        .await
        .map_err(|e| format!("Failed to re-run workflow: {}", e))
}

/// Re-run only failed jobs in a GitHub Actions workflow
#[tauri::command]
pub async fn github_rerun_failed_jobs(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .rerun_failed_jobs(run_id)
        .await
        .map_err(|e| format!("Failed to re-run failed jobs: {}", e))
}

/// Cancel an in-progress GitHub Actions workflow run
#[tauri::command]
pub async fn github_cancel_workflow(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if !is_plain_github_provider(provider.as_mut()) {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .cancel_actions_run(run_id)
        .await
        .map_err(|e| format!("Failed to cancel workflow: {}", e))
}

// ============ Filen Encrypted Notes ============

/// List all Filen encrypted notes
#[tauri::command]
pub async fn filen_notes_list(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filen::notes::FilenNote>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.list_notes().await.map_err(|e| e.to_string())
}

/// Create a new Filen encrypted note
#[tauri::command]
pub async fn filen_notes_create(
    state: State<'_, ProviderState>,
    title: String,
    content: String,
    note_type: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .create_note(&title, &content, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Get decrypted content of a Filen note
#[tauri::command]
pub async fn filen_notes_get_content(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<crate::providers::filen::notes::FilenNoteContent, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .get_note_content(&uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Edit content of a Filen encrypted note
#[tauri::command]
pub async fn filen_notes_edit_content(
    state: State<'_, ProviderState>,
    uuid: String,
    content: String,
    note_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .edit_note_content(&uuid, &content, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Edit title of a Filen encrypted note
#[tauri::command]
pub async fn filen_notes_edit_title(
    state: State<'_, ProviderState>,
    uuid: String,
    title: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .edit_note_title(&uuid, &title)
        .await
        .map_err(|e| e.to_string())
}

/// Change the type of a Filen note (text, md, code, rich, checklist)
#[tauri::command]
pub async fn filen_notes_change_type(
    state: State<'_, ProviderState>,
    uuid: String,
    note_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .change_note_type(&uuid, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Move a Filen note to trash
#[tauri::command]
pub async fn filen_notes_trash(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.trash_note(&uuid).await.map_err(|e| e.to_string())
}

/// Archive a Filen note
#[tauri::command]
pub async fn filen_notes_archive(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.archive_note(&uuid).await.map_err(|e| e.to_string())
}

/// Restore a Filen note from trash or archive
#[tauri::command]
pub async fn filen_notes_restore(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.restore_note(&uuid).await.map_err(|e| e.to_string())
}

/// Permanently delete a Filen note
#[tauri::command]
pub async fn filen_notes_delete(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.delete_note(&uuid).await.map_err(|e| e.to_string())
}

/// Returns the authVersion observed during Filen connect (/v3/auth/info).
#[tauri::command]
pub async fn filen_get_auth_version(
    state: State<'_, ProviderState>,
) -> Result<Option<u32>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    Ok(filen.auth_version())
}

/// Toggle favorite on a Filen note
#[tauri::command]
pub async fn filen_notes_toggle_favorite(
    state: State<'_, ProviderState>,
    uuid: String,
    favorite: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .toggle_note_favorite(&uuid, favorite)
        .await
        .map_err(|e| e.to_string())
}

/// Toggle pinned on a Filen note
#[tauri::command]
pub async fn filen_notes_toggle_pinned(
    state: State<'_, ProviderState>,
    uuid: String,
    pinned: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .toggle_note_pinned(&uuid, pinned)
        .await
        .map_err(|e| e.to_string())
}

/// Get version history for a Filen note
#[tauri::command]
pub async fn filen_notes_history(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<Vec<crate::providers::filen::notes::FilenNoteHistoryEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .get_note_history(&uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Restore a specific history version of a Filen note
#[tauri::command]
pub async fn filen_notes_history_restore(
    state: State<'_, ProviderState>,
    uuid: String,
    history_id: u64,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .restore_note_history(&uuid, history_id)
        .await
        .map_err(|e| e.to_string())
}

/// List all Filen note tags
#[tauri::command]
pub async fn filen_notes_tags_list(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filen::notes::FilenNoteTag>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.list_note_tags().await.map_err(|e| e.to_string())
}

/// Create a new Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_create(
    state: State<'_, ProviderState>,
    name: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .create_note_tag(&name)
        .await
        .map_err(|e| e.to_string())
}

/// Rename a Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_rename(
    state: State<'_, ProviderState>,
    tag_uuid: String,
    name: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .rename_note_tag(&tag_uuid, &name)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_delete(
    state: State<'_, ProviderState>,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .delete_note_tag(&tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Assign a tag to a Filen note
#[tauri::command]
pub async fn filen_notes_tag_note(
    state: State<'_, ProviderState>,
    note_uuid: String,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .tag_note(&note_uuid, &tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Remove a tag from a Filen note
#[tauri::command]
pub async fn filen_notes_untag_note(
    state: State<'_, ProviderState>,
    note_uuid: String,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .untag_note(&note_uuid, &tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Parse note type string to enum (delegates to notes module).
fn parse_note_type_str(s: &str) -> crate::providers::filen::notes::NoteType {
    crate::providers::filen::notes::parse_note_type(s)
}

// ============ S3 Enterprise Commands ============

/// Change storage class of an S3 object (via server-side copy)
#[tauri::command]
pub async fn s3_change_storage_class(
    state: State<'_, ProviderState>,
    path: String,
    storage_class: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.change_storage_class(&path, &storage_class)
        .await
        .map_err(|e| e.to_string())
}

/// Initiate Glacier/Deep Archive restore for an S3 object
#[tauri::command]
pub async fn s3_glacier_restore(
    state: State<'_, ProviderState>,
    path: String,
    days: u32,
    tier: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.glacier_restore(&path, days, &tier)
        .await
        .map_err(|e| e.to_string())
}

/// Get object tags for an S3 object
#[tauri::command]
pub async fn s3_get_object_tags(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.get_object_tags(&path).await.map_err(|e| e.to_string())
}

/// Set object tags on an S3 object (max 10 tags per AWS)
#[tauri::command]
pub async fn s3_set_object_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.set_object_tags(&path, &tags)
        .await
        .map_err(|e| e.to_string())
}

/// Delete all tags from an S3 object
#[tauri::command]
pub async fn s3_delete_object_tags(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.delete_object_tags(&path)
        .await
        .map_err(|e| e.to_string())
}

// ============ S3 Trash / Version Management (#266) ============

/// Result of an `s3_empty_trash` sweep (or its dry-run preview).
#[derive(serde::Serialize)]
pub struct S3EmptyTrashSummary {
    /// Number of versions + delete markers that were (or would be) purged.
    pub count: u64,
    /// Total bytes across those objects (delete markers count as 0).
    pub bytes: u64,
    /// True when nothing was deleted (preview only).
    pub dry_run: bool,
}

/// Browse the S3 soft-delete trash: every version and delete marker under
/// `prefix`. With `include_noncurrent` false, only delete markers and each key's
/// current version are returned (the classic trash view); true also lists older
/// versions. Peels past a crypt overlay to the concrete S3 transport and fills
/// each entry's decrypted `display_key` (a no-op when Crypt is off).
#[tauri::command]
pub async fn s3_list_trash(
    state: State<'_, ProviderState>,
    prefix: String,
    include_noncurrent: bool,
) -> Result<Vec<crate::providers::TrashEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Trash browse is only available for S3".into());
    }
    let mut entries = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .list_object_versions(&prefix, include_noncurrent)
        .await
        .map_err(|e| format!("List trash failed: {}", e))?;
    crate::crypt_overlay_provider::decode_overlay_trash_keys(&mut **provider, &mut entries);
    Ok(entries)
}

/// Act on one trash entry (identified by its raw `key` + `version_id`):
/// - `undelete`: drop a delete marker so the prior version becomes current
///   again (no data copy).
/// - `copy_forward`: copy an older version forward to a new current version,
///   keeping history intact.
/// - `purge`: permanently remove that specific version or marker (irreversible).
///
/// `key` is the raw backend token from `s3_list_trash` (ciphertext under a crypt
/// overlay), so this peels to the concrete S3 transport and passes it verbatim.
#[tauri::command]
pub async fn s3_restore_from_trash(
    state: State<'_, ProviderState>,
    key: String,
    version_id: String,
    mode: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Trash restore is only available for S3".into());
    }
    let s3 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider);
    match mode.as_str() {
        "copy_forward" => s3
            .restore_version(&key, &version_id)
            .await
            .map_err(|e| format!("Restore from trash failed: {}", e)),
        "undelete" | "purge" => s3
            .delete_version(&key, &version_id)
            .await
            .map_err(|e| format!("Restore from trash failed: {}", e)),
        other => Err(format!(
            "Unknown trash mode '{}' (expected 'undelete', 'copy_forward', or 'purge')",
            other
        )),
    }
}

/// Empty the S3 trash under `prefix`, purging every version and delete marker in
/// batches of 1000. With `dry_run` true, nothing is deleted and the returned
/// summary is a preview of what would be purged. Irreversible when executed.
#[tauri::command]
pub async fn s3_empty_trash(
    state: State<'_, ProviderState>,
    prefix: String,
    include_noncurrent: bool,
    dry_run: bool,
) -> Result<S3EmptyTrashSummary, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Empty trash is only available for S3".into());
    }
    let (count, bytes) = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .empty_object_versions(&prefix, include_noncurrent, dry_run)
        .await
        .map_err(|e| format!("Empty trash failed: {}", e))?;
    Ok(S3EmptyTrashSummary {
        count,
        bytes,
        dry_run,
    })
}

// ============ Azure Enterprise Commands ============

/// Set the access tier of an Azure blob (Hot, Cool, Cold, Archive)
#[tauri::command]
pub async fn azure_set_blob_tier(
    state: State<'_, ProviderState>,
    path: String,
    tier: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    azure
        .set_blob_tier(&path, &tier)
        .await
        .map_err(|e| e.to_string())
}

/// List soft-deleted blobs in the Azure container
#[tauri::command]
pub async fn azure_list_deleted_blobs(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    azure.list_deleted_blobs().await.map_err(|e| e.to_string())
}

/// Undelete a soft-deleted Azure blob
#[tauri::command]
pub async fn azure_undelete_blob(
    state: State<'_, ProviderState>,
    path: Option<String>,
    blob_name: Option<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    let resolved_path = path
        .or(blob_name)
        .ok_or_else(|| "Missing path or blobName".to_string())?;
    azure
        .undelete_blob(&resolved_path)
        .await
        .map_err(|e| e.to_string())
}

// ============ pCloud Trash Commands ============

/// List items in the Internxt trash
#[tauri::command]
pub async fn internxt_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Internxt {
        return Err("Only available for Internxt".into());
    }
    let internxt = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::internxt::InternxtProvider>()
        .ok_or("Failed to access Internxt provider")?;
    let mut entries = internxt.list_trash().await.map_err(|e| e.to_string())?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// List items in the pCloud trash
#[tauri::command]
pub async fn pcloud_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    let mut entries = pcloud.list_trash().await.map_err(|e| e.to_string())?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore item from pCloud trash
#[tauri::command]
pub async fn pcloud_restore_from_trash(
    state: State<'_, ProviderState>,
    id: String,
    is_folder: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud
        .restore_from_trash(&id, is_folder)
        .await
        .map_err(|e| e.to_string())
}

/// Empty pCloud trash
#[tauri::command]
pub async fn pcloud_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud.empty_trash().await.map_err(|e| e.to_string())
}

/// Permanently delete a single item from pCloud trash
#[tauri::command]
pub async fn pcloud_permanently_delete_trash(
    state: State<'_, ProviderState>,
    id: String,
    is_folder: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud
        .permanent_delete_from_trash(&id, is_folder)
        .await
        .map_err(|e| e.to_string())
}

// ============ kDrive Trash Commands ============

/// List items in the kDrive trash
#[tauri::command]
pub async fn kdrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    let mut entries = kdrive.list_trash().await.map_err(|e| e.to_string())?;
    crate::crypt_overlay_provider::decode_overlay_trash_names(&mut **provider, &mut entries);
    Ok(entries)
}

/// Restore item from kDrive trash
#[tauri::command]
pub async fn kdrive_restore_from_trash(
    state: State<'_, ProviderState>,
    file_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive
        .restore_from_trash(&file_id)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete item from kDrive trash
#[tauri::command]
pub async fn kdrive_permanently_delete_trash(
    state: State<'_, ProviderState>,
    file_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive
        .permanently_delete_trash(&file_id)
        .await
        .map_err(|e| e.to_string())
}

/// Empty the entire kDrive trash
#[tauri::command]
pub async fn kdrive_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive.empty_trash().await.map_err(|e| e.to_string())
}

// ─── Backblaze B2 native: hide / restore / permanent-delete ────────────────
//
// `delete()` on B2 native creates a hide marker: the file is invisible in
// listings but the previous version stays in the bucket. These three commands
// expose the recovery path explicitly: `b2_list_hidden` enumerates the soft
// deletes, `b2_restore_hidden` removes a single hide marker (file reappears),
// and `b2_permanent_delete` purges every version of a path so it can no
// longer be recovered.

/// List soft-deleted (hidden) files in the connected B2 bucket under the
/// given prefix. Returns the hide markers; the caller can call
/// `b2_restore_hidden` on a `path` to bring the underlying version back.
#[tauri::command]
pub async fn b2_list_hidden(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Backblaze {
        return Err("Only available for Backblaze B2".to_string());
    }
    let b2 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::B2Provider>()
        .ok_or_else(|| "B2 downcast failed".to_string())?;
    b2.list_hidden_files(&path).await.map_err(|e| e.to_string())
}

/// Restore a soft-deleted B2 file by removing its hide marker. The previous
/// content version reappears in normal listings. Returns an error if no hide
/// marker exists for the path.
#[tauri::command]
pub async fn b2_restore_hidden(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Backblaze {
        return Err("Only available for Backblaze B2".to_string());
    }
    let b2 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::B2Provider>()
        .ok_or_else(|| "B2 downcast failed".to_string())?;
    b2.restore_hidden_file(&path)
        .await
        .map_err(|e| e.to_string())
}

/// Hard-delete every version of a B2 file (including hide markers and any
/// historical content). This is unrecoverable: callers MUST confirm intent
/// at the UI layer. Returns the number of versions purged.
#[tauri::command]
pub async fn b2_permanent_delete(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u32, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Backblaze {
        return Err("Only available for Backblaze B2".to_string());
    }
    let b2 = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<crate::providers::B2Provider>()
        .ok_or_else(|| "B2 downcast failed".to_string())?;
    b2.permanent_delete_path(&path)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        decrypt_rel_aerocrypt, decrypt_rel_rclone, drain_in_flight_transfers,
        normalize_aerocrypt_remote_files_for_compare, normalize_rclone_remote_files_for_compare,
        rclone_decrypted_size, remote_matches_repo, run_cancellable_connect,
        run_cancellable_listing, ConnectTokenGuard, ConnectionCancelRegistry, ListingCancelState,
        ProviderConnectionParams, ProviderState, TransferOperationGuard, CONNECT_CANCELLED,
        LISTING_CANCELLED,
    };
    use crate::rclone_crypt::{
        derive_keys, derive_keys_with_tweak, encrypt_file_content, encrypt_name,
        FilenameEncryption, RcloneCryptKeys,
    };
    use crate::sync::FileInfo;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    // Connection-scoped overlay-key cache (instant re-arm after a view-only lock):
    // store keeps a re-armable copy, re-arm is non-consuming, a generation change
    // (connect/disconnect/swap) drops a stale entry so a cached key can never
    // re-arm onto a different connection, and an explicit hard lock wipes it.
    #[test]
    fn overlay_key_cache_rearm_invalidate_and_stale_generation() {
        use crate::crypt_overlay_provider::OverlayKeys;
        let make_keys = || {
            let (name_key, data_key, name_tweak) =
                derive_keys_with_tweak("overlay-pass", "overlay-salt").unwrap();
            OverlayKeys::Rclone(RcloneCryptKeys {
                name_key,
                data_key,
                name_tweak,
                filename_encryption: FilenameEncryption::Standard,
                off_suffix: String::new(),
                directory_name_encryption: true,
            })
        };
        let state = ProviderState::new();
        assert!(state.cached_overlay_for_rearm().is_none());

        // Store -> re-arm sees it, scope + kind preserved, and it is non-consuming
        // (the same connection can toggle off/on repeatedly).
        state.store_overlay_key_cache(
            make_keys(),
            "/Vault".to_string(),
            "rclone-crypt".to_string(),
        );
        let got = state.cached_overlay_for_rearm().expect("cache present");
        assert_eq!(got.1, "/Vault");
        assert_eq!(got.2, "rclone-crypt");
        assert!(state.cached_overlay_for_rearm().is_some());

        // A connection change (generation bump) invalidates a stale entry.
        state.connection_generation.fetch_add(1, Ordering::SeqCst);
        assert!(
            state.cached_overlay_for_rearm().is_none(),
            "stale-generation cache must never re-arm onto a new connection"
        );

        // Explicit hard lock wipes it.
        state.store_overlay_key_cache(
            make_keys(),
            "/Vault".to_string(),
            "rclone-crypt".to_string(),
        );
        assert!(state.cached_overlay_for_rearm().is_some());
        state.invalidate_overlay_key_cache();
        assert!(state.cached_overlay_for_rearm().is_none());
    }

    fn s3_params(path_style: Option<bool>) -> ProviderConnectionParams {
        ProviderConnectionParams {
            protocol: "s3".to_string(),
            provider_id: None,
            server: "http://localhost".to_string(),
            port: Some(3900),
            username: "access".to_string(),
            password: "secret".to_string(),
            initial_path: None,
            bucket: Some("garage-bucket".to_string()),
            region: Some("garage".to_string()),
            endpoint: None,
            path_style,
            anonymous: None,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            save_session: None,
            mega_mode: None,
            session_expires_at: None,
            logout_on_disconnect: None,
            private_key_path: None,
            key_passphrase: None,
            timeout: None,
            tls_mode: None,
            verify_cert: None,
            two_factor_code: None,
            totp_secret: None,
            filen_api_key: None,
            github_auth_mode: None,
            github_app_id: None,
            github_installation_id: None,
            github_pem_path: None,
            github_token_expires_at: None,
            github_branch: None,
            peer_namespace: None,
            peer_ticket: None,
            peer_local_folder: None,
            peer_role: None,
            connect_token: None,
            opendrive_default_privacy: None,
        }
    }

    fn rclone_compare_keys(directory_name_encryption: bool) -> RcloneCryptKeys {
        let (name_key, data_key, name_tweak) =
            derive_keys_with_tweak("compare-pass", "compare-salt").unwrap();
        RcloneCryptKeys {
            name_key,
            data_key,
            name_tweak,
            filename_encryption: FilenameEncryption::Standard,
            off_suffix: ".bin".to_string(),
            directory_name_encryption,
        }
    }

    fn compare_file_info(size: u64) -> FileInfo {
        FileInfo {
            name: "encrypted".to_string(),
            path: "/remote/encrypted".to_string(),
            size,
            modified: None,
            is_dir: false,
            checksum: Some("ciphertext-checksum".to_string()),
            checksum_alg: Some("sha256".to_string()),
        }
    }

    #[test]
    fn rclone_decrypted_size_matches_encrypted_content_lengths() {
        let (_, data_key) = derive_keys("compare-size", "salt").unwrap();
        for size in [0usize, 1, 65_535, 65_536, 65_537, 200_000] {
            let plaintext: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();
            assert_eq!(
                rclone_decrypted_size(encrypted.len() as u64),
                plaintext.len() as u64,
                "encrypted length {} should map back to plaintext length {}",
                encrypted.len(),
                plaintext.len()
            );
        }
    }

    #[test]
    fn guard_no_raw_crypt_write_refuses_only_when_crypt_capable_and_unwrapped() {
        let state = ProviderState::new();

        // Not crypt-capable at all: a raw write is fine (no overlay to corrupt).
        assert!(state.guard_no_raw_crypt_write("Upload").is_ok());

        // Crypt-capable AND wrapped: the live provider IS the crypt decorator, so
        // the write is mapped/encrypted transparently. Allowed.
        state.active_crypt_overlay.store(true, Ordering::SeqCst);
        state.overlay_wrapped.store(true, Ordering::SeqCst);
        assert!(state.guard_no_raw_crypt_write("Upload").is_ok());

        // Crypt-capable but UNWRAPPED (badge locked / stepped outside the
        // encrypted scope): a direct write hits the raw backend and would inject
        // plaintext into the encrypted store. Must fail closed.
        state.overlay_wrapped.store(false, Ordering::SeqCst);
        let err = state
            .guard_no_raw_crypt_write("Upload")
            .expect_err("unwrapped crypt-capable session must refuse a raw write");
        assert!(
            err.contains("crypt overlay"),
            "guard error should explain the crypt overlay block, got: {err}"
        );

        // Clearing capability re-opens the raw path (e.g. after disconnect).
        state.active_crypt_overlay.store(false, Ordering::SeqCst);
        assert!(state.guard_no_raw_crypt_write("Upload").is_ok());
    }

    #[test]
    fn arm_crypt_capability_fails_closed_until_wrapped() {
        let state = ProviderState::new();

        assert!(state.guard_no_raw_crypt_write("Upload").is_ok());

        state.arm_crypt_capability();
        let err = state
            .guard_no_raw_crypt_write("Upload")
            .expect_err("armed crypt-capable session must refuse raw writes until wrapped");
        assert!(
            err.contains("crypt overlay"),
            "guard error should explain the crypt overlay block, got: {err}"
        );

        state.overlay_wrapped.store(true, Ordering::SeqCst);
        assert!(state.guard_no_raw_crypt_write("Upload").is_ok());
    }

    #[test]
    fn decrypt_rel_rclone_decrypts_all_segments_when_directory_names_are_encrypted() {
        let keys = rclone_compare_keys(true);
        let encrypted_rel = ["alpha", "beta", "report.txt"]
            .into_iter()
            .map(|segment| encrypt_name(&keys.name_key, &keys.name_tweak, segment).unwrap())
            .collect::<Vec<_>>()
            .join("/");

        assert_eq!(
            decrypt_rel_rclone(&keys, &encrypted_rel).as_deref(),
            Some("alpha/beta/report.txt")
        );
    }

    #[test]
    fn decrypt_rel_rclone_decrypts_only_leaf_when_directory_names_are_plain() {
        let keys = rclone_compare_keys(false);
        let encrypted_leaf = encrypt_name(&keys.name_key, &keys.name_tweak, "report.txt").unwrap();
        let mixed_rel = format!("alpha/beta/{}", encrypted_leaf);

        assert_eq!(
            decrypt_rel_rclone(&keys, &mixed_rel).as_deref(),
            Some("alpha/beta/report.txt")
        );
    }

    #[test]
    fn rclone_compare_normalization_drops_foreign_names_and_maps_size() {
        let keys = rclone_compare_keys(true);
        let encrypted_leaf = encrypt_name(&keys.name_key, &keys.name_tweak, "report.txt").unwrap();
        let encrypted_blob = encrypt_file_content(b"report body", &keys.data_key).unwrap();
        let mut entries = HashMap::new();
        entries.insert(
            encrypted_leaf,
            compare_file_info(encrypted_blob.len() as u64),
        );
        entries.insert("not-base32-!!!".to_string(), compare_file_info(999));

        let normalized = normalize_rclone_remote_files_for_compare(&keys, entries);

        assert_eq!(normalized.len(), 1);
        let info = normalized.get("report.txt").unwrap();
        assert_eq!(info.name, "report.txt");
        assert_eq!(info.size, 11);
        assert_eq!(info.checksum, None);
        assert_eq!(info.checksum_alg, None);
    }

    #[test]
    fn aerocrypt_compare_normalization_decrypts_names_and_defers_size() {
        let master_key = [7u8; 32];
        let encrypted_rel = ["alpha", "beta", "report.txt"]
            .into_iter()
            .map(|segment| crate::aerocrypt::names::encrypt_filename(&master_key, segment).unwrap())
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            decrypt_rel_aerocrypt(&master_key, &encrypted_rel).as_deref(),
            Some("alpha/beta/report.txt")
        );

        let mut entries = HashMap::new();
        entries.insert(encrypted_rel, compare_file_info(123));
        entries.insert("not-base64-$$$".to_string(), compare_file_info(999));

        let normalized = normalize_aerocrypt_remote_files_for_compare(&master_key, entries);

        assert_eq!(normalized.len(), 1);
        let info = normalized.get("alpha/beta/report.txt").unwrap();
        assert_eq!(info.name, "report.txt");
        assert_eq!(info.size, 123);
        assert_eq!(info.checksum, None);
        assert_eq!(info.checksum_alg, None);
    }

    #[test]
    fn test_connection_cancel_registry_register_and_cleanup() {
        let registry = ConnectionCancelRegistry::new();
        assert_eq!(registry.active_count(), 0);
        let token = registry.register("conn-1");
        assert_eq!(registry.active_count(), 1);
        assert!(!token.is_cancelled());
        // The drop guard mirrors what the connect command does on every exit
        // path: removing the token so the map cannot accumulate stale entries.
        registry.unregister("conn-1");
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn test_connection_cancel_registry_cancel_signals_token() {
        let registry = ConnectionCancelRegistry::new();
        let token = registry.register("conn-2");
        assert!(registry.cancel("conn-2"), "live token must report found");
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_connection_cancel_registry_cancel_unknown_is_idempotent() {
        let registry = ConnectionCancelRegistry::new();
        // Cancelling a token that was never registered is a no-op returning false.
        assert!(!registry.cancel("missing"));
        // Once the entry is de-registered, a late cancel stays false (no panic).
        let token = registry.register("conn-3");
        assert!(registry.cancel("conn-3"));
        registry.unregister("conn-3");
        assert!(!registry.cancel("conn-3"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_connection_cancel_registry_reregister_cancels_previous() {
        let registry = ConnectionCancelRegistry::new();
        let first = registry.register("conn-4");
        // A retried attempt reusing the same id must not leave two live tokens.
        let second = registry.register("conn-4");
        assert!(
            first.is_cancelled(),
            "stale token must be cancelled on re-register"
        );
        assert!(!second.is_cancelled());
        assert_eq!(registry.active_count(), 1);
    }

    #[tokio::test]
    async fn test_select_cancel_returns_marker_and_guard_cleans_up() {
        let registry = ConnectionCancelRegistry::new();
        let key = "conn-select";
        let token = registry.register(key);
        let guard = ConnectTokenGuard::new(&registry, key.to_string());
        assert_eq!(registry.active_count(), 1);

        // A connect that never resolves; the cancel branch must win the select
        // exactly as it does in provider_connect / connect_ftp.
        let never = std::future::pending::<Result<(), String>>();
        registry.cancel(key);
        let outcome: Result<(), String> = tokio::select! {
            res = never => res,
            _ = token.cancelled() => Err(CONNECT_CANCELLED.to_string()),
        };
        assert_eq!(outcome.unwrap_err(), CONNECT_CANCELLED);

        drop(guard);
        assert_eq!(
            registry.active_count(),
            0,
            "guard must de-register the token on drop"
        );
    }

    #[tokio::test]
    async fn test_run_cancellable_connect_cancel_returns_marker() {
        // #360: an in-flight connect phase (here a never-resolving future)
        // aborts with CONNECT_CANCELLED once its token is cancelled, and the
        // token is de-registered afterwards.
        let registry = ConnectionCancelRegistry::new();
        let key = "conn-helper-cancel";
        let fut = run_cancellable_connect::<(), _>(&registry, Some(key), async {
            std::future::pending::<Result<(), String>>().await
        });
        tokio::pin!(fut);
        // Let the helper register + start polling, then cancel.
        tokio::select! {
            _ = &mut fut => panic!("future must not resolve before cancel"),
            _ = tokio::task::yield_now() => {}
        }
        assert_eq!(registry.active_count(), 1);
        assert!(registry.cancel(key));
        assert_eq!(fut.await.unwrap_err(), CONNECT_CANCELLED);
        assert_eq!(registry.active_count(), 0, "guard must de-register on drop");
    }

    #[tokio::test]
    async fn test_run_cancellable_listing_cancel_returns_marker_and_drops_future() {
        // The panel spinner's Cancel must abort a listing that never resolves,
        // and dropping the future is what releases the provider mutex the stuck
        // listing was holding, so the follow-up disconnect does not queue behind
        // it. Assert both: the marker comes back, and the future was dropped.
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let state = ListingCancelState::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(Arc::clone(&dropped));
        let fut = run_cancellable_listing::<(), _>(&state, async move {
            let _guard = flag;
            std::future::pending::<Result<(), String>>().await
        });
        tokio::pin!(fut);
        tokio::select! {
            _ = &mut fut => panic!("listing must not resolve before cancel"),
            _ = tokio::task::yield_now() => {}
        }
        assert!(state.is_armed(), "an in-flight listing must be cancellable");
        assert!(state.cancel(), "cancel must find the armed token");
        assert_eq!(fut.await.unwrap_err(), LISTING_CANCELLED);
        assert!(
            dropped.load(Ordering::SeqCst),
            "the aborted listing future must be dropped, freeing the provider lock"
        );
        assert!(!state.is_armed(), "guard must disarm the token on drop");
    }

    #[tokio::test]
    async fn test_run_cancellable_listing_passes_result_through_and_disarms() {
        let state = ListingCancelState::new();
        let outcome = run_cancellable_listing(&state, async { Ok::<u32, String>(7) }).await;
        assert_eq!(outcome.unwrap(), 7);
        assert!(!state.is_armed());
        // Nothing in flight: a Cancel click that lands late is a no-op, never an
        // error the UI has to special-case.
        assert!(!state.cancel());
    }

    #[tokio::test]
    async fn test_listing_cancel_state_stale_guard_never_disarms_newer_listing() {
        // A listing that resolves after a newer one took the slot must not clear
        // the newer one's token: the Cancel button would then signal nothing.
        let state = ListingCancelState::new();
        let (stale_id, _stale_token) = state.arm();
        let (_fresh_id, fresh_token) = state.arm();
        state.disarm(stale_id);
        assert!(
            state.is_armed(),
            "stale disarm must not clear the live token"
        );
        assert!(state.cancel());
        assert!(fresh_token.is_cancelled());
    }

    #[tokio::test]
    async fn test_run_cancellable_connect_without_token_runs_to_completion() {
        // No token: the future simply runs and its result passes through, with
        // nothing registered (the OAuth/MEGAcmd opt-out path).
        let registry = ConnectionCancelRegistry::new();
        let outcome: Result<u32, String> =
            run_cancellable_connect(&registry, None, async { Ok(42) }).await;
        assert_eq!(outcome.unwrap(), 42);
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn test_s3_provider_params_preserve_absent_path_style() {
        let config = s3_params(None).to_provider_config().unwrap();
        assert!(!config.extra.contains_key("path_style"));
    }

    #[test]
    fn test_s3_provider_params_preserve_explicit_virtual_host_style() {
        let config = s3_params(Some(false)).to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("path_style").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn test_s3_provider_params_absent_session_token() {
        let config = s3_params(None).to_provider_config().unwrap();
        assert!(!config.extra.contains_key("session_token"));
    }

    #[test]
    fn test_s3_provider_params_forward_session_token() {
        let mut params = s3_params(None);
        params.session_token = Some("FwoGZXIvYXdzEXAMPLEtoken==".to_string());
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("session_token").map(String::as_str),
            Some("FwoGZXIvYXdzEXAMPLEtoken==")
        );
    }

    #[test]
    fn test_s3_provider_params_trim_blank_session_token() {
        let mut params = s3_params(None);
        params.session_token = Some("   ".to_string());
        let config = params.to_provider_config().unwrap();
        assert!(!config.extra.contains_key("session_token"));
    }

    #[test]
    fn test_s3_provider_params_absent_assume_role() {
        let config = s3_params(None).to_provider_config().unwrap();
        assert!(!config.extra.contains_key("role_arn"));
        assert!(!config.extra.contains_key("role_external_id"));
        assert!(!config.extra.contains_key("role_session_name"));
        assert!(!config.extra.contains_key("role_duration_seconds"));
    }

    #[test]
    fn test_s3_provider_params_forward_assume_role() {
        let mut params = s3_params(None);
        params.role_arn = Some("arn:aws:iam::123456789012:role/Demo".to_string());
        params.role_external_id = Some("ext-42".to_string());
        params.role_session_name = Some("team-sync".to_string());
        params.role_duration_seconds = Some(7200);
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("role_arn").map(String::as_str),
            Some("arn:aws:iam::123456789012:role/Demo")
        );
        assert_eq!(
            config.extra.get("role_external_id").map(String::as_str),
            Some("ext-42")
        );
        assert_eq!(
            config.extra.get("role_session_name").map(String::as_str),
            Some("team-sync")
        );
        assert_eq!(
            config
                .extra
                .get("role_duration_seconds")
                .map(String::as_str),
            Some("7200")
        );
    }

    #[test]
    fn test_s3_provider_params_trim_blank_role_arn() {
        let mut params = s3_params(None);
        params.role_arn = Some("   ".to_string());
        let config = params.to_provider_config().unwrap();
        assert!(!config.extra.contains_key("role_arn"));
    }

    #[test]
    fn test_backblaze_provider_params_forward_bucket() {
        let mut params = s3_params(None);
        params.protocol = "backblaze".to_string();
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("bucket").map(String::as_str),
            Some("garage-bucket")
        );
    }

    #[test]
    fn test_webdav_provider_params_forward_anonymous() {
        let mut params = s3_params(None);
        params.protocol = "webdav".to_string();
        params.bucket = None;
        params.anonymous = Some(true);
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("anonymous").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn test_provider_params_forward_provider_id() {
        let mut params = s3_params(None);
        params.protocol = "webdav".to_string();
        params.bucket = None;
        params.provider_id = Some("nextcloud".to_string());
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("provider_id").map(String::as_str),
            Some("nextcloud")
        );
    }

    // SEC-GH-002: Exact repo matching with boundary detection
    #[test]
    fn test_remote_matches_exact_ssh() {
        assert!(remote_matches_repo(
            "origin\tgit@github.com:axpdev-lab/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_matches_exact_https() {
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_matches_without_git_suffix() {
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_prefix_collision() {
        // "aeroftp" should NOT match "aeroftp-old"
        assert!(!remote_matches_repo(
            "origin\tgit@github.com:axpdev-lab/aeroftp-old.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_different_owner() {
        assert!(!remote_matches_repo(
            "origin\tgit@github.com:other-org/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_case_insensitive() {
        assert!(remote_matches_repo(
            "origin\tgit@GitHub.com:AxpDev-Lab/AeroFTP.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_empty_line() {
        assert!(!remote_matches_repo("", "axpdev-lab", "aeroftp"));
    }

    #[test]
    fn test_remote_matches_end_of_line_boundary() {
        // URL ends at whitespace (fetch/push marker)
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp (push)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    // ============ Issue #233: in-flight transfer drain ============

    #[tokio::test]
    async fn transfer_operation_guard_tracks_in_flight_counter() {
        let state = ProviderState::new();
        assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 0);
        {
            let _g1 = TransferOperationGuard::acquire(&state);
            assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 1);
            let _g2 = TransferOperationGuard::acquire(&state);
            assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 2);
        }
        assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn drain_in_flight_transfers_returns_immediately_when_zero() {
        let state = ProviderState::new();
        let started = Instant::now();
        drain_in_flight_transfers(&state, Duration::from_secs(5)).await;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "drain on idle state must not wait, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn drain_in_flight_transfers_waits_for_guard_drop() {
        let state = Arc::new(ProviderState::new());
        let guard = TransferOperationGuard::acquire(&state);
        let state_clone = Arc::clone(&state);
        // Drop the guard after a short delay; drain must observe the
        // decrement (via the notify) and return promptly after.
        let dropper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(guard);
            // touch state_clone so the borrow lives long enough on
            // older rustc versions
            let _ = state_clone.in_flight_transfers.load(Ordering::SeqCst);
        });

        let started = Instant::now();
        drain_in_flight_transfers(&state, Duration::from_secs(5)).await;
        let elapsed = started.elapsed();
        dropper.await.unwrap();

        assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 0);
        assert!(
            elapsed >= Duration::from_millis(80),
            "drain must wait for the guard, only waited {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "drain must wake up promptly after the notify, took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn drain_in_flight_transfers_returns_on_timeout_when_held() {
        let state = ProviderState::new();
        let _held = TransferOperationGuard::acquire(&state);
        let started = Instant::now();
        drain_in_flight_transfers(&state, Duration::from_millis(150)).await;
        let elapsed = started.elapsed();
        // The guard is still alive on purpose; the drain must give up
        // after roughly the timeout (the warn log is best-effort, the
        // function MUST return so the caller can proceed).
        assert_eq!(state.in_flight_transfers.load(Ordering::SeqCst), 1);
        assert!(
            elapsed >= Duration::from_millis(140),
            "drain returned before its timeout, only waited {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "drain returned much later than its timeout: {:?}",
            elapsed
        );
    }
}
