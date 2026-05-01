// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Provider health check — progressive scan with Tauri events.
//!
//! Checks provider endpoints in parallel waves (5 concurrent),
//! emitting a Tauri event per result so the UI updates progressively.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Input: list of providers to check.
/// `protocol` and `port` are optional — when provided, FTP/FTPS/SFTP probes
/// switch to a TCP-connect strategy instead of broken HTTPS HEAD.
#[derive(Debug, Deserialize)]
pub struct HealthCheckTarget {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

/// Event payload emitted per provider
#[derive(Debug, Clone, Serialize)]
struct HealthScanEvent {
    id: String,
    status: String,
    latency_ms: u64,
    scan_id: String,
}

/// Maximum concurrent checks. Bumped from 5 to 10 to keep up with users
/// who have 50+ saved servers — the previous cap meant the safety timeout
/// would cancel half the wave as stale before they ever ran.
const MAX_CONCURRENT: usize = 10;
/// Delay between waves to spread the load (kept for the lazy-load wave effect).
const WAVE_DELAY_MS: u64 = 120;
/// Connection timeout for the TCP probe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Threshold for "slow" status
const SLOW_THRESHOLD_MS: u64 = 2000;

/// Default port per protocol when the saved profile didn't pin one.
fn default_port(proto: &str) -> u16 {
    match proto {
        "ftp" => 21,
        "ftps" => 990,
        "sftp" => 22,
        "webdav" => 443,
        "s3" => 443,
        _ => 443,
    }
}

/// TCP-connect probe — used for FTP/FTPS/SFTP where HEAD over HTTPS is
/// nonsense (port 21/22 doesn't speak HTTP and times out as "slow"/"down").
async fn check_tcp(host: &str, port: u16) -> (&'static str, u64) {
    let start = Instant::now();
    let addr = format!("{}:{}", host, port);
    let result = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr)).await;
    let elapsed = start.elapsed().as_millis() as u64;
    match result {
        Ok(Ok(_)) => {
            if elapsed > SLOW_THRESHOLD_MS {
                ("slow", elapsed)
            } else {
                ("up", elapsed)
            }
        }
        Ok(Err(_)) => ("down", elapsed),
        Err(_) => ("slow", elapsed), // connect timeout
    }
}

/// Extract host and optional port from a URL or host string.
/// Handles saved WebDAV/S3 values like `https://host:8443/path` and avoids
/// the frontend's historical `split(':')[0]` trap that turned URLs into
/// the literal hostname `https`.
fn host_port_from_input(input: &str) -> Option<(String, Option<u16>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .rsplit('@')
        .next()
        .unwrap_or(without_scheme);
    if authority.is_empty() {
        return None;
    }

    if let Some(rest) = authority.strip_prefix('[') {
        let host = rest.split(']').next()?;
        let port = rest.split("]:").nth(1).and_then(|p| p.parse::<u16>().ok());
        return Some((host.to_string(), port));
    }

    let mut host = authority;
    let mut port = None;
    if let Some((h, p)) = authority.rsplit_once(':') {
        if let Ok(parsed) = p.parse::<u16>() {
            host = h;
            port = Some(parsed);
        }
    }

    if host.is_empty() {
        None
    } else {
        Some((host.to_string(), port))
    }
}

/// Dispatch the right probe. We unified on TCP connect for every protocol —
/// HEAD over HTTPS was unreliable for WebDAV roots (401/405/redirects) and
/// for S3 endpoints (some flavors return 403 only after path-style routing),
/// and pointless for FTP/SFTP. TCP reachability is the common signal we
/// actually care about for the radial; the modal still does the deeper
/// DNS/TLS/HTTP breakdown for users who click "Health Check".
async fn check_one(target: &HealthCheckTarget) -> (&'static str, u64) {
    let proto = target.protocol.as_deref().unwrap_or("");
    let parsed = target
        .host
        .as_deref()
        .and_then(host_port_from_input)
        .or_else(|| host_port_from_input(&target.url));
    let (host, parsed_port) = match parsed {
        Some(v) => v,
        None => return ("down", 0),
    };
    let port = target
        .port
        .filter(|p| *p > 0)
        .or(parsed_port)
        .unwrap_or_else(|| default_port(proto));
    check_tcp(&host, port).await
}

/// Start a progressive health scan.
/// Checks providers in parallel waves, emitting `health-scan-result` events.
#[tauri::command]
pub async fn start_health_scan(
    app: AppHandle,
    targets: Vec<HealthCheckTarget>,
    scan_id: Option<String>,
) -> Result<(), String> {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let app = Arc::new(app);
    let scan_id = scan_id.unwrap_or_else(|| "legacy".to_string());

    // Process in waves: spawn tasks with semaphore, add delay between groups
    let mut handles = Vec::new();
    for (i, target) in targets.into_iter().enumerate() {
        let sem = semaphore.clone();
        let app_clone = app.clone();
        let scan_id = scan_id.clone();

        // Small stagger between spawns for the "lazy load" effect
        if i > 0 && i % MAX_CONCURRENT == 0 {
            tokio::time::sleep(Duration::from_millis(WAVE_DELAY_MS)).await;
        }

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            let (status, latency_ms) = check_one(&target).await;

            // Emit event to frontend
            let _ = app_clone.emit(
                "health-scan-result",
                HealthScanEvent {
                    id: target.id,
                    status: status.to_string(),
                    latency_ms,
                    scan_id,
                },
            );
        });
        handles.push(handle);
    }

    // Wait for all checks to complete
    for handle in handles {
        let _ = handle.await;
    }

    // Emit scan-complete event
    let _ = app.emit(
        "health-scan-complete",
        serde_json::json!({ "scan_id": scan_id }),
    );

    Ok(())
}
