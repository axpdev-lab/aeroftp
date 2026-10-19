// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-profile transfer Tauri commands.
//!
//! Exposes plan/execute/cancel for cross-profile transfers via the Tauri IPC
//! bridge, reusing the core engine from `cross_profile_transfer.rs`.

use crate::ai_tools::{create_temp_provider, load_saved_servers, SavedServerInfo};
use crate::cross_profile_transfer::{
    copy_one_file_with_options, failures_with_tally_gap, plan_transfer, should_skip_existing,
    CrossProfileCopyOptions, CrossProfileFileFailure, CrossProfileTransferEntry,
    CrossProfileTransferPlan, CrossProfileTransferRequest,
};
use crate::transfer_domain::{
    TransferBatchConfig, TransferDirection, TransferEntry, TransferFailure, TransferFailureKind,
    TransferOutcome,
};
use crate::transfer_event_sink::NoopTransferSink;
use crate::transfer_orchestrator::{execute_batch, TransferBatch, TransferExecutor};
use crate::util::{AbortOnDrop, ProviderGuard};
use crate::{TransferEvent, TransferProgress};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const PLAN_TTL_MS: u64 = 15 * 60 * 1000;
const PLAN_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const CROSS_PROFILE_MAX_CONCURRENT: u32 = 4;
const CROSS_PROFILE_MAX_RETRY: u32 = 3;
const CROSS_PROFILE_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
struct StoredCrossProfilePlan {
    source_server: SavedServerInfo,
    dest_server: SavedServerInfo,
    request: CrossProfileTransferRequest,
    plan: CrossProfileTransferPlan,
    source_mtime_precision: Option<Duration>,
    created_at_ms: u64,
}

// ── State ──────────────────────────────────────────────────────────────────

/// Managed state for cross-profile transfers, holding approved plans and per-transfer cancel flags.
pub struct CrossProfileState {
    approved_plans: Arc<Mutex<HashMap<String, StoredCrossProfilePlan>>>,
    cancel_tokens: Mutex<HashMap<String, CancellationToken>>,
    // Background sweeper task. Spawned lazily by `ensure_sweeper_started`
    // from inside an async Tauri command: `new()` runs during
    // `builder.manage(...)` which is before the Tokio runtime is alive,
    // and even the sync `setup` hook isn't inside a runtime context.
    sweeper: std::sync::Mutex<Option<AbortOnDrop<()>>>,
}

impl CrossProfileState {
    pub fn new() -> Self {
        Self {
            approved_plans: Arc::new(Mutex::new(HashMap::new())),
            cancel_tokens: Mutex::new(HashMap::new()),
            sweeper: std::sync::Mutex::new(None),
        }
    }

    /// Idempotent: arm the periodic TTL sweeper on the first write. Safe to
    /// call from any async context; a second call is a no-op.
    fn ensure_sweeper_started(&self) {
        let mut guard = self.sweeper.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let plans = Arc::clone(&self.approved_plans);
        *guard = Some(AbortOnDrop::spawn(async move {
            let mut interval = tokio::time::interval(PLAN_SWEEP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let cutoff = now_ms().saturating_sub(PLAN_TTL_MS);
                let mut locked = plans.lock().await;
                locked.retain(|_, item| item.created_at_ms >= cutoff);
            }
        }));
    }
}

// ── Request / Response types ───────────────────────────────────────────────

/// Request from the frontend to plan a cross-profile transfer.
#[derive(Debug, Clone, Deserialize)]
pub struct CrossProfilePlanRequest {
    pub source_profile_id: String,
    pub dest_profile_id: String,
    pub source_path: String,
    pub dest_path: String,
    pub recursive: bool,
    pub skip_existing: bool,
}

