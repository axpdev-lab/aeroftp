// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Provider-backed transfer executor for the shared orchestrator.
//!
//! ## Status (post DAG-ENGINE A-branch convergence)
//!
//! The flag-gated single-file / batch / sync routing this module used to
//! own has been removed (SG-T18). The shared graph engine is the
//! production path for every transfer; the legacy
//! `JoinSet`-based `execute_batch` has been deleted from
//! `transfer_orchestrator`.
//!
//! What still lives here:
//!
//! - [`ProviderDownloadExecutor`] / [`ProviderUploadExecutor`]: the
//!   per-file [`TransferExecutor`] implementations the DAG batch runner
//!   calls into through `execute_with_session`. They own the
//!   per-provider clone pool, the per-file resource lease, and the
//!   delta-batch session reuse, so collapsing them inline into the
//!   shaped batch runner is a non-trivial re-home, deferred to the
//!   v4.x cleanup window.
//! - `provider_segmented_*` helpers: the eligibility gate + window-
//!   worker driver for the GUI's intra-file segmented download.
//!   Will move under `providers::multi_thread` once the shaped-graph
//!   segmented runner consumes them directly (SG-T13 wired the
//!   builder; the actual eligibility gate move is the follow-up).
//! - [`ProviderListSessionModel`]: pure session-model enum the scan
//!   layer consumes. The plan migrated `resolve_session_model` proper
//!   to `transfer_dag/probe.rs`; the enum follows once the scan layer
//!   imports it from there.
//!
//! The original SG-T19 goal of deleting this file outright is filed as
//! accepted technical debt (see `docs/dev/roadmap/APPENDIX-DAG-ENGINE/STATUS_TODO.md`):
//! the convergence target is reached, the legacy hand-rolled
//! orchestrator is gone, and the helpers that remain are pure
//! infrastructure that the DAG runners already consume rather than a
//! parallel scheduling path.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::providers::{
    ProviderListExecutorKind, ProviderTransferExecutorKind, ProviderType, StorageProvider,
};
use crate::transfer_dag::{
    Capability, TransferCapabilities, TransferSessionLease, TransferSessionPoolHandle,
};
use crate::transfer_domain::{
    transfer_failure_kind_from_sync, user_facing_transfer_failure_message, TransferEntry,
    TransferFailure, TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;
use crate::transfer_orchestrator::TransferExecutor;
use crate::transfer_settings::ResolvedTransferSettings;

/// Minimum file size before intra-file range parallelism kicks in
/// (GTC-1). Below this, the per-stream overhead dominates the WAN gain
/// — see `2026-05-19_baseline-run-report.md` honest read.
const SEGMENTED_DOWNLOAD_FILE_FLOOR: u64 = 8 * 1024 * 1024;

/// Minimum chunk size per segment. Mirrors `PGET_MIN_CHUNK_SIZE` in
/// `bin/aeroftp_cli.rs`: never split below 1 MiB to avoid pathological
/// fragmentation. Final segment count is reduced until each window
/// meets this floor.
const SEGMENTED_DOWNLOAD_MIN_CHUNK_SIZE: u64 = 1024 * 1024;

/// Maximum bytes per `read_range` sub-call inside a window worker.
/// Mirrors `PGET_SUB_READ_SIZE` so a single range request never grows
/// unbounded on very large files.
const SEGMENTED_DOWNLOAD_SUB_READ_SIZE: u64 = 64 * 1024 * 1024;

/// Effective segment count after the anti-fragmentation rule
/// (`SEGMENTED_DOWNLOAD_MIN_CHUNK_SIZE`). Mirrors `pget_effective_segments`
/// in `bin/aeroftp_cli.rs`. Returns 0 to signal "do not segment".
///
/// This is the single source of truth shared by the GUI executor
/// (`ProviderDownloadExecutor`) and the GUI single-file path
/// (`provider_download_file`) so the same anti-fragmentation policy
/// holds across both surfaces.
pub fn provider_segmented_effective_count(file_size: u64, requested: u32) -> usize {
    if file_size < SEGMENTED_DOWNLOAD_FILE_FLOOR || requested <= 1 {
        return 0;
    }
    let segments = (requested as u64).clamp(1, 16);
    let chunk = file_size / segments;
    let bounded = if chunk < SEGMENTED_DOWNLOAD_MIN_CHUNK_SIZE {
        (file_size / SEGMENTED_DOWNLOAD_MIN_CHUNK_SIZE).max(1)
    } else {
        segments
    };
    if bounded <= 1 {
        0
    } else {
        bounded as usize
    }
}

fn is_plain_github_provider(provider: &mut dyn StorageProvider) -> bool {
    provider.provider_type() == ProviderType::GitHub
        && !crate::crypt_overlay_provider::is_crypt_overlay_provider(provider)
}

/// Eligibility probe for the segmented download path. Returns
/// `Some(N)` (the final segment count after pool cap + provider
/// capability) when the segmented path can be attempted, or `None`
/// when the caller must use a single-stream download. The capability
/// flags this gates on are the same PD-CLI-CONV-E / PD-SFTP-1 /
/// PD-FTP-1 flips: real session pool kind + strict concurrent range.
pub fn provider_segmented_download_eligible(
    primary: &dyn StorageProvider,
    file_size: u64,
    requested_segments: u32,
    max_workers: usize,
) -> Option<usize> {
    // Capability gate 1: must be a real clone-backed session pool.
    if !matches!(
        primary.transfer_executor_kind(),
        ProviderTransferExecutorKind::HttpClonePool
            | ProviderTransferExecutorKind::SftpConnectionPool
            | ProviderTransferExecutorKind::FtpConnectionPool
    ) {
        return None;
    }

    // Capability gate 2: provider must advertise strict concurrent range
    // download. XFER-01: accept `SupportedAfterProbe` as well as `Supported`,
    // since the runtime range read (and the single-stream fallback on failure)
    // validates providers that can only confirm range safety after a live probe
    // (e.g. several WebDAV servers). `Experimental`/`Unsupported` stay excluded.
    if !matches!(
        primary
            .transfer_capabilities()
            .strict_concurrent_range_download,
        Capability::Supported | Capability::SupportedAfterProbe
    ) {
        return None;
    }

    // Probe `clone_for_transfer()` once so we never start the
    // orchestration on a provider that lies about its pool kind.
    if primary.clone_for_transfer().is_err() {
        return None;
    }

    let segments = provider_segmented_effective_count(file_size, requested_segments);
    if segments < 2 {
        return None;
    }
    let segments = segments.min(max_workers.max(1));
    if segments < 2 {
        None
    } else {
        Some(segments)
    }
}

/// Run the segmented (intra-file range) download path against the
/// shared transport-agnostic engine. The caller is responsible for the
/// capability gate (use [`provider_segmented_download_eligible`]) and
/// for any retry/fallback envelope: this function returns `Ok(())` on
/// success and `Err(_)` on hard failure (no implicit fallback).
///
/// On entry: `segments >= 2`, `file_size >= SEGMENTED_DOWNLOAD_FILE_FLOOR`,
/// and `primary.clone_for_transfer()` is honestly supported.
///
/// On exit: `local_path` is renamed atomically from its `.aerotmp`
/// sibling on success; on any error the engine's `TempFileGuard`
/// drops the temp.
pub async fn run_provider_segmented_download(
    primary: &dyn StorageProvider,
    remote_path: &str,
    local_path: &str,
    file_size: u64,
    segments: usize,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    cancel_token: CancellationToken,
) -> Result<(), String> {
    use crate::providers::multi_thread::{
        aerotmp_path_for, run_concurrent_range_download, ConcurrentRangeConfig,
        ConcurrentRangeOutcome,
    };
    use crate::providers::ProviderError;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    if segments < 2 {
        return Err("segmented download: refusing to run with fewer than 2 segments".to_string());
    }

    // Pre-acquire N independent workers. The first failure aborts the
    // segmented path so the caller can fall back to single-stream.
    let mut workers: Vec<Box<dyn StorageProvider>> = Vec::with_capacity(segments);
    for i in 0..segments {
        match primary.clone_for_transfer() {
            Ok(w) => workers.push(w),
            Err(e) => {
                for mut w in workers {
                    let _ = w.disconnect().await;
                }
                return Err(format!(
                    "segmented download: clone {} of {} failed: {}",
                    i + 1,
                    segments,
                    e
                ));
            }
        }
    }

    let pool = Arc::new(tokio::sync::Mutex::new(VecDeque::from(workers)));
    let remote_owned = remote_path.to_string();

    let cfg = ConcurrentRangeConfig {
        final_path: PathBuf::from(local_path),
        provider_type: primary.provider_type(),
        total_size: file_size,
        streams: segments,
        max_streams: segments,
        max_parallel: segments,
    };

    let write_one_range = move |start_off: u64,
                                end_off: u64,
                                temp_path: PathBuf,
                                aggregate: Arc<std::sync::atomic::AtomicU64>,
                                cancel: CancellationToken| {
        let pool = pool.clone();
        let remote = remote_owned.clone();
        async move {
            use std::sync::atomic::Ordering;
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};

            let mut provider = {
                let mut guard = pool.lock().await;
                guard.pop_front().ok_or_else(|| {
                    ProviderError::TransferFailed(
                        "segmented download: worker pool exhausted (internal invariant)"
                            .to_string(),
                    )
                })?
            };

            let window_len = end_off - start_off + 1;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            file.seek(std::io::SeekFrom::Start(start_off))
                .await
                .map_err(ProviderError::IoError)?;

            let mut written = 0u64;
            while written < window_len {
                if cancel.is_cancelled() {
                    let _ = provider.disconnect().await;
                    return Err(ProviderError::TransferFailed(
                        "Transfer cancelled by user".to_string(),
                    ));
                }
                let sub_len = (window_len - written).min(SEGMENTED_DOWNLOAD_SUB_READ_SIZE);
                let data = provider
                    .read_range(&remote, start_off + written, sub_len)
                    .await
                    .map_err(|e| {
                        ProviderError::TransferFailed(format!(
                            "segmented download: read_range at offset {} failed: {}",
                            start_off + written,
                            e
                        ))
                    })?;
                if data.is_empty() {
                    let _ = provider.disconnect().await;
                    return Err(ProviderError::TransferFailed(format!(
                        "segmented download: short read at offset {} ({} of {} bytes, window [{}, {}])",
                        start_off + written,
                        written,
                        window_len,
                        start_off,
                        end_off
                    )));
                }
                file.write_all(&data)
                    .await
                    .map_err(ProviderError::IoError)?;
                written += data.len() as u64;
                aggregate.fetch_add(data.len() as u64, Ordering::Relaxed);
            }
            file.flush().await.map_err(ProviderError::IoError)?;
            let _ = provider.disconnect().await;

            if written != window_len {
                return Err(ProviderError::TransferFailed(format!(
                    "segmented download: window [{}, {}] wrote {} bytes, expected {}",
                    start_off, end_off, written, window_len
                )));
            }
            Ok(ConcurrentRangeOutcome::Completed)
        }
    };

    let outcome =
        run_concurrent_range_download(cfg, write_one_range, cancel_token, on_progress).await;

    match outcome {
        Ok(ConcurrentRangeOutcome::Completed) => {
            let temp = aerotmp_path_for(Path::new(local_path));
            if let Err(e) = tokio::fs::rename(&temp, local_path).await {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(format!("segmented download: finalize failed: {}", e));
            }
            Ok(())
        }
        Ok(ConcurrentRangeOutcome::ServerIgnoredRange) => Err(
            "segmented download: server ignored Range; falling back to single-stream".to_string(),
        ),
        Err(e) => Err(format!("segmented download: {}", e)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderExecutorSessionModel {
    LockedSingle {
        provider_type: Option<ProviderType>,
    },
    HttpClonePool {
        provider_type: ProviderType,
        max_leases: usize,
    },
    /// SFTP: N independent SSH connections re-dialled by clone workers
    /// (PD-SFTP-1). Same clone-worker dispatch as `HttpClonePool`; the
    /// session pool is labelled `Sftp`, never `HttpClone`.
    SftpConnectionPool {
        provider_type: ProviderType,
        max_leases: usize,
    },
    /// FTP: N independent FTP connections re-dialled by clone workers
    /// (PD-FTP-1, the FTP mirror of `SftpConnectionPool`). Same
    /// clone-worker dispatch; the session pool is labelled `Ftp`, never
    /// `HttpClone`. Distinct from the legacy executor-managed
    /// `FtpSessionPool` GUI path (which never reaches this resolver).
    FtpConnectionPool {
        provider_type: ProviderType,
        max_leases: usize,
    },
}

impl ProviderExecutorSessionModel {
    fn locked(provider_type: Option<ProviderType>) -> Self {
        Self::LockedSingle { provider_type }
    }

    fn session_pool(&self, label: &'static str) -> TransferSessionPoolHandle {
        match self {
            Self::LockedSingle { .. } => TransferSessionPoolHandle::legacy_single(label),
            Self::HttpClonePool { max_leases, .. } => {
                TransferSessionPoolHandle::http_clone(label, *max_leases)
            }
            Self::SftpConnectionPool { max_leases, .. } => {
                TransferSessionPoolHandle::sftp_connection(label, *max_leases)
            }
            Self::FtpConnectionPool { max_leases, .. } => {
                TransferSessionPoolHandle::ftp_connection(label, *max_leases)
            }
        }
    }

    fn is_clone_pool(&self) -> bool {
        matches!(
            self,
            Self::HttpClonePool { .. }
                | Self::SftpConnectionPool { .. }
                | Self::FtpConnectionPool { .. }
        )
    }

    /// Lease budget the resolver committed to. `LockedSingle` is 1;
    /// every pool kind carries its own resolved `max_leases`.
    pub fn max_leases(&self) -> usize {
        match self {
            Self::LockedSingle { .. } => 1,
            Self::HttpClonePool { max_leases, .. } => *max_leases,
            Self::SftpConnectionPool { max_leases, .. } => *max_leases,
            Self::FtpConnectionPool { max_leases, .. } => *max_leases,
        }
    }

    pub fn provider_type(&self) -> Option<ProviderType> {
        match self {
            Self::LockedSingle { provider_type } => *provider_type,
            Self::HttpClonePool { provider_type, .. }
            | Self::SftpConnectionPool { provider_type, .. }
            | Self::FtpConnectionPool { provider_type, .. } => Some(*provider_type),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderListSessionModel {
    LockedSingle {
        provider_type: Option<ProviderType>,
    },
    HttpClonePool {
        provider_type: ProviderType,
        max_leases: usize,
    },
}

impl ProviderListSessionModel {
    fn locked(provider_type: Option<ProviderType>) -> Self {
        Self::LockedSingle { provider_type }
    }

    pub fn session_pool(&self, label: &'static str) -> TransferSessionPoolHandle {
        match self {
            Self::LockedSingle { .. } => TransferSessionPoolHandle::legacy_single(label),
            Self::HttpClonePool { max_leases, .. } => {
                TransferSessionPoolHandle::http_list_clone(label, *max_leases)
            }
        }
    }

    pub fn max_leases(&self) -> usize {
        match self {
            Self::LockedSingle { .. } => 1,
            Self::HttpClonePool { max_leases, .. } => *max_leases,
        }
    }

    pub fn is_clone_pool(&self) -> bool {
        matches!(self, Self::HttpClonePool { .. })
    }
}

/// Decide the transfer session model purely from a provider's transfer
/// capabilities and declared executor kind (DAG-ENGINE F3-T06).
///
/// This is the executor-kind decision extracted as a pure function: no async,
/// no mutex lock, no I/O. `can_clone` is the one fact a probe must supply
/// (`clone_for_transfer().is_ok()`), and `advertised_max_sessions` is the
/// provider's fallback lease count when the capabilities do not pin one.
///
/// The decision is byte-identical to the previous inline logic; isolating it
/// makes the capability-driven choice unit-testable without a live provider.
pub fn resolve_session_model(
    provider_type: ProviderType,
    caps: &TransferCapabilities,
    executor_kind: ProviderTransferExecutorKind,
    can_clone: bool,
    advertised_max_sessions: u16,
    max_concurrent: usize,
) -> ProviderExecutorSessionModel {
    let scheduler_can_parallelize = caps.file_parallel == Capability::Supported
        && caps.session_pool == Capability::Supported
        && can_clone;

    if !scheduler_can_parallelize {
        return ProviderExecutorSessionModel::locked(Some(provider_type));
    }

    let max_leases = caps
        .max_file_slots
        .unwrap_or(advertised_max_sessions)
        .max(1) as usize;
    let max_leases = max_leases.min(max_concurrent.max(1)).max(1);

    match executor_kind {
        ProviderTransferExecutorKind::HttpClonePool => {
            ProviderExecutorSessionModel::HttpClonePool {
                provider_type,
                max_leases,
            }
        }
        ProviderTransferExecutorKind::SftpConnectionPool => {
            ProviderExecutorSessionModel::SftpConnectionPool {
                provider_type,
                max_leases,
            }
        }
        ProviderTransferExecutorKind::FtpConnectionPool => {
            ProviderExecutorSessionModel::FtpConnectionPool {
                provider_type,
                max_leases,
            }
        }
        ProviderTransferExecutorKind::LockedSingle => {
            ProviderExecutorSessionModel::locked(Some(provider_type))
        }
    }
}

/// Thin async wrapper over [`resolve_session_model`]: locks the provider,
/// gathers the capability inputs and the `clone_for_transfer` probe, then
/// delegates the decision to the pure function.
pub async fn resolve_provider_executor_session_model(
    provider: &Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    max_concurrent: usize,
) -> ProviderExecutorSessionModel {
    let provider_lock = provider.lock().await;
    let Some(provider) = provider_lock.as_ref() else {
        return ProviderExecutorSessionModel::locked(None);
    };

    resolve_session_model(
        provider.provider_type(),
        &provider.transfer_capabilities(),
        provider.transfer_executor_kind(),
        provider.clone_for_transfer().is_ok(),
        provider.transfer_executor_max_sessions(),
        max_concurrent,
    )
}

pub async fn resolve_provider_list_session_model(
    provider: &Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    requested_checkers: usize,
) -> ProviderListSessionModel {
    let provider_lock = provider.lock().await;
    let Some(provider) = provider_lock.as_ref() else {
        return ProviderListSessionModel::locked(None);
    };

    let provider_type = provider.provider_type();
    let caps = provider.transfer_capabilities();
    let scanner_can_clone =
        provider.list_executor_kind() == ProviderListExecutorKind::HttpClonePool;
    let scheduler_can_parallelize =
        caps.list_parallel == Capability::Supported && provider.clone_for_list().is_ok();

    if scanner_can_clone && scheduler_can_parallelize {
        let advertised = caps
            .max_checker_slots
            .unwrap_or_else(|| provider.list_executor_max_sessions())
            .max(1) as usize;
        ProviderListSessionModel::HttpClonePool {
            provider_type,
            max_leases: advertised.min(requested_checkers.max(1)).max(1),
        }
    } else {
        ProviderListSessionModel::locked(Some(provider_type))
    }
}

pub struct ProviderDownloadExecutor {
    sink: Arc<dyn TransferEventSink>,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
    session_model: ProviderExecutorSessionModel,
}

impl ProviderDownloadExecutor {
    pub fn new(
        sink: Arc<dyn TransferEventSink>,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        cancel_token: CancellationToken,
        session_model: ProviderExecutorSessionModel,
    ) -> Self {
        Self {
            sink,
            provider,
            runtime_settings,
            cancel_token,
            session_model,
        }
    }

    async fn clone_worker(&self) -> Result<Box<dyn StorageProvider>, String> {
        let provider_lock = self.provider.lock().await;
        provider_lock
            .as_ref()
            .ok_or_else(|| "Provider disconnected".to_string())
            .and_then(|provider| provider.clone_for_transfer().map_err(|e| e.to_string()))
    }

    async fn execute_locked(&self, entry: TransferEntry) -> TransferOutcome {
        let mut provider_lock = self.provider.lock().await;
        match provider_lock.as_mut() {
            Some(provider) => self.execute_with_provider(entry, provider.as_mut()).await,
            None => self.failed_download(entry, "Provider disconnected".to_string()),
        }
    }

    async fn execute_with_provider(
        &self,
        entry: TransferEntry,
        provider: &mut dyn StorageProvider,
    ) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();
        self.emit_download_start(&entry, &file_transfer_id);

        let retry_policy = self.runtime_settings.retry_policy();
        let mut last_error = String::new();

        for attempt in 0..=retry_policy.max_retries {
            if self.cancel_token.is_cancelled() {
                last_error = "Transfer cancelled by user".to_string();
                break;
            }

            if attempt > 0 {
                let delay = retry_policy.delay_for_attempt(attempt);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                    _ = self.cancel_token.cancelled() => {
                        last_error = "Transfer cancelled by user".to_string();
                        break;
                    }
                }
            }

            match self
                .download_attempt(provider, &entry, &file_transfer_id, attempt)
                .await
            {
                Ok(()) => {
                    crate::preserve_remote_mtime(&entry.local_path, entry.modified.as_deref());
                    self.emit_download_complete(&entry, &file_transfer_id);
                    return TransferOutcome::Success;
                }
                Err(error) => {
                    last_error = error;
                    if self.cancel_token.is_cancelled() {
                        break;
                    }
                    let err_info =
                        crate::sync::classify_sync_error(&last_error, Some(&entry.remote_path));
                    if attempt >= retry_policy.max_retries || !err_info.retryable {
                        break;
                    }
                    warn!(
                        "Retrying provider download {} (attempt {}/{}): {}",
                        entry.remote_path,
                        attempt + 1,
                        retry_policy.max_retries,
                        err_info.message
                    );
                }
            }
        }

        self.failed_download(entry, last_error)
    }

    async fn download_attempt(
        &self,
        provider: &mut dyn StorageProvider,
        entry: &TransferEntry,
        file_transfer_id: &str,
        attempt: u32,
    ) -> Result<(), String> {
        let sink = Arc::clone(&self.sink);
        let transfer_id = file_transfer_id.to_string();
        let display_name = entry.display_name.clone();
        let remote_path = entry.remote_path.clone();
        let remote_path_for_progress = entry.remote_path.clone();
        let local_path = entry.local_path.clone();
        let file_size = entry.size;
        let cancel_token = self.cancel_token.clone();
        let dl_start = std::time::Instant::now();

        let tmp_path = format!("{}.aerotmp", local_path);
        let partial_offset = if attempt > 0 && provider.supports_resume() {
            tokio::fs::metadata(&tmp_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        // GTC-1: opportunistic intra-file range parallelism.
        //
        // Gated on (attempt == 0 && no .aerotmp resume) so a hard
        // failure in the segmented path cleanly degrades to the legacy
        // single-stream path on the next retry, preserving the existing
        // retry envelope. The capability check inside
        // `try_segmented_download_attempt` re-locks the session model,
        // provider capability flag, and file-size floor honestly.
        if attempt == 0 && partial_offset == 0 && self.runtime_settings.download_segments > 1 {
            if let Some(segmented_result) = self
                .try_segmented_download_attempt(
                    provider,
                    entry,
                    file_transfer_id,
                    dl_start,
                    file_size,
                )
                .await
            {
                return segmented_result;
            }
        }

        let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> =
            Some(Box::new(move |transferred, total| {
                if cancel_token.is_cancelled() {
                    return;
                }
                let percentage = if total > 0 {
                    ((transferred as f64 / total as f64) * 100.0) as u8
                } else {
                    0
                };
                let elapsed = dl_start.elapsed().as_secs_f64();
                let speed = if elapsed > 0.1 {
                    (transferred as f64 / elapsed) as u64
                } else {
                    0
                };
                let remaining = total.max(file_size).saturating_sub(transferred);
                let eta = if speed > 0 {
                    (remaining as f64 / speed as f64) as u64
                } else {
                    0
                };
                sink.emit_transfer_event(crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: display_name.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: display_name.clone(),
                        transferred,
                        total: total.max(file_size),
                        percentage,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        direction: "download".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: Some(remote_path_for_progress.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                });
            }));

        let size_based_secs = file_size / 50_000;
        let effective_timeout = self
            .runtime_settings
            .timeout_seconds
            .max(size_based_secs + self.runtime_settings.timeout_seconds);

        let dl_future = if partial_offset > 0 {
            tracing::info!(
                "Resuming download from {} bytes (attempt {}): {}",
                partial_offset,
                attempt,
                remote_path
            );
            provider.resume_download(&remote_path, &local_path, partial_offset, progress_cb)
        } else {
            provider.download(&remote_path, &local_path, progress_cb)
        };

        // FINDING-4 Part B: race the in-flight download against the session
        // cancel token so a user Stop drops the current file promptly instead of
        // only taking effect at the next file/retry boundary. Dropping the future
        // tears the transport stream down (russh SFTP is async).
        let cancel = self.cancel_token.clone();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("Transfer cancelled by user".to_string()),
            res = tokio::time::timeout(Duration::from_secs(effective_timeout), dl_future) => {
                match res {
                    Ok(result) => result.map_err(|e| e.to_string()),
                    Err(_) => Err(format!(
                        "Download timed out after {} seconds",
                        effective_timeout
                    )),
                }
            }
        }
    }

    /// Try the segmented (intra-file range) download path. Returns
    /// `None` when the path is not applicable — the caller falls
    /// through to the legacy single-stream branch. Returns
    /// `Some(Ok(()))` on success (the legacy branch is then skipped)
    /// and `Some(Err(_))` on hard failure (the caller's retry envelope
    /// is responsible for the next attempt, which will be
    /// single-stream because the segmented branch is gated on
    /// `attempt == 0`).
    async fn try_segmented_download_attempt(
        &self,
        primary: &mut dyn StorageProvider,
        entry: &TransferEntry,
        file_transfer_id: &str,
        dl_start: std::time::Instant,
        file_size: u64,
    ) -> Option<Result<(), String>> {
        // Single-source-of-truth gate: capability + session-pool kind +
        // anti-fragmentation count + pool-cap clamp.
        let segments = provider_segmented_download_eligible(
            primary,
            file_size,
            self.runtime_settings.download_segments,
            self.session_model.max_leases(),
        )?;

        // Progress bridge to the executor's TransferEventSink, same
        // shape as the legacy progress callback.
        let sink = Arc::clone(&self.sink);
        let transfer_id = file_transfer_id.to_string();
        let display_name = entry.display_name.clone();
        let remote_path_for_progress = entry.remote_path.clone();
        let cancel_for_progress = self.cancel_token.clone();
        let total_for_progress = file_size;
        let on_progress: Option<Box<dyn Fn(u64, u64) + Send>> =
            Some(Box::new(move |transferred, total| {
                if cancel_for_progress.is_cancelled() {
                    return;
                }
                let total = total.max(total_for_progress);
                let percentage = if total > 0 {
                    ((transferred as f64 / total as f64) * 100.0) as u8
                } else {
                    0
                };
                let elapsed = dl_start.elapsed().as_secs_f64();
                let speed = if elapsed > 0.1 {
                    (transferred as f64 / elapsed) as u64
                } else {
                    0
                };
                let remaining = total.saturating_sub(transferred);
                let eta = if speed > 0 {
                    (remaining as f64 / speed as f64) as u64
                } else {
                    0
                };
                sink.emit_transfer_event(crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: display_name.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: display_name.clone(),
                        transferred,
                        total,
                        percentage,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        direction: "download".to_string(),
                        total_files: None,
                        path: None,
                    }),
                    path: Some(remote_path_for_progress.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                });
            }));

        Some(
            run_provider_segmented_download(
                primary,
                &entry.remote_path,
                &entry.local_path,
                file_size,
                segments,
                on_progress,
                self.cancel_token.clone(),
            )
            .await,
        )
    }

    fn emit_download_start(&self, entry: &TransferEntry, file_transfer_id: &str) {
        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_start".to_string(),
            transfer_id: file_transfer_id.to_string(),
            filename: entry.display_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Downloading: {}", entry.remote_path)),
            progress: Some(crate::TransferProgress {
                transfer_id: file_transfer_id.to_string(),
                filename: entry.display_name.clone(),
                transferred: 0,
                total: entry.size,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: "download".to_string(),
                total_files: None,
                path: None,
            }),
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });
    }

    fn emit_download_complete(&self, entry: &TransferEntry, file_transfer_id: &str) {
        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_complete".to_string(),
            transfer_id: file_transfer_id.to_string(),
            filename: entry.display_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Downloaded: {}", entry.display_name)),
            progress: None,
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });
    }

    fn failed_download(&self, entry: TransferEntry, last_error: String) -> TransferOutcome {
        let failure = if self.cancel_token.is_cancelled() || last_error.contains("cancelled") {
            TransferFailure {
                kind: crate::transfer_domain::TransferFailureKind::Cancelled,
                message: "Transfer cancelled by user".to_string(),
                retryable: false,
            }
        } else {
            let error_info =
                crate::sync::classify_sync_error(&last_error, Some(&entry.remote_path));
            let failure_kind = transfer_failure_kind_from_sync(&error_info.kind);
            TransferFailure {
                kind: failure_kind,
                message: user_facing_transfer_failure_message(&failure_kind).to_string(),
                retryable: error_info.retryable,
            }
        };

        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_error".to_string(),
            transfer_id: entry.id,
            filename: entry.display_name.clone(),
            direction: "download".to_string(),
            message: Some(failure.message.clone()),
            progress: None,
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });

        TransferOutcome::Failed(failure)
    }
}

