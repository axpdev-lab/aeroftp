// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Professional server speed test (Phase 2).
//!
//! Phase 2 adds:
//! - streaming download to a local tempfile (no full-file RAM buffer)
//! - per-test progress events with `test_id`/`server_name` so the frontend
//!   can distinguish parallel runs in compare mode
//! - multi-server compare API (`speedtest_compare`) with concurrency cap
//! - expert custom test sizes (1 MB - 1 GB) gated by an explicit confirmation
//!   flag passed from the UI.

use crate::provider_commands::ProviderConnectionParams;
use crate::providers::{ProviderFactory, StorageProvider};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};
use tempfile::NamedTempFile;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

const ONE_MB: u64 = 1024 * 1024;
const ONE_GB: u64 = 1024 * ONE_MB;
/// Sizes that don't need explicit confirmation: 1, 10, 100 MB.
const STANDARD_SIZES: [u64; 3] = [ONE_MB, 10 * ONE_MB, 100 * ONE_MB];
/// Size threshold above which the UI must pass `expert_confirmed = true`.
const EXPERT_CONFIRM_THRESHOLD: u64 = 100 * ONE_MB;
/// Hard ceiling for any speed test, expert mode included.
const MAX_TEST_SIZE: u64 = ONE_GB;
const EVENT_NAME: &str = "speedtest-progress";
const COMPARE_DEFAULT_PARALLEL: u8 = 2;
const COMPARE_MAX_PARALLEL: u8 = 4;
/// Hard cap on compare-mode test count. Prevents accidental N-server
/// expensive runs (object storage egress, time, local disk) when an operator
/// multi-selects many profiles. Aligns with the UI Select-all cap of 8.
const COMPARE_MAX_TESTS: usize = 8;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SpeedTestState {
    /// True while either a single run or a compare run is active.
    busy: Mutex<bool>,
    /// Parent cancel token for the active run; cancelling it cascades to all
    /// child tasks (each parallel test in a compare run is a child token).
    cancel: Mutex<Option<CancellationToken>>,
}

impl SpeedTestState {
    pub fn new() -> Self {
        Self {
            busy: Mutex::new(false),
            cancel: Mutex::new(None),
        }
    }
}

