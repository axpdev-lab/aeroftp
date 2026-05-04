// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! InfiniCloud REST API v2 ("Muramasa") client.
//!
//! Provides auto-discovery of the user's node server and quota queries.
//! All file operations go through the standard WebDAV provider: this module
//! only handles the two BASIC-auth REST endpoints:
//!   - `GET /v2/api/ba/user`           (API server: node discovery)
//!   - `GET /v2/api/ba/dataset/(capacity)` (Node server: quota)

use base64::Engine;
use log::info;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.infini-cloud.net/v2/api";

// ── Response types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub result: i32,
    pub user: String,
    #[serde(default)]
    pub introduce_code: String,
    pub node: String,
    pub webdav_url: String,
    /// Contract capacity in GB
    pub capacity: u64,
    #[serde(default)]
    pub api_key: Option<ApiKeyInfo>,
    #[serde(default)]
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub activated_time: String,
    #[serde(default)]
    pub bonus: Option<BonusInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BonusInfo {
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub until: String,
    /// Bonus capacity in GB
    #[serde(default)]
    pub increase_capacity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetCapacity {
    pub result: i32,
    pub dataset: std::collections::HashMap<String, DatasetInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInfo {
    /// Total quota in bytes
    pub quota: u64,
    /// Used capacity in bytes
    pub used: u64,
}

/// Flattened quota response for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaInfo {
    /// Total quota in bytes
    pub total: u64,
    /// Used capacity in bytes
    pub used: u64,
    /// Available capacity in bytes
    pub available: u64,
}

// ── Core API functions ───────────────────────────────────────────────

fn build_basic_header(username: &str, password: &SecretString) -> String {
    let credentials = format!("{}:{}", username, password.expose_secret());
    let encoded = base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes());
    format!("Basic {}", encoded)
}

/// Discover the user's node server, WebDAV URL, and capacity.
///
/// Calls `GET /v2/api/ba/user` on the API server.
/// This also updates the user's "last login" timestamp, preventing
/// account deletion due to inactivity.
async fn discover(
    username: &str,
    password: &SecretString,
    api_key: &str,
) -> Result<UserInfo, String> {
    let client = Client::builder()
        .user_agent(crate::providers::AEROFTP_USER_AGENT)
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;
    let auth = build_basic_header(username, password);

    let resp = client
        .get(format!("{}/ba/user", API_BASE))
        .header("Authorization", &auth)
        .header("X-TeraCLOUD-API-KEY", api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("InfiniCloud API request failed: {}", e))?;

    match resp.status().as_u16() {
        200 => {
            let info: UserInfo = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse InfiniCloud response: {}", e))?;
            if info.result != 0 {
                return Err(format!(
                    "InfiniCloud API returned error code {}",
                    info.result
                ));
            }
            info!(
                "InfiniCloud discovery OK: node={}, capacity={}GB",
                info.node, info.capacity
            );
            Ok(info)
        }
        202 => Err(
            "Your InfiniCloud account was created recently and the node server is not ready yet. \
             Please try again in a few minutes."
                .into(),
        ),
        401 => Err(
            "Invalid credentials. Please check your User ID and Apps Password \
             (My Page → Apps Connection)."
                .into(),
        ),
        403 => Err("Invalid API Key. Please verify your developer API key.".into()),
        code => {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("InfiniCloud API returned HTTP {}: {}", code, body))
        }
    }
}

/// Get dataset capacity (quota + used) from the user's node server.
///
/// Calls `GET /v2/api/ba/dataset/(capacity)` on the node server.
async fn get_quota(
    node: &str,
    username: &str,
    password: &SecretString,
    api_key: &str,
) -> Result<QuotaInfo, String> {
    let client = Client::builder()
        .user_agent(crate::providers::AEROFTP_USER_AGENT)
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;
    let auth = build_basic_header(username, password);

    let url = format!("https://{}/v2/api/ba/dataset/(capacity)", node);
    let resp = client
        .get(&url)
        .header("Authorization", &auth)
        .header("X-TeraCLOUD-API-KEY", api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("InfiniCloud quota request failed: {}", e))?;

    match resp.status().as_u16() {
        200 => {
            let data: DatasetCapacity = resp
                .json()
                .await
                .map_err(|e| format!("Failed to parse quota response: {}", e))?;
            if data.result != 0 {
                return Err(format!("Quota API returned error code {}", data.result));
            }
            // The root dataset is keyed as "__ROOT__"
            let root = data
                .dataset
                .get("__ROOT__")
                .ok_or("No __ROOT__ dataset in quota response")?;
            Ok(QuotaInfo {
                total: root.quota,
                used: root.used,
                available: root.quota.saturating_sub(root.used),
            })
        }
        404 => Err("Dataset not found on node server.".into()),
        code => {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("Quota API returned HTTP {}: {}", code, body))
        }
    }
}

// ── Tauri commands ───────────────────────────────────────────────────

/// Discover InfiniCloud node server and WebDAV URL for the given user.
#[tauri::command]
pub async fn infinicloud_discover(
    username: String,
    password: String,
    api_key: String,
) -> Result<UserInfo, String> {
    let secret = SecretString::from(password);
    let api_key = api_key.trim().to_string();
    info!(
        "InfiniCloud discover: user={}, api_key_len={}",
        username,
        api_key.len()
    );
    discover(&username, &secret, &api_key).await
}

/// Get InfiniCloud storage quota (total/used/available in bytes).
#[tauri::command]
pub async fn infinicloud_quota(
    node: String,
    username: String,
    password: String,
    api_key: String,
) -> Result<QuotaInfo, String> {
    let secret = SecretString::from(password);
    get_quota(&node, &username, &secret, &api_key).await
}
