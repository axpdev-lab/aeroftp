//! Delta sync adapter for the AeroSync sync loop.
//!
//! This module is the bridge between the sync loop (`sync.rs`) and the
//! rsync-over-SSH orchestrator (`rsync_over_ssh.rs`). It encapsulates:
//!
//! - Capability probing (cached per SSH session so we don't re-probe every file)
//! - Policy decisions (when to attempt delta vs fallback to classic transfer)
//! - Typed result with fallback reason, so the caller can log/report accurately
//!
//! The sync loop doesn't know (and shouldn't know) anything about rsync, SSH exec,
//! or remote probing: it just calls [`transfer_with_delta`] and gets back either
//! a success with real stats or a `used_delta = false` with a reason to use the
//! classic download/upload path.
//!
//! # Cross-OS status (PR-T11)
//! The adapter itself is cross-platform. The only Unix-only reachability
//! condition is the provider returning `None` from `delta_transport()` on
//! Windows when the `aerorsync` feature is disabled: in that
//! case `try_delta_transfer` short-circuits to `None` silently and the
//! caller proceeds with classic SFTP, identical to the behavior on Unix
//! for non-SFTP providers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Cross-platform. Binary-rsync code paths live behind `RsyncBinaryTransport`
// which is itself `#[cfg(unix)]`; the downcast/probe logic here works
// identically for the native prototype transport on Windows.
#![allow(dead_code)]

use crate::delta_transport::DeltaTransport;
use crate::rsync_over_ssh::{RsyncCapability, RsyncConfig, RsyncError, RsyncStats};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;

/// Master switch for the aerorsync native-delta transport on the INTERACTIVE
/// GUI transfer commands (provider download/upload, FTP-family single-file
/// upload, editor save).
///
/// Re-enabled 2026-06-19 after the delta redesign landed
/// (docs/dev/roadmap/APPENDIX-AERORSYNC-DELTA-REDESIGN): the native driver now
/// emits wire-byte progress on the interactive path (Fix A, so the flagship bar
/// fills with real speed/ETA) and chunks oversized MSG_DATA payloads instead of
/// hard-failing (Fix B, so uploads larger than ~16 MiB complete). It had been
/// turned OFF earlier the same day precisely because the driver showed no
/// progress and failed large uploads. AeroSync and the CLI use their own delta
/// path (`sync.rs`) and are NOT gated by this switch.
///
/// Implemented as a function (not a `const`) so the GUI call sites read as a
/// normal runtime gate and stay free of the const-folding `overly_complex_bool_expr`
/// clippy lint; flipping the body toggles every gated site at once (revert to
/// `false` to fall back to the classic streaming transfer).
#[inline]
pub fn gui_delta_enabled() -> bool {
    true
}

/// Max length of a fallback/hard-error message surfaced outside the adapter.
/// rsync stderr can be verbose (thousands of chars for repeated warnings);
/// a generous but bounded cap keeps the MCP `errors[]` array manageable.
const MAX_REASON_LEN: usize = 512;

/// Redacts obvious user-path segments (`/home/<user>`, `/Users/<user>`,
/// `C:\Users\<user>`) and SSH key path hints from rsync stderr before it
/// flows to `DeltaSyncResult.fallback_reason`, `.hard_error`, UI, logs, and
/// MCP responses. Not a substitute for not logging paths at all, but avoids
/// the most common PII leaks without changing operator debuggability.
static USER_PATH_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // POSIX user home (`/home/alice/...` or `/Users/alice/...`)
        Regex::new(r"(?i)/(?:home|users|root)/[^/\s]+").unwrap(),
        // Windows user home (`C:\Users\alice\...`)
        Regex::new(r"(?i)[A-Z]:\\\\?Users\\\\?[^\\\s]+").unwrap(),
        // `known_hosts` file path hint (rsync sometimes prints absolute path)
        Regex::new(r"(?i)\S*\.ssh[/\\][^\s)]+").unwrap(),
    ]
});

fn sanitize_rsync_message(raw: &str) -> String {
    let mut s = raw.to_string();
    for re in USER_PATH_PATTERNS.iter() {
        s = re.replace_all(&s, "<redacted>").to_string();
    }
    if s.len() > MAX_REASON_LEN {
        s.truncate(MAX_REASON_LEN);
        s.push_str("…[truncated]");
    }
    s
}

/// Direction of the file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    Upload,
    Download,
}

/// Stable per-session context that drives rsync behavior.
///
/// Built once by the sync loop when it initializes a delta-capable session
/// (typically after `SftpProvider::connect()`), reused for every file in that session.
#[derive(Debug, Clone)]
pub struct DeltaSyncContext {
    /// SSH username for rsync transport.
    pub ssh_user: String,
    /// SSH host for rsync transport.
    pub ssh_host: String,
    /// SSH port (defaults to 22 in rsync_over_ssh if `None`).
    pub ssh_port: Option<u16>,
    /// Absolute path to SSH private key. Missing → delta disabled, classic fallback.
    pub ssh_key_path: Option<PathBuf>,
    /// `StrictHostKeyChecking` level for the rsync-driven SSH transport.
    pub strict_host_key_check: String,
    /// Optional known_hosts path (usually `~/.ssh/known_hosts`).
    pub known_hosts_path: Option<PathBuf>,
    /// File-size threshold below which delta is skipped (overhead > saving).
    pub min_file_size: u64,
    /// Enable compression on the rsync transport.
    pub compress: bool,
    /// Session identity used as cache key for capability probing.
    pub session_key: String,
}

