// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-profile transfer Tauri commands.
//!
//! Exposes plan/execute/cancel for cross-profile transfers via the Tauri IPC
//! bridge, reusing the core engine from `cross_profile_transfer.rs`.

use crate::ai_tools::{create_temp_provider, load_saved_servers, SavedServerInfo};
use crate::cross_profile_transfer::{
    copy_one_file, plan_transfer, should_skip_existing, CrossProfileTransferEntry,
    CrossProfileTransferPlan, CrossProfileTransferRequest,
};
use crate::util::{AbortOnDrop, ProviderGuard};
use crate::{TransferEvent, TransferProgress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const PLAN_TTL_MS: u64 = 15 * 60 * 1000;
const PLAN_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct StoredCrossProfilePlan {
    source_server: SavedServerInfo,
    dest_server: SavedServerInfo,
    request: CrossProfileTransferRequest,
    plan: CrossProfileTransferPlan,
    created_at_ms: u64,
}

// ── State ──────────────────────────────────────────────────────────────────

/// Managed state for cross-profile transfers, holding approved plans and per-transfer cancel flags.
pub struct CrossProfileState {
    approved_plans: Arc<Mutex<HashMap<String, StoredCrossProfilePlan>>>,
    cancel_tokens: Mutex<HashMap<String, CancellationToken>>,
    // Background sweeper task. Spawned lazily by `ensure_sweeper_started`
    // from inside an async Tauri command — `new()` runs during
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
    pub total_bytes: u64,
    pub duration_ms: u64,
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn emit_transfer_event(app: &AppHandle, event: TransferEvent) {
    let _ = app.emit("transfer_event", event);
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

    let result = async {
        let mut source =
            ProviderGuard::new(create_temp_provider(&stored.source_server).await?, "source");
        let mut dest = ProviderGuard::new(
            create_temp_provider(&stored.dest_server).await?,
            "destination",
        );
        let plan = stored.plan;
        let total = plan.entries.len() as u64;

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
            },
        );

        let start = std::time::Instant::now();
        let mut transferred: u64 = 0;
        let mut skipped: u64 = 0;
        let mut failed: u64 = 0;
        let mut bytes_transferred: u64 = 0;
        let mut was_cancelled = false;

        for entry in &plan.entries {
            let event_path = entry.source_path.clone();

            if cancelled.is_cancelled() {
                was_cancelled = true;
                emit_transfer_event(
                    &app,
                    TransferEvent {
                        event_type: "cancelled".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: String::new(),
                        direction: "cross-profile".to_string(),
                        message: Some("Transfer cancelled by user".to_string()),
                        progress: None,
                        path: None,
                    },
                );
                break;
            }

            if stored.request.skip_existing {
                if let Ok(true) =
                    should_skip_existing(dest.provider_mut(), &entry.dest_path, entry).await
                {
                    skipped += 1;
                    emit_transfer_event(
                        &app,
                        TransferEvent {
                            event_type: "file_skip".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: entry.display_name.clone(),
                            direction: "cross-profile".to_string(),
                            message: Some("Skipped existing destination file".to_string()),
                            progress: Some(TransferProgress {
                                transfer_id: transfer_id.clone(),
                                filename: entry.display_name.clone(),
                                transferred: transferred + skipped,
                                total,
                                percentage: (((transferred + skipped) * 100)
                                    .checked_div(total)
                                    .unwrap_or(0))
                                .min(100) as u8,
                                speed_bps: 0,
                                eta_seconds: 0,
                                direction: "cross-profile".to_string(),
                                total_files: Some(total),
                                path: Some(event_path.clone()),
                            }),
                            path: Some(event_path.clone()),
                        },
                    );
                    continue;
                }
            }

            emit_transfer_event(
                &app,
                TransferEvent {
                    event_type: "file_start".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: entry.display_name.clone(),
                    direction: "cross-profile".to_string(),
                    message: None,
                    progress: Some(TransferProgress {
                        transfer_id: transfer_id.clone(),
                        filename: entry.display_name.clone(),
                        transferred,
                        total,
                        percentage: ((transferred * 100).checked_div(total).unwrap_or(0)).min(100)
                            as u8,
                        speed_bps: 0,
                        eta_seconds: 0,
                        direction: "cross-profile".to_string(),
                        total_files: Some(total),
                        path: Some(event_path.clone()),
                    }),
                    path: Some(event_path.clone()),
                },
            );

            const MAX_RETRY: u32 = 3;
            let mut file_ok = false;
            for attempt in 1..=MAX_RETRY {
                match copy_one_file(
                    source.provider_mut(),
                    dest.provider_mut(),
                    &entry.source_path,
                    &entry.dest_path,
                    entry.modified.as_deref(),
                )
                .await
                {
                    Ok(()) => {
                        file_ok = true;
                        break;
                    }
                    Err(e) => {
                        if attempt == MAX_RETRY {
                            failed += 1;
                            emit_transfer_event(
                                &app,
                                TransferEvent {
                                    event_type: "file_error".to_string(),
                                    transfer_id: transfer_id.clone(),
                                    filename: entry.display_name.clone(),
                                    direction: "cross-profile".to_string(),
                                    message: Some(e.to_string()),
                                    progress: None,
                                    path: Some(event_path.clone()),
                                },
                            );
                        } else {
                            tokio::select! {
                                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                                _ = cancelled.cancelled() => {
                                    was_cancelled = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            if was_cancelled {
                emit_transfer_event(
                    &app,
                    TransferEvent {
                        event_type: "cancelled".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: String::new(),
                        direction: "cross-profile".to_string(),
                        message: Some("Transfer cancelled by user".to_string()),
                        progress: None,
                        path: None,
                    },
                );
                break;
            }

            if file_ok {
                transferred += 1;
                bytes_transferred += entry.size;
                let elapsed_ms = start.elapsed().as_millis() as u64;
                emit_transfer_event(
                    &app,
                    TransferEvent {
                        event_type: "file_complete".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: entry.display_name.clone(),
                        direction: "cross-profile".to_string(),
                        message: None,
                        progress: Some(TransferProgress {
                            transfer_id: transfer_id.clone(),
                            filename: entry.display_name.clone(),
                            transferred: transferred + skipped,
                            total,
                            percentage: (((transferred + skipped) * 100)
                                .checked_div(total)
                                .unwrap_or(100))
                            .min(100) as u8,
                            speed_bps: (bytes_transferred * 1000)
                                .checked_div(elapsed_ms)
                                .unwrap_or(0),
                            eta_seconds: 0,
                            direction: "cross-profile".to_string(),
                            total_files: Some(total),
                            path: Some(event_path.clone()),
                        }),
                        path: Some(event_path.clone()),
                    },
                );
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        if !was_cancelled {
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
                },
            );
        }

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

        Ok(CrossProfileTransferSummary {
            transfer_id: transfer_id.clone(),
            planned_files: total,
            transferred_files: transferred,
            skipped_files: skipped,
            failed_files: failed,
            total_bytes: bytes_transferred,
            duration_ms,
        })
    }
    .await;

    clear_cancel_token(&state, &transfer_id).await;
    result
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
