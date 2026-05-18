// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Provider-backed transfer executor for the shared orchestrator.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::providers::{
    ProviderListExecutorKind, ProviderTransferExecutorKind, ProviderType, StorageProvider,
};
use crate::transfer_dag::{Capability, TransferSessionLease, TransferSessionPoolHandle};
use crate::transfer_domain::{
    transfer_failure_kind_from_sync, user_facing_transfer_failure_message, TransferEntry,
    TransferFailure, TransferOutcome,
};
use crate::transfer_orchestrator::TransferExecutor;
use crate::transfer_settings::ResolvedTransferSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderExecutorSessionModel {
    LockedSingle {
        provider_type: Option<ProviderType>,
    },
    HttpClonePool {
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
        }
    }

    fn is_clone_pool(&self) -> bool {
        matches!(self, Self::HttpClonePool { .. })
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

pub async fn resolve_provider_executor_session_model(
    provider: &Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    max_concurrent: usize,
) -> ProviderExecutorSessionModel {
    let provider_lock = provider.lock().await;
    let Some(provider) = provider_lock.as_ref() else {
        return ProviderExecutorSessionModel::locked(None);
    };

    let provider_type = provider.provider_type();
    let caps = provider.transfer_capabilities();
    let executor_can_clone =
        provider.transfer_executor_kind() == ProviderTransferExecutorKind::HttpClonePool;
    let scheduler_can_parallelize = caps.file_parallel == Capability::Supported
        && caps.session_pool == Capability::Supported
        && provider.clone_for_transfer().is_ok();

    if executor_can_clone && scheduler_can_parallelize {
        let advertised = caps
            .max_file_slots
            .unwrap_or_else(|| provider.transfer_executor_max_sessions())
            .max(1) as usize;
        ProviderExecutorSessionModel::HttpClonePool {
            provider_type,
            max_leases: advertised.min(max_concurrent.max(1)).max(1),
        }
    } else {
        ProviderExecutorSessionModel::locked(Some(provider_type))
    }
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
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    cancel_token: CancellationToken,
    session_model: ProviderExecutorSessionModel,
}

impl ProviderDownloadExecutor {
    pub fn new(
        app: AppHandle,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        cancel_token: CancellationToken,
        session_model: ProviderExecutorSessionModel,
    ) -> Self {
        Self {
            app,
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
        let app = self.app.clone();
        let transfer_id = file_transfer_id.to_string();
        let display_name = entry.display_name.clone();
        let remote_path = entry.remote_path.clone();
        let remote_path_for_progress = entry.remote_path.clone();
        let local_path = entry.local_path.clone();
        let file_size = entry.size;
        let cancel_token = self.cancel_token.clone();
        let dl_start = std::time::Instant::now();

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
                        fallback_reason: None,
                    },
                );
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

        match tokio::time::timeout(Duration::from_secs(effective_timeout), dl_future).await {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err(format!(
                "Download timed out after {} seconds",
                effective_timeout
            )),
        }
    }

    fn emit_download_start(&self, entry: &TransferEntry, file_transfer_id: &str) {
        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
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
            },
        );
    }

    fn emit_download_complete(&self, entry: &TransferEntry, file_transfer_id: &str) {
        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_complete".to_string(),
                transfer_id: file_transfer_id.to_string(),
                filename: entry.display_name.clone(),
                direction: "download".to_string(),
                message: Some(format!("Downloaded: {}", entry.display_name)),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
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

        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: entry.id,
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

pub struct ProviderUploadExecutor {
    app: AppHandle,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    runtime_settings: ResolvedTransferSettings,
    commit_message: Option<String>,
    cancel_token: CancellationToken,
    session_model: ProviderExecutorSessionModel,
}

impl ProviderUploadExecutor {
    pub fn new(
        app: AppHandle,
        provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
        runtime_settings: ResolvedTransferSettings,
        commit_message: Option<String>,
        cancel_token: CancellationToken,
        session_model: ProviderExecutorSessionModel,
    ) -> Self {
        Self {
            app,
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
        let commit_message = self.commit_message.clone();
        let size_secs = file_size / 50_000;
        let eff_timeout = self
            .runtime_settings
            .timeout_seconds
            .max(size_secs + self.runtime_settings.timeout_seconds);

        if provider.provider_type() == ProviderType::GitHub {
            let github = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            return match tokio::time::timeout(
                Duration::from_secs(eff_timeout),
                github.upload_file(&local_path, &remote_path, commit_message.as_deref()),
            )
            .await
            {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err(format!("Upload timed out after {} seconds", eff_timeout)),
            };
        }

        let app = self.app.clone();
        let transfer_id = file_transfer_id.to_string();
        let display_name = entry.display_name.clone();
        let remote_path_for_progress = entry.remote_path.clone();
        let cancel_token = self.cancel_token.clone();
        let ul_start = std::time::Instant::now();

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
                        },
                    );
                })),
            ),
        )
        .await
        {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err(format!("Upload timed out after {} seconds", eff_timeout)),
        }
    }

    fn emit_upload_start(&self, entry: &TransferEntry, file_transfer_id: &str, file_size: u64) {
        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
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
            },
        );
    }

    fn emit_upload_complete(&self, entry: &TransferEntry, file_transfer_id: &str) {
        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_complete".to_string(),
                transfer_id: file_transfer_id.to_string(),
                filename: entry.display_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!("Uploaded: {}", entry.display_name)),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
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

        let _ = self.app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: entry.id,
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
impl TransferExecutor for ProviderDownloadExecutor {
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
}
