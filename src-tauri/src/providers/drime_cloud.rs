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
use tracing::{info, warn};

use super::{
    FileVersion, ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
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
struct DrimeUser {
    id: Option<u64>,
    #[allow(dead_code)]
    email: Option<String>,
    #[allow(dead_code)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeUserResponse {
    user: Option<DrimeUser>,
}

// ─── S3 Multipart Upload Response Types ─────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeMultipartCreateResponse {
    key: Option<String>,
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeSignedUrl {
    #[serde(rename = "partNumber")]
    part_number: u32,
    url: String,
}

#[derive(Debug, Deserialize)]
struct DrimeSignPartUrlsResponse {
    urls: Option<Vec<DrimeSignedUrl>>,
}

// ─── Share Link Response ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeShareLink {
    hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeShareLinkResponse {
    link: Option<DrimeShareLink>,
}

// ─── File Backup/Version Response ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeBackupEntry {
    id: Option<serde_json::Value>,
    name: Option<String>,
    file_size: Option<u64>,
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeBackupPagination {
    data: Option<Vec<DrimeBackupEntry>>,
}

#[derive(Debug, Deserialize)]
struct DrimeBackupResponse {
    pagination: Option<DrimeBackupPagination>,
}

// ─── Storage Response ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeStorageResponse {
    /// Storage consumed in bytes
    #[serde(default)]
    used: Option<u64>,
    /// Remaining capacity in bytes (API field: "available")
    #[serde(default)]
    available: Option<u64>,
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

/// Response from /file-entries/duplicate: { entries: [...], status: "success" }
#[derive(Debug, Deserialize)]
struct DrimeEntriesResponse {
    entries: Option<Vec<DrimeFileResponse>>,
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
    /// Authenticated user ID (from /cli/loggedUser)
    user_id: Option<u64>,
}

/// M3: Maximum number of cached directory entries to prevent unbounded memory growth.
const DIR_CACHE_MAX_ENTRIES: usize = 10_000;

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
            user_id: None,
        }
    }

    // ─── Helpers ─────────────────────────────────────────────────────────

    /// M3: Insert into dir_cache with eviction when cap is reached.
    /// Clears the entire cache when it exceeds DIR_CACHE_MAX_ENTRIES,
    /// allowing it to repopulate naturally during navigation.
    fn dir_cache_insert(&mut self, key: String, value: DirInfo) {
        if self.dir_cache.len() >= DIR_CACHE_MAX_ENTRIES {
            tracing::debug!("[DRIME] dir_cache reached {} entries, evicting all", self.dir_cache.len());
            self.dir_cache.clear();
        }
        self.dir_cache.insert(key, value);
    }

    /// M7: Returns Result instead of silently falling back to an empty header on invalid tokens.
    /// An empty Authorization header would cause silent auth failures that are hard to debug.
    fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        HeaderValue::from_str(&format!("Bearer {}", self.config.api_token.expose_secret()))
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("Invalid characters in API token: {}", e)
            ))
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
                    format!("{}?page={}&perPage=100&workspaceId=0&type=folder", Self::api_url("/drive/file-entries"), page)
                } else {
                    format!("{}?page={}&perPage=100&workspaceId=0&parentIds[]={}", Self::api_url("/drive/file-entries"), page, current_id)
                };

                let resp = self.client.get(&url)
                    .header(AUTHORIZATION, self.auth_header()?)
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
                                self.dir_cache_insert(current_path.clone(), DirInfo { id: id.clone() });
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
                format!("{}?page={}&perPage=100&workspaceId=0", Self::api_url("/drive/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=100&workspaceId=0&parentIds[]={}", Self::api_url("/drive/file-entries"), page, folder_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header()?)
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

    /// S3 multipart upload for files >= 5 MB
    /// Flow: create → sign URLs → PUT chunks → complete → register entry
    async fn upload_multipart(
        &self,
        data: Vec<u8>,
        filename: &str,
        parent_id: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        const CHUNK_SIZE: usize = 5 * 1024 * 1024; // 5 MB per Drime API spec
        let file_size = data.len() as u64;
        let total_parts = data.len().div_ceil(CHUNK_SIZE) as u32;

        // Infer MIME and extension from filename
        let extension = filename.rsplit('.').next().unwrap_or("bin").to_string();
        let mime = mime_guess::from_ext(&extension)
            .first_or_octet_stream()
            .to_string();

        drime_log(&format!(
            "Multipart upload: {} ({} bytes, {} parts)", filename, file_size, total_parts
        ));

        // Step 1: Create multipart upload
        let create_body = serde_json::json!({
            "filename": filename,
            "mime": mime,
            "size": file_size,
            "extension": extension,
            "workspaceId": 0
        });
        if !parent_id.is_empty() {
            // parentId added below via mutable json
        }
        let mut create_json: serde_json::Value = create_body;
        if !parent_id.is_empty() {
            create_json["parentId"] = serde_json::json!(parent_id.parse::<i64>().unwrap_or(0));
        }

        let resp = self.client.post(Self::api_url("/s3/multipart/create"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(create_json.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Multipart create failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Multipart create failed: {}", sanitize_api_error(&body)
            )));
        }

        let create_resp: DrimeMultipartCreateResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse multipart create response: {}", e))
        })?;

        let key = create_resp.key.ok_or_else(|| {
            ProviderError::ServerError("Missing 'key' in multipart create response".to_string())
        })?;
        let upload_id = create_resp.upload_id.ok_or_else(|| {
            ProviderError::ServerError("Missing 'uploadId' in multipart create response".to_string())
        })?;

        drime_log(&format!("Multipart created: uploadId={}", &upload_id[..20.min(upload_id.len())]));

        // Step 2: Get signed URLs for all parts
        let part_numbers: Vec<u32> = (1..=total_parts).collect();
        let sign_body = serde_json::json!({
            "key": key,
            "uploadId": upload_id,
            "partNumbers": part_numbers
        });

        let resp = self.client.post(Self::api_url("/s3/multipart/batch-sign-part-urls"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(sign_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Sign part URLs failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Sign part URLs failed: {}", sanitize_api_error(&body)
            )));
        }

        let sign_resp: DrimeSignPartUrlsResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse sign URLs response: {}", e))
        })?;

        let signed_urls = sign_resp.urls.ok_or_else(|| {
            ProviderError::ServerError("Missing 'urls' in sign response".to_string())
        })?;

        // Step 3: Upload each chunk to its signed URL
        let mut completed_parts: Vec<serde_json::Value> = Vec::new();
        let mut bytes_uploaded: u64 = 0;

        for signed in &signed_urls {
            let part_num = signed.part_number as usize;
            let start = (part_num - 1) * CHUNK_SIZE;
            let end = (start + CHUNK_SIZE).min(data.len());
            let chunk = &data[start..end];

            let resp = self.client.put(&signed.url)
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!(
                    "Upload part {} failed: {}", part_num, e
                )))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Upload part {} failed: {}", part_num, sanitize_api_error(&body)
                )));
            }

            // Extract ETag from response headers (with quotes)
            let etag = resp.headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_default();

            completed_parts.push(serde_json::json!({
                "PartNumber": signed.part_number,
                "ETag": etag
            }));

            bytes_uploaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(bytes_uploaded, file_size);
            }
        }

        // Step 4: Complete multipart upload
        let complete_body = serde_json::json!({
            "key": key,
            "uploadId": upload_id,
            "parts": completed_parts
        });

        let resp = self.client.post(Self::api_url("/s3/multipart/complete"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(complete_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Multipart complete failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Multipart complete failed: {}", sanitize_api_error(&body)
            )));
        }

        // Step 5: Register file entry in Drime
        let s3_filename = key.rsplit('/').next().unwrap_or(&key);
        let mut entry_body = serde_json::json!({
            "filename": s3_filename,
            "size": file_size,
            "clientName": filename,
            "clientMime": mime,
            "clientExtension": extension,
            "workspaceId": 0
        });
        if !parent_id.is_empty() {
            entry_body["parentId"] = serde_json::json!(parent_id.parse::<i64>().unwrap_or(0));
        }

        let resp = self.client.post(Self::api_url("/s3/entries"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(entry_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("S3 entry registration failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "S3 entry registration failed: {}", sanitize_api_error(&body)
            )));
        }

        drime_log(&format!("Multipart upload complete: {} ({} bytes, {} parts)", filename, file_size, total_parts));
        Ok(())
    }

    /// Execute a request with exponential backoff on 429 (rate limit)
    async fn request_with_retry(
        &self,
        build_request: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        const MAX_RETRIES: u32 = 3;
        let mut delay = std::time::Duration::from_secs(1);

        for attempt in 0..=MAX_RETRIES {
            let resp = build_request()
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))?;

            if resp.status().as_u16() == 429 {
                if attempt < MAX_RETRIES {
                    // Use Retry-After header if present, otherwise exponential backoff
                    let wait = resp.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(std::time::Duration::from_secs)
                        .unwrap_or(delay);

                    warn!("[DRIME] Rate limited (429), retrying in {:?} (attempt {}/{})", wait, attempt + 1, MAX_RETRIES);
                    tokio::time::sleep(wait).await;
                    delay *= 2; // exponential backoff: 1s, 2s, 4s
                    continue;
                }
                return Err(ProviderError::ServerError(
                    "Rate limited by Drime Cloud API (429). Please try again later.".to_string()
                ));
            }

            return Ok(resp);
        }

        unreachable!()
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

        // Validate token via /cli/loggedUser (purpose-built for auth check + returns user info)
        let url = Self::api_url("/cli/loggedUser");
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
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

        // Detect HTML response (SPA catch-all for non-existent routes)
        if body.starts_with("<!") || body.starts_with("<html") {
            return Err(ProviderError::ConnectionFailed(
                "Server returned HTML instead of JSON. The API might not be available.".to_string()
            ));
        }

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

        // Extract user ID for folder tree operations
        if let Ok(user_resp) = serde_json::from_str::<DrimeUserResponse>(&body) {
            if let Some(user) = user_resp.user {
                self.user_id = user.id;
                drime_log(&format!("Authenticated as user_id={:?}", self.user_id));
            }
        }

        // Initialize root
        self.current_folder_id = String::new();
        self.current_path = "/".to_string();
        self.dir_cache_insert("/".to_string(), DirInfo { id: String::new() });

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
                format!("{}?page={}&perPage=50&workspaceId=0", Self::api_url("/drive/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=50&workspaceId=0&parentIds[]={}", Self::api_url("/drive/file-entries"), page, folder_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header()?)
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
                        self.dir_cache_insert(dir_path, DirInfo { id });
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
            .header(AUTHORIZATION, self.auth_header()?)
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
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Download failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed: {}", sanitize_api_error(&body)
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        const MULTIPART_THRESHOLD: u64 = 5 * 1024 * 1024; // 5 MB

        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        // M9: Full file read into memory — no streaming upload API available for Drime Cloud.
        // This limits practical upload size to available RAM. For files >500MB, users should
        // consider alternative providers with chunked upload support (S3, OneDrive, Dropbox).
        let data = tokio::fs::read(local_path).await
            .map_err(ProviderError::IoError)?;

        let file_size = data.len() as u64;
        drime_log(&format!("Uploading {} ({} bytes) to folder '{}'", filename, file_size, parent_id));

        // Delete existing file before overwrite
        if let Some((existing_id, _, _)) = self.find_file_in_folder(&parent_id, filename).await? {
            drime_log(&format!("File {} exists (id={}), deleting before overwrite", filename, existing_id));
            let del_body = serde_json::json!({ "entryIds": [existing_id.parse::<i64>().unwrap_or(0)] });
            let _ = self.client.post(Self::api_url("/file-entries/delete"))
                .header(AUTHORIZATION, self.auth_header()?)
                .header(CONTENT_TYPE, "application/json")
                .body(del_body.to_string())
                .send()
                .await;
        }

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        // Route by file size: < 5MB direct upload, >= 5MB S3 multipart
        if file_size >= MULTIPART_THRESHOLD {
            self.upload_multipart(data, filename, &parent_id, on_progress).await?;
        } else {
            // Direct upload via multipart POST
            let mut form = reqwest::multipart::Form::new();
            if !parent_id.is_empty() {
                form = form.text("parentId", parent_id.clone());
            }
            form = form.text("workspaceId", "0");

            let part = reqwest::multipart::Part::bytes(data)
                .file_name(filename.to_string())
                .mime_str("application/octet-stream")
                .map_err(|e| ProviderError::ServerError(format!("MIME error: {}", e)))?;
            form = form.part("file", part);

            let resp = self.client.post(Self::api_url("/uploads"))
                .header(AUTHORIZATION, self.auth_header()?)
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

        let url = Self::api_url("/folders?workspaceId=0");
        let resp = self.client.post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
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
                self.dir_cache_insert(resolved, DirInfo { id });
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

        // Batch delete endpoint (moves to trash by default)
        let body = serde_json::json!({ "entryIds": [file_id.parse::<i64>().unwrap_or(0)] });
        let resp = self.client.post(Self::api_url("/file-entries/delete"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
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

        self.dir_cache.remove(&resolved);
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self.find_file_in_folder(&from_parent_id, from_name).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        let file_id_num = file_id.parse::<i64>().unwrap_or(0);

        // Cross-folder move: use /file-entries/move first
        if from_parent != to_parent {
            let to_parent_id = self.resolve_folder_id(to_parent).await?;
            drime_log(&format!("Moving {} to folder '{}'", from_name, to_parent_id));

            let dest_id: serde_json::Value = if to_parent_id.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!(to_parent_id.parse::<i64>().unwrap_or(0))
            };

            let move_body = serde_json::json!({
                "entryIds": [file_id_num],
                "destinationId": dest_id
            });
            let resp = self.client.post(Self::api_url("/file-entries/move?workspaceId=0"))
                .header(AUTHORIZATION, self.auth_header()?)
                .header(CONTENT_TYPE, "application/json")
                .body(move_body.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Move failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Move failed ({}): {}", status, sanitize_api_error(&body)
                )));
            }
        }

        // Rename if name changed
        if from_name != to_name {
            drime_log(&format!("Renaming {} → {} (id={})", from_name, to_name, file_id));

            let url = Self::api_url(&format!("/file-entries/{}?workspaceId=0", file_id));
            let resp = self.client.put(&url)
                .header(AUTHORIZATION, self.auth_header()?)
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
        }

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
                format!("{}?page={}&perPage=100&workspaceId=0", Self::api_url("/drive/file-entries"), page)
            } else {
                format!("{}?page={}&perPage=100&workspaceId=0&parentIds[]={}", Self::api_url("/drive/file-entries"), page, parent_id)
            };

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, self.auth_header()?)
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
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self.find_file_in_folder(&from_parent_id, from_name).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        let file_id_num = file_id.parse::<i64>().unwrap_or(0);

        // Resolve destination folder (may differ from source)
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        drime_log(&format!("Duplicating {} (id={}) → {}", from_name, file_id, resolved_to));

        // POST /file-entries/duplicate with destinationId for direct cross-folder copy
        let mut dup_body = serde_json::json!({ "entryIds": [file_id_num] });
        if from_parent != to_parent {
            let dest_id: serde_json::Value = if to_parent_id.is_empty() {
                serde_json::Value::Null // root
            } else {
                serde_json::json!(to_parent_id.parse::<i64>().unwrap_or(0))
            };
            dup_body["destinationId"] = dest_id;
        }

        let resp = self.client.post(Self::api_url("/file-entries/duplicate?workspaceId=0"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(dup_body.to_string())
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

        // Rename if destination name differs from source
        // API returns { entries: [...], status: "success" }
        if from_name != to_name {
            if let Ok(resp_data) = resp.json::<DrimeEntriesResponse>().await {
                if let Some(first) = resp_data.entries.as_ref().and_then(|e| e.first()) {
                    if let Some(dup_id) = first.id_str() {
                        let rename_url = Self::api_url(&format!("/file-entries/{}?workspaceId=0", dup_id));
                        let rename_resp = self.client.put(&rename_url)
                            .header(AUTHORIZATION, self.auth_header()?)
                            .header(CONTENT_TYPE, "application/json")
                            .body(serde_json::json!({ "name": to_name }).to_string())
                            .send()
                            .await
                            .map_err(|e| ProviderError::ConnectionFailed(format!("Rename after copy failed: {}", e)))?;

                        if !rename_resp.status().is_success() {
                            drime_log(&format!("Warning: rename after duplicate failed ({})", rename_resp.status()));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = Self::api_url("/user/space-usage?workspaceId=0");
        let auth = self.auth_header()?;
        let resp = self.request_with_retry(|| {
            self.client.get(&url)
                .header(AUTHORIZATION, auth.clone())
        }).await?;

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
        let available = storage.available.unwrap_or(0);

        Ok(StorageInfo {
            used,
            total: used + available,
            free: available,
        })
    }

    // ─── Search ─────────────────────────────────────────────────────────

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, _path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        drime_log(&format!("Searching for '{}'", pattern));

        let mut entries = Vec::new();
        let mut page = 1u32;

        loop {
            let url = format!(
                "{}?workspaceId=0&query={}&page={}&perPage=50",
                Self::api_url("/drive/file-entries"),
                urlencoding::encode(pattern),
                page
            );

            let auth = self.auth_header()?;
            let resp = self.request_with_retry(|| {
                self.client.get(&url)
                    .header(AUTHORIZATION, auth.clone())
            }).await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Search failed: {}", sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse search response: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in files {
                let name = file.name.clone().unwrap_or_default();
                let is_dir = file.file_type.as_deref() == Some("folder");

                entries.push(RemoteEntry {
                    name: name.clone(),
                    path: format!("/{}", name), // search results don't include full path
                    is_dir,
                    size: file.size.unwrap_or(0),
                    modified: file.updated_at.as_deref().and_then(Self::parse_date),
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

        drime_log(&format!("Search '{}' found {} results", pattern, entries.len()));
        Ok(entries)
    }

    // ─── Share Links ────────────────────────────────────────────────────

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!("Creating share link for {} (id={})", filename, file_id));

        let mut body = serde_json::json!({
            "allow_download": true,
            "allow_edit": false
        });

        if let Some(secs) = expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            body["expires_at"] = serde_json::json!(expires_at.to_rfc3339());
        }

        let url = Self::api_url(&format!("/file-entries/{}/shareable-link", file_id));
        let auth = self.auth_header()?;
        let resp = self.request_with_retry(|| {
            self.client.post(&url)
                .header(AUTHORIZATION, auth.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
        }).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create share link failed ({}): {}", status, sanitize_api_error(&resp_body)
            )));
        }

        let link_resp: DrimeShareLinkResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse share link response: {}", e))
        })?;

        let hash = link_resp.link
            .and_then(|l| l.hash)
            .ok_or_else(|| ProviderError::ServerError("Missing hash in share link response".to_string()))?;

        let share_url = format!("https://app.drime.cloud/drive/shares/{}", hash);
        drime_log(&format!("Share link created: {}", share_url));
        Ok(share_url)
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!("Removing share link for {} (id={})", filename, file_id));

        let url = Self::api_url(&format!("/file-entries/{}/shareable-link", file_id));
        let auth = self.auth_header()?;
        let resp = self.request_with_retry(|| {
            self.client.delete(&url)
                .header(AUTHORIZATION, auth.clone())
        }).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Remove share link failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    // ─── File Versions ──────────────────────────────────────────────────

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self.find_file_in_folder(&parent_id, filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!("Listing versions for {} (id={})", filename, file_id));

        let url = format!("{}?file_id={}&perPage=50", Self::api_url("/file-backup"), file_id);
        let auth = self.auth_header()?;
        let resp = self.request_with_retry(|| {
            self.client.get(&url)
                .header(AUTHORIZATION, auth.clone())
        }).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List versions failed: {}", sanitize_api_error(&body)
            )));
        }

        let backup_resp: DrimeBackupResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse backup response: {}", e))
        })?;

        let versions = backup_resp.pagination
            .and_then(|p| p.data)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let id = entry.id.as_ref().map(|v| match v {
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })?;
                Some(FileVersion {
                    id,
                    modified: entry.created_at.as_deref().and_then(Self::parse_date),
                    size: entry.file_size.unwrap_or(0),
                    modified_by: entry.name,
                })
            })
            .collect();

        Ok(versions)
    }
}