pub struct ProviderUploadExecutor {
    sink: Arc<dyn TransferEventSink>,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    commit_message: Option<String>,
    cancel_token: CancellationToken,
    session_model: ProviderExecutorSessionModel,
}

impl ProviderUploadExecutor {
    pub fn new(
        sink: Arc<dyn TransferEventSink>,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        commit_message: Option<String>,
        cancel_token: CancellationToken,
        session_model: ProviderExecutorSessionModel,
    ) -> Self {
        Self {
            sink,
            provider,
            runtime_settings,
            commit_message,
            cancel_token,
            session_model,
        }
    }

    async fn clone_worker(&self) -> Result<Box<dyn StorageProvider>, String> {
        let provider_lock = self.provider.lock().await;
        provider_lock
            .as_ref()
            .ok_or_else(|| "Provider disconnected".to_string())
            .and_then(|provider| provider.clone_for_transfer().map_err(|e| e.to_string()))
    }

    async fn execute_locked(&self, entry: TransferEntry) -> TransferOutcome {
        let mut provider_lock = self.provider.lock().await;
        match provider_lock.as_mut() {
            Some(provider) => self.execute_with_provider(entry, provider.as_mut()).await,
            None => self.failed_upload(entry, "Provider disconnected".to_string()),
        }
    }