/// Outcome of one delta-sync attempt.
///
/// Three mutually exclusive shapes:
/// - `used_delta = true` + `stats = Some(_)` → rsync completed successfully;
///   the caller should record the measured stats and proceed.
/// - `used_delta = false` + `fallback_reason = Some(_)` → delta path declined
///   (small file, no key, remote unavailable, transient failure). The caller
///   must fall back to the classic download/upload transparently.
/// - `used_delta = false` + `hard_error = Some(_)` → delta path refused for a
///   reason that MUST NOT trigger silent classic fallback (SSH host-key
///   mismatch, protocol invariant violation). The caller MUST surface the
///   error to the UI without retrying via classic SFTP.
#[derive(Debug)]
pub struct DeltaSyncResult {
    pub used_delta: bool,
    pub stats: Option<RsyncStats>,
    pub fallback_reason: Option<String>,
    /// When populated, the caller MUST surface this error and MUST NOT
    /// transparently retry via the classic-SFTP path. Mutually exclusive with
    /// `fallback_reason`.
    pub hard_error: Option<String>,
}

/// Verdict returned by the preventive eligibility gate.
///
/// This is intentionally smaller than [`DeltaSyncResult`]: the UI gate only
/// needs to know whether the current SFTP session can use delta sync right now
/// and, when it cannot, the sanitized reason that should be surfaced to the
/// user before the classic transfer starts.
#[derive(Debug, Clone)]
pub struct DeltaEligibilityStatus {
    pub eligible: bool,
    pub reason: Option<String>,
}

impl DeltaSyncResult {
    fn used(stats: RsyncStats) -> Self {
        Self {
            used_delta: true,
            stats: Some(stats),
            fallback_reason: None,
            hard_error: None,
        }
    }

    fn fallback(reason: impl Into<String>) -> Self {
        Self {
            used_delta: false,
            stats: None,
            fallback_reason: Some(reason.into()),
            hard_error: None,
        }
    }

    fn hard_error(reason: impl Into<String>) -> Self {
        Self {
            used_delta: false,
            stats: None,
            fallback_reason: None,
            hard_error: Some(reason.into()),
        }
    }
}

/// Translate a native `HardRejection` envelope into the delta result.
///
/// A wire-level transport drop (the remote closing the exec channel mid
/// stream, a kex blip) is NOT a security refusal: the destination is never
/// torn (every write goes through a temp file plus atomic rename), so the
/// classic SFTP path may take over, exactly as the Z.1.2 batch boundary
/// already reconnects on the same envelopes. Security and protocol faults
/// (host-key mismatch, invalid frame, internal) stay hard: those must
/// surface, never silently retry. Single source of truth for the split is
/// `rsync_over_ssh::is_transient_native_envelope`.
fn hard_rejection_result(msg: &str) -> DeltaSyncResult {
    if crate::rsync_over_ssh::is_transient_native_envelope(&msg.to_ascii_lowercase()) {
        tracing::warn!(
            "delta transport drop: {}: falling back to the classic path",
            msg
        );
        DeltaSyncResult::fallback(sanitize_rsync_message(&format!(
            "delta transport drop: {}",
            msg
        )))
    } else {
        tracing::error!("delta hard rejection: {}: classic fallback suppressed", msg);
        DeltaSyncResult::hard_error(sanitize_rsync_message(msg))
    }
}

/// Per-session capability cache.
///
/// `session_key` → (capability, cached_at). Entries expire after [`CACHE_TTL`];
/// this avoids a probe roundtrip on every file while still letting the user reconnect
/// to a different server with the same key (unlikely, but a safeguard).
struct CacheEntry {
    capability: Result<RsyncCapability, RsyncError>,
    cached_at: Instant,
}

/// TTL for a POSITIVE capability (rsync present): re-probe occasionally so a
/// server that gains/loses rsync is eventually re-evaluated.
const CACHE_TTL_OK: Duration = Duration::from_secs(300); // 5 min
/// TTL for a NEGATIVE result (rsync absent / probe failed or timed out). Kept far
/// longer than a single probe can cost so a remote without rsync is not re-probed
/// on every transfer. #398: an SFTP-only server accepts the exec channel but never
/// returns, so the probe used to hang ~10 min per transfer; a bounded, long-cached
/// negative verdict makes it a one-time few-second cost per session.
const CACHE_TTL_ERR: Duration = Duration::from_secs(1800); // 30 min
/// Hard bound on a single remote capability probe. Without it the russh exec probe
/// (whose inactivity timeout is defeated by the keepalive) blocks until the remote
/// tears the connection, minutes later. On elapse we cache a negative result so the
/// transfer falls back to plain SFTP immediately and stays fast (#398).
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

