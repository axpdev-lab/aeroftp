//! Abstraction over the transport that performs delta sync.
//!
//! Today the only implementation ([`RsyncBinaryTransport`]) wraps the local
//! `rsync` binary over an SSH channel. A future Rust-native rsync protocol
//! implementation (the long-term "strada C" effort) will add a second
//! [`RsyncNativeTransport`] that drives the protocol entirely in-process.
//!
//! The sync loop and the [`crate::delta_sync_rsync`] adapter only see this
//! trait; they never name a concrete transport. When the native transport
//! lands, the factory that constructs a `Box<dyn DeltaTransport>` gains a
//! branch and the rest of the stack keeps compiling unchanged.
//!
//! ## Contract
//! Implementations are expected to be cheap to construct (they mostly hold
//! configuration + optional shared handles) and reusable across many file
//! transfers within a single sync session. Capability probing is idempotent
//! and safe to call repeatedly, though callers typically memoize.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// The trait and the abstract contract are cross-platform. Only the binary-rsync
// implementation is Unix-only, and is gated surgically below.
#![allow(dead_code)]

#[cfg(unix)]
use crate::providers::sftp::SharedSshHandle;
#[cfg(unix)]
use crate::rsync_over_ssh::{probe_local_rsync, probe_rsync, rsync_download, rsync_upload};
use crate::rsync_over_ssh::{RsyncCapability, RsyncConfig, RsyncError, RsyncStats};
use async_trait::async_trait;
use std::path::Path;

/// Transport abstraction over any delta-capable sync mechanism.
///
/// Implementations must be `Send + Sync` because a single instance is shared
/// by the sync loop across many files. All methods are `&self` so callers
/// don't need a lock; implementations either hold their own internal sync or
/// lean on types (like `Arc<TokioMutex<Handle>>`) that already provide it.
#[async_trait]
pub trait DeltaTransport: Send + Sync {
    /// Stable identifier for this transport, used in logs and UI ("rsync 3.2.7
    /// via ssh" vs. "native-rsync protocol 31"). Cheap to call.
    fn name(&self) -> &'static str;

    /// Probe the remote side for capability. Returns an opaque
    /// [`RsyncCapability`] regardless of implementation — subprocess rsync and
    /// native rsync both report the same shape.
    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError>;

    /// Probe the local side for capability. For the subprocess transport this
    /// checks that a `rsync` binary is on PATH (or bundled on Windows). For a
    /// native transport this is typically a no-op returning `Ok(())`.
    async fn probe_local(&self) -> Result<(), RsyncError>;

    /// Download `remote_path` into `local_path` with delta semantics.
    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError>;

    /// Upload `local_path` to `remote_path` with delta semantics.
    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError>;

    /// Begin a session-reuse batch.
    ///
    /// Default impl returns a [`NoopBatch`], a marker that signals the
    /// transport does not support session reuse. Sync loops should detect
    /// this via [`DeltaBatch::is_noop`] and fall back to the per-file
    /// single-shot path ([`DeltaTransport::upload`] / [`download`]) without
    /// invoking the batch methods.
    ///
    /// Transports that DO support session reuse (the native rsync prototype
    /// in P3-T01 W3.2) override this to return a concrete `DeltaBatch` impl
    /// that keeps a single SSH session alive across N file transfers — the
    /// session_count in [`BatchStats`] reflects how many handshakes the
    /// batch actually paid for.
    ///
    /// [`NoopBatch`]: NoopBatch
    /// [`DeltaBatch::is_noop`]: DeltaBatch::is_noop
    /// [`download`]: Self::download
    /// [`BatchStats`]: BatchStats
    async fn begin_batch(&self) -> Result<Box<dyn DeltaBatch>, RsyncError> {
        Ok(Box::new(NoopBatch::new()))
    }
}

/// Stats accumulated by a [`DeltaBatch`] from creation to [`finalize`].
///
/// `session_count` is the headline metric for P3-T01 W3: when the batch reuses
/// a single SSH session perfectly across all files it reports `1`. Values > 1
/// mean the underlying SSH session was rebuilt mid-batch (e.g. a transient
/// TCP RST or idle timeout) and the batch transparently reconnected.
///
/// `bytes_on_wire` aggregates the wire byte count across all files; useful for
/// the post-sync UI line "Delta: 84 files in 1 session, 1.2 MB on wire".
///
/// `partial` is set when [`DeltaBatch::cancel`] was observed before all
/// queued files completed — N successful + 1 in-flight cancelled (with
/// `.aerotmp` rollback) + remaining never tried.
///
/// [`finalize`]: DeltaBatch::finalize
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchStats {
    pub files_transferred: u64,
    pub bytes_on_wire: u64,
    pub session_count: u32,
    pub partial: bool,
}