    async fn execute_with_provider(
        &self,
        entry: TransferEntry,
        provider: &mut dyn StorageProvider,
    ) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();
        let file_size = transfer_entry_upload_size(&entry).await;
        self.emit_upload_start(&entry, &file_transfer_id, file_size);

        let retry_policy = self.runtime_settings.retry_policy();
        let mut last_error = String::new();

        for attempt in 0..=retry_policy.max_retries {
            if self.cancel_token.is_cancelled() {
                last_error = "Transfer cancelled by user".to_string();
                break;
            }

            if attempt > 0 {
                let delay = retry_policy.delay_for_attempt(attempt);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                    _ = self.cancel_token.cancelled() => {
                        last_error = "Transfer cancelled by user".to_string();
                        break;
                    }
                }
            }

            match self
                .upload_attempt(provider, &entry, &file_transfer_id, file_size)
                .await
            {
                Ok(()) => {
                    self.emit_upload_complete(&entry, &file_transfer_id);
                    return TransferOutcome::Success;
                }
                Err(error) => {
                    last_error = error;
                    if self.cancel_token.is_cancelled() {
                        break;
                    }
                    let err_info =
                        crate::sync::classify_sync_error(&last_error, Some(&entry.local_path));
                    if attempt >= retry_policy.max_retries || !err_info.retryable {
                        break;
                    }
                    warn!(
                        "Retrying provider upload {} (attempt {}/{}): {}",
                        entry.local_path,
                        attempt + 1,
                        retry_policy.max_retries,
                        err_info.message
                    );
                }
            }
        }

        self.failed_upload(entry, last_error)
    }

    async fn upload_attempt(
        &self,
        provider: &mut dyn StorageProvider,
        entry: &TransferEntry,
        file_transfer_id: &str,
        file_size: u64,
    ) -> Result<(), String> {
        let remote_path = entry.remote_path.clone();
        let local_path = entry.local_path.clone();
        // Reject names with characters this provider's backend forbids before
        // the per-file upload (covers folder uploads and engine/CLI transfers).
        // Surfaces as a clear per-file error instead of a silent backend failure.
        crate::restricted_chars::validate_path(provider.provider_type(), &remote_path)
            .map_err(|e| e.to_string())?;
        let commit_message = self.commit_message.clone();
        let size_secs = file_size / 50_000;
        let eff_timeout = self
            .runtime_settings
            .timeout_seconds
            .max(size_secs + self.runtime_settings.timeout_seconds);

        if is_plain_github_provider(provider) {
            let github = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            let upload = async {
                match tokio::time::timeout(
                    Duration::from_secs(eff_timeout),
                    github.upload_file(&local_path, &remote_path, commit_message.as_deref()),
                )
                .await
                {
                    Ok(result) => result.map_err(|e| e.to_string()),
                    Err(_) => Err(format!("Upload timed out after {} seconds", eff_timeout)),
                }
            };
            return tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    Err("Transfer cancelled by user".to_string())
                }
                result = upload => result,
            };
        }

        let sink = Arc::clone(&self.sink);
        let transfer_id = file_transfer_id.to_string();
        let display_name = entry.display_name.clone();
        let remote_path_for_progress = entry.remote_path.clone();
        let cancel_token = self.cancel_token.clone();
        // FINDING-4 Part B: separate handle to race the in-flight upload against
        // a user Stop (the one above is moved into the progress closure).
        let cancel_race = self.cancel_token.clone();
        let ul_start = std::time::Instant::now();

        let upload_fut = tokio::time::timeout(
            Duration::from_secs(eff_timeout),
            provider.upload(
                &local_path,
                &remote_path,
                Some(Box::new(move |transferred, total| {
                    if cancel_token.is_cancelled() {
                        return;
                    }

                    let percentage = if total > 0 {
                        ((transferred as f64 / total as f64) * 100.0) as u8
                    } else {
                        0
                    };
                    let elapsed = ul_start.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.1 {
                        (transferred as f64 / elapsed) as u64
                    } else {
                        0
                    };
                    let remaining = total.max(file_size).saturating_sub(transferred);
                    let eta = if speed > 0 {
                        (remaining as f64 / speed as f64) as u64
                    } else {
                        0
                    };

                    sink.emit_transfer_event(crate::TransferEvent {
                        event_type: "progress".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: display_name.clone(),
                        direction: "upload".to_string(),
                        message: None,
                        progress: Some(crate::TransferProgress {
                            transfer_id: transfer_id.clone(),
                            filename: display_name.clone(),
                            transferred,
                            total: total.max(file_size),
                            percentage,
                            speed_bps: speed,
                            eta_seconds: eta as u32,
                            direction: "upload".to_string(),
                            total_files: None,
                            path: None,
                        }),
                        path: Some(remote_path_for_progress.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    });
                })),
            ),
        );
        tokio::select! {
            biased;
            _ = cancel_race.cancelled() => Err("Transfer cancelled by user".to_string()),
            res = upload_fut => match res {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err(format!("Upload timed out after {} seconds", eff_timeout)),
            },
        }
    }

    fn emit_upload_start(&self, entry: &TransferEntry, file_transfer_id: &str, file_size: u64) {
        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_start".to_string(),
            transfer_id: file_transfer_id.to_string(),
            filename: entry.display_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Uploading: {}", entry.remote_path)),
            progress: Some(crate::TransferProgress {
                transfer_id: file_transfer_id.to_string(),
                filename: entry.display_name.clone(),
                transferred: 0,
                total: file_size,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: "upload".to_string(),
                total_files: None,
                path: None,
            }),
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });
    }

    fn emit_upload_complete(&self, entry: &TransferEntry, file_transfer_id: &str) {
        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_complete".to_string(),
            transfer_id: file_transfer_id.to_string(),
            filename: entry.display_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Uploaded: {}", entry.display_name)),
            progress: None,
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });
    }

    fn failed_upload(&self, entry: TransferEntry, last_error: String) -> TransferOutcome {
        let failure = if self.cancel_token.is_cancelled() || last_error.contains("cancelled") {
            TransferFailure {
                kind: crate::transfer_domain::TransferFailureKind::Cancelled,
                message: "Transfer cancelled by user".to_string(),
                retryable: false,
            }
        } else {
            let error_info = crate::sync::classify_sync_error(&last_error, Some(&entry.local_path));
            let failure_kind = transfer_failure_kind_from_sync(&error_info.kind);
            TransferFailure {
                kind: failure_kind,
                message: user_facing_transfer_failure_message(&failure_kind).to_string(),
                retryable: error_info.retryable,
            }
        };

        self.sink.emit_transfer_event(crate::TransferEvent {
            event_type: "file_error".to_string(),
            transfer_id: entry.id,
            filename: entry.display_name.clone(),
            direction: "upload".to_string(),
            message: Some(failure.message.clone()),
            progress: None,
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });

        TransferOutcome::Failed(failure)
    }
}