impl Default for SpeedTestState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpeedTestRunRequest {
    pub connection: ProviderConnectionParams,
    pub size_bytes: u64,
    pub remote_dir: String,
    pub server_name: Option<String>,
    /// Frontend-supplied id used to route progress events. Optional for
    /// back-compat: a fresh uuid is generated when missing.
    pub test_id: Option<String>,
    /// Set to `true` when the UI has explicitly confirmed running with a
    /// non-standard size (above 100 MB). Required for expert sizes.
    #[serde(default)]
    pub expert_confirmed: bool,
    /// SHA-256 verification of the downloaded copy. Defaults to true.
    /// Skipping speeds up runs on very large payloads when the operator
    /// already trusts the link, mirroring the CLI's `--no-integrity`.
    #[serde(default = "default_true")]
    pub verify_integrity: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpeedTestCompareRequest {
    pub tests: Vec<SpeedTestRunRequest>,
    pub max_parallel: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestResult {
    pub test_id: String,
    pub server_name: Option<String>,
    pub protocol: String,
    pub remote_path: String,
    pub temp_file_name: String,
    pub size_bytes: u64,
    pub upload_duration_ms: u64,
    pub download_duration_ms: u64,
    /// Time from `provider.download()` issued to the first progress callback
    /// reporting any transferred bytes. Approximates TTFB on the data plane.
    pub download_ttfb_ms: Option<u64>,
    pub upload_bytes_per_sec: f64,
    pub download_bytes_per_sec: f64,
    pub upload_mbps: f64,
    pub download_mbps: f64,
    /// True when SHA-256 integrity check was performed (vs explicitly skipped).
    pub integrity_checked: bool,
    /// True only when the check was performed AND hashes matched. False both
    /// for corruption AND for skipped runs — always read together with
    /// `integrity_checked` to disambiguate.
    pub integrity_verified: bool,
    pub upload_sha256: String,
    pub download_sha256: String,
    pub temp_file_cleaned: bool,
    pub cleanup_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestRunOutcome {
    pub test_id: String,
    pub server_name: Option<String>,
    pub result: Option<SpeedTestResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestCompareResult {
    pub size_bytes: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub results: Vec<SpeedTestRunOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestProgress {
    pub test_id: String,
    pub server_name: Option<String>,
    pub phase: &'static str,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: Option<f64>,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn speedtest_cancel(state: State<'_, SpeedTestState>) -> Result<(), String> {
    let token = state.cancel.lock().await.clone();
    if let Some(token) = token {
        token.cancel();
        Ok(())
    } else {
        Err("No speed test is running".to_string())
    }
}

#[tauri::command]
pub async fn speedtest_run(
    app: AppHandle,
    state: State<'_, SpeedTestState>,
    request: SpeedTestRunRequest,
) -> Result<SpeedTestResult, String> {
    {
        let mut busy = state.busy.lock().await;
        if *busy {
            return Err("A speed test is already running".to_string());
        }
        *busy = true;
    }

    let token = CancellationToken::new();
    *state.cancel.lock().await = Some(token.clone());

    let outcome = run_one(app, request, token).await;

    *state.cancel.lock().await = None;
    *state.busy.lock().await = false;

    match outcome.result {
        Some(r) => Ok(r),
        None => Err(outcome.error.unwrap_or_else(|| "Unknown error".to_string())),
    }
}

#[tauri::command]
pub async fn speedtest_compare(
    app: AppHandle,
    state: State<'_, SpeedTestState>,
    request: SpeedTestCompareRequest,
) -> Result<SpeedTestCompareResult, String> {
    if request.tests.is_empty() {
        return Err("Compare run needs at least one test".to_string());
    }
    if request.tests.len() > COMPARE_MAX_TESTS {
        return Err(format!(
            "Compare run is capped at {} tests; got {}",
            COMPARE_MAX_TESTS,
            request.tests.len()
        ));
    }
    let size_bytes = request.tests[0].size_bytes;
    if !request.tests.iter().all(|t| t.size_bytes == size_bytes) {
        return Err("All tests in a compare run must use the same size".to_string());
    }

    {
        let mut busy = state.busy.lock().await;
        if *busy {
            return Err("A speed test is already running".to_string());
        }
        *busy = true;
    }

    let token = CancellationToken::new();
    *state.cancel.lock().await = Some(token.clone());

    let result = run_compare_inner(app, request, token).await;

    *state.cancel.lock().await = None;
    *state.busy.lock().await = false;

    Ok(result)
}

// ---------------------------------------------------------------------------
// Compare orchestrator
// ---------------------------------------------------------------------------

async fn run_compare_inner(
    app: AppHandle,
    request: SpeedTestCompareRequest,
    parent_token: CancellationToken,
) -> SpeedTestCompareResult {
    let started_at_ms = unix_ms();
    let max_parallel = request
        .max_parallel
        .unwrap_or(COMPARE_DEFAULT_PARALLEL)
        .clamp(1, COMPARE_MAX_PARALLEL);

    let size_bytes = request.tests[0].size_bytes;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel as usize));
    let mut handles = Vec::with_capacity(request.tests.len());

    for test in request.tests.into_iter() {
        let app_cloned = app.clone();
        let token = parent_token.child_token();
        let sem = Arc::clone(&semaphore);
        handles.push(tokio::spawn(async move {
            let _permit = match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    return SpeedTestRunOutcome {
                        test_id: test
                            .test_id
                            .clone()
                            .unwrap_or_else(|| Uuid::new_v4().to_string()),
                        server_name: test.server_name.clone(),
                        result: None,
                        error: Some("Semaphore closed".to_string()),
                    };
                }
            };
            run_one(app_cloned, test, token).await
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(outcome) => results.push(outcome),
            Err(e) => results.push(SpeedTestRunOutcome {
                test_id: Uuid::new_v4().to_string(),
                server_name: None,
                result: None,
                error: Some(format!("Task panicked: {}", e)),
            }),
        }
    }

    SpeedTestCompareResult {
        size_bytes,
        started_at_ms,
        finished_at_ms: unix_ms(),
        results,
    }
}

// ---------------------------------------------------------------------------
// Single test core
// ---------------------------------------------------------------------------

async fn run_one(
    app: AppHandle,
    request: SpeedTestRunRequest,
    token: CancellationToken,
) -> SpeedTestRunOutcome {
    let test_id = request
        .test_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let server_name = request.server_name.clone();

    match run_speedtest_inner(app, request, token, test_id.clone()).await {
        Ok(result) => SpeedTestRunOutcome {
            test_id,
            server_name,
            result: Some(result),
            error: None,
        },
        Err(err) => SpeedTestRunOutcome {
            test_id,
            server_name,
            result: None,
            error: Some(err),
        },
    }
}

async fn run_speedtest_inner(
    app: AppHandle,
    request: SpeedTestRunRequest,
    token: CancellationToken,
    test_id: String,
) -> Result<SpeedTestResult, String> {
    validate_size(request.size_bytes, request.expert_confirmed)?;
    validate_supported_protocol(&request.connection.protocol)?;

    let server_name = request.server_name.clone();
    emit_progress(
        &app,
        &test_id,
        server_name.as_deref(),
        "connecting",
        0,
        request.size_bytes,
        None,
    );

    // Allocate BOTH local tempfiles BEFORE opening any network connection.
    // This way, OS-level allocation failures cannot orphan a remote file or
    // leak a connected provider — there's nothing to clean up yet.
    let size = request.size_bytes;
    let (local_upload_tmp, upload_sha256) =
        tokio::task::spawn_blocking(move || create_random_temp_file(size))
            .await
            .map_err(|e| format!("Payload task panicked: {}", e))??;
    let download_target = NamedTempFile::new()
        .map_err(|e| format!("Failed to create download tmp: {}", e))?;
    let download_target_path = download_target.path().to_path_buf();

    let mut config = request.connection.to_provider_config()?;
    let protocol = config.provider_type.to_string();
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    config.zeroize_password();

    run_cancelable(token.clone(), provider.connect(), "Test cancelled while connecting").await?;

    let temp_file_name = format!(".aeroftp-speedtest-{}.bin", Uuid::new_v4());
    let remote_path = join_remote_path(&request.remote_dir, &temp_file_name);

    info!(
        "speedtest: {} {} bytes -> {} (test_id={})",
        protocol, request.size_bytes, remote_path, test_id
    );

    // ---------------------- UPLOAD ----------------------
    let upload_started_at = Instant::now();
    let upload_result = {
        let phase_started_at = Instant::now();
        let app_for_progress = app.clone();
        let token_for_progress = token.clone();
        let total = request.size_bytes;
        let test_id_for_progress = test_id.clone();
        let server_name_for_progress = server_name.clone();
        // Throttle: cap progress events to ~10 Hz per test to avoid overloading
        // the IPC bridge during 4-way compare runs (audit P1-8).
        let last_emit_ms = Arc::new(AtomicU64::new(0));
        let last_emit_clone = Arc::clone(&last_emit_ms);
        let progress = Box::new(move |transferred: u64, total_bytes: u64| {
            let elapsed_ms_now = phase_started_at.elapsed().as_millis() as u64;
            let last = last_emit_clone.load(Ordering::Relaxed);
            let total_now = if total_bytes > 0 { total_bytes } else { total };
            let is_terminal = transferred >= total_now;
            if !is_terminal && elapsed_ms_now.saturating_sub(last) < 100 {
                return;
            }
            last_emit_clone.store(elapsed_ms_now, Ordering::Relaxed);
            let elapsed = phase_started_at.elapsed().as_secs_f64().max(0.001);
            let bps = transferred as f64 / elapsed;
            emit_progress(
                &app_for_progress,
                &test_id_for_progress,
                server_name_for_progress.as_deref(),
                "uploading",
                transferred,
                total_now,
                Some(bps),
            );
            if token_for_progress.is_cancelled() {
                warn!("speedtest: cancellation requested during upload (test_id={})", test_id_for_progress);
            }
        });
        emit_progress(&app, &test_id, server_name.as_deref(), "uploading", 0, request.size_bytes, None);
        run_cancelable(
            token.clone(),
            provider.upload(
                local_upload_tmp.path().to_string_lossy().as_ref(),
                &remote_path,
                Some(progress),
            ),
            "Test cancelled during upload",
        )
        .await
    };
    let upload_duration_ms = elapsed_ms(upload_started_at);
    let remote_may_exist = true;

    if let Err(err) = upload_result {
        let (cleaned, cleanup_error) =
            cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist, &test_id, server_name.as_deref()).await;
        let _ = provider.disconnect().await;
        if cleaned {
            return Err(err);
        }
        let suffix = cleanup_error
            .as_ref()
            .map(|e| format!(" Cleanup failed: {}", e))
            .unwrap_or_default();
        return Err(format!("{}{}", err, suffix));
    }

    if token.is_cancelled() {
        let _ = cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist, &test_id, server_name.as_deref()).await;
        let _ = provider.disconnect().await;
        return Err("Test cancelled".to_string());
    }

    // ---------------------- DOWNLOAD (streaming) ----------------------
    // download_target / download_target_path were pre-allocated before connect.
    let download_started_at = Instant::now();
    emit_progress(&app, &test_id, server_name.as_deref(), "downloading", 0, request.size_bytes, None);

    let ttfb_ms: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let download_result = {
        let phase_started_at = Instant::now();
        let app_for_progress = app.clone();
        let token_for_progress = token.clone();
        let total = request.size_bytes;
        let test_id_for_progress = test_id.clone();
        let server_name_for_progress = server_name.clone();
        let ttfb_for_cb = Arc::clone(&ttfb_ms);
        let last_emit_ms = Arc::new(AtomicU64::new(0));
        let last_emit_clone = Arc::clone(&last_emit_ms);
        let progress = Box::new(move |transferred: u64, total_bytes: u64| {
            // TTFB sampled on every callback (cheap), but emission is throttled.
            if transferred > 0 && ttfb_for_cb.load(Ordering::Relaxed) == 0 {
                let ms = phase_started_at.elapsed().as_millis() as u64;
                ttfb_for_cb.store(ms.max(1), Ordering::Relaxed);
            }
            let elapsed_ms_now = phase_started_at.elapsed().as_millis() as u64;
            let last = last_emit_clone.load(Ordering::Relaxed);
            let total_now = if total_bytes > 0 { total_bytes } else { total };
            let is_terminal = transferred >= total_now;
            if !is_terminal && elapsed_ms_now.saturating_sub(last) < 100 {
                return;
            }
            last_emit_clone.store(elapsed_ms_now, Ordering::Relaxed);
            let elapsed = phase_started_at.elapsed().as_secs_f64().max(0.001);
            let bps = transferred as f64 / elapsed;
            emit_progress(
                &app_for_progress,
                &test_id_for_progress,
                server_name_for_progress.as_deref(),
                "downloading",
                transferred,
                total_now,
                Some(bps),
            );
            if token_for_progress.is_cancelled() {
                warn!("speedtest: cancellation requested during download (test_id={})", test_id_for_progress);
            }
        });

        run_cancelable(
            token.clone(),
            provider.download(
                &remote_path,
                download_target_path.to_string_lossy().as_ref(),
                Some(progress),
            ),
            "Test cancelled during download",
        )
        .await
    };
    let ttfb_value = ttfb_ms.load(Ordering::Relaxed);
    let download_ttfb_ms = if ttfb_value > 0 { Some(ttfb_value) } else { None };
    let download_duration_ms = elapsed_ms(download_started_at);

    if let Err(err) = download_result {
        let (cleaned, cleanup_error) =
            cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist, &test_id, server_name.as_deref()).await;
        let _ = provider.disconnect().await;
        if cleaned {
            return Err(err);
        }
        let suffix = cleanup_error
            .as_ref()
            .map(|e| format!(" Cleanup failed: {}", e))
            .unwrap_or_default();
        return Err(format!("{}{}", err, suffix));
    }

