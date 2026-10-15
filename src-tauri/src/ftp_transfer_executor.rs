// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! FTP-backed transfer executor for the shared orchestrator.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::ftp_session_pool::{FtpSessionLease, FtpSessionPool};
use crate::transfer_dag::{TransferSessionLease, TransferSessionPoolHandle};
use crate::transfer_domain::{
    TransferEntry, TransferFailure, TransferFailureKind, TransferOutcome,
};
use crate::transfer_orchestrator::TransferExecutor;
use crate::transfer_settings::ResolvedTransferSettings;

/// DAG-P2-07: whole-file attempts per entry id (first try counts as 1, so
/// retries = attempts - 1). Removed on read via
/// [`TransferExecutor::take_transfer_attempts`] so nothing accumulates.
type AttemptCounts = std::sync::Mutex<std::collections::HashMap<String, u32>>;

/// Real pool handle, so the DAG waits for a session before the governor job.
fn executor_session_pool(label: &str, pool: &Arc<FtpSessionPool>) -> TransferSessionPoolHandle {
    TransferSessionPoolHandle::ftp(label, Arc::clone(pool))
}

/// Health-check and cwd reset before an attempt. A failed check meters nothing.
async fn prepare_lease(lease: &FtpSessionLease) -> Result<(), String> {
    lease.ensure_healthy().await?;
    lease.reset_state().await
}

/// Meter one attempt directly before its real transfer runs (first try
/// counts as 1, so retries = attempts - 1): a cancel during backoff or
/// mid-transfer still leaves an honest count behind, and a session that
/// failed its readiness check before the attempt never touched the wire, so
/// it meters nothing. Same convention as `note_transfer_attempt` in the
/// provider executor.
fn note_transfer_attempt(attempt_counts: &AttemptCounts, entry_id: &str, attempt: u32) {
    attempt_counts
        .lock()
        .expect("attempt counts poisoned")
        .insert(entry_id.to_string(), attempt);
}

