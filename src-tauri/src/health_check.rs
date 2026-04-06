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
use tokio::sync::Semaphore;

/// Input: list of providers to check
#[derive(Debug, Deserialize)]
pub struct HealthCheckTarget {
    pub id: String,
    pub url: String,
}

/// Event payload emitted per provider
#[derive(Debug, Clone, Serialize)]
struct HealthScanEvent {
    id: String,
    status: String,
    latency_ms: u64,
}

/// Maximum concurrent checks
const MAX_CONCURRENT: usize = 5;
/// Delay between waves to spread the load
const WAVE_DELAY_MS: u64 = 200;
/// Connection timeout
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Total request timeout
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Threshold for "slow" status
const SLOW_THRESHOLD_MS: u64 = 2000;

/// Check a single provider endpoint via HEAD request.
/// Any HTTP response (even 4xx/5xx) means the server is reachable.
async fn check_one(url: &str) -> (&'static str, u64) {
    let client = match reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(_) => return ("down", 0),
    };

    let start = Instant::now();
    let result = client.head(url).send().await;
    let elapsed = start.elapsed().as_millis() as u64;

    match result {
        Ok(_) => {
            if elapsed > SLOW_THRESHOLD_MS {
                ("slow", elapsed)
            } else {
                ("up", elapsed)
            }
        }
        Err(e) => {
            if e.is_timeout() {
                ("slow", elapsed)
            } else {
                ("down", elapsed)
            }
        }
    }
}

/// Start a progressive health scan.
/// Checks providers in parallel waves, emitting `health-scan-result` events.
#[tauri::command]
pub async fn start_health_scan(
    app: AppHandle,
    targets: Vec<HealthCheckTarget>,
) -> Result<(), String> {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let app = Arc::new(app);

    // Process in waves: spawn tasks with semaphore, add delay between groups
    let mut handles = Vec::new();
    for (i, target) in targets.into_iter().enumerate() {
        let sem = semaphore.clone();
        let app_clone = app.clone();

        // Small stagger between spawns for the "lazy load" effect
        if i > 0 && i % MAX_CONCURRENT == 0 {
            tokio::time::sleep(Duration::from_millis(WAVE_DELAY_MS)).await;
        }

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            let (status, latency_ms) = check_one(&target.url).await;

            // Emit event to frontend
            let _ = app_clone.emit(
                "health-scan-result",
                HealthScanEvent {
                    id: target.id,
                    status: status.to_string(),
                    latency_ms,
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
    let _ = app.emit("health-scan-complete", ());

    Ok(())
}