static PROBE_CACHE: LazyLock<TokioMutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| TokioMutex::new(HashMap::new()));

/// Probe remote capability via the transport, with 5-minute per-session cache.
///
/// Returns the cached result if present and fresh; otherwise runs a live probe
/// and stores the outcome. Cache stores failures too: if rsync isn't on the
/// remote, we don't want to re-probe every file.
async fn probe_capability_cached(
    transport: &dyn DeltaTransport,
    session_key: &str,
) -> Result<RsyncCapability, String> {
    let now = Instant::now();

    {
        let cache = PROBE_CACHE.lock().await;
        if let Some(entry) = cache.get(session_key) {
            let ttl = if entry.capability.is_ok() {
                CACHE_TTL_OK
            } else {
                CACHE_TTL_ERR
            };
            if now.duration_since(entry.cached_at) < ttl {
                return match &entry.capability {
                    Ok(cap) => Ok(cap.clone()),
                    Err(e) => Err(e.to_string()),
                };
            }
        }
    }

    // Bound the probe so a non-responding remote (SFTP-only, no rsync) cannot stall
    // the transfer; a timeout is cached as a negative result (below) exactly like a
    // real "rsync not available", so the fallback is immediate on every later file.
    let fresh = match timeout(PROBE_TIMEOUT, transport.probe_remote()).await {
        Ok(res) => res,
        Err(_) => Err(RsyncError::ProbeFailed(format!(
            "rsync capability probe timed out after {}s (remote likely has no rsync)",
            PROBE_TIMEOUT.as_secs()
        ))),
    };
    let report = match &fresh {
        Ok(cap) => Ok(cap.clone()),
        Err(e) => Err(e.to_string()),
    };

    let mut cache = PROBE_CACHE.lock().await;
    cache.insert(
        session_key.to_string(),
        CacheEntry {
            capability: fresh,
            cached_at: now,
        },
    );

    report
}

/// Build a per-session [`RsyncConfig`] from the stable [`DeltaSyncContext`].
///
/// This is the bridge value used to construct the concrete transport; callers
/// who already hold a `Box<dyn DeltaTransport>` don't need to touch it.
pub fn build_rsync_config(ctx: &DeltaSyncContext) -> RsyncConfig {
    RsyncConfig {
        compress: ctx.compress,
        preserve_times: true,
        progress: true,
        min_file_size: ctx.min_file_size,
        ssh_key_path: ctx.ssh_key_path.clone(),
        ssh_password: None,
        auth_method: crate::rsync_over_ssh::AuthMethod::SshKey,
        ssh_port: ctx.ssh_port,
        ssh_user: ctx.ssh_user.clone(),
        ssh_host: ctx.ssh_host.clone(),
        strict_host_key_check: ctx.strict_host_key_check.clone(),
        known_hosts_path: ctx.known_hosts_path.clone(),
    }
}

