// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Professional server speed test.
//!
//! Runs an isolated, temporary-provider upload/download/delete cycle against a
//! saved server profile. This deliberately bypasses the active file-browser
//! session so benchmarking never steals or mutates the user's current provider.

use crate::provider_commands::ProviderConnectionParams;
use crate::providers::{ProviderFactory, StorageProvider};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};
use tempfile::NamedTempFile;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

const ONE_MB: u64 = 1024 * 1024;
const ALLOWED_SIZES: [u64; 3] = [ONE_MB, 10 * ONE_MB, 100 * ONE_MB];
const EVENT_NAME: &str = "speedtest-progress";

pub struct SpeedTestState {
    running: Mutex<bool>,
    cancel_token: Mutex<Option<CancellationToken>>,
}

impl SpeedTestState {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(false),
            cancel_token: Mutex::new(None),
        }
    }
}

impl Default for SpeedTestState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpeedTestRunRequest {
    pub connection: ProviderConnectionParams,
    pub size_bytes: u64,
    pub remote_dir: String,
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestResult {
    pub server_name: Option<String>,
    pub protocol: String,
    pub remote_path: String,
    pub temp_file_name: String,
    pub size_bytes: u64,
    pub upload_duration_ms: u64,
    pub download_duration_ms: u64,
    pub upload_bytes_per_sec: f64,
    pub download_bytes_per_sec: f64,
    pub upload_mbps: f64,
    pub download_mbps: f64,
    pub integrity_verified: bool,
    pub upload_sha256: String,
    pub download_sha256: String,
    pub temp_file_cleaned: bool,
    pub cleanup_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestProgress {
    pub phase: &'static str,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: Option<f64>,
}

#[tauri::command]
pub async fn speedtest_cancel(state: State<'_, SpeedTestState>) -> Result<(), String> {
    let token = state.cancel_token.lock().await.clone();
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
        let mut running = state.running.lock().await;
        if *running {
            return Err("A speed test is already running".to_string());
        }
        *running = true;
    }

    let token = CancellationToken::new();
    *state.cancel_token.lock().await = Some(token.clone());

    let result = run_speedtest_inner(app, request, token).await;

    *state.cancel_token.lock().await = None;
    *state.running.lock().await = false;

    result
}

async fn run_speedtest_inner(
    app: AppHandle,
    request: SpeedTestRunRequest,
    token: CancellationToken,
) -> Result<SpeedTestResult, String> {
    validate_size(request.size_bytes)?;
    validate_supported_protocol(&request.connection.protocol)?;

    emit_progress(&app, "connecting", 0, request.size_bytes, None);

    let mut config = request.connection.to_provider_config()?;
    let protocol = config.provider_type.to_string();
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    config.zeroize_password();

    run_cancelable(token.clone(), provider.connect(), "Test cancelled while connecting").await?;

    let temp_file_name = format!(".aeroftp-speedtest-{}.bin", Uuid::new_v4());
    let remote_path = join_remote_path(&request.remote_dir, &temp_file_name);
    let (local_tmp, upload_sha256) = create_random_temp_file(request.size_bytes)?;

    info!(
        "speedtest: {} {} bytes -> {}",
        protocol, request.size_bytes, remote_path
    );

    let upload_started_at = Instant::now();
    let upload_result = {
        let phase_started_at = Instant::now();
        let app_for_progress = app.clone();
        let token_for_progress = token.clone();
        let total = request.size_bytes;
        let progress = Box::new(move |transferred: u64, total_bytes: u64| {
            let elapsed = phase_started_at.elapsed().as_secs_f64().max(0.001);
            let bps = transferred as f64 / elapsed;
            emit_progress(
                &app_for_progress,
                "uploading",
                transferred,
                if total_bytes > 0 { total_bytes } else { total },
                Some(bps),
            );
            if token_for_progress.is_cancelled() {
                warn!("speedtest: cancellation requested during upload");
            }
        });
        emit_progress(&app, "uploading", 0, request.size_bytes, None);
        run_cancelable(
            token.clone(),
            provider.upload(
                local_tmp.path().to_string_lossy().as_ref(),
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
        let (cleaned, cleanup_error) = cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist).await;
        let _ = provider.disconnect().await;
        let suffix = cleanup_error
            .as_ref()
            .map(|e| format!(" Cleanup failed: {}", e))
            .unwrap_or_default();
        if cleaned {
            return Err(err);
        }
        return Err(format!("{}{}", err, suffix));
    }

    if token.is_cancelled() {
        let _ = cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist).await;
        let _ = provider.disconnect().await;
        return Err("Test cancelled".to_string());
    }

    let download_started_at = Instant::now();
    emit_progress(&app, "downloading", 0, request.size_bytes, None);
    let downloaded = run_cancelable(
        token.clone(),
        provider.download_to_bytes(&remote_path),
        "Test cancelled during download",
    )
    .await;
    let download_duration_ms = elapsed_ms(download_started_at);

    let downloaded = match downloaded {
        Ok(bytes) => bytes,
        Err(err) => {
            let (cleaned, cleanup_error) =
                cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist).await;
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
    };

    emit_progress(
        &app,
        "downloading",
        downloaded.len() as u64,
        request.size_bytes,
        Some(bytes_per_sec(request.size_bytes, download_duration_ms)),
    );

    let download_sha256 = sha256_hex(&downloaded);
    let integrity_verified = upload_sha256 == download_sha256;

    let (temp_file_cleaned, cleanup_error) =
        cleanup_remote(&app, provider.as_mut(), &remote_path, remote_may_exist).await;
    let _ = provider.disconnect().await;

    emit_progress(&app, "done", request.size_bytes, request.size_bytes, None);

    let upload_bytes_per_sec = bytes_per_sec(request.size_bytes, upload_duration_ms);
    let download_bytes_per_sec = bytes_per_sec(request.size_bytes, download_duration_ms);

    Ok(SpeedTestResult {
        server_name: request.server_name,
        protocol,
        remote_path,
        temp_file_name,
        size_bytes: request.size_bytes,
        upload_duration_ms,
        download_duration_ms,
        upload_bytes_per_sec,
        download_bytes_per_sec,
        upload_mbps: upload_bytes_per_sec * 8.0 / 1_000_000.0,
        download_mbps: download_bytes_per_sec * 8.0 / 1_000_000.0,
        integrity_verified,
        upload_sha256,
        download_sha256,
        temp_file_cleaned,
        cleanup_error,
    })
}

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
) -> (bool, Option<String>) {
    if !should_try {
        return (true, None);
    }
    emit_progress(app, "cleaning_up", 0, 0, None);
    match provider.delete(remote_path).await {
        Ok(()) => (true, None),
        Err(err) => {
            let msg = err.to_string();
            warn!("speedtest: cleanup failed for {}: {}", remote_path, msg);
            (false, Some(msg))
        }
    }
}

