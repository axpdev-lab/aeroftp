// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Provider-backed transfer executor for the shared orchestrator.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::providers::{ProviderType, StorageProvider};
use crate::transfer_domain::{
    transfer_failure_kind_from_sync, user_facing_transfer_failure_message, TransferEntry,
    TransferFailure, TransferOutcome,
};
use crate::transfer_orchestrator::TransferExecutor;
use crate::transfer_settings::ResolvedTransferSettings;

pub struct ProviderDownloadExecutor {
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
}

impl ProviderDownloadExecutor {
    pub fn new(
        app: AppHandle,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            app,
            provider,
            runtime_settings,
            cancel_token,
        }
    }
}

pub struct ProviderUploadExecutor {
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    commit_message: Option<String>,
    cancel_token: CancellationToken,
}

impl ProviderUploadExecutor {
    pub fn new(
        app: AppHandle,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        commit_message: Option<String>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            app,
            provider,
            runtime_settings,
            commit_message,
            cancel_token,
        }
    }
}

#[async_trait]
impl TransferExecutor for ProviderDownloadExecutor {
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
            let file_size = entry.size;
            let cancel_token = self.cancel_token.clone();
            let result = {
                let mut provider_lock = self.provider.lock().await;
                if self.cancel_token.is_cancelled() {
                    last_error = "Transfer cancelled by user".to_string();
                    break;
                }

                match provider_lock.as_mut() {
                    Some(provider) => {
                        let dl_start = std::time::Instant::now();
                        // Resume-aware: on retries, if provider supports resume
                        // and a partial .aerotmp exists, resume from where we left off
                        let tmp_path = format!("{}.aerotmp", &local_path);
                        let partial_offset = if attempt > 0 && provider.supports_resume() {
                            tokio::fs::metadata(&tmp_path)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0)
                        } else {
                            0
                        };
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
                                    },
                                );
                            }));

                        let dl_future = if partial_offset > 0 {
                            tracing::info!(
                                "Resuming download from {} bytes (attempt {}): {}",
                                partial_offset,
                                attempt,
                                remote_path
                            );
                            provider.resume_download(
                                &remote_path,
                                &local_path,
                                partial_offset,
                                progress_cb,
                            )
                        } else {
                            provider.download(&remote_path, &local_path, progress_cb)
                        };

                        // Dynamic timeout: base timeout + file_size / 50 KB/s (pessimistic slow connection)
                        // Ensures large files on slow connections (e.g. 70 MB at 180 KB/s) don't time out
                        let size_based_secs = file_size / 50_000; // 50 KB/s minimum assumed speed
                        let effective_timeout = self
                            .runtime_settings
                            .timeout_seconds
                            .max(size_based_secs + self.runtime_settings.timeout_seconds);
                        match tokio::time::timeout(
                            Duration::from_secs(effective_timeout),
                            dl_future,
                        )
                        .await
                        {
                            Ok(result) => result.map_err(|e| e.to_string()),
                            Err(_) => Err(format!(
                                "Download timed out after {} seconds",
                                effective_timeout
                            )),
                        }
                    }
                    None => Err("Provider disconnected".to_string()),
                }
            };

            match result {
                Ok(()) => {
                    crate::preserve_remote_mtime(&entry.local_path, entry.modified.as_deref());
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
impl TransferExecutor for ProviderUploadExecutor {
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
            let commit_message = self.commit_message.clone();
            let cancel_token = self.cancel_token.clone();
            let result = {
                let mut provider_lock = self.provider.lock().await;
                if self.cancel_token.is_cancelled() {
                    last_error = "Transfer cancelled by user".to_string();
                    break;
                }

                match provider_lock.as_mut() {
                    Some(provider) => {
                        if provider.provider_type() == ProviderType::GitHub {
                            let github = provider
                                .as_any_mut()
                                .downcast_mut::<crate::providers::github::GitHubProvider>()
                                .ok_or_else(|| "Failed to access GitHub provider".to_string());
                            match github {
                                Ok(github) => {
                                    let size_secs = file_size / 50_000;
                                    let eff_timeout = self
                                        .runtime_settings
                                        .timeout_seconds
                                        .max(size_secs + self.runtime_settings.timeout_seconds);
                                    match tokio::time::timeout(
                                        Duration::from_secs(eff_timeout),
                                        github.upload_file(
                                            &local_path,
                                            &remote_path,
                                            commit_message.as_deref(),
                                        ),
                                    )
                                    .await
                                    {
                                        Ok(result) => result.map_err(|e| e.to_string()),
                                        Err(_) => Err(format!(
                                            "Upload timed out after {} seconds",
                                            eff_timeout
                                        )),
                                    }
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            let ul_start = std::time::Instant::now();
                            let size_secs = file_size / 50_000;
                            let eff_timeout = self
                                .runtime_settings
                                .timeout_seconds
                                .max(size_secs + self.runtime_settings.timeout_seconds);
                            match tokio::time::timeout(
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
                                        let remaining =
                                            total.max(file_size).saturating_sub(transferred);
                                        let eta = if speed > 0 {
                                            (remaining as f64 / speed as f64) as u64
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
                                            },
                                        );
                                    })),
                                ),
                            )
                            .await
                            {
                                Ok(result) => result.map_err(|e| e.to_string()),
                                Err(_) => {
                                    Err(format!("Upload timed out after {} seconds", eff_timeout))
                                }
                            }
                        }
                    }
                    None => Err("Provider disconnected".to_string()),
                }
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
