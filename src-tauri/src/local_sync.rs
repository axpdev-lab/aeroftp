//! Z.2.3: Tauri command for local-to-local sync.
//!
//! Mirrors what `cmd_sync_local_to_local` does in the CLI (see
//! `src/bin/aeroftp_cli.rs`) but exposed to the frontend so the AeroSync
//! UI can present a "Local → Local" mode that walks a source directory,
//! routes large files through `LocalDeltaTransport`, and falls back to
//! plain copy otherwise. No SSH, no provider plumbing.

#![cfg(feature = "aerorsync")]

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::aerorsync::local_transport::LocalDeltaTransport;
use crate::delta_transport::DeltaTransport;
use crate::rsync_over_ssh::{RsyncError, DEFAULT_MIN_FILE_SIZE};
use crate::sync::VerifyPolicy;

#[derive(Debug, Clone, Deserialize)]
pub struct LocalSyncRequest {
    pub source: String,
    pub destination: String,
    /// Comma-or-newline separated globs to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// When true, route everything through plain `fs::copy` (skip delta).
    #[serde(default)]
    pub no_delta: bool,
    /// Preview mode: walk + report, do not write.
    #[serde(default)]
    pub dry_run: bool,
    /// CO-1: AeroSync Plan tab "Speed mode" preset. Carried for
    /// observability today; future slices map it to parallel workers
    /// and per-file compression toggles.
    #[serde(default)]
    pub speed_mode: Option<String>,
    /// CO-1: AeroSync Plan tab "Verify policy". When set, each
    /// successfully transferred file is checked against the policy
    /// (size / size+mtime / size+sha256). Failures increment the
    /// report error count and append a `verify` error message.
    #[serde(default)]
    pub verify_policy: Option<VerifyPolicy>,
    /// CO-3: AeroSync bandwidth caps in KB/s. 0 / None = unlimited.
    /// `local_sync_run` is a single in-process pipeline; both caps
    /// fold into a single effective limit (`max(upload, download)`)
    /// because there is no real "wire" direction in a local-to-
    /// local mirror.
    #[serde(default)]
    pub upload_limit_kbps: Option<u64>,
    #[serde(default)]
    pub download_limit_kbps: Option<u64>,
}

/// CO-2: per-file result row surfaced in the Sync tab table.
///
/// Capped at `MAX_ENTRY_ROWS` to keep the payload bounded on multi-
/// thousand-file runs; the aggregate counters in `LocalSyncReport`
/// keep the full picture.
#[derive(Debug, Clone, Serialize)]
pub struct LocalSyncEntry {
    /// Relative path from the source root.
    pub name: String,
    /// One of: `delta`, `copy`, `skip`, `dry-run`, `error`.
    pub action: String,
    /// Wire bytes for this file (0 on skip / dry-run / error).
    pub bytes: u64,
    /// `ok` on success, `error` on failure, `skipped` on exclude
    /// match, `dry-run` in preview mode.
    pub status: String,
}