    // Hash the downloaded tempfile from disk (streaming, no full-file RAM).
    // Skipped when the caller opts out of integrity verification, matching the
    // CLI's --no-integrity flag for faster runs on large payloads.
    //
    // Tri-state model (per senior audit): never report "verified" when the
    // check did not actually run.
    //   integrity_checked=false → integrity_verified=false (neutral)
    //   integrity_checked=true  → integrity_verified=(hash matches)
    let integrity_checked = request.verify_integrity;
    let (download_sha256, integrity_verified) = if integrity_checked {
        match hash_file_async(&download_target_path).await {
            Ok(h) => {
                let verified = h == upload_sha256;
                (h, verified)
            }
            Err(e) => {
                let (_cleaned, _err) = cleanup_remote(
                    &app,
                    provider.as_mut(),
                    &remote_path,
                    remote_may_exist,
                    &test_id,
                    server_name.as_deref(),
                )
                .await;
                let _ = provider.disconnect().await;
                return Err(format!("Failed to hash downloaded file: {}", e));
            }
        }
    } else {
        (String::new(), false)
    };

    let (temp_file_cleaned, cleanup_error) =
        cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist, &test_id, server_name.as_deref()).await;
    let _ = provider.disconnect().await;

    emit_progress(&app, &test_id, server_name.as_deref(), "done", request.size_bytes, request.size_bytes, None);

    let upload_bytes_per_sec = bytes_per_sec(request.size_bytes, upload_duration_ms);
    let download_bytes_per_sec = bytes_per_sec(request.size_bytes, download_duration_ms);

    Ok(SpeedTestResult {
        test_id,
        server_name,
        protocol,
        remote_path,
        temp_file_name,
        size_bytes: request.size_bytes,
        upload_duration_ms,
        download_duration_ms,
        download_ttfb_ms,
        upload_bytes_per_sec,
        download_bytes_per_sec,
        upload_mbps: upload_bytes_per_sec * 8.0 / 1_000_000.0,
        download_mbps: download_bytes_per_sec * 8.0 / 1_000_000.0,
        integrity_checked,
        integrity_verified,
        upload_sha256,
        download_sha256,
        temp_file_cleaned,
        cleanup_error,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn run_cancelable<F, T, E>(
    token: CancellationToken,
    future: F,
    cancelled_message: &'static str,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    tokio::select! {
        result = future => result.map_err(|e| e.to_string()),
        _ = token.cancelled() => Err(cancelled_message.to_string()),
    }
}

async fn cleanup_remote(
    app: &AppHandle,
    provider: &mut dyn StorageProvider,
    remote_path: &str,
    should_try: bool,
    test_id: &str,
    server_name: Option<&str>,
) -> (bool, Option<String>) {
    if !should_try {
        return (true, None);
    }
    emit_progress(app, test_id, server_name, "cleaning_up", 0, 0, None);
    match provider.delete(remote_path).await {
        Ok(()) => (true, None),
        Err(err) => {
            let msg = err.to_string();
            warn!("speedtest: cleanup failed for {}: {}", remote_path, msg);
            (false, Some(msg))
        }
    }
}

fn validate_size(size_bytes: u64, expert_confirmed: bool) -> Result<(), String> {
    if size_bytes == 0 {
        return Err("Speed test size must be > 0".to_string());
    }
    if size_bytes > MAX_TEST_SIZE {
        return Err(format!(
            "Speed test size {} exceeds maximum of 1 GB",
            size_bytes
        ));
    }
    if STANDARD_SIZES.contains(&size_bytes) {
        return Ok(());
    }
    if size_bytes <= EXPERT_CONFIRM_THRESHOLD {
        return Err("Unsupported speed test size".to_string());
    }
    if !expert_confirmed {
        return Err("Expert size requires explicit confirmation".to_string());
    }
    Ok(())
}

fn validate_supported_protocol(protocol: &str) -> Result<(), String> {
    match protocol.to_lowercase().as_str() {
        "ftp" | "ftps" | "sftp" | "s3" | "webdav" => Ok(()),
        other => Err(format!("Speed Test is not available for {} yet", other)),
    }
}

fn join_remote_path(dir: &str, file: &str) -> String {
    let trimmed = dir.trim();
    if trimmed.is_empty() || trimmed == "/" {
        format!("/{file}")
    } else {
        format!("{}/{}", trimmed.trim_end_matches('/'), file)
    }
}

fn create_random_temp_file(size_bytes: u64) -> Result<(NamedTempFile, String), String> {
    let mut tmp = NamedTempFile::new().map_err(|e| format!("Failed to create temp file: {}", e))?;
    let mut rng = rand::thread_rng();
    let mut hasher = Sha256::new();
    let mut remaining = size_bytes;
    let mut chunk = vec![0u8; 1024 * 1024];

    while remaining > 0 {
        let n = remaining.min(chunk.len() as u64) as usize;
        rng.fill_bytes(&mut chunk[..n]);
        tmp.write_all(&chunk[..n])
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        hasher.update(&chunk[..n]);
        remaining -= n as u64;
    }
    tmp.flush()
        .map_err(|e| format!("Failed to flush temp file: {}", e))?;

    Ok((tmp, format!("{:x}", hasher.finalize())))
}

async fn hash_file_async(path: &std::path::Path) -> Result<String, String> {
    // Run on the blocking pool so SHA-256 work for large payloads (1 GB) does
    // not stall the async runtime, especially during 4-way compare runs.
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path).map_err(|e| format!("open: {}", e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf).map_err(|e| format!("read: {}", e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|e| format!("hash task panicked: {}", e))?
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().max(1) as u64
}

fn bytes_per_sec(bytes: u64, duration_ms: u64) -> f64 {
    bytes as f64 * 1000.0 / duration_ms.max(1) as f64
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_progress(
    app: &AppHandle,
    test_id: &str,
    server_name: Option<&str>,
    phase: &'static str,
    transferred_bytes: u64,
    total_bytes: u64,
    bytes_per_sec: Option<f64>,
) {
    let _ = app.emit(
        EVENT_NAME,
        SpeedTestProgress {
            test_id: test_id.to_string(),
            server_name: server_name.map(|s| s.to_string()),
            phase,
            transferred_bytes,
            total_bytes,
            bytes_per_sec,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_path_handles_root() {
        assert_eq!(
            join_remote_path("/", ".aeroftp-speedtest-x.bin"),
            "/.aeroftp-speedtest-x.bin"
        );
    }

    #[test]
    fn join_remote_path_trims_trailing_slash() {
        assert_eq!(join_remote_path("/tmp/", "x.bin"), "/tmp/x.bin");
    }

    #[test]
    fn bytes_per_sec_uses_duration_ms() {
        assert_eq!(bytes_per_sec(10 * ONE_MB, 1000), 10.0 * ONE_MB as f64);
    }

    #[test]
    fn standard_sizes_accepted_without_expert_flag() {
        assert!(validate_size(ONE_MB, false).is_ok());
        assert!(validate_size(10 * ONE_MB, false).is_ok());
        assert!(validate_size(100 * ONE_MB, false).is_ok());
    }

    #[test]
    fn non_standard_below_threshold_rejected() {
        // 2 MB is between standards, must be rejected even with expert flag
        // because the threshold gate only applies above 100 MB.
        assert!(validate_size(2 * ONE_MB, true).is_err());
        assert!(validate_size(50 * ONE_MB, true).is_err());
    }

    #[test]
    fn expert_size_requires_confirmation() {
        let size = 250 * ONE_MB;
        assert!(validate_size(size, false).is_err());
        assert!(validate_size(size, true).is_ok());
    }

    #[test]
    fn over_max_rejected() {
        assert!(validate_size(ONE_GB + 1, true).is_err());
        assert!(validate_size(ONE_GB, true).is_ok());
    }

    #[test]
    fn zero_size_rejected() {
        assert!(validate_size(0, true).is_err());
        assert!(validate_size(0, false).is_err());
    }

    #[test]
    fn temp_filename_contract() {
        let name = format!(".aeroftp-speedtest-{}.bin", Uuid::new_v4());
        assert!(name.starts_with(".aeroftp-speedtest-"));
        assert!(name.ends_with(".bin"));
    }

    #[test]
    fn compare_max_tests_constant() {
        // Audit P1-7: enforce a hard cap on compare-mode test count to prevent
        // accidental expensive runs. Constant must remain <= UI Select-all (8).
        assert_eq!(COMPARE_MAX_TESTS, 8);
    }

    #[test]
    fn supported_protocols() {
        for p in ["ftp", "ftps", "sftp", "s3", "webdav", "FTP", "S3"] {
            assert!(validate_supported_protocol(p).is_ok(), "{} should be supported", p);
        }
        for p in ["dropbox", "github", "mega", "azure"] {
            assert!(validate_supported_protocol(p).is_err(), "{} should be rejected", p);
        }
    }
}

// ---------------------------------------------------------------------------
// Score normalization (compare ranking)
// ---------------------------------------------------------------------------

/// Compute a normalized score for a single result against the maxima of the
/// compare run. Inputs > 0; max values default to 1.0 to avoid division-by-zero.
///
/// Score weighting: 0.45 download, 0.35 upload, 0.10 integrity, 0.10 cleanup.
///
/// Tri-state integrity contribution (per senior audit 2026-04-26):
///   integrity_checked = false → 0.5 (neutral; not verified, not penalized)
///   integrity_checked = true, integrity_verified = true  → 1.0
///   integrity_checked = true, integrity_verified = false → 0.0 (corrupted)
#[allow(dead_code)]
pub fn compute_score(
    download_mbps: f64,
    upload_mbps: f64,
    max_download: f64,
    max_upload: f64,
    integrity_checked: bool,
    integrity_verified: bool,
    cleanup_ok: bool,
) -> f64 {
    let nd = if max_download > 0.0 {
        (download_mbps / max_download).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let nu = if max_upload > 0.0 {
        (upload_mbps / max_upload).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let ni = if !integrity_checked {
        0.5
    } else if integrity_verified {
        1.0
    } else {
        0.0
    };
    let nc = if cleanup_ok { 1.0 } else { 0.0 };
    0.45 * nd + 0.35 * nu + 0.10 * ni + 0.10 * nc
}

#[cfg(test)]
mod score_tests {
    use super::*;

    #[test]
    fn perfect_run_scores_one() {
        assert!((compute_score(100.0, 50.0, 100.0, 50.0, true, true, true) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cleanup_failure_costs_ten_percent() {
        let s = compute_score(100.0, 50.0, 100.0, 50.0, true, true, false);
        assert!((s - 0.90).abs() < 1e-9);
    }

    #[test]
    fn integrity_corruption_costs_ten_percent() {
        let s = compute_score(100.0, 50.0, 100.0, 50.0, true, false, true);
        assert!((s - 0.90).abs() < 1e-9);
    }

    #[test]
    fn integrity_skipped_is_neutral_half_credit() {
        // Skipped run gets 0.5 of the integrity weight -> total = 0.45+0.35+0.05+0.10 = 0.95
        let s = compute_score(100.0, 50.0, 100.0, 50.0, false, false, true);
        assert!((s - 0.95).abs() < 1e-9);
    }

    #[test]
    fn integrity_skipped_never_outscores_verified() {
        let verified = compute_score(100.0, 50.0, 100.0, 50.0, true, true, true);
        let skipped = compute_score(100.0, 50.0, 100.0, 50.0, false, false, true);
        assert!(skipped < verified);
    }

    #[test]
    fn download_dominates_upload() {
        let fast_dl = compute_score(100.0, 0.0, 100.0, 100.0, true, true, true);
        let fast_ul = compute_score(0.0, 100.0, 100.0, 100.0, true, true, true);
        assert!(fast_dl > fast_ul);
    }

    #[test]
    fn zero_max_values_yield_zero_throughput_component() {
        // Only integrity + cleanup contribute (0.20 total)
        let s = compute_score(0.0, 0.0, 0.0, 0.0, true, true, true);
        assert!((s - 0.20).abs() < 1e-9);
    }
}

// ---------------------------------------------------------------------------
// History persistence (SQLite)
// ---------------------------------------------------------------------------

use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use tauri::Manager;

const HISTORY_RETENTION: i64 = 1000;

pub struct SpeedTestHistoryDb(pub StdMutex<Connection>);

fn history_acquire(db: &SpeedTestHistoryDb) -> std::sync::MutexGuard<'_, Connection> {
    db.0.lock().unwrap_or_else(|e| {
        log::warn!("Speed test history DB mutex was poisoned, recovering: {e}");
        e.into_inner()
    })
}

fn history_db_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|_| "Cannot resolve app config dir".to_string())?;
    Ok(dir.join("speedtest_history.db"))
}

pub fn init_history_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|e| format!("Pragma error: {e}"))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS speedtest_results (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            server_id TEXT,
            server_name TEXT,
            host_hash TEXT,
            protocol TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            upload_bps REAL NOT NULL,
            download_bps REAL NOT NULL,
            upload_ms INTEGER NOT NULL,
            download_ms INTEGER NOT NULL,
            integrity_verified INTEGER NOT NULL,
            cleanup_ok INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
         );

         CREATE INDEX IF NOT EXISTS idx_st_server_created ON speedtest_results(server_id, created_at DESC);
         CREATE INDEX IF NOT EXISTS idx_st_created ON speedtest_results(created_at DESC);",
    )
    .map_err(|e| format!("Schema error: {e}"))?;

    Ok(())
}

pub fn init_history_db(app: &AppHandle) -> Result<Connection, String> {
    let path = history_db_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let conn = Connection::open(&path)
        .map_err(|_| "Failed to initialize speed test history database".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    init_history_schema(&conn)?;
    Ok(conn)
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpeedTestHistoryRecordRequest {
    pub server_id: Option<String>,
    pub server_name: Option<String>,
    pub host_hash: Option<String>,
    pub protocol: String,
    pub size_bytes: u64,
    pub upload_bytes_per_sec: f64,
    pub download_bytes_per_sec: f64,
    pub upload_duration_ms: u64,
    pub download_duration_ms: u64,
    pub integrity_verified: bool,
    pub cleanup_ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestHistoryEntry {
    pub id: i64,
    pub server_id: Option<String>,
    pub server_name: Option<String>,
    pub host_hash: Option<String>,
    pub protocol: String,
    pub size_bytes: u64,
    pub upload_bytes_per_sec: f64,
    pub download_bytes_per_sec: f64,
    pub upload_duration_ms: u64,
    pub download_duration_ms: u64,
    pub integrity_verified: bool,
    pub cleanup_ok: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestHistorySummary {
    pub server_id: Option<String>,
    pub samples: u32,
    pub last: Option<SpeedTestHistoryEntry>,
    pub best_download: Option<SpeedTestHistoryEntry>,
    pub best_upload: Option<SpeedTestHistoryEntry>,
    pub median_download_bps: Option<f64>,
    pub median_upload_bps: Option<f64>,
    /// True if `last` is more than 30% slower than the median for download.
    pub regression_warning: bool,
}

#[tauri::command]
pub async fn speedtest_history_record(
    db: State<'_, SpeedTestHistoryDb>,
    record: SpeedTestHistoryRecordRequest,
) -> Result<i64, String> {
    let conn = history_acquire(&db);
    // Defense in depth (audit P1-11): never persist a user-supplied display name.
    // The column survives for schema-stability with older DB files; new rows
    // store NULL regardless of what the caller sends. Display names are
    // resolved live from the profile list at render time.
    let _suppressed_name = record.server_name; // intentionally dropped
    conn.execute(
        "INSERT INTO speedtest_results
         (server_id, server_name, host_hash, protocol, size_bytes,
          upload_bps, download_bps, upload_ms, download_ms,
          integrity_verified, cleanup_ok)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            record.server_id,
            None::<String>,
            record.host_hash,
            record.protocol,
            record.size_bytes as i64,
            record.upload_bytes_per_sec,
            record.download_bytes_per_sec,
            record.upload_duration_ms as i64,
            record.download_duration_ms as i64,
            record.integrity_verified as i32,
            record.cleanup_ok as i32,
        ],
    )
    .map_err(|e| format!("Insert: {e}"))?;
    let id = conn.last_insert_rowid();
    // Trim oldest rows past the retention cap.
    let _ = conn.execute(
        "DELETE FROM speedtest_results
         WHERE id IN (
             SELECT id FROM speedtest_results
             ORDER BY created_at DESC
             LIMIT -1 OFFSET ?1
         )",
        params![HISTORY_RETENTION],
    );
    Ok(id)
}

#[tauri::command]
pub async fn speedtest_history_list(
    db: State<'_, SpeedTestHistoryDb>,
    server_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<SpeedTestHistoryEntry>, String> {
    let conn = history_acquire(&db);
    let limit = limit.unwrap_or(50).min(500) as i64;

    let row_to_entry = |row: &rusqlite::Row| -> rusqlite::Result<SpeedTestHistoryEntry> {
        Ok(SpeedTestHistoryEntry {
            id: row.get(0)?,
            server_id: row.get(1)?,
            server_name: row.get(2)?,
            host_hash: row.get(3)?,
            protocol: row.get(4)?,
            size_bytes: row.get::<_, i64>(5)? as u64,
            upload_bytes_per_sec: row.get(6)?,
            download_bytes_per_sec: row.get(7)?,
            upload_duration_ms: row.get::<_, i64>(8)? as u64,
            download_duration_ms: row.get::<_, i64>(9)? as u64,
            integrity_verified: row.get::<_, i32>(10)? != 0,
            cleanup_ok: row.get::<_, i32>(11)? != 0,
            created_at: row.get(12)?,
        })
    };

    /// Collect rows, logging (not silently swallowing) any decode error so a
    /// schema drift or DB-level corruption is observable in the logs.
    fn collect_logged<I, T>(rows: I) -> Vec<T>
    where
        I: IntoIterator<Item = rusqlite::Result<T>>,
    {
        let mut out = Vec::new();
        for r in rows {
            match r {
                Ok(v) => out.push(v),
                Err(e) => log::warn!("speedtest_history_list: row decode failed: {e}"),
            }
        }
        out
    }

    let entries: Vec<SpeedTestHistoryEntry> = if let Some(sid) = server_id {
        let mut stmt = conn
            .prepare(
                "SELECT id, server_id, server_name, host_hash, protocol, size_bytes,
                        upload_bps, download_bps, upload_ms, download_ms,
                        integrity_verified, cleanup_ok, created_at
                 FROM speedtest_results
                 WHERE server_id = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| format!("Prepare: {e}"))?;
        let rows = stmt
            .query_map(params![sid, limit], row_to_entry)
            .map_err(|e| format!("Query: {e}"))?;
        collect_logged(rows)
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT id, server_id, server_name, host_hash, protocol, size_bytes,
                        upload_bps, download_bps, upload_ms, download_ms,
                        integrity_verified, cleanup_ok, created_at
                 FROM speedtest_results
                 ORDER BY created_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Prepare: {e}"))?;
        let rows = stmt
            .query_map(params![limit], row_to_entry)
            .map_err(|e| format!("Query: {e}"))?;
        collect_logged(rows)
    };

    Ok(entries)
}

#[tauri::command]
pub async fn speedtest_history_summary(
    db: State<'_, SpeedTestHistoryDb>,
    server_id: String,
) -> Result<SpeedTestHistorySummary, String> {
    // Pull recent rows, compute medians and best in Rust.
    let entries = speedtest_history_list(db, Some(server_id.clone()), Some(100)).await?;
    if entries.is_empty() {
        return Ok(SpeedTestHistorySummary {
            server_id: Some(server_id),
            samples: 0,
            last: None,
            best_download: None,
            best_upload: None,
            median_download_bps: None,
            median_upload_bps: None,
            regression_warning: false,
        });
    }
    let last = entries.first().cloned();
    let best_download = entries
        .iter()
        .max_by(|a, b| {
            a.download_bytes_per_sec
                .partial_cmp(&b.download_bytes_per_sec)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned();
    let best_upload = entries
        .iter()
        .max_by(|a, b| {
            a.upload_bytes_per_sec
                .partial_cmp(&b.upload_bytes_per_sec)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned();
    let median_dl = median_f64(entries.iter().map(|e| e.download_bytes_per_sec).collect());
    let median_ul = median_f64(entries.iter().map(|e| e.upload_bytes_per_sec).collect());
    let regression_warning = match (last.as_ref(), median_dl) {
        (Some(l), Some(m)) if m > 0.0 => l.download_bytes_per_sec < m * 0.7,
        _ => false,
    };
    Ok(SpeedTestHistorySummary {
        server_id: Some(server_id),
        samples: entries.len() as u32,
        last,
        best_download,
        best_upload,
        median_download_bps: median_dl,
        median_upload_bps: median_ul,
        regression_warning,
    })
}

#[tauri::command]
pub async fn speedtest_history_clear(
    db: State<'_, SpeedTestHistoryDb>,
    server_id: Option<String>,
) -> Result<u32, String> {
    let conn = history_acquire(&db);
    let deleted = if let Some(sid) = server_id {
        conn.execute(
            "DELETE FROM speedtest_results WHERE server_id = ?1",
            params![sid],
        )
        .map_err(|e| format!("Delete: {e}"))?
    } else {
        conn.execute("DELETE FROM speedtest_results", [])
            .map_err(|e| format!("Delete: {e}"))?
    };
    Ok(deleted as u32)
}

fn median_f64(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn median_handles_empty() {
        assert!(median_f64(vec![]).is_none());
    }

    #[test]
    fn median_odd_length() {
        assert_eq!(median_f64(vec![3.0, 1.0, 2.0]), Some(2.0));
    }

    #[test]
    fn median_even_length() {
        assert_eq!(median_f64(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }

    #[test]
    fn schema_initializes() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(init_history_schema(&conn).is_ok());
        // Insert + retrieve roundtrip
        conn.execute(
            "INSERT INTO speedtest_results
             (server_id, server_name, host_hash, protocol, size_bytes,
              upload_bps, download_bps, upload_ms, download_ms,
              integrity_verified, cleanup_ok)
             VALUES ('s1','MyServer','abc','ftp',1048576,100.0,200.0,1000,500,1,1)",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM speedtest_results", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