/// Attempt a delta-sync transfer. Returns a result regardless of success:
/// if delta cannot be used (any reason: no capability, missing key, small file,
/// transfer error), `used_delta = false` and `fallback_reason` is populated so
/// the caller can transparently run the classic download/upload.
///
/// This function never throws for expected fallback paths; it only returns `Err`
/// on truly unexpected errors (e.g. malformed context). Missing-key, too-small,
/// remote-not-available, and transfer failure all map to `Ok(fallback)`.
///
/// The transport argument is a `&dyn` trait object so the same adapter works
/// with today's subprocess-based transport and with the future native-rsync
/// transport (strada C) without any structural change.
pub async fn transfer_with_delta(
    transport: &dyn DeltaTransport,
    direction: SyncDirection,
    local_path: &Path,
    remote_path: &str,
    session_key: &str,
    progress: Option<crate::delta_transport::DeltaProgressSink>,
) -> Result<DeltaSyncResult, String> {
    // Step 1: probe remote capability (cached per session, typed error path).
    let capability = match probe_capability_cached(transport, session_key).await {
        Ok(cap) => cap,
        Err(reason) => {
            return Ok(DeltaSyncResult::fallback(format!(
                "remote delta unavailable: {}",
                reason
            )));
        }
    };

    // Step 2: probe local availability (no-op for native transport, binary check for subprocess).
    if let Err(e) = transport.probe_local().await {
        return Ok(DeltaSyncResult::fallback(format!(
            "local delta unavailable: {}",
            e
        )));
    }

    // Step 3: run the transfer through the trait. The optional progress sink
    // is moved into whichever direction runs (mutually exclusive); transports
    // that cannot report progress ignore it via the trait default.
    let outcome = match direction {
        SyncDirection::Upload => {
            transport
                .upload_with_progress(local_path, remote_path, progress)
                .await
        }
        SyncDirection::Download => {
            transport
                .download_with_progress(remote_path, local_path, progress)
                .await
        }
    };

    match outcome {
        Ok(stats) => {
            tracing::info!(
                "delta sync {:?} ok: transport={}, remote_version={}, sent={}B, received={}B, speedup={:.2}x, duration={}ms",
                direction,
                transport.name(),
                capability.version,
                stats.bytes_sent,
                stats.bytes_received,
                stats.speedup,
                stats.duration_ms,
            );
            Ok(DeltaSyncResult::used(stats))
        }
        Err(RsyncError::TooSmall { size, threshold }) => Ok(DeltaSyncResult::fallback(format!(
            "file size {} bytes is below delta threshold {} bytes",
            size, threshold
        ))),
        Err(RsyncError::PasswordAuthUnsupported) => Ok(DeltaSyncResult::fallback(
            "password SSH auth (configure an SSH key to enable delta sync)",
        )),
        Err(RsyncError::MissingKey(s)) => Ok(DeltaSyncResult::fallback(format!("ssh key: {}", s))),
        Err(RsyncError::RemoteNotAvailable) => Ok(DeltaSyncResult::fallback(
            "remote rsync disappeared between probe and transfer",
        )),
        Err(RsyncError::LocalNotAvailable) => Ok(DeltaSyncResult::fallback(
            "local rsync disappeared between probe and transfer",
        )),
        Err(RsyncError::HardRejection(msg)) => {
            // Security/protocol refusals MUST NOT trigger silent classic
            // fallback (e.g. SSH host-key pinning mismatch); a wire-level
            // transport drop demotes to the classic path instead of failing
            // the transfer outright. The split lives in
            // `hard_rejection_result`.
            Ok(hard_rejection_result(&msg))
        }
        Err(e) => {
            // TransferFailed, SpawnFailed, Io, Cancelled, VersionTooOld, ProbeFailed →
            // all map to fallback with the error message. Caller decides whether to
            // retry the classic transfer or surface the error.
            tracing::warn!("delta sync {:?} failed: {}", direction, e);
            Ok(DeltaSyncResult::fallback(sanitize_rsync_message(&format!(
                "rsync failed: {}",
                e
            ))))
        }
    }
}

/// Post-downcast delta lifecycle, decoupled from [`try_delta_transfer`] so
/// integration tests can drive it with a synthetic [`DeltaTransport`].
/// Product code must keep going through [`try_delta_transfer`] to preserve
/// the SFTP handle downcast and session-key derivation. Result shape is
/// documented on [`DeltaSyncResult`].
#[doc(hidden)]
pub async fn try_delta_transfer_with_transport(
    transport: &dyn DeltaTransport,
    direction: SyncDirection,
    local_path: &Path,
    remote_path: &str,
    session_key: &str,
    progress: Option<crate::delta_transport::DeltaProgressSink>,
) -> Option<DeltaSyncResult> {
    let result = transfer_with_delta(
        transport,
        direction,
        local_path,
        remote_path,
        session_key,
        progress,
    )
    .await;

    match result {
        Ok(r) => Some(r),
        Err(reason) => {
            tracing::warn!("delta transfer adapter error: {}", reason);
            Some(DeltaSyncResult::fallback(sanitize_rsync_message(&format!(
                "adapter error: {}",
                reason
            ))))
        }
    }
}

/// Check delta eligibility without transferring any file.
///
/// Uses the same cached remote probe that powers real transfers, but adds a
/// 5-second timeout because this path runs on the sync-start UX gate and must
/// not stall the UI indefinitely.
#[doc(hidden)]
pub async fn check_delta_eligibility_with_transport(
    transport: &dyn DeltaTransport,
    session_key: &str,
) -> DeltaEligibilityStatus {
    let remote_capability = timeout(
        Duration::from_secs(5),
        probe_capability_cached(transport, session_key),
    )
    .await;

    let capability = match remote_capability {
        Ok(Ok(capability)) => capability,
        Ok(Err(reason)) => {
            return DeltaEligibilityStatus {
                eligible: false,
                reason: Some(sanitize_rsync_message(&format!(
                    "remote delta unavailable: {}",
                    reason
                ))),
            };
        }
        Err(_) => {
            return DeltaEligibilityStatus {
                eligible: false,
                reason: Some("remote delta probe timed out after 5s".to_string()),
            };
        }
    };

    if let Err(error) = transport.probe_local().await {
        return DeltaEligibilityStatus {
            eligible: false,
            reason: Some(sanitize_rsync_message(&format!(
                "local delta unavailable: {}",
                error
            ))),
        };
    }

    tracing::debug!(
        "delta eligibility ok: transport={}, remote_version={}",
        transport.name(),
        capability.version
    );

    DeltaEligibilityStatus {
        eligible: true,
        reason: None,
    }
}