#[async_trait]
impl TransferExecutor for ProviderDownloadExecutor {
    fn provider_type(&self) -> Option<ProviderType> {
        self.session_model.provider_type()
    }

    fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
        self.session_model.session_pool("provider-download")
    }

    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        self.execute_locked(entry).await
    }

    async fn execute_with_session(
        &self,
        entry: TransferEntry,
        session_lease: TransferSessionLease,
    ) -> TransferOutcome {
        if !self.session_model.is_clone_pool() {
            let outcome = self.execute_locked(entry).await;
            drop(session_lease);
            return outcome;
        }

        let outcome = match self.clone_worker().await {
            Ok(mut provider) => self.execute_with_provider(entry, provider.as_mut()).await,
            Err(error) => self.failed_download(entry, error),
        };
        drop(session_lease);
        outcome
    }
}

#[async_trait]
impl TransferExecutor for ProviderUploadExecutor {
    fn provider_type(&self) -> Option<ProviderType> {
        self.session_model.provider_type()
    }

    fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
        self.session_model.session_pool("provider-upload")
    }

    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        self.execute_locked(entry).await
    }

    async fn execute_with_session(
        &self,
        entry: TransferEntry,
        session_lease: TransferSessionLease,
    ) -> TransferOutcome {
        if !self.session_model.is_clone_pool() {
            let outcome = self.execute_locked(entry).await;
            drop(session_lease);
            return outcome;
        }

        let outcome = match self.clone_worker().await {
            Ok(mut provider) => self.execute_with_provider(entry, provider.as_mut()).await,
            Err(error) => self.failed_upload(entry, error),
        };
        drop(session_lease);
        outcome
    }
}