/// Terminal outcome for a caller that never got a session (`execute()` on a
/// closed or exhausted pool): the same file_start / file_error pair the
/// retry loop emits around its attempts, with the acquire error as the
/// failure. The DAG path reports a session-acquire failure itself and never
/// calls the executor without a lease.
fn fail_without_session(
    app: &AppHandle,
    entry: &TransferEntry,
    direction: &str,
    error: &str,
    cancelled: bool,
) -> TransferOutcome {
    let verb = if direction == "download" {
        "Downloading"
    } else {
        "Uploading"
    };
    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type: "file_start".to_string(),
            transfer_id: entry.id.clone(),
            filename: entry.display_name.clone(),
            direction: direction.to_string(),
            message: Some(format!("{verb}: {}", entry.remote_path)),
            progress: Some(crate::TransferProgress {
                transfer_id: entry.id.clone(),
                filename: entry.display_name.clone(),
                transferred: 0,
                total: entry.size,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: direction.to_string(),
                total_files: None,
                path: None,
            }),
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let failure = if cancelled {
        TransferFailure::new(
            TransferFailureKind::Cancelled,
            "Transfer cancelled by user",
            false,
        )
    } else {
        TransferFailure::from_raw_message(error)
    };

    crate::transfer_event_sink::emit_gui_transfer_event(
        app,
        crate::TransferEvent {
            event_type: "file_error".to_string(),
            transfer_id: entry.id.clone(),
            filename: entry.display_name.clone(),
            direction: direction.to_string(),
            message: Some(failure.message.clone()),
            progress: None,
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    TransferOutcome::Failed(failure)
}

pub struct FtpDownloadExecutor {
    app: AppHandle,
    pool: Arc<FtpSessionPool>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
    attempt_counts: AttemptCounts,
}

impl FtpDownloadExecutor {
    pub fn new(
        app: AppHandle,
        pool: Arc<FtpSessionPool>,
        runtime_settings: ResolvedTransferSettings,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            app,
            pool,
            runtime_settings,
            cancel_token,
            attempt_counts: AttemptCounts::default(),
        }
    }

    /// Retry against the lease the DAG already holds, then release it once.
    async fn execute_with_lease(
        &self,
        entry: TransferEntry,
        lease: FtpSessionLease,
    ) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();

        crate::transfer_event_sink::emit_gui_transfer_event(
            &self.app,
            crate::TransferEvent {
                event_type: "file_start".to_string(),
                transfer_id: file_transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "download".to_string(),
                message: Some(format!("Downloading: {}", entry.remote_path)),
                progress: Some(crate::TransferProgress {
                    transfer_id: file_transfer_id.clone(),
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
            },
        );

        let retry_policy = self.runtime_settings.retry_policy();
        let mut last_error = String::new();
        let mut succeeded = false;

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

            let app = self.app.clone();
            let transfer_id = file_transfer_id.clone();
            let display_name = entry.display_name.clone();
            let remote_path = entry.remote_path.clone();
            let remote_path_for_progress = entry.remote_path.clone();
            let local_path = entry.local_path.clone();
            let local_path_for_mtime = entry.local_path.clone();
            let file_size = entry.size;
            let cancel_token = self.cancel_token.clone();
            let modified = entry.modified.clone();

            let result = match prepare_lease(&lease).await {
                Ok(()) => {
                    note_transfer_attempt(&self.attempt_counts, &entry.id, attempt + 1);
                    let manager = lease.manager();
                    async move {
                        let manager =
                            manager.ok_or("FTP session lease is no longer valid".to_string())?;
                        let mut ftp = manager.lock().await;
                        // Scale timeout based on file size: at least 2s per MB, minimum from settings
                        let size_based_timeout = (file_size / (1024 * 1024)).max(1) * 2 + 30;
                        let effective_timeout = self
                            .runtime_settings
                            .timeout_seconds
                            .max(size_based_timeout);
                        ftp.apply_transfer_timeout(effective_timeout);

                        let (parent_dir, remote_name) = split_remote_path(&remote_path);
                        ftp.change_dir(&parent_dir).await.map_err(|e| {
                            format!("Failed to change directory to {}: {}", parent_dir, e)
                        })?;

                        let speed_meter = crate::transfer_speed::SpeedMeter::new();
                        ftp.download_file_with_progress(&remote_name, &local_path, |transferred| {
                            if cancel_token.is_cancelled() {
                                return false;
                            }

                            let speed = speed_meter.bps(transferred);
                            let percentage = if file_size > 0 {
                                ((transferred as f64 / file_size as f64) * 100.0) as u8
                            } else {
                                0
                            };
                            let eta = if speed > 0 && file_size > transferred {
                                ((file_size - transferred) / speed) as u32
                            } else {
                                0
                            };

                            crate::transfer_event_sink::emit_gui_transfer_event(
                                &app,
                                crate::TransferEvent {
                                    event_type: "progress".to_string(),
                                    transfer_id: transfer_id.clone(),
                                    filename: display_name.clone(),
                                    direction: "download".to_string(),
                                    message: None,
                                    progress: Some(crate::TransferProgress {
                                        transfer_id: transfer_id.clone(),
                                        filename: display_name.clone(),
                                        transferred,
                                        total: file_size,
                                        percentage,
                                        speed_bps: speed,
                                        eta_seconds: eta,
                                        direction: "download".to_string(),
                                        total_files: None,
                                        path: None,
                                    }),
                                    path: Some(remote_path_for_progress.clone()),
                                    delta_stats: None,
                                    fallback_reason: None,
                                },
                            );

                            true
                        })
                        .await
                        .map_err(|e| e.to_string())?;

                        crate::preserve_remote_mtime(&local_path_for_mtime, modified.as_deref());
                        Ok::<(), String>(())
                    }
                    .await
                }
                Err(error) => Err(error),
            };

            match result {
                Ok(()) => {
                    succeeded = true;
                    break;
                }
                Err(error) => {
                    last_error = error;

                    if self.cancel_token.is_cancelled()
                        || crate::transfer_dag::error::message_names_a_cancellation(&last_error)
                    {
                        break;
                    }

                    let error_info =
                        crate::sync::classify_sync_error(&last_error, Some(&entry.remote_path));
                    if attempt >= retry_policy.max_retries || !error_info.retryable {
                        break;
                    }

                    warn!(
                        "Retrying FTP download {} (attempt {}/{}): {}",
                        entry.remote_path,
                        attempt + 1,
                        retry_policy.max_retries,
                        error_info.message
                    );
                }
            }
        }

        // One release: the lease outlives every attempt.
        if let Err(release_error) = lease.release().await {
            warn!(
                "Failed to reset FTP lease after {}: {}",
                entry.remote_path, release_error
            );
        }

        if succeeded {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &self.app,
                crate::TransferEvent {
                    event_type: "file_complete".to_string(),
                    transfer_id: file_transfer_id.clone(),
                    filename: entry.display_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Downloaded: {}", entry.display_name)),
                    progress: None,
                    path: Some(entry.remote_path.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return TransferOutcome::Success;
        }

        let failure = if self.cancel_token.is_cancelled()
            || crate::transfer_dag::error::message_names_a_cancellation(&last_error)
        {
            TransferFailure::new(
                TransferFailureKind::Cancelled,
                "Transfer cancelled by user",
                false,
            )
        } else {
            TransferFailure::from_raw_message(&last_error)
        };

        crate::transfer_event_sink::emit_gui_transfer_event(
            &self.app,
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: file_transfer_id,
                filename: entry.display_name.clone(),
                direction: "download".to_string(),
                message: Some(failure.message.clone()),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        TransferOutcome::Failed(failure)
    }
}

#[async_trait]
impl TransferExecutor for FtpDownloadExecutor {
    fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
        executor_session_pool("ftp-download-executor", &self.pool)
    }

    fn take_transfer_attempts(&self, entry_id: &str) -> Option<u32> {
        self.attempt_counts
            .lock()
            .expect("attempt counts poisoned")
            .remove(entry_id)
    }

    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        // Trait entry for a caller that does not already hold a lease.
        match self.pool.acquire().await {
            Ok(lease) => self.execute_with_lease(entry, lease).await,
            Err(error) => fail_without_session(
                &self.app,
                &entry,
                "download",
                &error,
                self.cancel_token.is_cancelled(),
            ),
        }
    }

    async fn execute_with_session(
        &self,
        entry: TransferEntry,
        session_lease: TransferSessionLease,
    ) -> TransferOutcome {
        let lease = match session_lease.into_ftp() {
            Ok(ftp_lease) => match ftp_lease.into_inner() {
                Some(lease) => lease,
                None => {
                    warn!("FTP download executor got an already-released lease");
                    return fail_without_session(
                        &self.app,
                        &entry,
                        "download",
                        "FTP session lease was already released",
                        self.cancel_token.is_cancelled(),
                    );
                }
            },
            Err(other) => {
                warn!(
                    "FTP download executor received a {:?} session lease",
                    other.kind()
                );
                drop(other);
                return fail_without_session(
                    &self.app,
                    &entry,
                    "download",
                    "FTP executor received a non-FTP session lease",
                    self.cancel_token.is_cancelled(),
                );
            }
        };
        self.execute_with_lease(entry, lease).await
    }
}

pub struct FtpUploadExecutor {
    app: AppHandle,
    pool: Arc<FtpSessionPool>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
    attempt_counts: AttemptCounts,
}

impl FtpUploadExecutor {
    pub fn new(
        app: AppHandle,
        pool: Arc<FtpSessionPool>,
        runtime_settings: ResolvedTransferSettings,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            app,
            pool,
            runtime_settings,
            cancel_token,
            attempt_counts: AttemptCounts::default(),
        }
    }

    /// Same held-lease retry loop as download.
    async fn execute_with_lease(
        &self,
        entry: TransferEntry,
        lease: FtpSessionLease,
    ) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();
        let file_size = if entry.size > 0 {
            entry.size
        } else {
            tokio::fs::metadata(&entry.local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        };

        crate::transfer_event_sink::emit_gui_transfer_event(
            &self.app,
            crate::TransferEvent {
                event_type: "file_start".to_string(),
                transfer_id: file_transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!("Uploading: {}", entry.remote_path)),
                progress: Some(crate::TransferProgress {
                    transfer_id: file_transfer_id.clone(),
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
            },
        );

        let retry_policy = self.runtime_settings.retry_policy();
        let mut last_error = String::new();
        let mut succeeded = false;

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

            let app = self.app.clone();
            let transfer_id = file_transfer_id.clone();
            let display_name = entry.display_name.clone();
            let remote_path = entry.remote_path.clone();
            let remote_path_for_progress = entry.remote_path.clone();
            let local_path = entry.local_path.clone();
            let cancel_token = self.cancel_token.clone();

            let result = match prepare_lease(&lease).await {
                Ok(()) => {
                    note_transfer_attempt(&self.attempt_counts, &entry.id, attempt + 1);
                    let manager = lease.manager();
                    async move {
                        let manager =
                            manager.ok_or("FTP session lease is no longer valid".to_string())?;
                        let mut ftp = manager.lock().await;
                        // Scale timeout based on file size: at least 1s per MB, minimum from settings
                        let size_based_timeout = (file_size / (1024 * 1024)).max(1) * 2 + 30;
                        let effective_timeout = self
                            .runtime_settings
                            .timeout_seconds
                            .max(size_based_timeout);
                        ftp.apply_transfer_timeout(effective_timeout);
                        let (parent_dir, remote_name) = split_remote_path(&remote_path);
                        ftp.change_dir(&parent_dir).await.map_err(|e| {
                            format!("Failed to change directory to {}: {}", parent_dir, e)
                        })?;

                        let speed_meter = crate::transfer_speed::SpeedMeter::new();
                        ftp.upload_file_with_progress(
                            &local_path,
                            &remote_name,
                            file_size,
                            |transferred| {
                                if cancel_token.is_cancelled() {
                                    return false;
                                }

                                let speed = speed_meter.bps(transferred);
                                let percentage = if file_size > 0 {
                                    ((transferred as f64 / file_size as f64) * 100.0) as u8
                                } else {
                                    0
                                };
                                let eta = if speed > 0 && file_size > transferred {
                                    ((file_size - transferred) / speed) as u32
                                } else {
                                    0
                                };

                                crate::transfer_event_sink::emit_gui_transfer_event(
                                    &app,
                                    crate::TransferEvent {
                                        event_type: "progress".to_string(),
                                        transfer_id: transfer_id.clone(),
                                        filename: display_name.clone(),
                                        direction: "upload".to_string(),
                                        message: None,
                                        progress: Some(crate::TransferProgress {
                                            transfer_id: transfer_id.clone(),
                                            filename: display_name.clone(),
                                            transferred,
                                            total: file_size,
                                            percentage,
                                            speed_bps: speed,
                                            eta_seconds: eta,
                                            direction: "upload".to_string(),
                                            total_files: None,
                                            path: None,
                                        }),
                                        path: Some(remote_path_for_progress.clone()),
                                        delta_stats: None,
                                        fallback_reason: None,
                                    },
                                );

                                true
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                        Ok::<(), String>(())
                    }
                    .await
                }
                Err(error) => Err(error),
            };

            match result {
                Ok(()) => {
                    succeeded = true;
                    break;
                }
                Err(error) => {
                    last_error = error;

                    if self.cancel_token.is_cancelled()
                        || crate::transfer_dag::error::message_names_a_cancellation(&last_error)
                    {
                        break;
                    }

                    let error_info =
                        crate::sync::classify_sync_error(&last_error, Some(&entry.local_path));
                    if attempt >= retry_policy.max_retries || !error_info.retryable {
                        break;
                    }

                    warn!(
                        "Retrying FTP upload {} (attempt {}/{}): {}",
                        entry.local_path,
                        attempt + 1,
                        retry_policy.max_retries,
                        error_info.message
                    );
                }
            }
        }

        if let Err(release_error) = lease.release().await {
            warn!(
                "Failed to reset FTP lease after {}: {}",
                entry.remote_path, release_error
            );
        }

        if succeeded {
            crate::transfer_event_sink::emit_gui_transfer_event(
                &self.app,
                crate::TransferEvent {
                    event_type: "file_complete".to_string(),
                    transfer_id: file_transfer_id.clone(),
                    filename: entry.display_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Uploaded: {}", entry.display_name)),
                    progress: None,
                    path: Some(entry.remote_path.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return TransferOutcome::Success;
        }

        let failure = if self.cancel_token.is_cancelled()
            || crate::transfer_dag::error::message_names_a_cancellation(&last_error)
        {
            TransferFailure::new(
                TransferFailureKind::Cancelled,
                "Transfer cancelled by user",
                false,
            )
        } else {
            TransferFailure::from_raw_message(&last_error)
        };

        crate::transfer_event_sink::emit_gui_transfer_event(
            &self.app,
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: file_transfer_id,
                filename: entry.display_name.clone(),
                direction: "upload".to_string(),
                message: Some(failure.message.clone()),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        TransferOutcome::Failed(failure)
    }
}

#[async_trait]
impl TransferExecutor for FtpUploadExecutor {
    fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
        executor_session_pool("ftp-upload-executor", &self.pool)
    }

    fn take_transfer_attempts(&self, entry_id: &str) -> Option<u32> {
        self.attempt_counts
            .lock()
            .expect("attempt counts poisoned")
            .remove(entry_id)
    }

    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        // Trait entry for a caller that does not already hold a lease.
        match self.pool.acquire().await {
            Ok(lease) => self.execute_with_lease(entry, lease).await,
            Err(error) => fail_without_session(
                &self.app,
                &entry,
                "upload",
                &error,
                self.cancel_token.is_cancelled(),
            ),
        }
    }

    async fn execute_with_session(
        &self,
        entry: TransferEntry,
        session_lease: TransferSessionLease,
    ) -> TransferOutcome {
        let lease = match session_lease.into_ftp() {
            Ok(ftp_lease) => match ftp_lease.into_inner() {
                Some(lease) => lease,
                None => {
                    warn!("FTP upload executor got an already-released lease");
                    return fail_without_session(
                        &self.app,
                        &entry,
                        "upload",
                        "FTP session lease was already released",
                        self.cancel_token.is_cancelled(),
                    );
                }
            },
            Err(other) => {
                warn!(
                    "FTP upload executor received a {:?} session lease",
                    other.kind()
                );
                drop(other);
                return fail_without_session(
                    &self.app,
                    &entry,
                    "upload",
                    "FTP executor received a non-FTP session lease",
                    self.cancel_token.is_cancelled(),
                );
            }
        };
        self.execute_with_lease(entry, lease).await
    }
}

fn split_remote_path(remote_path: &str) -> (String, String) {
    let path = Path::new(remote_path);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| remote_path.trim_matches('/').to_string());

    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .filter(|parent| !parent.is_empty())
        .unwrap_or_else(|| "/".to_string());

    let normalized_parent = if parent.is_empty() {
        "/".to_string()
    } else {
        parent
    };
    (normalized_parent, file_name)
}

#[cfg(test)]
mod tests {
    use super::{executor_session_pool, note_transfer_attempt, split_remote_path, AttemptCounts};
    use std::sync::Arc;

    #[test]
    fn split_remote_path_extracts_parent_and_file() {
        let (parent, file) = split_remote_path("/alpha/beta/file.txt");
        assert_eq!(parent, "/alpha/beta");
        assert_eq!(file, "file.txt");
    }

    #[test]
    fn split_remote_path_defaults_root_for_single_segment() {
        let (parent, file) = split_remote_path("file.txt");
        assert_eq!(parent, "/");
        assert_eq!(file, "file.txt");
    }

    /// An empty pool must time out. A no-wait token would return immediately.
    #[tokio::test]
    async fn executor_session_pool_handle_gates_on_the_real_pool() {
        for label in ["ftp-download-executor", "ftp-upload-executor"] {
            let pool = Arc::new(crate::ftp_session_pool::FtpSessionPool::empty_for_test(50));
            let handle = executor_session_pool(label, &pool);

            assert_eq!(
                handle.capacity().max_leases,
                pool.total_sessions().max(1),
                "{label}: capacity mirrors the pool's session count"
            );

            let started = std::time::Instant::now();
            let error = match handle.acquire().await {
                Err(error) => error,
                Ok(_) => panic!("{label}: an empty pool cannot hand out a lease"),
            };
            assert!(
                matches!(error, crate::transfer_dag::SessionPoolError::Timeout { .. }),
                "{label}: exhausted pool must time out: {error}"
            );
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(25),
                "{label}: acquire must wait on the pool, not return a no-wait token"
            );
        }
    }

    /// Records the latest attempt. A readiness failure never reaches this point.
    #[test]
    fn note_transfer_attempt_records_the_latest_attempt() {
        let counts = AttemptCounts::default();
        note_transfer_attempt(&counts, "entry-1", 1);
        note_transfer_attempt(&counts, "entry-1", 2);
        assert_eq!(
            counts
                .lock()
                .expect("attempt counts poisoned")
                .remove("entry-1"),
            Some(2)
        );
        assert!(
            counts.lock().expect("attempt counts poisoned").is_empty(),
            "the entry left nothing else behind"
        );
    }
}