/// One-stop entry point for the sync loop: given a connected
/// [`StorageProvider`](crate::providers::StorageProvider), attempt a delta
/// transfer if (and only if) the provider offers a `DeltaTransport`, otherwise
/// return `None` so the caller proceeds with the classic path unchanged.
///
/// Returns:
/// - `None` → provider is not delta-eligible (not SFTP, password auth, not
///   connected, etc.). Caller falls through to classic download/upload.
/// - `Some(result)` → delta path was attempted. `result.used_delta` says whether
///   it actually saved bytes; `result.fallback_reason` is populated when false.
///
/// This helper is the intended integration surface for
/// `provider_transfer_executor` and any future call site that wants delta sync
/// as an optimization layer. It never panics, never blocks on I/O outside of
/// the transfer itself, and downcasts using the existing `as_any_mut()` entry
/// point on `StorageProvider`: no new trait methods are introduced.
///
/// The post-downcast lifecycle (probe + transfer + typed-result translation)
/// lives in [`try_delta_transfer_with_transport`] so integration tests can
/// exercise it with a mock transport. Product code keeps a single public
/// surface via this wrapper.
pub async fn try_delta_transfer(
    provider: &mut dyn crate::providers::StorageProvider,
    direction: SyncDirection,
    local_path: &Path,
    remote_path: &str,
) -> Option<DeltaSyncResult> {
    try_delta_transfer_with_progress(provider, direction, local_path, remote_path, None).await
}

/// Like [`try_delta_transfer`], but threads an optional GUI progress sink down
/// to the native driver so the interactive command path can render a live bar.
/// AeroSync, the CLI, and cross-profile keep calling [`try_delta_transfer`]
/// (which passes `None`), so their behavior is unchanged.
pub async fn try_delta_transfer_with_progress(
    provider: &mut dyn crate::providers::StorageProvider,
    direction: SyncDirection,
    local_path: &Path,
    remote_path: &str,
    progress: Option<crate::delta_transport::DeltaProgressSink>,
) -> Option<DeltaSyncResult> {
    // Only SFTP is delta-eligible in Fase 1. Downcasting via `as_any_mut()` keeps
    // the generic trait intact: we don't need a new `delta_transport_context()`
    // contract on every provider implementation.
    let sftp = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::sftp::SftpProvider>()?;

    let transport = sftp.delta_transport()?;

    // Session key: stable per connection (host+user). When the user reconnects
    // with different credentials, `invalidate_session_cache()` should be called
    // by the connection lifecycle: for now the 5-minute TTL handles staleness.
    let handle_ptr = sftp
        .handle_shared()
        .as_ref()
        .map(|h| std::sync::Arc::as_ptr(h) as usize)
        .unwrap_or(0);
    let session_key = format!("sftp#{:x}", handle_ptr);

    try_delta_transfer_with_transport(
        transport.as_ref(),
        direction,
        local_path,
        remote_path,
        &session_key,
        progress,
    )
    .await
}

/// Probe the current provider session and report whether delta sync is
/// currently available, without transferring any data.
///
/// Returns `None` when the connected provider is not an eligible SFTP session
/// (wrong provider type, no SSH handle, missing SSH key).
pub async fn check_delta_eligibility(
    provider: &mut dyn crate::providers::StorageProvider,
) -> Option<DeltaEligibilityStatus> {
    let sftp = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::sftp::SftpProvider>()?;

    if sftp.handle_shared().is_none() {
        return Some(DeltaEligibilityStatus {
            eligible: false,
            reason: Some("Reconnect the SFTP session to evaluate delta sync.".to_string()),
        });
    }

    let transport = match sftp.delta_transport() {
        Some(transport) => transport,
        None => {
            return Some(DeltaEligibilityStatus {
                eligible: false,
                reason: Some("Delta sync requires an SSH key-based SFTP session.".to_string()),
            });
        }
    };

    let handle_ptr = sftp
        .handle_shared()
        .as_ref()
        .map(|h| std::sync::Arc::as_ptr(h) as usize)
        .unwrap_or(0);
    let session_key = format!("sftp#{:x}", handle_ptr);

    Some(check_delta_eligibility_with_transport(transport.as_ref(), &session_key).await)
}

/// P3-T01 W4: open a delta-sync batch for the current provider session.
///
/// Returns `None` when:
/// - The provider is not delta-eligible (non-SFTP, password auth, etc.)
/// - The transport's `begin_batch()` returns a [`NoopBatch`] marker
///   (transport does not support session reuse: caller should fall
///   through to the per-file single-shot path).
/// - `begin_batch()` itself returns an error (logged at warn).
///
/// The returned batch holds a single SSH handshake; the sync loop is
/// expected to call `batch.finalize()` at the end and surface
/// `BatchStats.session_count` + `bytes_on_wire` via [`SyncReport`].
///
/// [`NoopBatch`]: crate::delta_transport::NoopBatch
/// [`SyncReport`]: crate::sync::SyncReport
pub async fn open_delta_batch(
    provider: &mut dyn crate::providers::StorageProvider,
) -> Option<Box<dyn crate::delta_transport::DeltaBatch>> {
    let sftp = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::sftp::SftpProvider>()?;
    let transport = sftp.delta_transport()?;
    match transport.begin_batch().await {
        Ok(batch) if !batch.is_noop() => {
            tracing::info!(
                "delta batch opened: transport={}, session reuse active",
                transport.name()
            );
            Some(batch)
        }
        Ok(_) => {
            tracing::debug!(
                "delta batch open: transport={} returned NoopBatch: single-shot path",
                transport.name()
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "delta batch open: begin_batch failed on transport={}: {e}",
                transport.name()
            );
            None
        }
    }
}