/// Response returned after planning, with a backend-issued plan ID required for execution.
#[derive(Debug, Clone, Serialize)]
pub struct CrossProfilePlanResponse {
    pub plan_id: String,
    pub source_profile_id: String,
    pub dest_profile_id: String,
    pub source_profile: String,
    pub dest_profile: String,
    pub entries: Vec<CrossProfileTransferEntry>,
    pub directories: Vec<CrossProfileTransferEntry>,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// Request to execute a previously planned transfer.
#[derive(Debug, Clone, Deserialize)]
pub struct CrossProfileExecuteRequest {
    pub plan_id: String,
}

/// Summary returned to the frontend after execution.
#[derive(Debug, Clone, Serialize)]
pub struct CrossProfileTransferSummary {
    pub transfer_id: String,
    pub planned_files: u64,
    pub transferred_files: u64,
    pub skipped_files: u64,
    pub failed_files: u64,
    /// Planned files that did not land in transferred, skipped, or failed
    /// before reconciliation, plus per-file errors when the runner has them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<crate::cross_profile_transfer::CrossProfileFileFailure>,
    pub total_bytes: u64,
    pub duration_ms: u64,
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn emit_transfer_event(app: &AppHandle, event: TransferEvent) {
    crate::transfer_event_sink::emit_gui_transfer_event(app, event);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

fn validate_remote_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("{} must not be empty", label));
    }
    if path.len() > 4096 {
        return Err(format!("{} exceeds 4096 characters", label));
    }
    if path.contains('\0') {
        return Err(format!("{} contains null bytes", label));
    }
    if path.starts_with('-') {
        return Err(format!(
            "{} must not start with '-' (argument injection risk)",
            label
        ));
    }

    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err(format!("{} must not contain '..' path traversal", label));
        }
    }

    Ok(())
}

fn resolve_server_by_id(
    servers: &[SavedServerInfo],
    profile_id: &str,
) -> Result<SavedServerInfo, String> {
    servers
        .iter()
        .find(|server| server.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("Saved profile '{}' not found", profile_id))
}

async fn store_plan(state: &CrossProfileState, plan_id: String, stored: StoredCrossProfilePlan) {
    state.ensure_sweeper_started();
    let mut plans = state.approved_plans.lock().await;
    let cutoff = now_ms().saturating_sub(PLAN_TTL_MS);
    plans.retain(|_, item| item.created_at_ms >= cutoff);
    plans.insert(plan_id, stored);
}

async fn take_plan(
    state: &CrossProfileState,
    plan_id: &str,
) -> Result<StoredCrossProfilePlan, String> {
    let mut plans = state.approved_plans.lock().await;
    let cutoff = now_ms().saturating_sub(PLAN_TTL_MS);
    plans.retain(|_, item| item.created_at_ms >= cutoff);
    plans.remove(plan_id).ok_or_else(|| {
        "Transfer plan not found or expired. Rebuild the plan and approve it again.".to_string()
    })
}

async fn register_cancel_token(state: &CrossProfileState, transfer_id: &str) -> CancellationToken {
    let cancel_token = CancellationToken::new();
    let mut tokens = state.cancel_tokens.lock().await;
    tokens.insert(transfer_id.to_string(), cancel_token.clone());
    cancel_token
}

async fn clear_cancel_token(state: &CrossProfileState, transfer_id: &str) {
    let mut tokens = state.cancel_tokens.lock().await;
    tokens.remove(transfer_id);
}

struct CrossProfileBatchCounters {
    transferred: AtomicU64,
    skipped: AtomicU64,
    failed: AtomicU64,
    terminal: AtomicU64,
    bytes_transferred: AtomicU64,
    cancel_emitted: AtomicBool,
    /// File errors already counted in `failed`. A std mutex: `emit_file_error`
    /// is synchronous and does not hold the guard across an await.
    failures: std::sync::Mutex<Vec<CrossProfileFileFailure>>,
}