async fn transfer_entry_upload_size(entry: &TransferEntry) -> u64 {
    if entry.size > 0 {
        entry.size
    } else {
        tokio::fs::metadata(&entry.local_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::SessionLeaseKind;

    #[test]
    fn effective_segments_below_file_floor_returns_zero() {
        // 4 MiB file with 4 segments requested: below the 8 MiB floor,
        // segmented path must be skipped.
        let n = provider_segmented_effective_count(4 * 1024 * 1024, 4);
        assert_eq!(n, 0);
    }

    #[test]
    fn effective_segments_single_request_returns_zero() {
        // Even a large file with `segments == 1` is just single-stream.
        let n = provider_segmented_effective_count(64 * 1024 * 1024, 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn effective_segments_full_file_above_floor_returns_request() {
        // 64 MiB / 4 = 16 MiB per chunk, well above the 1 MiB
        // anti-fragmentation floor.
        let n = provider_segmented_effective_count(64 * 1024 * 1024, 4);
        assert_eq!(n, 4);
    }

    #[test]
    fn effective_segments_caps_to_max_16() {
        // The executor itself clamps to 16 inside `effective_segments`
        // (`download_segments` is also clamped at resolve time, but the
        // helper must stay defensive).
        let n = provider_segmented_effective_count(1024 * 1024 * 1024, 64);
        assert_eq!(n, 16);
    }

    #[test]
    fn effective_segments_anti_fragmentation_drops_count() {
        // 9 MiB / 16 segments would mean ~576 KiB per chunk, below the
        // 1 MiB minimum. The helper must reduce the count so each chunk
        // is at least 1 MiB.
        let n = provider_segmented_effective_count(9 * 1024 * 1024, 16);
        // 9 MiB / 1 MiB = 9 → cap at 9.
        assert_eq!(n, 9);
    }

    #[test]
    fn locked_provider_model_uses_single_legacy_lease() {
        let model = ProviderExecutorSessionModel::locked(Some(ProviderType::Sftp));
        let pool = model.session_pool("provider-test");

        assert_eq!(pool.capacity().kind, SessionLeaseKind::LegacySingle);
        assert_eq!(pool.capacity().max_leases, 1);
    }

    #[test]
    fn http_clone_provider_model_clamps_to_resolved_capacity() {
        let model = ProviderExecutorSessionModel::HttpClonePool {
            provider_type: ProviderType::S3,
            max_leases: 4,
        };
        let pool = model.session_pool("provider-test");

        assert_eq!(pool.capacity().kind, SessionLeaseKind::HttpClone);
        assert_eq!(pool.capacity().max_leases, 4);
    }

    #[test]
    fn sftp_connection_model_uses_sftp_lease_kind_not_http() {
        let model = ProviderExecutorSessionModel::SftpConnectionPool {
            provider_type: ProviderType::Sftp,
            max_leases: 4,
        };
        let pool = model.session_pool("provider-test");

        assert!(model.is_clone_pool());
        assert_eq!(pool.capacity().kind, SessionLeaseKind::Sftp);
        assert_eq!(pool.capacity().max_leases, 4);
    }

    #[test]
    fn locked_list_model_uses_single_legacy_lease() {
        let model = ProviderListSessionModel::locked(Some(ProviderType::WebDav));
        let pool = model.session_pool("provider-list-test");

        assert_eq!(pool.capacity().kind, SessionLeaseKind::LegacySingle);
        assert_eq!(pool.capacity().max_leases, 1);
    }

    #[test]
    fn http_clone_list_model_uses_list_lease_kind() {
        let model = ProviderListSessionModel::HttpClonePool {
            provider_type: ProviderType::S3,
            max_leases: 3,
        };
        let pool = model.session_pool("provider-list-test");

        assert_eq!(pool.capacity().kind, SessionLeaseKind::HttpList);
        assert_eq!(pool.capacity().max_leases, 3);
    }

    // ---- F3-T06: pure executor-kind resolver --------------------------------

    /// Capabilities that advertise a real parallel session pool.
    fn parallel_caps() -> TransferCapabilities {
        TransferCapabilities {
            file_parallel: Capability::Supported,
            session_pool: Capability::Supported,
            ..TransferCapabilities::default()
        }
    }

    #[test]
    fn resolve_session_model_locks_when_not_parallelizable() {
        // file_parallel Unsupported: locked regardless of the executor kind.
        let model = resolve_session_model(
            ProviderType::S3,
            &TransferCapabilities::default(),
            ProviderTransferExecutorKind::HttpClonePool,
            true,
            8,
            4,
        );
        assert!(matches!(
            model,
            ProviderExecutorSessionModel::LockedSingle { .. }
        ));
    }

    #[test]
    fn resolve_session_model_locks_when_clone_is_unavailable() {
        // The capabilities advertise a pool, but the clone probe failed.
        let model = resolve_session_model(
            ProviderType::Ftp,
            &parallel_caps(),
            ProviderTransferExecutorKind::FtpConnectionPool,
            false,
            8,
            4,
        );
        assert!(matches!(
            model,
            ProviderExecutorSessionModel::LockedSingle { .. }
        ));
    }

    #[test]
    fn resolve_session_model_clamps_http_clone_leases_to_max_concurrent() {
        let caps = TransferCapabilities {
            max_file_slots: Some(8),
            ..parallel_caps()
        };
        let model = resolve_session_model(
            ProviderType::S3,
            &caps,
            ProviderTransferExecutorKind::HttpClonePool,
            true,
            8,
            3,
        );
        match model {
            ProviderExecutorSessionModel::HttpClonePool { max_leases, .. } => {
                assert_eq!(max_leases, 3, "user max_concurrent caps the lease count");
            }
            other => panic!("expected HttpClonePool, got {other:?}"),
        }
    }

    #[test]
    fn resolve_session_model_keeps_sftp_and_ftp_kinds_distinct() {
        let caps = TransferCapabilities {
            max_file_slots: Some(4),
            ..parallel_caps()
        };
        let sftp = resolve_session_model(
            ProviderType::Sftp,
            &caps,
            ProviderTransferExecutorKind::SftpConnectionPool,
            true,
            4,
            8,
        );
        assert!(matches!(
            sftp,
            ProviderExecutorSessionModel::SftpConnectionPool { max_leases: 4, .. }
        ));
        let ftp = resolve_session_model(
            ProviderType::Ftp,
            &caps,
            ProviderTransferExecutorKind::FtpConnectionPool,
            true,
            4,
            8,
        );
        assert!(matches!(
            ftp,
            ProviderExecutorSessionModel::FtpConnectionPool { max_leases: 4, .. }
        ));
    }

    #[test]
    fn resolve_session_model_falls_back_to_advertised_sessions_without_caps_slots() {
        // max_file_slots None: the provider's advertised count is the source.
        let caps = TransferCapabilities {
            max_file_slots: None,
            ..parallel_caps()
        };
        let model = resolve_session_model(
            ProviderType::S3,
            &caps,
            ProviderTransferExecutorKind::HttpClonePool,
            true,
            6,
            16,
        );
        match model {
            ProviderExecutorSessionModel::HttpClonePool { max_leases, .. } => {
                assert_eq!(max_leases, 6, "advertised session count is the fallback");
            }
            other => panic!("expected HttpClonePool, got {other:?}"),
        }
    }
}