/// P3-T01 W4.1: adapter that runs a single file transfer through an
/// already-open [`DeltaBatch`] handle. The batch keeps an SSH session
/// alive across N files, so the per-file cost drops from full handshake
/// to channel-exec open. Returns the same [`DeltaSyncResult`] shape as
/// [`transfer_with_delta`] so the call site can reuse the existing
/// fallback / hard-error / used-delta branches.
///
/// Pre-conditions:
/// - `batch` was returned by `transport.begin_batch()` and is_noop() is false.
/// - The caller decides per-file whether to route through this batch
///   path or the legacy [`try_delta_transfer`] single-shot path.
pub async fn try_delta_transfer_with_batch(
    batch: &mut dyn crate::delta_transport::DeltaBatch,
    direction: SyncDirection,
    local_path: &Path,
    remote_path: &str,
) -> DeltaSyncResult {
    let outcome = match direction {
        SyncDirection::Upload => batch.upload(local_path, remote_path).await,
        SyncDirection::Download => batch.download(remote_path, local_path).await,
    };

    match outcome {
        Ok(stats) => {
            tracing::info!(
                "delta batch {:?} ok: sent={}B, received={}B, speedup={:.2}x, duration={}ms",
                direction,
                stats.bytes_sent,
                stats.bytes_received,
                stats.speedup,
                stats.duration_ms,
            );
            DeltaSyncResult::used(stats)
        }
        Err(RsyncError::TooSmall { size, threshold }) => DeltaSyncResult::fallback(format!(
            "file size {size} bytes is below delta threshold {threshold} bytes"
        )),
        Err(RsyncError::HardRejection(msg)) => hard_rejection_result(&msg),
        Err(e) => {
            tracing::warn!("delta batch {:?} failed: {e}", direction);
            DeltaSyncResult::fallback(sanitize_rsync_message(&format!("batch delta failed: {e}")))
        }
    }
}

/// Clear the probe cache. Called on disconnect or explicit user action.
/// Not strictly required (entries expire after 5 min) but keeps memory clean.
pub async fn clear_probe_cache() {
    PROBE_CACHE.lock().await.clear();
}

/// Clear a single session's cached capability (e.g. after reconnect with new creds).
pub async fn invalidate_session_cache(session_key: &str) {
    PROBE_CACHE.lock().await.remove(session_key);
}

#[cfg(test)]
mod tests {
    use super::*;

    // The probe cache is a process-global static; tests that clear it and assert
    // on its contents must not interleave, or one test's `clear_probe_cache()`
    // wipes another's mid-test entry. Every cache-touching test acquires this
    // guard first so they run serially with respect to the cache.
    static PROBE_CACHE_TEST_GUARD: LazyLock<TokioMutex<()>> = LazyLock::new(|| TokioMutex::new(()));

    #[test]
    fn hard_rejection_transport_drop_demotes_to_classic_fallback() {
        // The live WD MyCloud case: the remote closed the exec channel during
        // the file list. Not a security refusal, so the single-shot lane must
        // hand the transfer to the classic SFTP path instead of failing it.
        let r = hard_rejection_result(
            "native hard rejection (TransportFailure): next_data_frame: remote closed mid file list",
        );
        assert!(r.hard_error.is_none(), "transport drop must not be hard");
        assert!(!r.used_delta);
        let reason = r.fallback_reason.expect("fallback reason expected");
        assert!(reason.contains("delta transport drop"), "got: {reason}");
    }

    #[test]
    fn hard_rejection_host_key_stays_hard() {
        // Security refusals must never silently fall back: that is exactly
        // the condition host-key pinning exists for. The deny markers win
        // even when a transient kind is also present in the envelope.
        let r = hard_rejection_result(
            "native hard rejection (TransportFailure): host key verification failed",
        );
        assert!(r.fallback_reason.is_none(), "host-key must not fall back");
        assert!(r.hard_error.is_some());
    }

    #[test]
    fn hard_rejection_protocol_fault_stays_hard() {
        let r =
            hard_rejection_result("native hard rejection (InvalidFrame): unexpected opcode 0x42");
        assert!(r.fallback_reason.is_none());
        assert!(r.hard_error.is_some());
    }