/// Session-reuse handle returned by [`DeltaTransport::begin_batch`].
///
/// A batch wraps a long-lived underlying transport session (typically a
/// single SSH exec channel for native rsync) and routes N file transfers
/// through it without paying handshake cost on each one. The contract:
///
/// - `upload`/`download` are sequential — no parallelism in P3-T01.
/// - `cancel` is cooperative and idempotent: it sets a flag; in-flight
///   transfers observe it on chunk boundaries and unwind via the
///   `StreamingAtomicWriter` Drop invariant (the `<target>.aerotmp` is
///   left orphan, the original target file is intact).
/// - `finalize` consumes the batch and surfaces the [`BatchStats`]. After
///   `finalize` the underlying session is closed.
///
/// Marker impls (see [`NoopBatch`]) override [`is_noop`] to `true` so the
/// caller can detect "this transport doesn't support session reuse" and
/// fall back to the single-shot path.
///
/// [`is_noop`]: DeltaBatch::is_noop
#[async_trait]
pub trait DeltaBatch: Send + Sync {
    /// True for marker batches that do not actually reuse a session.
    /// Default: false. Override in [`NoopBatch`].
    fn is_noop(&self) -> bool {
        false
    }

    /// Upload `local_path` to `remote_path` reusing the batch session.
    /// Returns per-file [`RsyncStats`]; aggregate stats live in [`BatchStats`].
    async fn upload(
        &mut self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError>;

    /// Download `remote_path` to `local_path` reusing the batch session.
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError>;

    /// Cooperative cancel. Idempotent: subsequent calls are no-ops.
    /// Cancel after [`finalize`] is also a no-op.
    fn cancel(&self);

    /// Consume the batch and return aggregated stats. The underlying
    /// session is torn down here. After `finalize` the batch is gone.
    async fn finalize(self: Box<Self>) -> Result<BatchStats, RsyncError>;
}

/// Marker [`DeltaBatch`] returned by the default [`DeltaTransport::begin_batch`]
/// impl. Signals that the transport does not support session reuse — the
/// sync loop must use the single-shot [`DeltaTransport::upload`] /
/// [`DeltaTransport::download`] methods instead.
///
/// Calling `upload` or `download` on a `NoopBatch` returns
/// [`RsyncError::TransferFailed`] with a diagnostic message; callers should
/// branch on [`DeltaBatch::is_noop`] before reaching that path.
pub struct NoopBatch {
    stats: BatchStats,
}

impl NoopBatch {
    pub fn new() -> Self {
        Self {
            stats: BatchStats::default(),
        }
    }
}

impl Default for NoopBatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DeltaBatch for NoopBatch {
    fn is_noop(&self) -> bool {
        true
    }

    async fn upload(
        &mut self,
        _local_path: &Path,
        _remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        Err(RsyncError::TransferFailed {
            exit: -1,
            stderr: "NoopBatch::upload called — transport does not support session reuse, \
                     use the single-shot DeltaTransport::upload instead"
                .into(),
        })
    }

    async fn download(
        &mut self,
        _remote_path: &str,
        _local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        Err(RsyncError::TransferFailed {
            exit: -1,
            stderr: "NoopBatch::download called — transport does not support session reuse, \
                     use the single-shot DeltaTransport::download instead"
                .into(),
        })
    }

    fn cancel(&self) {
        // No-op: NoopBatch never holds a session, never has files in flight.
    }

    async fn finalize(self: Box<Self>) -> Result<BatchStats, RsyncError> {
        Ok(self.stats)
    }
}

/// Delta transport backed by the local `rsync` binary driven via SSH.
///
/// Construction is zero-cost: the handle is an `Arc` clone, the config is a
/// small struct. Reuse one instance per sync session and pass it to the
/// adapter layer as `&dyn DeltaTransport`.
///
/// **Platform:** Unix-only. The implementation spawns the system `rsync`
/// binary, which is not available on Windows as a first-class dependency.
/// Windows delivers delta sync through the native prototype transport
/// (`AerorsyncDeltaTransport`) gated behind the `aerorsync`
/// cargo feature.
#[cfg(unix)]
pub struct RsyncBinaryTransport {
    config: RsyncConfig,
    /// Only required for `probe_remote` (runs a command on the existing SSH
    /// session). Downloads/uploads spawn their own rsync subprocess which
    /// opens its own SSH transport, so the handle is not held during transfer.
    handle: Option<SharedSshHandle>,
}

#[cfg(unix)]
impl RsyncBinaryTransport {
    pub fn new(config: RsyncConfig, handle: Option<SharedSshHandle>) -> Self {
        Self { config, handle }
    }
}

#[cfg(unix)]
#[async_trait]
impl DeltaTransport for RsyncBinaryTransport {
    fn name(&self) -> &'static str {
        "rsync-binary-over-ssh"
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        let handle = self.handle.clone().ok_or(RsyncError::RemoteNotAvailable)?;
        probe_rsync(handle).await
    }