const MAX_ENTRY_ROWS: usize = 5000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct LocalSyncReport {
    pub status: String,
    pub uploaded: u32,
    pub skipped: u32,
    pub errors: u32,
    pub elapsed_ms: u128,
    pub total_payload_bytes: u64,
    pub bytes_on_wire: u64,
    pub savings_ratio: f64,
    pub error_messages: Vec<String>,
    /// CO-2: per-file rows for the Sync tab results table. Capped
    /// at `MAX_ENTRY_ROWS` to keep the payload bounded on multi-
    /// thousand-file runs.
    #[serde(default)]
    pub entries: Vec<LocalSyncEntry>,
    /// CO-2: true when the run captured more files than
    /// `MAX_ENTRY_ROWS` and the table is therefore truncated.
    #[serde(default)]
    pub entries_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalSyncProgress {
    pub processed: u32,
    pub total: u32,
    pub current_path: String,
    pub bytes_on_wire: u64,
    pub used_delta: bool,
}

/// CO-2: bounded push helper for per-file rows. Once the report has
/// reached `MAX_ENTRY_ROWS` we stop appending and just flag the
/// truncation so the UI can surface it.
fn push_entry(report: &mut LocalSyncReport, entry: LocalSyncEntry) {
    if report.entries.len() >= MAX_ENTRY_ROWS {
        report.entries_truncated = true;
        return;
    }
    report.entries.push(entry);
}

/// Walk `source` and mirror every file into `destination`. Files at or above
/// `DEFAULT_MIN_FILE_SIZE` (1 MiB) go through `LocalDeltaTransport` unless
/// `no_delta` is set; smaller files fall back to plain copy.
#[tauri::command]
pub async fn local_sync_run(
    app: AppHandle,
    request: LocalSyncRequest,
) -> Result<LocalSyncReport, String> {
    let source = PathBuf::from(&request.source);
    let destination = PathBuf::from(&request.destination);

    if !source.exists() {
        return Err(format!("Source path not found: {}", source.display()));
    }
    if !source.is_dir() {
        return Err(format!("Source must be a directory: {}", source.display()));
    }
    if !request.dry_run {
        std::fs::create_dir_all(&destination)
            .map_err(|e| format!("create destination {}: {}", destination.display(), e))?;
    }

    let exclude_matchers: Vec<globset::GlobMatcher> = request
        .exclude
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect();

    // Pre-walk to count total files (for accurate progress denominator).
    let total_files: u32 = walkdir::WalkDir::new(&source)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count() as u32;

    let start = Instant::now();
    let mut report = LocalSyncReport {
        status: "ok".to_string(),
        ..Default::default()
    };

    let transport = if request.no_delta {
        None
    } else {
        Some(LocalDeltaTransport::new(DEFAULT_MIN_FILE_SIZE))
    };

    // CO-3: fold both caps into a single effective bytes/s limit.
    // Zero / None disables the throttle.
    let throttle_bps: u64 = {
        let up = request.upload_limit_kbps.unwrap_or(0);
        let down = request.download_limit_kbps.unwrap_or(0);
        let max_kbps = up.max(down);
        max_kbps.saturating_mul(1024)
    };

    let mut processed: u32 = 0;
    for entry in walkdir::WalkDir::new(&source).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                report.errors += 1;
                report.error_messages.push(format!("walk: {}", e));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = match entry.path().strip_prefix(&source) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let rel_str = relative.to_string_lossy().to_string();
        if rel_str.is_empty() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        let rel_path_ref: &Path = relative.as_path();
        let fname_path_ref: &Path = Path::new(&fname);
        if exclude_matchers
            .iter()
            .any(|m| m.is_match(rel_path_ref) || m.is_match(fname_path_ref))
        {
            report.skipped += 1;
            push_entry(
                &mut report,
                LocalSyncEntry {
                    name: rel_str.clone(),
                    action: "skip".to_string(),
                    bytes: 0,
                    status: "skipped".to_string(),
                },
            );
            processed += 1;
            continue;
        }

        let src = entry.path().to_path_buf();
        let dst = destination.join(&relative);

        if request.dry_run {
            report.uploaded += 1;
            push_entry(
                &mut report,
                LocalSyncEntry {
                    name: rel_str.clone(),
                    action: "dry-run".to_string(),
                    bytes: 0,
                    status: "dry-run".to_string(),
                },
            );
            processed += 1;
            let _ = app.emit(
                "local-sync-progress",
                LocalSyncProgress {
                    processed,
                    total: total_files,
                    current_path: rel_str,
                    bytes_on_wire: 0,
                    used_delta: false,
                },
            );
            continue;
        }

        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                report.errors += 1;
                report
                    .error_messages
                    .push(format!("mkdir {}: {}", parent.display(), e));
                push_entry(
                    &mut report,
                    LocalSyncEntry {
                        name: rel_str.clone(),
                        action: "error".to_string(),
                        bytes: 0,
                        status: "error".to_string(),
                    },
                );
                processed += 1;
                continue;
            }
        }

        let src_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        report.total_payload_bytes = report.total_payload_bytes.saturating_add(src_size);

        let mut used_delta = false;
        let mut wire_bytes: u64 = 0;

        if let Some(t) = transport.as_ref() {
            match t.upload(&src, dst.to_string_lossy().as_ref()).await {
                Ok(s) => {
                    used_delta = true;
                    wire_bytes = s.bytes_sent;
                }
                Err(RsyncError::TooSmall { .. } | RsyncError::TransferFailed { .. }) => {
                    // Fallback to plain copy below.
                }
                Err(e) => {
                    report.errors += 1;
                    report.error_messages.push(format!(
                        "delta {}: {}; falling back to plain copy",
                        rel_str, e
                    ));
                }
            }
        }

        if !used_delta {
            match std::fs::copy(&src, &dst) {
                Ok(n) => wire_bytes = n,
                Err(e) => {
                    report.errors += 1;
                    report
                        .error_messages
                        .push(format!("copy {}: {}", rel_str, e));
                    push_entry(
                        &mut report,
                        LocalSyncEntry {
                            name: rel_str.clone(),
                            action: "error".to_string(),
                            bytes: 0,
                            status: "error".to_string(),
                        },
                    );
                    processed += 1;
                    continue;
                }
            }
        }

        report.uploaded += 1;
        report.bytes_on_wire = report.bytes_on_wire.saturating_add(wire_bytes);
        let mut entry_status: &str = "ok";
        let entry_action: &str = if used_delta { "delta" } else { "copy" };

        // CO-1: post-copy verification. Skipped when the policy is
        // `None`, when the request did not include one (legacy
        // callers), or when the destination file is missing because
        // an earlier branch already accounted the error.
        if let Some(policy) = request.verify_policy.as_ref() {
            if *policy != VerifyPolicy::None {
                let expected_mtime = std::fs::metadata(&src)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(<chrono::DateTime<chrono::Utc>>::from);
                let dst_str = dst.to_string_lossy().to_string();
                let verify = crate::sync::verify_local_file(
                    dst_str.as_str(),
                    src_size,
                    expected_mtime,
                    policy,
                    None,
                );
                if !verify.passed {
                    report.errors += 1;
                    report.error_messages.push(format!(
                        "verify {}: {}",
                        rel_str,
                        verify
                            .message
                            .unwrap_or_else(|| "policy not satisfied".to_string()),
                    ));
                    entry_status = "error";
                }
            }
        }

        push_entry(
            &mut report,
            LocalSyncEntry {
                name: rel_str.clone(),
                action: entry_action.to_string(),
                bytes: wire_bytes,
                status: entry_status.to_string(),
            },
        );

        // CO-3: post-file throttle. Sleeps for the time the configured
        // cap implies for the bytes we just produced. Skipped when no
        // cap is set or the file did not move bytes.
        if throttle_bps > 0 && wire_bytes > 0 {
            let secs = wire_bytes as f64 / throttle_bps as f64;
            // Hard cap the per-file delay at 30s to avoid pathological
            // pauses if someone sets a 1 KB/s limit on a 30 MB file.
            let secs = secs.min(30.0);
            if secs > 0.001 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
            }
        }

        processed += 1;

        let _ = app.emit(
            "local-sync-progress",
            LocalSyncProgress {
                processed,
                total: total_files,
                current_path: rel_str,
                bytes_on_wire: wire_bytes,
                used_delta,
            },
        );
    }

    report.elapsed_ms = start.elapsed().as_millis();
    report.savings_ratio = if report.total_payload_bytes > 0 {
        report.bytes_on_wire as f64 / report.total_payload_bytes as f64
    } else {
        0.0
    };
    if report.errors > 0 {
        report.status = "partial".to_string();
    }
    Ok(report)
}
