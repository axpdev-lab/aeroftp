//! The application's `DeltaTransport` implementation for the local
//! transport, and the rendering that goes with it.
//!
//! The local transport is not the remote one with the network taken out:
//! it reports an infinite speedup when nothing had to be sent, and its
//! soft failures carry exit code 0, because the caller reads them as "use
//! the plain copy instead", not as a failed rsync. Both are visible to the
//! user through `RsyncStats` and `RsyncError`, so they are reproduced here
//! rather than folded into the shared rendering.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::Path;

use async_trait::async_trait;

use crate::aerorsync::local_transport::{local_probe, LocalDeltaTransport, LOCAL_TRANSPORT_NAME};
use crate::aerorsync::types::{TransferError, TransferReport};
use crate::aerorsync_adapter::errors::probe_to_capability;
use crate::delta_transport::DeltaTransport;
use crate::rsync_over_ssh::{RsyncCapability, RsyncError, RsyncStats};

/// Render a local transfer's report. Same seven fields the local path
/// always filled, including the infinite speedup when nothing was sent:
/// on this path that is not a division by zero, it is the honest answer,
/// because the destination was rebuilt without sending anything.
fn local_report_to_rsync_stats(report: TransferReport) -> RsyncStats {
    let speedup = if report.session.bytes_sent == 0 {
        f64::INFINITY
    } else {
        report.total_size as f64 / report.session.bytes_sent as f64
    };
    RsyncStats {
        bytes_sent: report.session.bytes_sent,
        bytes_received: report.session.bytes_received,
        total_size: report.total_size,
        speedup,
        duration_ms: report.duration_ms,
        copy_blocks: report.session.copy_blocks,
        warnings: report.warnings,
    }
}

/// Render a local transfer's failure. The soft ones keep exit code 0,
/// which is what this path has always reported: there is no rsync
/// process here, and 0 is how the caller tells "nothing ran, fall back"
/// apart from a real transfer failure.
fn local_error_to_rsync(err: TransferError) -> RsyncError {
    match err {
        TransferError::Soft { detail } => RsyncError::TransferFailed {
            exit: 0,
            stderr: detail,
        },
        TransferError::Hard { detail } => RsyncError::HardRejection(detail),
        TransferError::Io(io) => RsyncError::Io(io),
        TransferError::TooSmall { size, threshold } => RsyncError::TooSmall { size, threshold },
        // The local path has no driver, so it never builds this variant;
        // the arm exists because the carrier is shared and a silent
        // catch-all would hide the day it does.
        TransferError::Native { error, committed } => RsyncError::HardRejection(format!(
            "local delta reported a native error ({:?}, committed={}): {}",
            error.kind, committed, error.detail
        )),
    }
}

#[async_trait]
impl DeltaTransport for LocalDeltaTransport {
    fn name(&self) -> &'static str {
        LOCAL_TRANSPORT_NAME
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        Ok(probe_to_capability(local_probe()))
    }

    async fn probe_local(&self) -> Result<(), RsyncError> {
        Ok(())
    }

    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError> {
        // `remote_path` is interpreted as a local filesystem path.
        self.transfer(local_path, Path::new(remote_path))
            .await
            .map(local_report_to_rsync_stats)
            .map_err(local_error_to_rsync)
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        // Inverted direction: `remote_path` is the source on the local fs.
        self.transfer(Path::new(remote_path), local_path)
            .await
            .map(local_report_to_rsync_stats)
            .map_err(local_error_to_rsync)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait says one name, the module owns it: if the copy in the
    /// implementation and the constant ever drift, this fails.
    #[test]
    fn local_transport_name_matches_the_module_constant() {
        let transport = LocalDeltaTransport::new(0);
        assert_eq!(transport.name(), LOCAL_TRANSPORT_NAME);
        assert_eq!(LOCAL_TRANSPORT_NAME, "aerorsync-local");
    }

    /// A local transfer that sent nothing reports an infinite speedup,
    /// not the 1.0 the remote rendering uses. Both are deliberate and
    /// this pins the difference.
    #[test]
    fn a_local_transfer_that_sent_nothing_reports_an_infinite_speedup() {
        let stats = local_report_to_rsync_stats(TransferReport {
            total_size: 4096,
            ..TransferReport::default()
        });
        assert!(stats.speedup.is_infinite());
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.total_size, 4096);
    }

    /// A soft local failure keeps exit code 0.
    #[test]
    fn a_soft_local_failure_keeps_exit_zero() {
        match local_error_to_rsync(TransferError::Soft {
            detail: "local delta apply failed: boom".into(),
        }) {
            RsyncError::TransferFailed { exit, stderr } => {
                assert_eq!(exit, 0, "the local path has never reported -1");
                assert_eq!(stderr, "local delta apply failed: boom");
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
    }

    /// Moved with the trait implementation it exercises: the local path
    /// reports "protocol 31, in process" so the delta dispatch keeps
    /// working without a remote.
    #[tokio::test]
    async fn probe_remote_reports_local_capability() {
        let transport = LocalDeltaTransport::new(1024);
        let cap = transport.probe_remote().await.expect("probe");
        assert_eq!(cap.protocol, 31);
        assert!(cap.version.contains("local"));
    }

    /// Moved with the trait implementation it exercises: upload writes
    /// the second path and download reads it, which is the direction
    /// inversion the two methods encode.
    #[tokio::test]
    async fn upload_and_download_are_symmetric() {
        let dir = tmp_dir();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        let payload = vec![0x55u8; 1_500_000];
        tokio::fs::write(&src, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024);
        // upload semantics: local -> remote (interpreted as local fs path)
        transport
            .upload(&src, dst.to_string_lossy().as_ref())
            .await
            .expect("upload");
        assert_eq!(tokio::fs::read(&dst).await.unwrap(), payload);

        // download semantics: remote -> local
        let dst2 = dir.path().join("c.bin");
        transport
            .download(src.to_string_lossy().as_ref(), &dst2)
            .await
            .expect("download");
        assert_eq!(tokio::fs::read(&dst2).await.unwrap(), payload);
    }

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }
}