    async fn probe_local(&self) -> Result<(), RsyncError> {
        probe_local_rsync().await.map(|_| ())
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        rsync_download(remote_path, local_path, &self.config).await
    }

    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError> {
        rsync_upload(local_path, remote_path, &self.config).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> RsyncConfig {
        RsyncConfig {
            ssh_user: "u".into(),
            ssh_host: "h".into(),
            ssh_key_path: Some(PathBuf::from("/dev/null")),
            ..Default::default()
        }
    }

    #[test]
    fn binary_transport_name_is_stable() {
        let t = RsyncBinaryTransport::new(test_config(), None);
        assert_eq!(t.name(), "rsync-binary-over-ssh");
    }

    #[tokio::test]
    async fn probe_remote_fails_cleanly_without_handle() {
        let t = RsyncBinaryTransport::new(test_config(), None);
        match t.probe_remote().await {
            Err(RsyncError::RemoteNotAvailable) => {}
            other => panic!("expected RemoteNotAvailable, got {:?}", other),
        }
    }

    #[test]
    fn trait_object_is_constructible() {
        // Compile-time guard: verify `RsyncBinaryTransport: DeltaTransport`
        // produces a valid trait object. If this breaks, a future impl added
        // something that isn't dyn-compatible.
        let t: Box<dyn DeltaTransport> = Box::new(RsyncBinaryTransport::new(test_config(), None));
        assert_eq!(t.name(), "rsync-binary-over-ssh");
    }

    // P3-T01 W3.1 — DeltaBatch / NoopBatch / default begin_batch tests.
    // The first two pin the compat contract: any transport that does NOT
    // override `begin_batch` (today: every transport, including
    // `RsyncBinaryTransport`) inherits the `NoopBatch` marker, which the
    // sync loop will detect via `is_noop()` and bypass.

    #[tokio::test]
    async fn rsync_binary_transport_inherits_default_noop_batch() {
        let t = RsyncBinaryTransport::new(test_config(), None);
        let batch = t.begin_batch().await.expect("default begin_batch never fails");
        assert!(
            batch.is_noop(),
            "RsyncBinaryTransport must inherit the NoopBatch default until W3.2 wires \
             a real session-reuse impl"
        );
    }

    #[tokio::test]
    async fn default_begin_batch_returns_noop_for_unimplemented_transports() {
        // Defines a minimal DeltaTransport that never overrides begin_batch.
        // Mirrors the "future transport that doesn't yet support session
        // reuse" case — it must compile and report is_noop()=true.
        struct UnimplementedTransport;

        #[async_trait]
        impl DeltaTransport for UnimplementedTransport {
            fn name(&self) -> &'static str {
                "unimplemented-test-transport"
            }
            async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
                Err(RsyncError::RemoteNotAvailable)
            }
            async fn probe_local(&self) -> Result<(), RsyncError> {
                Ok(())
            }
            async fn download(
                &self,
                _remote_path: &str,
                _local_path: &Path,
            ) -> Result<RsyncStats, RsyncError> {
                Err(RsyncError::RemoteNotAvailable)
            }
            async fn upload(
                &self,
                _local_path: &Path,
                _remote_path: &str,
            ) -> Result<RsyncStats, RsyncError> {
                Err(RsyncError::RemoteNotAvailable)
            }
        }

        let t = UnimplementedTransport;
        let batch = t.begin_batch().await.expect("default begin_batch is infallible");
        assert!(batch.is_noop());
    }

    #[tokio::test]
    async fn noop_batch_finalize_returns_default_stats() {
        let batch: Box<dyn DeltaBatch> = Box::new(NoopBatch::new());
        let stats = batch.finalize().await.expect("NoopBatch finalize is infallible");
        assert_eq!(stats, BatchStats::default());
        assert_eq!(stats.files_transferred, 0);
        assert_eq!(stats.bytes_on_wire, 0);
        assert_eq!(stats.session_count, 0);
        assert!(!stats.partial);
    }

    #[tokio::test]
    async fn noop_batch_upload_surfaces_helpful_error() {
        let mut batch = NoopBatch::new();
        let result = batch
            .upload(Path::new("/tmp/x"), "/remote/x")
            .await
            .expect_err("NoopBatch::upload must always fail");
        match result {
            RsyncError::TransferFailed { stderr, .. } => {
                assert!(
                    stderr.contains("session reuse"),
                    "error must mention session reuse so callers know to use single-shot path: {stderr}"
                );
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn noop_batch_cancel_is_idempotent() {
        let batch = NoopBatch::new();
        // Cancel before any operation — no panic, no state change.
        batch.cancel();
        batch.cancel();
        batch.cancel();
        // Finalize is still infallible afterwards.
        let stats = Box::new(batch).finalize().await.expect("infallible");
        assert_eq!(stats, BatchStats::default());
    }
}