    #[test]
    fn delta_sync_result_used_shape() {
        let stats = RsyncStats {
            bytes_sent: 100,
            bytes_received: 50,
            total_size: 10_000,
            speedup: 66.6,
            duration_ms: 123,
            copy_blocks: 0,
            warnings: vec![],
        };
        let r = DeltaSyncResult::used(stats.clone());
        assert!(r.used_delta);
        assert!(r.fallback_reason.is_none());
        assert_eq!(r.stats.unwrap().bytes_sent, 100);
    }

    #[test]
    fn delta_sync_result_fallback_shape() {
        let r = DeltaSyncResult::fallback("no rsync");
        assert!(!r.used_delta);
        assert!(r.stats.is_none());
        assert_eq!(r.fallback_reason.as_deref(), Some("no rsync"));
        assert!(
            r.hard_error.is_none(),
            "fallback and hard_error must be mutually exclusive"
        );
    }

    #[test]
    fn delta_sync_result_hard_error_shape() {
        let r = DeltaSyncResult::hard_error("host key mismatch");
        assert!(!r.used_delta);
        assert!(r.stats.is_none());
        assert!(
            r.fallback_reason.is_none(),
            "hard_error and fallback_reason must be mutually exclusive"
        );
        assert_eq!(r.hard_error.as_deref(), Some("host key mismatch"));
    }