impl CrossProfileBatchCounters {
    fn new() -> Self {
        Self {
            transferred: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            terminal: AtomicU64::new(0),
            bytes_transferred: AtomicU64::new(0),
            cancel_emitted: AtomicBool::new(false),
            failures: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn record_failure(&self, failure: CrossProfileFileFailure) {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(failure);
    }

    fn take_failures(&self) -> Vec<CrossProfileFileFailure> {
        std::mem::take(
            &mut *self
                .failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

struct CrossProfileExecutor {
    app: AppHandle,
    transfer_id: String,
    source_server: SavedServerInfo,
    dest_server: SavedServerInfo,
    skip_existing: bool,
    source_mtime_precision: Option<Duration>,
    cancel_token: CancellationToken,
    total: u64,
    speed_meter: crate::transfer_speed::SpeedMeter,
    counters: Arc<CrossProfileBatchCounters>,
}

#[async_trait]
impl TransferExecutor for CrossProfileExecutor {
    async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
        self.execute_entry(entry).await
    }
}

impl CrossProfileExecutor {
    async fn execute_entry(&self, entry: TransferEntry) -> TransferOutcome {
        if self.cancel_token.is_cancelled() {
            self.emit_cancelled_once();
            return transfer_cancelled();
        }

        if self.skip_existing {
            match create_temp_provider(&self.dest_server).await {
                Ok(provider) => {
                    let mut dest = ProviderGuard::new(provider, "destination");
                    match should_skip_existing(
                        self.source_mtime_precision,
                        dest.provider_mut(),
                        &entry.local_path,
                        &entry_to_cross_profile(&entry),
                    )
                    .await
                    {
                        Ok(true) => {
                            let _ = dest.disconnect().await;
                            self.emit_file_skip(&entry);
                            return TransferOutcome::Skipped {
                                reason: "Skipped existing destination file".to_string(),
                            };
                        }
                        Ok(false) => {
                            let _ = dest.disconnect().await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                "cross-profile: skip-existing check failed for {}: {}",
                                entry.local_path,
                                err
                            );
                            let _ = dest.disconnect().await;
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "cross-profile: destination provider for skip-existing failed: {}",
                        err
                    );
                }
            }
        }

        self.emit_file_start(&entry);

        for attempt in 1..=CROSS_PROFILE_MAX_RETRY {
            if self.cancel_token.is_cancelled() {
                self.emit_cancelled_once();
                return transfer_cancelled();
            }

            let result = self.copy_entry_once(&entry).await;
            match result {
                Ok(()) => {
                    self.emit_file_complete(&entry);
                    return TransferOutcome::Success;
                }
                Err(err) if attempt == CROSS_PROFILE_MAX_RETRY => {
                    self.emit_file_error(&entry, &err);
                    return transfer_failed(err.to_string());
                }
                Err(err) => {
                    tracing::warn!(
                        "cross-profile: attempt {}/{} failed for {}: {}",
                        attempt,
                        CROSS_PROFILE_MAX_RETRY,
                        entry.remote_path,
                        err
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(CROSS_PROFILE_RETRY_DELAY) => {}
                        _ = self.cancel_token.cancelled() => {
                            self.emit_cancelled_once();
                            return transfer_cancelled();
                        }
                    }
                }
            }
        }

        transfer_failed("cross-profile retry loop exhausted".to_string())
    }

    async fn copy_entry_once(
        &self,
        entry: &TransferEntry,
    ) -> Result<(), crate::providers::ProviderError> {
        let mut source = ProviderGuard::new(
            create_temp_provider(&self.source_server)
                .await
                .map_err(crate::providers::ProviderError::TransferFailed)?,
            "source",
        );
        let mut dest = ProviderGuard::new(
            create_temp_provider(&self.dest_server)
                .await
                .map_err(crate::providers::ProviderError::TransferFailed)?,
            "destination",
        );

        let result = copy_one_file_with_options(
            source.provider_mut(),
            dest.provider_mut(),
            &entry.remote_path,
            &entry.local_path,
            entry.modified.as_deref(),
            CrossProfileCopyOptions {
                source_size: Some(entry.size),
                download_segments:
                    crate::transfer_settings::DownloadSegmentsRequest::MeasuredDefault,
                cancel_token: self.cancel_token.clone(),
            },
        )
        .await;

        if let Err(err) = source.disconnect().await {
            tracing::warn!(
                "cross-profile: failed to disconnect source provider: {}",
                err
            );
        }
        if let Err(err) = dest.disconnect().await {
            tracing::warn!(
                "cross-profile: failed to disconnect destination provider: {}",
                err
            );
        }

        result
    }

    fn emit_file_start(&self, entry: &TransferEntry) {
        emit_transfer_event(
            &self.app,
            TransferEvent {
                event_type: "file_start".to_string(),
                transfer_id: self.transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "cross-profile".to_string(),
                message: None,
                progress: Some(
                    self.progress(entry, self.counters.terminal.load(Ordering::Relaxed)),
                ),
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    fn emit_file_skip(&self, entry: &TransferEntry) {
        self.counters.skipped.fetch_add(1, Ordering::Relaxed);
        let terminal = self.counters.terminal.fetch_add(1, Ordering::Relaxed) + 1;
        emit_transfer_event(
            &self.app,
            TransferEvent {
                event_type: "file_skip".to_string(),
                transfer_id: self.transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "cross-profile".to_string(),
                message: Some("Skipped existing destination file".to_string()),
                progress: Some(self.progress(entry, terminal)),
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    fn emit_file_complete(&self, entry: &TransferEntry) {
        self.counters.transferred.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_transferred
            .fetch_add(entry.size, Ordering::Relaxed);
        let terminal = self.counters.terminal.fetch_add(1, Ordering::Relaxed) + 1;
        emit_transfer_event(
            &self.app,
            TransferEvent {
                event_type: "file_complete".to_string(),
                transfer_id: self.transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "cross-profile".to_string(),
                message: None,
                progress: Some(self.progress(entry, terminal)),
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    fn emit_file_error(&self, entry: &TransferEntry, err: &crate::providers::ProviderError) {
        self.counters.failed.fetch_add(1, Ordering::Relaxed);
        self.counters.terminal.fetch_add(1, Ordering::Relaxed);
        self.counters.record_failure(CrossProfileFileFailure {
            source_path: entry.remote_path.clone(),
            dest_path: entry.local_path.clone(),
            error: err.to_string(),
        });
        emit_transfer_event(
            &self.app,
            TransferEvent {
                event_type: "file_error".to_string(),
                transfer_id: self.transfer_id.clone(),
                filename: entry.display_name.clone(),
                direction: "cross-profile".to_string(),
                message: Some(err.to_string()),
                progress: None,
                path: Some(entry.remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    fn emit_cancelled_once(&self) {
        if self.counters.cancel_emitted.swap(true, Ordering::Relaxed) {
            return;
        }
        emit_transfer_event(
            &self.app,
            TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: self.transfer_id.clone(),
                filename: String::new(),
                direction: "cross-profile".to_string(),
                message: Some("Transfer cancelled by user".to_string()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    fn progress(&self, entry: &TransferEntry, terminal: u64) -> TransferProgress {
        let bytes = self.counters.bytes_transferred.load(Ordering::Relaxed);
        TransferProgress {
            transfer_id: self.transfer_id.clone(),
            filename: entry.display_name.clone(),
            transferred: terminal,
            total: self.total,
            percentage: ((terminal * 100).checked_div(self.total).unwrap_or(0)).min(100) as u8,
            speed_bps: self.speed_meter.bps(bytes),
            eta_seconds: 0,
            direction: "cross-profile".to_string(),
            total_files: Some(self.total),
            path: Some(entry.remote_path.clone()),
        }
    }
}

fn entry_to_cross_profile(entry: &TransferEntry) -> CrossProfileTransferEntry {
    CrossProfileTransferEntry {
        source_path: entry.remote_path.clone(),
        dest_path: entry.local_path.clone(),
        display_name: entry.display_name.clone(),
        size: entry.size,
        modified: entry.modified.clone(),
        is_dir: false,
    }
}

fn transfer_failed(message: String) -> TransferOutcome {
    TransferOutcome::Failed(TransferFailure::new(
        TransferFailureKind::Unknown,
        message,
        false,
    ))
}

fn transfer_cancelled() -> TransferOutcome {
    TransferOutcome::Failed(TransferFailure::new(
        TransferFailureKind::Cancelled,
        "Transfer cancelled by user",
        false,
    ))
}

// ── Tauri Commands ─────────────────────────────────────────────────────────

/// Plan a cross-profile transfer without executing it (dry-run).
/// Returns the full plan plus a backend-issued plan token required for execution.
#[tauri::command]
pub async fn cross_profile_plan(
    state: tauri::State<'_, CrossProfileState>,
    request: CrossProfilePlanRequest,
) -> Result<CrossProfilePlanResponse, String> {
    if request.source_profile_id == request.dest_profile_id {
        return Err("Source and destination must be different saved profiles".to_string());
    }

    validate_remote_path(&request.source_path, "source_path")?;
    validate_remote_path(&request.dest_path, "dest_path")?;

    let servers = load_saved_servers()?;
    let source_server = resolve_server_by_id(&servers, &request.source_profile_id)?;
    let dest_server = resolve_server_by_id(&servers, &request.dest_profile_id)?;

    let mut source = ProviderGuard::new(create_temp_provider(&source_server).await?, "source");
    let mut dest = ProviderGuard::new(create_temp_provider(&dest_server).await?, "destination");

    let core_request = CrossProfileTransferRequest {
        source_profile: source_server.name.clone(),
        dest_profile: dest_server.name.clone(),
        source_path: request.source_path.clone(),
        dest_path: request.dest_path.clone(),
        recursive: request.recursive,
        dry_run: true,
        skip_existing: request.skip_existing,
    };

    let plan = plan_transfer(source.provider_mut(), dest.provider_mut(), &core_request)
        .await
        .map_err(|e| format!("Planning failed: {}", e))?;
    // FTP learns whether its dates are comparable from the listing used by
    // planning (MLSD versus LIST), not just from the protocol name.
    let source_mtime_precision = source.provider_mut().mtime_precision();
    if let Err(err) = source.disconnect().await {
        tracing::warn!(
            "cross-profile: failed to disconnect source provider: {}",
            err
        );
    }
    if let Err(err) = dest.disconnect().await {
        tracing::warn!(
            "cross-profile: failed to disconnect destination provider: {}",
            err
        );
    }
    let plan_id = uuid::Uuid::new_v4().to_string();

    store_plan(
        &state,
        plan_id.clone(),
        StoredCrossProfilePlan {
            source_server,
            dest_server,
            request: core_request,
            plan: plan.clone(),
            source_mtime_precision,
            created_at_ms: now_ms(),
        },
    )
    .await;

    Ok(CrossProfilePlanResponse {
        plan_id,
        source_profile_id: request.source_profile_id,
        dest_profile_id: request.dest_profile_id,
        source_profile: plan.source_profile,
        dest_profile: plan.dest_profile,
        entries: plan.entries,
        directories: plan.directories,
        total_files: plan.total_files,
        total_bytes: plan.total_bytes,
    })
}

/// Execute a previously approved cross-profile transfer with progress events.
#[tauri::command]
pub async fn cross_profile_execute(
    app: AppHandle,
    state: tauri::State<'_, CrossProfileState>,
    request: CrossProfileExecuteRequest,
) -> Result<CrossProfileTransferSummary, String> {
    let transfer_id = request.plan_id.clone();
    let stored = take_plan(&state, &request.plan_id).await?;
    let cancelled = register_cancel_token(&state, &transfer_id).await;

    let result: Result<CrossProfileTransferSummary, String> = async {
        let plan = stored.plan;
        let total = plan.entries.len() as u64;
        let max_concurrent = CROSS_PROFILE_MAX_CONCURRENT.min(total.max(1) as u32);

        emit_transfer_event(
            &app,
            TransferEvent {
                event_type: "start".to_string(),
                transfer_id: transfer_id.clone(),
                filename: format!("{} file(s)", total),
                direction: "cross-profile".to_string(),
                message: Some(format!(
                    "{} -> {}",
                    stored.request.source_profile, stored.request.dest_profile
                )),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );

        let start = Instant::now();
        if !plan.directories.is_empty() {
            let mut dest = ProviderGuard::new(
                create_temp_provider(&stored.dest_server).await?,
                "destination",
            );
            let directories_result = crate::cross_profile_transfer::ensure_planned_directories(
                dest.provider_mut(),
                &plan.directories,
                || cancelled.is_cancelled(),
            )
            .await;
            let _ = dest.disconnect().await;
            directories_result.map_err(|error| error.to_string())?;
        }
        let counters = Arc::new(CrossProfileBatchCounters::new());
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_watcher = AbortOnDrop::spawn({
            let cancelled = cancelled.clone();
            let cancel_flag = cancel_flag.clone();
            async move {
                cancelled.cancelled().await;
                cancel_flag.store(true, Ordering::Relaxed);
            }
        });
        let executor = Arc::new(CrossProfileExecutor {
            app: app.clone(),
            transfer_id: transfer_id.clone(),
            source_server: stored.source_server,
            dest_server: stored.dest_server,
            skip_existing: stored.request.skip_existing,
            source_mtime_precision: stored.source_mtime_precision,
            cancel_token: cancelled.clone(),
            total,
            speed_meter: crate::transfer_speed::SpeedMeter::new(),
            counters: counters.clone(),
        });
        let entries = plan
            .entries
            .into_iter()
            .map(|entry| TransferEntry {
                id: entry.source_path.clone(),
                display_name: entry.display_name,
                remote_path: entry.source_path,
                local_path: entry.dest_path,
                size: entry.size,
                modified: entry.modified,
            })
            .collect::<Vec<_>>();
        let batch = TransferBatch {
            id: transfer_id.clone(),
            display_name: format!("{} file(s)", total),
            direction: TransferDirection::Upload,
            config: TransferBatchConfig {
                max_concurrent,
                max_retries: 0,
                timeout_ms: 30_000,
                max_backlog: crate::transfer_domain::default_transfer_max_backlog(),
                schedule: Default::default(),
            },
            entries,
        };

        let batch_result = execute_batch(
            Arc::new(NoopTransferSink),
            batch,
            executor.clone(),
            cancel_flag,
            None,
        )
        .await;
        drop(cancel_watcher);

        let duration_ms = start.elapsed().as_millis() as u64;
        let transferred = counters.transferred.load(Ordering::Relaxed);
        let skipped = counters.skipped.load(Ordering::Relaxed);
        let mut failed = counters.failed.load(Ordering::Relaxed);
        let bytes_transferred = counters.bytes_transferred.load(Ordering::Relaxed);
        let gap =
            crate::cross_profile_transfer::transfer_tally_gap(total, transferred, skipped, failed);
        if gap > 0 {
            failed += gap;
        }
        let failures = failures_with_tally_gap(counters.take_failures(), gap);
        let was_cancelled = batch_result.cancelled || cancelled.is_cancelled();

        if was_cancelled {
            executor.emit_cancelled_once();
        } else {
            emit_transfer_event(
                &app,
                TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: format!(
                        "{} transferred, {} skipped, {} failed",
                        transferred, skipped, failed
                    ),
                    direction: "cross-profile".to_string(),
                    message: None,
                    progress: Some(TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: String::new(),
                        transferred: transferred + skipped,
                        total,
                        percentage: 100,
                        speed_bps: (bytes_transferred * 1000)
                            .checked_div(duration_ms)
                            .unwrap_or(0),
                        eta_seconds: 0,
                        direction: "cross-profile".to_string(),
                        total_files: Some(total),
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }

        Ok(CrossProfileTransferSummary {
            transfer_id: transfer_id.clone(),
            planned_files: total,
            transferred_files: transferred,
            skipped_files: skipped,
            failed_files: failed,
            failures,
            total_bytes: bytes_transferred,
            duration_ms,
        })
    }
    .await;

    // Every start must have a terminal event, including failures before the
    // executor exists (destination connection or directory preflight). Stop
    // takes precedence if it arrived while the failed operation was pending.
    if let Err(error) = &result {
        emit_transfer_event(
            &app,
            setup_failure_event(&transfer_id, error, cancelled.is_cancelled()),
        );
    }
    clear_cancel_token(&state, &transfer_id).await;
    result
}

fn setup_failure_event(transfer_id: &str, error: &str, cancelled: bool) -> TransferEvent {
    TransferEvent {
        event_type: if cancelled { "cancelled" } else { "error" }.to_string(),
        transfer_id: transfer_id.to_string(),
        filename: String::new(),
        direction: "cross-profile".to_string(),
        message: Some(if cancelled {
            "Transfer cancelled by user".to_string()
        } else {
            error.to_string()
        }),
        progress: None,
        path: None,
        delta_stats: None,
        fallback_reason: None,
    }
}

/// Cancel an in-progress cross-profile transfer.
#[tauri::command]
pub async fn cross_profile_cancel(
    state: tauri::State<'_, CrossProfileState>,
    transfer_id: String,
) -> Result<(), String> {
    let tokens = state.cancel_tokens.lock().await;
    let token = tokens
        .get(&transfer_id)
        .cloned()
        .ok_or_else(|| format!("Transfer '{}' is not active", transfer_id))?;
    token.cancel();
    Ok(())
}
