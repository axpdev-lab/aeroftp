//! Drime Cloud Storage Provider (Bedrive/BeDrive platform)
//!
//! Implements StorageProvider for Drime Cloud using the Bedrive REST API.
//! Uses API Token (Bearer) for authentication — no OAuth2 flow needed.
//!
//! API Base: https://app.drime.cloud/api/v1
//! Auth: Authorization: Bearer {token}
//! IDs: Numeric (but stored as String internally)
//! Pagination: page-based (page + perPage)
//! File entries use hash-based download URLs

use async_trait::async_trait;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
    sanitize_api_error, DrimeCloudConfig,
};

const API_BASE: &str = "https://app.drime.cloud/api/v1";

fn drime_log(msg: &str) {
    info!("[DRIME] {}", msg);
}

// ─── API Response Types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeFile {
    id: Option<serde_json::Value>, // numeric or string
    name: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>, // "file" or "folder"
    #[serde(alias = "file_size")]
    size: Option<u64>,
    #[serde(alias = "updated_at", alias = "modified_at")]
    updated_at: Option<String>, // ISO 8601 or timestamp
    #[serde(default)]
    mime_type: Option<String>,
    /// Encrypted hash for download URLs (Bedrive-specific)
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    parent_id: Option<serde_json::Value>,
}

impl DrimeFile {
    /// Extract ID as string regardless of JSON type (number or string)
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct DrimeListResponse {
    data: Option<Vec<DrimeFile>>,
    #[allow(dead_code)]
    #[serde(default)]
    current_page: Option<u32>,
    #[serde(default)]
    last_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DrimeStorageResponse {
    /// Bedrive returns "used" in bytes
    #[serde(alias = "usedSpace")]
    used: Option<u64>,
    /// Bedrive returns "total" or "availableSpace" in bytes
    #[serde(alias = "availableSpace")]
    total: Option<u64>,
    #[allow(dead_code)]
    percentage: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DrimeFolderResponse {
    id: Option<serde_json::Value>,
    #[allow(dead_code)]
    name: Option<String>,
}

impl DrimeFolderResponse {
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct DrimeFileResponse {
    id: Option<serde_json::Value>,
    #[allow(dead_code)]
    name: Option<String>,
}

impl DrimeFileResponse {
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

// ─── Dir Cache ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirInfo {
    id: String,
}

// ─── Provider ────────────────────────────────────────────────────────────

pub struct DrimeCloudProvider {
    config: DrimeCloudConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    current_folder_id: String,
    dir_cache: HashMap<String, DirInfo>,
}

impl DrimeCloudProvider {
    pub fn new(config: DrimeCloudConfig) -> Self {
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .default_headers(default_headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            current_path: "/".to_string(),
            current_folder_id: String::new(), // root has no ID, empty = root
            dir_cache: HashMap::new(),
        }
    }

    // ─── Helpers ─────────────────────────────────────────────────────────

    fn auth_header(&self) -> HeaderValue {
        HeaderValue::from_str(&format!("Bearer {}", self.config.api_token.expose_secret()))
            .unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    fn api_url(path: &str) -> String {
        format!("{}{}", API_BASE, path)
    }

    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim().replace('\\', "/");
        if trimmed.is_empty() || trimmed == "/" {
            return "/".to_string();
        }
        let p = if trimmed.starts_with('/') { trimmed } else { format!("/{}", trimmed) };
        p.trim_end_matches('/').to_string()
    }

    fn resolve_path(&self, path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "." {
            return self.current_path.clone();
        }
        let normalized = Self::normalize_path(trimmed);
        if normalized.starts_with('/') {
            normalized
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, normalized)
        }
    }

    fn split_path(path: &str) -> (&str, &str) {
        let normalized = path.trim_end_matches('/');
        match normalized.rfind('/') {
            Some(0) | None => ("/", normalized.trim_start_matches('/')),
            Some(pos) => (&normalized[..pos], &normalized[pos + 1..]),
        }
    }

    // ─── Folder Resolution ───────────────────────────────────────────────

    async fn resolve_folder_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if normalized == "/" {
            return Ok(String::new()); // root = no parent_id
        }

        // Check cache
        if let Some(info) = self.dir_cache.get(&normalized) {
            return Ok(info.id.clone());
        }

        // Walk path components
        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_id = String::new(); // root
        let mut current_path = String::new();

        for part in &parts {
            current_path = format!("{}/{}", current_path, part);

            if let Some(info) = self.dir_cache.get(&current_path) {
                current_id = info.id.clone();
                continue;
            }

            // List children to find the folder
            let mut page = 1u32;
            let mut found = false;

            loop {
                let url = if current_id.is_empty() {
                    format!("{}?page={}&perPage=100&type=folder", Self::api_url("/file-entries"), page)
                } else {
                    format!("{}?page={}&perPage=100&parentId={}", Self::api_url("/file-entries"), page, current_id)
                };

                let resp = self.client.get(&url)
                    .header(AUTHORIZATION, self.auth_header())
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(format!("List failed: {}", e)))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::ServerError(format!(
                        "List {} failed ({}): {}", current_path, status, sanitize_api_error(&body)
                    )));
                }

                let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                    ProviderError::ServerError(format!("Parse list response failed: {}", e))
                })?;