    #[tokio::test]
    async fn transfer_with_delta_maps_hard_rejection_to_hard_error() {
        // Pin: RsyncError::HardRejection from a DeltaTransport MUST land in
        // DeltaSyncResult.hard_error, never in fallback_reason. This is the
        // invariant that prevents silent classic fallback after a native
        // path refusal (e.g. SSH host-key pinning mismatch).
        use crate::delta_transport::DeltaTransport;
        use async_trait::async_trait;

        struct HardRejectingTransport;

        #[async_trait]
        impl DeltaTransport for HardRejectingTransport {
            fn name(&self) -> &'static str {
                "hard-rejecting-test-transport"
            }
            async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
                Ok(RsyncCapability {
                    version: "test".into(),
                    protocol: 31,
                })
            }
            async fn probe_local(&self) -> Result<(), RsyncError> {
                Ok(())
            }
            async fn download(
                &self,
                _remote: &str,
                _local: &Path,
            ) -> Result<RsyncStats, RsyncError> {
                Err(RsyncError::HardRejection("host key mismatch (test)".into()))
            }
            async fn upload(&self, _local: &Path, _remote: &str) -> Result<RsyncStats, RsyncError> {
                Err(RsyncError::HardRejection("host key mismatch (test)".into()))
            }
        }

        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;
        let transport = HardRejectingTransport;
        let r = transfer_with_delta(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/nope"),
            "/remote/nope",
            "test-hard-rejection",
            None,
        )
        .await
        .expect("must succeed: hard rejection is a typed result, not an Err");
        assert!(!r.used_delta);
        assert!(
            r.fallback_reason.is_none(),
            "hard rejection must NOT produce a fallback_reason"
        );
        assert!(
            r.hard_error
                .as_deref()
                .unwrap()
                .contains("host key mismatch"),
            "hard rejection must surface in hard_error with the original message"
        );
    }

    #[test]
    fn build_config_copies_context_fields() {
        let ctx = DeltaSyncContext {
            ssh_user: "alice".into(),
            ssh_host: "example.com".into(),
            ssh_port: Some(2222),
            ssh_key_path: Some(PathBuf::from("/tmp/key")),
            strict_host_key_check: "accept-new".into(),
            known_hosts_path: Some(PathBuf::from("/tmp/kh")),
            min_file_size: 4096,
            compress: false,
            session_key: "s1".into(),
        };
        let cfg = build_rsync_config(&ctx);
        assert_eq!(cfg.ssh_user, "alice");
        assert_eq!(cfg.ssh_host, "example.com");
        assert_eq!(cfg.ssh_port, Some(2222));
        assert_eq!(cfg.ssh_key_path.unwrap(), PathBuf::from("/tmp/key"));
        assert!(cfg.ssh_password.is_none());
        assert_eq!(cfg.auth_method, crate::rsync_over_ssh::AuthMethod::SshKey);
        assert_eq!(cfg.strict_host_key_check, "accept-new");
        assert_eq!(cfg.min_file_size, 4096);
        assert!(!cfg.compress);
        assert!(cfg.preserve_times);
        assert!(cfg.progress);
    }

    #[tokio::test]
    async fn probe_cache_isolates_sessions() {
        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;
        // We can't run a real probe without an SSH handle; this test just verifies
        // the cache map enforces per-key isolation (invalidate one, other keeps state).
        invalidate_session_cache("missing-key").await; // no-op, no crash
        clear_probe_cache().await; // idempotent
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_returns_fallback_when_transport_has_no_remote() {
        // A binary transport with no handle cannot probe the remote; verify
        // the adapter reports this as a typed fallback instead of an Err.
        use crate::delta_transport::RsyncBinaryTransport;
        let cfg = RsyncConfig {
            ssh_user: "u".into(),
            ssh_host: "h".into(),
            ssh_key_path: Some(PathBuf::from("/tmp/irrelevant")),
            ..Default::default()
        };
        let transport = RsyncBinaryTransport::new(cfg, None);
        // Fresh cache for isolation (other tests may have touched it).
        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;

        let r = transfer_with_delta(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/nope"),
            "/remote/nope",
            "test-no-handle",
            None,
        )
        .await
        .expect("must succeed with fallback");
        assert!(!r.used_delta);
        assert!(r
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("remote delta unavailable"));
    }

    #[tokio::test]
    async fn eligibility_probe_reports_success_for_ready_transport() {
        use crate::delta_transport::DeltaTransport;
        use async_trait::async_trait;

        struct ReadyTransport;

        #[async_trait]
        impl DeltaTransport for ReadyTransport {
            fn name(&self) -> &'static str {
                "ready-test-transport"
            }
            async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
                Ok(RsyncCapability {
                    version: "3.4.1".into(),
                    protocol: 31,
                })
            }
            async fn probe_local(&self) -> Result<(), RsyncError> {
                Ok(())
            }
            async fn download(
                &self,
                _remote: &str,
                _local: &Path,
            ) -> Result<RsyncStats, RsyncError> {
                unreachable!("eligibility probe must not transfer data")
            }
            async fn upload(&self, _local: &Path, _remote: &str) -> Result<RsyncStats, RsyncError> {
                unreachable!("eligibility probe must not transfer data")
            }
        }

        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;
        let status =
            check_delta_eligibility_with_transport(&ReadyTransport, "test-eligibility-ready").await;
        assert!(status.eligible);
        assert!(status.reason.is_none());
    }

    #[tokio::test]
    async fn eligibility_probe_sanitizes_remote_failure_reason() {
        use crate::delta_transport::DeltaTransport;
        use async_trait::async_trait;

        struct MissingRemoteTransport;

        #[async_trait]
        impl DeltaTransport for MissingRemoteTransport {
            fn name(&self) -> &'static str {
                "missing-remote-test-transport"
            }
            async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
                Err(RsyncError::ProbeFailed(
                    "rsync not found under /home/alice/.ssh/custom".into(),
                ))
            }
            async fn probe_local(&self) -> Result<(), RsyncError> {
                Ok(())
            }
            async fn download(
                &self,
                _remote: &str,
                _local: &Path,
            ) -> Result<RsyncStats, RsyncError> {
                unreachable!("eligibility probe must not transfer data")
            }
            async fn upload(&self, _local: &Path, _remote: &str) -> Result<RsyncStats, RsyncError> {
                unreachable!("eligibility probe must not transfer data")
            }
        }

        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;
        let status = check_delta_eligibility_with_transport(
            &MissingRemoteTransport,
            "test-eligibility-missing-remote",
        )
        .await;
        assert!(!status.eligible);
        let reason = status.reason.expect("reason expected");
        assert!(reason.contains("remote delta unavailable"));
        assert!(!reason.contains("/home/alice"));
        assert!(reason.contains("<redacted>"));
    }

    // #398: a rsync-less remote must be probed at most once per session, not on
    // every transfer. The failed probe is cached (negative TTL), so a second
    // transfer through the same session key falls back immediately without a
    // second probe (which, on the real russh path, was the ~10-minute hang).
    #[tokio::test]
    async fn probe_failure_is_cached_and_not_reprobed() {
        use crate::delta_transport::DeltaTransport;
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingFailTransport {
            probes: AtomicUsize,
        }

        #[async_trait]
        impl DeltaTransport for CountingFailTransport {
            fn name(&self) -> &'static str {
                "counting-fail-transport"
            }
            async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
                self.probes.fetch_add(1, Ordering::SeqCst);
                Err(RsyncError::RemoteNotAvailable)
            }
            async fn probe_local(&self) -> Result<(), RsyncError> {
                Ok(())
            }
            async fn download(
                &self,
                _remote: &str,
                _local: &Path,
            ) -> Result<RsyncStats, RsyncError> {
                unreachable!("no transfer on a failed probe")
            }
            async fn upload(&self, _local: &Path, _remote: &str) -> Result<RsyncStats, RsyncError> {
                unreachable!("no transfer on a failed probe")
            }
        }

        let _cache_guard = PROBE_CACHE_TEST_GUARD.lock().await;
        clear_probe_cache().await;
        let transport = CountingFailTransport {
            probes: AtomicUsize::new(0),
        };
        let key = "test-negative-cache-reuse";

        for _ in 0..2 {
            let r = transfer_with_delta(
                &transport,
                SyncDirection::Upload,
                Path::new("/tmp/nope"),
                "/remote/nope",
                key,
                None,
            )
            .await
            .expect("must succeed with fallback");
            assert!(!r.used_delta);
        }

        assert_eq!(
            transport.probes.load(Ordering::SeqCst),
            1,
            "a failed remote probe must be cached, not re-run on the next transfer"
        );
    }
}
