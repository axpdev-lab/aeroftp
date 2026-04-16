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
use crate::{TransferEvent, TransferProgress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

const PLAN_TTL_MS: u64 = 15 * 60 * 1000;

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
    approved_plans: Mutex<HashMap<String, StoredCrossProfilePlan>>,
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl CrossProfileState {
    pub fn new() -> Self {
        Self {
            approved_plans: Mutex::new(HashMap::new()),
            cancel_flags: Mutex::new(HashMap::new()),
        }
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

async fn register_cancel_flag(state: &CrossProfileState, transfer_id: &str) -> Arc<AtomicBool> {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let mut flags = state.cancel_flags.lock().await;
    flags.insert(transfer_id.to_string(), cancel_flag.clone());
    cancel_flag
}

async fn clear_cancel_flag(state: &CrossProfileState, transfer_id: &str) {
    let mut flags = state.cancel_flags.lock().await;
    flags.remove(transfer_id);
}

async fn disconnect_with_log(
    provider: &mut Box<dyn crate::providers::StorageProvider>,
    label: &str,
) {
    if let Err(err) = provider.disconnect().await {
        eprintln!("cross-profile: failed to disconnect {} provider: {}", label, err);
    }
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

    let mut source = create_temp_provider(&source_server).await?;
    let mut dest = create_temp_provider(&dest_server).await?;

    let core_request = CrossProfileTransferRequest {
        source_profile: source_server.name.clone(),
        dest_profile: dest_server.name.clone(),
        source_path: request.source_path.clone(),
        dest_path: request.dest_path.clone(),
        recursive: request.recursive,
        dry_run: true,
        skip_existing: request.skip_existing,
    };

    let plan_result = plan_transfer(source.as_mut(), dest.as_mut(), &core_request).await;
    disconnect_with_log(&mut source, "source").await;
    disconnect_with_log(&mut dest, "destination").await;

    let plan = plan_result.map_err(|e| format!("Planning failed: {}", e))?;
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
    let cancelled = register_cancel_flag(&state, &transfer_id).await;

    let mut source = create_temp_provider(&stored.source_server).await?;
    let mut dest = create_temp_provider(&stored.dest_server).await?;
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

        if cancelled.load(Ordering::Relaxed) {
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
            if let Ok(true) = should_skip_existing(dest.as_mut(), &entry.dest_path, entry).await {
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
                            percentage: (((transferred + skipped) * 100).checked_div(total).unwrap_or(0)).min(100) as u8,
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
                    percentage: ((transferred * 100).checked_div(total).unwrap_or(0)).min(100) as u8,
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
                source.as_mut(),
                dest.as_mut(),
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
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
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
                        percentage: (((transferred + skipped) * 100).checked_div(total).unwrap_or(100)).min(100) as u8,
                        speed_bps: (bytes_transferred * 1000).checked_div(elapsed_ms).unwrap_or(0),
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
                    speed_bps: (bytes_transferred * 1000).checked_div(duration_ms).unwrap_or(0),
                    eta_seconds: 0,
                    direction: "cross-profile".to_string(),
                    total_files: Some(total),
                    path: None,
                }),
                path: None,
            },
        );
    }

    disconnect_with_log(&mut source, "source").await;
    disconnect_with_log(&mut dest, "destination").await;
    clear_cancel_flag(&state, &transfer_id).await;

    Ok(CrossProfileTransferSummary {
        transfer_id,
        planned_files: total,
        transferred_files: transferred,
        skipped_files: skipped,
        failed_files: failed,
        total_bytes: bytes_transferred,
        duration_ms,
    })
}

/// Cancel an in-progress cross-profile transfer.
#[tauri::command]
pub async fn cross_profile_cancel(
    state: tauri::State<'_, CrossProfileState>,
    transfer_id: String,
) -> Result<(), String> {
    let flags = state.cancel_flags.lock().await;
    let flag = flags
        .get(&transfer_id)
        .cloned()
        .ok_or_else(|| format!("Transfer '{}' is not active", transfer_id))?;
    flag.store(true, Ordering::Relaxed);
    Ok(())
}
