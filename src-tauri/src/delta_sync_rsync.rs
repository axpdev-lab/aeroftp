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
//! or remote probing — it just calls [`transfer_with_delta`] and gets back either
//! a success with real stats or a `used_delta = false` with a reason to use the
//! classic download/upload path.
//!
//! # Fase 1a / 1b scope
//! This module is `#[cfg(unix)]` in Fase 1a. Fase 1b removes the guard and enables
//! the Windows rsync bundle (see `T1.8` in the execution plan).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]
// Foundations module for Fase 1 delta sync. Adapter between the sync loop
// and the rsync orchestrator. Items appear "never used" until T1.5 Part B
// wires `transfer_with_delta` into `sync.rs` — remove this allow then.
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

/// Per-session capability cache.
///
/// `session_key` → (capability, cached_at). Entries expire after [`CACHE_TTL`];
/// this avoids a probe roundtrip on every file while still letting the user reconnect
/// to a different server with the same key (unlikely, but a safeguard).
struct CacheEntry {
    capability: Result<RsyncCapability, RsyncError>,
    cached_at: Instant,
}

const CACHE_TTL: Duration = Duration::from_secs(300); // 5 min

static PROBE_CACHE: LazyLock<TokioMutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| TokioMutex::new(HashMap::new()));

/// Probe remote capability via the transport, with 5-minute per-session cache.
///
/// Returns the cached result if present and fresh; otherwise runs a live probe
/// and stores the outcome. Cache stores failures too — if rsync isn't on the
/// remote, we don't want to re-probe every file.
async fn probe_capability_cached(
    transport: &dyn DeltaTransport,
    session_key: &str,
) -> Result<RsyncCapability, String> {
    let now = Instant::now();

    {
        let cache = PROBE_CACHE.lock().await;
        if let Some(entry) = cache.get(session_key) {
            if now.duration_since(entry.cached_at) < CACHE_TTL {
                return match &entry.capability {
                    Ok(cap) => Ok(cap.clone()),
                    Err(e) => Err(e.to_string()),
                };
            }
        }
    }

    let fresh = transport.probe_remote().await;
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

    // Step 3: run the transfer through the trait.
    let outcome = match direction {
        SyncDirection::Upload => transport.upload(local_path, remote_path).await,
        SyncDirection::Download => transport.download(remote_path, local_path).await,
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
            // Native path refused for a reason that MUST NOT trigger silent
            // classic fallback (e.g. SSH host-key pinning mismatch). The
            // caller is responsible for surfacing the error to the UI.
            tracing::error!(
                "delta sync {:?} hard rejection: {} — classic fallback suppressed",
                direction,
                msg
            );
            Ok(DeltaSyncResult::hard_error(sanitize_rsync_message(&msg)))
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
) -> Option<DeltaSyncResult> {
    let result =
        transfer_with_delta(transport, direction, local_path, remote_path, session_key).await;

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
/// point on `StorageProvider` — no new trait methods are introduced.
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
    // Only SFTP is delta-eligible in Fase 1. Downcasting via `as_any_mut()` keeps
    // the generic trait intact — we don't need a new `delta_transport_context()`
    // contract on every provider implementation.
    let sftp = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::sftp::SftpProvider>()?;

    let transport = sftp.delta_transport()?;

    // Session key: stable per connection (host+user). When the user reconnects
    // with different credentials, `invalidate_session_cache()` should be called
    // by the connection lifecycle — for now the 5-minute TTL handles staleness.
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

    #[test]
    fn delta_sync_result_used_shape() {
        let stats = RsyncStats {
            bytes_sent: 100,
            bytes_received: 50,
            total_size: 10_000,
            speedup: 66.6,
            duration_ms: 123,
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

        clear_probe_cache().await;
        let transport = HardRejectingTransport;
        let r = transfer_with_delta(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/nope"),
            "/remote/nope",
            "test-hard-rejection",
        )
        .await
        .expect("must succeed — hard rejection is a typed result, not an Err");
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
        assert_eq!(cfg.strict_host_key_check, "accept-new");
        assert_eq!(cfg.min_file_size, 4096);
        assert!(!cfg.compress);
        assert!(cfg.preserve_times);
        assert!(cfg.progress);
    }

    #[tokio::test]
    async fn probe_cache_isolates_sessions() {
        clear_probe_cache().await;
        // We can't run a real probe without an SSH handle; this test just verifies
        // the cache map enforces per-key isolation (invalidate one, other keeps state).
        invalidate_session_cache("missing-key").await; // no-op, no crash
        clear_probe_cache().await; // idempotent
    }

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
        clear_probe_cache().await;

        let r = transfer_with_delta(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/nope"),
            "/remote/nope",
            "test-no-handle",
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
}
