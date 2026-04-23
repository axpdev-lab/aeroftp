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
/// (`NativeRsyncDeltaTransport`) gated behind the `proto_native_rsync`
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
}
