// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! FTP-backed transfer executor for the shared orchestrator.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::ftp_session_pool::FtpSessionPool;
use crate::transfer_domain::{
    transfer_failure_kind_from_sync, user_facing_transfer_failure_message, TransferEntry,
    TransferFailure, TransferFailureKind, TransferOutcome,
};
use crate::transfer_orchestrator::TransferExecutor;
use crate::transfer_settings::ResolvedTransferSettings;

pub struct FtpDownloadExecutor {
    app: AppHandle,
    pool: Arc<FtpSessionPool>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
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
        }
    }
}

pub struct FtpUploadExecutor {
    app: AppHandle,
    pool: Arc<FtpSessionPool>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
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
        }
    }
}

#[async_trait]
impl TransferExecutor for FtpDownloadExecutor {
    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();

        let _ = self.app.emit(
            "transfer_event",
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
            },
        );

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

            let result = match self.pool.acquire().await {
                Ok(lease) => {
                    let transfer_result = {
                        let manager = lease.manager();
                        async move {
                            let manager = manager
                                .ok_or("FTP session lease is no longer valid".to_string())?;
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

                            let started_at = Instant::now();
                            ftp.download_file_with_progress(
                                &remote_name,
                                &local_path,
                                |transferred| {
                                    if cancel_token.is_cancelled() {
                                        return false;
                                    }

                                    let elapsed = started_at.elapsed().as_secs_f64();
                                    let speed = if elapsed > 0.0 {
                                        (transferred as f64 / elapsed) as u64
                                    } else {
                                        0
                                    };
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

                                    let _ = app.emit(
                                        "transfer_event",
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
                                        },
                                    );

                                    true
                                },
                            )
                            .await
                            .map_err(|e| e.to_string())?;

                            crate::preserve_remote_mtime(
                                &local_path_for_mtime,
                                modified.as_deref(),
                            );
                            Ok::<(), String>(())
                        }
                    }
                    .await;

                    if let Err(release_error) = lease.release().await {
                        warn!(
                            "Failed to reset FTP lease after {}: {}",
                            entry.remote_path, release_error
                        );
                    }

                    transfer_result
                }
                Err(error) => Err(error),
            };

            match result {
                Ok(()) => {
                    let _ = self.app.emit(
                        "transfer_event",
                        crate::TransferEvent {
                            event_type: "file_complete".to_string(),
                            transfer_id: file_transfer_id.clone(),
                            filename: entry.display_name.clone(),
                            direction: "download".to_string(),
                            message: Some(format!("Downloaded: {}", entry.display_name)),
                            progress: None,
                            path: Some(entry.remote_path.clone()),
                            delta_stats: None,
                        },
                    );
                    return TransferOutcome::Success;
                }
                Err(error) => {
                    last_error = error;

                    if self.cancel_token.is_cancelled() || last_error.contains("cancelled by user")
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

        let failure = if self.cancel_token.is_cancelled() || last_error.contains("cancelled") {
            TransferFailure {
                kind: TransferFailureKind::Cancelled,
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

        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: file_transfer_id,
                filename: entry.display_name.clone(),
                direction: "download".to_string(),
                message: Some(failure.message.clone()),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
            },
        );

        TransferOutcome::Failed(failure)
    }
}

#[async_trait]
impl TransferExecutor for FtpUploadExecutor {
    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        let file_transfer_id = entry.id.clone();
        let file_size = if entry.size > 0 {
            entry.size
        } else {
            tokio::fs::metadata(&entry.local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        };

        let _ = self.app.emit(
            "transfer_event",
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
            },
        );

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

            let app = self.app.clone();
            let transfer_id = file_transfer_id.clone();
            let display_name = entry.display_name.clone();
            let remote_path = entry.remote_path.clone();
            let remote_path_for_progress = entry.remote_path.clone();
            let local_path = entry.local_path.clone();
            let cancel_token = self.cancel_token.clone();

            let result = match self.pool.acquire().await {
                Ok(lease) => {
                    let transfer_result = {
                        let manager = lease.manager();
                        async move {
                            let manager = manager
                                .ok_or("FTP session lease is no longer valid".to_string())?;
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

                            let started_at = Instant::now();
                            ftp.upload_file_with_progress(
                                &local_path,
                                &remote_name,
                                file_size,
                                |transferred| {
                                    if cancel_token.is_cancelled() {
                                        return false;
                                    }

                                    let elapsed = started_at.elapsed().as_secs_f64();
                                    let speed = if elapsed > 0.0 {
                                        (transferred as f64 / elapsed) as u64
                                    } else {
                                        0
                                    };
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

                                    let _ = app.emit(
                                        "transfer_event",
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
                                        },
                                    );

                                    true
                                },
                            )
                            .await
                            .map_err(|e| e.to_string())?;

                            Ok::<(), String>(())
                        }
                    }
                    .await;

                    if let Err(release_error) = lease.release().await {
                        warn!(
                            "Failed to reset FTP lease after {}: {}",
                            entry.remote_path, release_error
                        );
                    }

                    transfer_result
                }
                Err(error) => Err(error),
            };

            match result {
                Ok(()) => {
                    let _ = self.app.emit(
                        "transfer_event",
                        crate::TransferEvent {
                            event_type: "file_complete".to_string(),
                            transfer_id: file_transfer_id.clone(),
                            filename: entry.display_name.clone(),
                            direction: "upload".to_string(),
                            message: Some(format!("Uploaded: {}", entry.display_name)),
                            progress: None,
                            path: Some(entry.remote_path.clone()),
                            delta_stats: None,
                        },
                    );
                    return TransferOutcome::Success;
                }
                Err(error) => {
                    last_error = error;

                    if self.cancel_token.is_cancelled() || last_error.contains("cancelled by user")
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

        let failure = if self.cancel_token.is_cancelled() || last_error.contains("cancelled") {
            TransferFailure {
                kind: TransferFailureKind::Cancelled,
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

        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: file_transfer_id,
                filename: entry.display_name.clone(),
                direction: "upload".to_string(),
                message: Some(failure.message.clone()),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
            },
        );

        TransferOutcome::Failed(failure)
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
    use super::split_remote_path;

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
}