                let files = list_resp.data.unwrap_or_default();
                let last_page = list_resp.last_page.unwrap_or(1);

                for file in &files {
                    let is_folder = file.file_type.as_deref() == Some("folder");
                    if is_folder {
                        if let (Some(ref name), Some(id)) = (&file.name, file.id_str()) {
                            if name.eq_ignore_ascii_case(part) {
                                self.dir_cache.insert(current_path.clone(), DirInfo { id: id.clone() });
                                current_id = id;
                                found = true;
                                break;
                            }
                        }
                    }
                }

                if found || page >= last_page {
                    break;
                }
                page += 1;
            }

            if !found {
                return Err(ProviderError::NotFound(format!("Folder '{}' not found in {}", part, current_path)));
            }
        }

        Ok(current_id)
    }

    /// Find a file by name in a given folder, returns (file_id, is_dir, hash)
    async fn find_file_in_folder(&self, folder_id: &str, filename: &str) -> Result<Option<(String, bool, Option<String>)>, ProviderError> {
        let mut page = 1u32;

        loop {
            let url = if folder_id.is_empty() {
                format!("{}?page={}&perPage=100", Self::api_url("/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=100&parentId={}", Self::api_url("/file-entries"), page, folder_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Find file failed: {}", e)))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Find file failed: {}", sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse find response failed: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in &files {
                if let (Some(ref name), Some(id)) = (&file.name, file.id_str()) {
                    if name.eq_ignore_ascii_case(filename) {
                        let is_dir = file.file_type.as_deref() == Some("folder");
                        return Ok(Some((id, is_dir, file.hash.clone())));
                    }
                }
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        Ok(None)
    }

    /// Parse a Drime date string into "YYYY-MM-DD HH:MM:SS" format
    fn parse_date(date_str: &str) -> Option<String> {
        // Try ISO 8601 format "2025-01-15T10:30:00.000000Z"
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(date_str) {
            return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
        }
        // Try without fractional seconds
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&date_str.replace(' ', "T")) {
            return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
        }
        // Return as-is if it looks like a date (safe truncation at char boundary)
        if date_str.len() >= 10 {
            let end = 19.min(date_str.len());
            let safe_end = if date_str.is_char_boundary(end) { end } else { date_str.len().min(end) };
            return Some(date_str[..safe_end].to_string());
        }
        None
    }
}

// ─── StorageProvider Implementation ──────────────────────────────────────

#[async_trait]
impl StorageProvider for DrimeCloudProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::DrimeCloud
    }

    fn display_name(&self) -> String {
        "Drime Cloud".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        drime_log("Connecting to Drime Cloud");

        // Validate token by listing root file-entries (most reliable Bedrive endpoint)
        let url = format!("{}?page=1&perPage=1", Self::api_url("/file-entries"));
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| {
                drime_log(&format!("Connection error: {} (is_timeout={}, is_connect={})", e, e.is_timeout(), e.is_connect()));
                ProviderError::ConnectionFailed(format!("Connection failed: {}", e))
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to read response: {}", e))
        })?;
        drime_log(&format!("Auth check response: status={}, len={}", status, body.len()));

        // Bedrive returns HTML for non-existent routes — detect SPA catch-all
        if body.starts_with("<!") || body.starts_with("<html") {
            return Err(ProviderError::ConnectionFailed(
                "Server returned HTML instead of JSON. The API might not be available.".to_string()
            ));
        }

        // Bedrive returns "Unauthenticated." for invalid tokens
        if status.as_u16() == 401 || body.contains("Unauthenticated") {
            return Err(ProviderError::AuthenticationFailed(
                "Invalid API token. Generate one at app.drime.cloud → Account Settings → Developers".to_string()
            ));
        }

        if !status.is_success() {
            return Err(ProviderError::ConnectionFailed(format!(
                "Drime Cloud connection failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // Initialize root
        self.current_folder_id = String::new();
        self.current_path = "/".to_string();
        self.dir_cache.insert("/".to_string(), DirInfo { id: String::new() });

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                drime_log(&format!("Navigating to initial path: {}", normalized));
                match self.resolve_folder_id(&normalized).await {
                    Ok(id) => {
                        self.current_path = normalized;
                        self.current_folder_id = id;
                    }
                    Err(e) => {
                        drime_log(&format!("Initial path error (using root): {}", e));
                    }
                }
            }
        }

        self.connected = true;
        drime_log("Connected successfully");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.current_path = "/".to_string();
        self.current_folder_id = String::new();
        self.dir_cache.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
        } else if path == ".." {
            let mut parts: Vec<&str> = self.current_path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        };

        let folder_id = self.resolve_folder_id(&new_path).await?;
        self.current_folder_id = folder_id;
        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&resolved).await?;

        let mut entries = Vec::new();
        let mut page = 1u32;

        loop {
            let url = if folder_id.is_empty() {
                format!("{}?page={}&perPage=50", Self::api_url("/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=50&parentId={}", Self::api_url("/file-entries"), page, folder_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(format!("List failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "List {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse list response failed: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in files {
                let name = file.name.clone().unwrap_or_else(|| {
                    file.id_str().unwrap_or_else(|| "unnamed".to_string())
                });
                let is_dir = file.file_type.as_deref() == Some("folder");
                let size = file.size.unwrap_or(0);
                let modified = file.updated_at.as_deref().and_then(Self::parse_date);

                // Cache directories
                if is_dir {
                    if let Some(id) = file.id_str() {
                        let dir_path = if resolved == "/" {
                            format!("/{}", name)
                        } else {
                            format!("{}/{}", resolved, name)
                        };
                        self.dir_cache.insert(dir_path, DirInfo { id });
                    }
                }

                let entry_path = if resolved == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", resolved, name)
                };

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    metadata: HashMap::new(),
                    mime_type: file.mime_type,
                });
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        // Update current position
        self.current_path = resolved;
        self.current_folder_id = folder_id;

        Ok(entries)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _is_dir, file_hash) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        drime_log(&format!("Downloading file {} (id={})", filename, file_id));

        // Bedrive uses hash-based download, fall back to ID-based
        let url = if let Some(ref hash) = file_hash {
            Self::api_url(&format!("/file-entries/download/{}", hash))
        } else {
            Self::api_url(&format!("/file-entries/{}/download", file_id))
        };
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Download request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::ServerError(format!("Failed to read download body: {}", e)))?;

        if let Some(ref cb) = on_progress {
            cb(bytes.len() as u64, total_size);
        }

        tokio::fs::write(local_path, &bytes).await
            .map_err(ProviderError::IoError)?;

        drime_log(&format!("Downloaded {} ({} bytes)", filename, bytes.len()));
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, file_hash) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        let url = if let Some(ref hash) = file_hash {
            Self::api_url(&format!("/file-entries/download/{}", hash))
        } else {
            Self::api_url(&format!("/file-entries/{}/download", file_id))
        };
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Download failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed: {}", sanitize_api_error(&body)
            )));
        }

        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::ServerError(format!("Read body failed: {}", e)))?;

        Ok(bytes.to_vec())
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let data = tokio::fs::read(local_path).await
            .map_err(ProviderError::IoError)?;

        let file_size = data.len() as u64;
        drime_log(&format!("Uploading {} ({} bytes) to folder '{}'", filename, file_size, parent_id));

        // Preemptive delete if file already exists
        if let Some((existing_id, _, _)) = self.find_file_in_folder(&parent_id, filename).await? {
            drime_log(&format!("File {} exists (id={}), deleting before overwrite", filename, existing_id));
            let del_url = Self::api_url(&format!("/file-entries/{}", existing_id));
            let _ = self.client.delete(&del_url)
                .header(AUTHORIZATION, self.auth_header())
                .send()
                .await;
        }

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        // Upload via multipart POST (Bedrive)
        let mut form = reqwest::multipart::Form::new();

        // Add parentId if not root
        if !parent_id.is_empty() {
            form = form.text("parentId", parent_id.clone());
        }

        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::ServerError(format!("MIME error: {}", e)))?;
        form = form.part("file", part);

        let url = Self::api_url("/uploads");
        let resp = self.client.post(&url)
            .header(AUTHORIZATION, self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Upload request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Upload failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        if let Some(ref cb) = on_progress {
            cb(file_size, file_size);
        }

        drime_log(&format!("Uploaded {} successfully", filename));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, dir_name) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        drime_log(&format!("Creating directory '{}' in folder '{}'", dir_name, parent_id));

        let body = if parent_id.is_empty() {
            serde_json::json!({
                "name": dir_name
            })
        } else {
            serde_json::json!({
                "name": dir_name,
                "parentId": parent_id
            })
        };

        let url = Self::api_url("/folders");
        let resp = self.client.post(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Mkdir failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create directory failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // Cache the new dir
        if let Ok(folder_resp) = resp.json::<DrimeFolderResponse>().await {
            if let Some(id) = folder_resp.id_str() {
                self.dir_cache.insert(resolved, DirInfo { id });
            }
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!("Deleting {} (id={})", filename, file_id));

        let url = Self::api_url(&format!("/file-entries/{}", file_id));
        let resp = self.client.delete(&url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Delete failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Delete failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // Remove from cache if directory
        self.dir_cache.remove(&resolved);

        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (_to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self.find_file_in_folder(&from_parent_id, from_name).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        drime_log(&format!("Renaming {} → {} (id={})", from_name, to_name, file_id));

        // Bedrive API: PUT /file-entries/{id} with {name}
        let url = Self::api_url(&format!("/file-entries/{}", file_id));
        let resp = self.client.put(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "name": to_name }).to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Rename failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Rename failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // Update cache
        self.dir_cache.remove(&resolved_from);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        // Search for the file in the parent folder
        let mut page = 1u32;

        loop {
            let url = if parent_id.is_empty() {
                format!("{}?page={}&perPage=100", Self::api_url("/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=100&parentId={}", Self::api_url("/file-entries"), page, parent_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Stat failed: {}", e)))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Stat failed: {}", sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse stat response failed: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in &files {
                if let Some(ref name) = file.name {
                    if name.eq_ignore_ascii_case(filename) {
                        let is_dir = file.file_type.as_deref() == Some("folder");
                        let size = file.size.unwrap_or(0);
                        let modified = file.updated_at.as_deref().and_then(Self::parse_date);

                        return Ok(RemoteEntry {
                            name: name.clone(),
                            path: resolved,
                            is_dir,
                            size,
                            modified,
                            permissions: None,
                            owner: None,
                            group: None,
                            is_symlink: false,
                            link_target: None,
                            metadata: HashMap::new(),
                            mime_type: file.mime_type.clone(),
                        });
                    }
                }
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        Err(ProviderError::NotFound(format!("'{}' not found", filename)))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Drime Cloud — 20GB Secure Cloud Storage".to_string())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self.find_file_in_folder(&from_parent_id, from_name).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        drime_log(&format!("Duplicating {} (id={})", from_name, file_id));

        // Bedrive API: POST /file-entries/duplicate with entryIds array
        let url = Self::api_url("/file-entries/duplicate");
        let resp = self.client.post(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "entryIds": [file_id.parse::<i64>().unwrap_or(0)] }).to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Copy failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Copy failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // If destination is different directory, move the duplicate
        let resolved_to = self.resolve_path(to);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        // Try to get the duplicated file info from response
        if let Ok(Some(resp_data)) = resp.json::<DrimeFileResponse>().await.map(Some) {
            if let Some(dup_id) = resp_data.id_str() {
                // Move if different parent
                if from_parent != to_parent {
                    let move_url = Self::api_url("/file-entries/move");
                    let dup_id_num = dup_id.parse::<i64>().unwrap_or(0);
                    if let Err(e) = self.client.post(&move_url)
                        .header(AUTHORIZATION, self.auth_header())
                        .header(CONTENT_TYPE, "application/json")
                        .body(serde_json::json!({
                            "entryIds": [dup_id_num],
                            "destinationId": to_parent_id
                        }).to_string())
                        .send()
                        .await
                    {
                        drime_log(&format!("Warning: move after duplicate failed: {}", e));
                    }
                }
                // Rename if needed
                if from_name != to_name {
                    let rename_url = Self::api_url(&format!("/file-entries/{}", dup_id));
                    if let Err(e) = self.client.put(&rename_url)
                        .header(AUTHORIZATION, self.auth_header())
                        .header(CONTENT_TYPE, "application/json")
                        .body(serde_json::json!({ "name": to_name }).to_string())
                        .send()
                        .await
                    {
                        drime_log(&format!("Warning: rename after duplicate failed: {}", e));
                    }
                }
            }
        }

        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = Self::api_url("/user/space-usage");
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Quota request failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Quota failed: {}", sanitize_api_error(&body)
            )));
        }

        let storage: DrimeStorageResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse quota response failed: {}", e))
        })?;

        let used = storage.used.unwrap_or(0);
        let total = storage.total.unwrap_or(0);

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }
}