fn validate_size(size_bytes: u64) -> Result<(), String> {
    if ALLOWED_SIZES.contains(&size_bytes) {
        Ok(())
    } else {
        Err("Unsupported speed test size".to_string())
    }
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().max(1) as u64
}

fn bytes_per_sec(bytes: u64, duration_ms: u64) -> f64 {
    bytes as f64 * 1000.0 / duration_ms.max(1) as f64
}

fn emit_progress(
    app: &AppHandle,
    phase: &'static str,
    transferred_bytes: u64,
    total_bytes: u64,
    bytes_per_sec: Option<f64>,
) {
    let _ = app.emit(
        EVENT_NAME,
        SpeedTestProgress {
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
        assert_eq!(join_remote_path("/", ".aeroftp-speedtest-x.bin"), "/.aeroftp-speedtest-x.bin");
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
    fn allowed_sizes_rejects_unknown_size() {
        assert!(validate_size(10 * ONE_MB).is_ok());
        assert!(validate_size(2 * ONE_MB).is_err());
    }

    #[test]
    fn sha256_detects_corruption() {
        assert_ne!(sha256_hex(b"abc"), sha256_hex(b"abd"));
    }

    #[test]
    fn temp_filename_contract() {
        let name = format!(".aeroftp-speedtest-{}.bin", Uuid::new_v4());
        assert!(name.starts_with(".aeroftp-speedtest-"));
        assert!(name.ends_with(".bin"));
    }
}
