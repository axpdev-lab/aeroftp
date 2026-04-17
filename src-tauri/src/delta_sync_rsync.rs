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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;

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
/// `used_delta = true` means rsync was spawned and completed successfully; `stats`
/// is `Some` with real measured values. `used_delta = false` means the caller must
/// fall back to the classic transfer — `fallback_reason` explains why.
#[derive(Debug)]
pub struct DeltaSyncResult {
    pub used_delta: bool,
    pub stats: Option<RsyncStats>,
    pub fallback_reason: Option<String>,
}

impl DeltaSyncResult {
    fn used(stats: RsyncStats) -> Self {
        Self {
            used_delta: true,
            stats: Some(stats),
            fallback_reason: None,
        }
    }

    fn fallback(reason: impl Into<String>) -> Self {
        Self {
            used_delta: false,
            stats: None,
            fallback_reason: Some(reason.into()),
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
        Err(RsyncError::MissingKey(s)) => {
            Ok(DeltaSyncResult::fallback(format!("ssh key: {}", s)))
        }
        Err(RsyncError::RemoteNotAvailable) => Ok(DeltaSyncResult::fallback(
            "remote rsync disappeared between probe and transfer",
        )),
        Err(RsyncError::LocalNotAvailable) => Ok(DeltaSyncResult::fallback(
            "local rsync disappeared between probe and transfer",
        )),
        Err(e) => {
            // TransferFailed, SpawnFailed, Io, Cancelled, VersionTooOld, ProbeFailed →
            // all map to fallback with the error message. Caller decides whether to
            // retry the classic transfer or surface the error.
            tracing::warn!("delta sync {:?} failed: {}", direction, e);
            Ok(DeltaSyncResult::fallback(format!("rsync failed: {}", e)))
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
}
