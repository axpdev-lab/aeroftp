//! The application's `DeltaTransport` and `DeltaBatch` implementations
//! for the aerorsync module.
//!
//! The trait methods are the only place that has to produce an
//! `RsyncError` or an `RsyncStats`, which is why they live on this side
//! of the fence: the module cannot name those types, and it cannot
//! import this adapter either. Each method calls the module's
//! crate-owned entry point and renders the outcome through
//! [`crate::aerorsync_adapter::errors`].
//!
//! The implementations stay on the module's own types, so every caller
//! that constructs an `AerorsyncDeltaTransport` and uses it as a
//! `dyn DeltaTransport` keeps compiling unchanged: an implementation is
//! resolved crate-wide, not by the module it is written in.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::Path;

use async_trait::async_trait;

use crate::aerorsync::delta_transport_impl::{
    AerorsyncBatch, AerorsyncDeltaTransport, AERORSYNC_TRANSPORT_NAME,
};
use crate::aerorsync_adapter::errors::{
    probe_error_to_rsync, probe_to_capability, report_to_rsync_stats, to_rsync_error,
};
use crate::delta_transport::{BatchStats, DeltaBatch, DeltaTransport};
use crate::rsync_over_ssh::{RsyncCapability, RsyncError, RsyncStats};

#[async_trait]
impl DeltaTransport for AerorsyncDeltaTransport {
    fn name(&self) -> &'static str {
        AERORSYNC_TRANSPORT_NAME
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        // U-04: a non-zero exit or a transport failure becomes
        // `RsyncError::RemoteNotAvailable` so the probe cache
        // (`PROBE_CACHE`, 5-minute TTL) memoises a typed "unavailable"
        // verdict: without this, every file in a multi-file sync would
        // enter the native path, pay a fresh SSH setup, fail at
        // `open_raw_stream`, and only then fall back to classic.
        match self.probe().await {
            Ok(probe) => Ok(probe_to_capability(probe)),
            Err(failure) => {
                let rsync_error = probe_error_to_rsync(failure.error);
                // An SSH leg that never came up is returned without a
                // warning of its own, as it was before the module owned
                // the probe: the connect failure is already reported
                // where the connection was asked for.
                if failure.at_connect || matches!(rsync_error, RsyncError::HardRejection(_)) {
                    return Err(rsync_error);
                }
                let (host, port) = self.endpoint();
                tracing::warn!(
                    "native rsync probe failed for {}:{}: {}: marking remote unavailable",
                    host,
                    port,
                    rsync_error
                );
                Err(rsync_error)
            }
        }
    }

    async fn probe_local(&self) -> Result<(), RsyncError> {
        Ok(())
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        self.download_inner(remote_path, local_path, None)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    async fn download_with_progress(
        &self,
        remote_path: &str,
        local_path: &Path,
        progress: Option<crate::aerorsync::progress::ProgressSink>,
    ) -> Result<RsyncStats, RsyncError> {
        self.download_inner(remote_path, local_path, progress)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path, None)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    async fn upload_with_progress(
        &self,
        local_path: &Path,
        remote_path: &str,
        progress: Option<crate::aerorsync::progress::ProgressSink>,
    ) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path, progress)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    /// Open a session-reuse batch. A failed handshake degrades to
    /// [`crate::delta_transport::NoopBatch`] so the sync loop falls back
    /// to the single-shot per-file path without losing the file.
    async fn begin_batch(&self) -> Result<Box<dyn DeltaBatch>, RsyncError> {
        match self.open_batch().await {
            Ok(batch) => Ok(Box::new(batch)),
            Err(e) => {
                tracing::warn!(
                    "AerorsyncDeltaTransport::begin_batch: russh connect failed ({}); \
                     falling back to NoopBatch: sync loop will use single-shot per-file path",
                    e
                );
                Ok(Box::new(crate::delta_transport::NoopBatch::new()))
            }
        }
    }
}

#[async_trait]
impl DeltaBatch for AerorsyncBatch {
    async fn upload(
        &mut self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        self.upload_file(local_path, remote_path)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        self.download_file(remote_path, local_path)
            .await
            .map(report_to_rsync_stats)
            .map_err(to_rsync_error)
    }

    fn cancel(&self) {
        self.cancel_batch();
    }

    async fn finalize(self: Box<Self>) -> Result<BatchStats, RsyncError> {
        let (files_transferred, bytes_on_wire, session_count, partial) = self.batch_totals().await;
        Ok(BatchStats {
            files_transferred,
            bytes_on_wire,
            session_count,
            partial,
        })
    }
}
