//! 4shared Cloud Storage Provider
//!
//! Implements StorageProvider for 4shared using their REST API v1.2.
//! Uses OAuth 1.0a (HMAC-SHA1) for authentication.

use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use tracing::{info, debug};

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo,
};
use super::oauth1::{self, OAuth1Credentials};
use super::http_retry::{HttpRetryConfig, send_with_retry};
use super::types::FourSharedConfig;

/// 4shared API base URL.
/// FS-002: HTTPS is enforced — all API calls use this constant. Do not change to HTTP.
const API_BASE: &str = "https://api.4shared.com/v1_2";
/// 4shared upload URL.
/// FS-002: HTTPS is enforced — all upload calls use this constant. Do not change to HTTP.
const UPLOAD_BASE: &str = "https://upload.4shared.com/v1_2";

/// Maximum items per page for 4shared API list operations
const PAGE_SIZE: u32 = 1000;

// FS-008: StatusBar path/quota overlap is a frontend CSS issue, fixed in
// src/components/StatusBar.tsx (min-w-0 flex-1). Not applicable to this file.

// ============ Custom Deserializers ============

/// Deserialize a value that may be either a number or a string containing a number.
/// 4shared API sometimes returns Long fields as JSON strings.
///
/// FS-003: Handles edge cases — empty strings, null, booleans, very large u64,
/// negative numbers, and unparseable strings (returns None instead of error).
fn string_or_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    use serde::de;

    struct StringOrI64Visitor;

    impl<'de> de::Visitor<'de> for StringOrI64Visitor {
        type Value = Option<i64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a number or string-encoded number or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E: de::Error>(self, _v: bool) -> Result<Self::Value, E> {
            // FS-003: Unexpected type — treat as absent rather than failing
            Ok(None)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            // FS-003: Saturate at i64::MAX for very large u64 values instead of wrapping
            Ok(Some(v.min(i64::MAX as u64) as i64))
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            // FS-003: Guard against NaN/Infinity
            if v.is_finite() {
                Ok(Some(v as i64))
            } else {
                Ok(None)
            }
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            // FS-003: Return None on unparseable strings instead of propagating error,
            // since 4shared API may return unexpected string values for numeric fields
            match trimmed.parse::<i64>() {
                Ok(n) => Ok(Some(n)),
                Err(_) => {
                    debug!("string_or_i64: could not parse '{}' as i64, returning None", trimmed);
                    Ok(None)
                }
            }
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            self.visit_str(&v)
        }
    }

    deserializer.deserialize_any(StringOrI64Visitor)
}

// ============ API Response Types ============

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FourSharedUser {
    #[serde(rename = "rootFolderId")]
    root_folder_id: Option<String>,
    email: Option<String>,
    name: Option<String>,
    #[serde(rename = "firstName")]
    first_name: Option<String>,
    #[serde(rename = "lastName")]
    last_name: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    #[serde(rename = "totalSpace")]
    total_space: Option<i64>,
    #[serde(default, deserialize_with = "string_or_i64")]
    #[serde(rename = "usedSpace")]
    used_space: Option<i64>,
    #[serde(default, deserialize_with = "string_or_i64")]
    #[serde(rename = "freeSpace")]
    free_space: Option<i64>,
}

/// Folder object per 4shared REST API docs.
/// Field names match the actual API response: numChildren, numFiles, modified.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FourSharedFolder {
    id: Option<String>,
    name: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    path: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    #[serde(rename = "numChildren")]
    num_children: Option<i64>,
    #[serde(default, deserialize_with = "string_or_i64")]
    #[serde(rename = "numFiles")]
    num_files: Option<i64>,
    modified: Option<String>,
    access: Option<String>,
    #[serde(rename = "ownerId")]
    owner_id: Option<String>,
    status: Option<String>,
}

/// File object per 4shared REST API docs.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FourSharedFile {
    id: Option<String>,
    name: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    size: Option<i64>,
    modified: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    md5: Option<String>,
    #[serde(rename = "downloadPage")]
    download_page: Option<String>,
    status: Option<String>,
}

/// Upload response from upload.4shared.com
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FourSharedUploadResponse {
    id: Option<String>,
    name: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    size: Option<i64>,
}

/// 4shared Storage Provider
pub struct FourSharedProvider {
    config: FourSharedConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    current_folder_id: String,
    root_folder_id: String,
    /// path -> folder_id cache
    folder_cache: HashMap<String, String>,
    /// path -> file_id cache
    file_cache: HashMap<String, String>,
    account_email: Option<String>,
}

impl FourSharedProvider {
    pub fn new(config: FourSharedConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            current_path: "/".to_string(),
            current_folder_id: String::new(),
            root_folder_id: String::new(),
            folder_cache: HashMap::new(),
            file_cache: HashMap::new(),
            account_email: None,
        }
    }

    /// Build OAuth1Credentials from config
    fn credentials(&self) -> OAuth1Credentials {
        OAuth1Credentials {
            consumer_key: self.config.consumer_key.clone(),
            consumer_secret: self.config.consumer_secret.expose_secret().to_string(),
            token: self.config.access_token.expose_secret().to_string(),
            token_secret: self.config.access_token_secret.expose_secret().to_string(),
        }
    }

    /// FS-009: Shared retry config for all HTTP requests
    fn retry_config() -> HttpRetryConfig {
        HttpRetryConfig {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 15_000,
            backoff_multiplier: 2.0,
        }
    }

    /// Make a signed GET request with automatic retry on 429/5xx (FS-009)
    async fn signed_get(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("GET", url, &self.credentials(), &[]);
        let request = self.client
            .get(url)
            .header("Authorization", &auth)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed POST request (form-urlencoded body) with retry (FS-009)
    async fn signed_post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("POST", url, &self.credentials(), form);

        // Build URL-encoded body manually
        let body: String = form.iter()
            .map(|(k, v)| format!("{}={}", oauth1::percent_encode(k), oauth1::percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let request = self.client
            .post(url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed DELETE request with retry (FS-009)
    async fn signed_delete(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("DELETE", url, &self.credentials(), &[]);
        let request = self.client
            .delete(url)
            .header("Authorization", &auth)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed PUT request with form body and retry (FS-009)
    async fn signed_put_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("PUT", url, &self.credentials(), form);

        let body: String = form.iter()
            .map(|(k, v)| format!("{}={}", oauth1::percent_encode(k), oauth1::percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let request = self.client
            .put(url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Normalize an absolute path (ensure starts with /, no trailing slash)
    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "/" || trimmed == "." || trimmed == "./" {
            return "/".to_string();
        }
        let parts: Vec<&str> = trimmed.split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .collect();
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    }

    /// Resolve a path relative to current_path.
    /// - "." or "" → current_path
    /// - "/Cloud" → "/Cloud" (absolute, as-is)
    /// - "Cloud" → current_path + "/Cloud" (relative)
    fn resolve_path(&self, path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == "./" {
            return self.current_path.clone();
        }
        if trimmed.starts_with('/') {
            return Self::normalize_path(trimmed);
        }
        // Relative path — join with current_path
        let base = if self.current_path == "/" { String::new() } else { self.current_path.clone() };
        Self::normalize_path(&format!("{}/{}", base, trimmed))
    }

    /// Split path into (parent_path, name)
    fn split_path(normalized: &str) -> (String, String) {
        match normalized.rfind('/') {
            Some(0) => ("/".to_string(), normalized[1..].to_string()),
            Some(idx) => (normalized[..idx].to_string(), normalized[idx + 1..].to_string()),
            None => ("/".to_string(), normalized.to_string()),
        }
    }

    /// Evict cache if it grows beyond the limit to prevent unbounded memory growth
    fn enforce_cache_limit(cache: &mut HashMap<String, String>) {
        if cache.len() > 10_000 {
            // Trim half instead of clearing all — preserves hot entries
            let keys: Vec<String> = cache.keys().take(cache.len() / 2).cloned().collect();
            for k in keys {
                cache.remove(&k);
            }
        }
    }

    /// Resolve a path to its folder ID, walking from root and caching
    async fn resolve_folder_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if let Some(id) = self.folder_cache.get(&normalized) {
            return Ok(id.clone());
        }

        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_id = self.root_folder_id.clone();
        let mut built_path = String::new();

        for part in parts {
            built_path = format!("{}/{}", built_path, part);

            if let Some(id) = self.folder_cache.get(&built_path) {
                current_id = id.clone();
                continue;
            }

            let url = format!("{}/folders/{}/children", API_BASE, current_id);
            let resp = self.signed_get(&url).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                info!("resolve_folder_id children failed ({}): {}", status, &body[..body.len().min(200)]);
                return Err(ProviderError::NotFound(format!("Path not found: {}", path)));
            }

            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(format!("Read children body: {}", e)))?;
            let folders = Self::parse_folder_list(&body);

            let found = folders.iter().find(|f| {
                f.name.as_deref().unwrap_or("") == part
            });

            match found {
                Some(folder) => {
                    let fid = folder.id.clone().unwrap_or_default();
                    current_id = fid.clone();
                    Self::enforce_cache_limit(&mut self.folder_cache);
                    self.folder_cache.insert(built_path.clone(), fid);
                }
                None => return Err(ProviderError::NotFound(format!("Folder not found: {}", part))),
            }
        }

        Ok(current_id)
    }

    /// Resolve a file path to its file ID
    async fn resolve_file_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if let Some(id) = self.file_cache.get(&normalized) {
            return Ok(id.clone());
        }

        let (parent_path, file_name) = Self::split_path(&normalized);
        let folder_id = self.resolve_folder_id(&parent_path).await?;

        let url = format!("{}/folders/{}/files", API_BASE, folder_id);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            info!("resolve_file_id files failed ({}): {}", status, &body[..body.len().min(200)]);
            return Err(ProviderError::NotFound(format!("File not found: {}", path)));
        }

        let body = resp.text().await
            .map_err(|e| ProviderError::ParseError(format!("Read files body: {}", e)))?;
        let files = Self::parse_file_list(&body);

        Self::enforce_cache_limit(&mut self.file_cache);
        for file in &files {
            if let (Some(name), Some(id)) = (&file.name, &file.id) {
                let fpath = if parent_path == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", parent_path, name)
                };
                self.file_cache.insert(fpath, id.clone());
            }
        }

        self.file_cache
            .get(&normalized)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))
    }

    /// Download file bytes from 4shared (uses retry via signed_get — FS-009)
    async fn download_bytes(&self, file_id: &str) -> Result<Vec<u8>, ProviderError> {
        let url = format!("{}/files/{}/download", API_BASE, file_id);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Download failed ({}): {}", status, body),
            ));
        }

        resp.bytes().await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))
    }

    /// Upload bytes to 4shared folder (FS-009: uses retry)
    #[allow(dead_code)]
    async fn upload_bytes(
        &self,
        folder_id: &str,
        file_name: &str,
        content: Vec<u8>,
    ) -> Result<Option<String>, ProviderError> {
        let sign_url = format!("{}/files", UPLOAD_BASE);
        let extra = [
            ("folderId", folder_id),
            ("fileName", file_name),
        ];
        let auth = oauth1::authorization_header("POST", &sign_url, &self.credentials(), &extra);

        let url = format!(
            "{}/files?folderId={}&fileName={}",
            UPLOAD_BASE,
            folder_id,
            oauth1::percent_encode(file_name)
        );

        let request = self.client
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/octet-stream")
            .body(content)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let resp = send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Upload failed ({}): {}", status, body),
            ));
        }

        let file_id: Option<String> = resp.json::<FourSharedUploadResponse>().await
            .ok()
            .and_then(|r| r.id);

        Ok(file_id)
    }

    /// Extract a JSON array from the body: tries raw array, then wrapper object keys.
    fn extract_json_array(body: &str, keys: &[&str]) -> Option<Vec<serde_json::Value>> {
        let val: serde_json::Value = serde_json::from_str(body).ok()?;

        // Raw array
        if let Some(arr) = val.as_array() {
            return Some(arr.clone());
        }

        // Wrapper object with known keys
        if val.is_object() {
            for key in keys {
                if let Some(arr) = val.get(*key).and_then(|v| v.as_array()) {
                    return Some(arr.clone());
                }
            }
            // Single object → wrap
            return Some(vec![val]);
        }

        None
    }

    /// Parse folder list response with per-entry fallback.
    /// Never fails — returns empty vec on completely unparseable body.
    /// FS-004: Logs skipped entries at debug level with raw JSON for diagnostics.
    fn parse_folder_list(body: &str) -> Vec<FourSharedFolder> {
        // Try direct array parse first (fast path)
        if let Ok(folders) = serde_json::from_str::<Vec<FourSharedFolder>>(body) {
            return folders;
        }

        // Extract array, then parse each entry individually (skip failures)
        if let Some(items) = Self::extract_json_array(body, &["children", "folders", "items", "data"]) {
            let mut folders = Vec::new();
            for (i, item) in items.into_iter().enumerate() {
                match serde_json::from_value::<FourSharedFolder>(item.clone()) {
                    Ok(f) => folders.push(f),
                    Err(e) => {
                        let raw = item.to_string();
                        debug!(
                            "4shared: skipping folder entry {}: {} — raw: {}",
                            i, e, &raw[..raw.len().min(200)]
                        );
                    }
                }
            }
            return folders;
        }

        debug!("4shared: could not parse folder list body: {}", &body[..body.len().min(300)]);
        Vec::new()
    }

    /// Parse file list response with per-entry fallback.
    /// Never fails — returns empty vec on completely unparseable body.
    /// FS-004: Logs skipped entries at debug level with raw JSON for diagnostics.
    fn parse_file_list(body: &str) -> Vec<FourSharedFile> {
        // Try direct array parse first (fast path)
        if let Ok(files) = serde_json::from_str::<Vec<FourSharedFile>>(body) {
            return files;
        }

        // Extract array, then parse each entry individually (skip failures)
        if let Some(items) = Self::extract_json_array(body, &["files", "children", "items", "data"]) {
            let mut files = Vec::new();
            for (i, item) in items.into_iter().enumerate() {
                match serde_json::from_value::<FourSharedFile>(item.clone()) {
                    Ok(f) => files.push(f),
                    Err(e) => {
                        let raw = item.to_string();
                        debug!(
                            "4shared: skipping file entry {}: {} — raw: {}",
                            i, e, &raw[..raw.len().min(200)]
                        );
                    }
                }
            }
            return files;
        }

        debug!("4shared: could not parse file list body: {}", &body[..body.len().min(300)]);
        Vec::new()
    }
}

#[async_trait]
impl StorageProvider for FourSharedProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::FourShared
    }

    fn display_name(&self) -> String {
        "4shared".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        info!("Connecting to 4shared...");

        let url = format!("{}/user", API_BASE);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(
                format!("4shared auth failed ({}): {}", status, body),
            ));
        }

        // Read raw body for robust parsing — 4shared may return unexpected field types
        let body_text = resp.text().await
            .map_err(|e| ProviderError::ParseError(format!("Failed to read response: {}", e)))?;

        let user: FourSharedUser = serde_json::from_str(&body_text)
            .map_err(|e| ProviderError::ParseError(
                format!("Failed to parse user info: {}. Body: {}", e, &body_text[..body_text.len().min(200)])
            ))?;

        self.root_folder_id = user.root_folder_id.unwrap_or_default();
        self.current_folder_id = self.root_folder_id.clone();
        self.account_email = user.email.clone();

        self.folder_cache.insert("/".to_string(), self.root_folder_id.clone());

        self.connected = true;
        info!(
            "Connected to 4shared as {} (root={})",
            user.email.as_deref().unwrap_or("unknown"),
            self.root_folder_id
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.folder_cache.clear();
        self.file_cache.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let normalized = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&normalized).await?;

        let mut entries = Vec::new();

        // 1. List subfolders with pagination (FS-006)
        let mut offset: u32 = 0;
        loop {
            let folders_url = format!(
                "{}/folders/{}/children?offset={}&limit={}",
                API_BASE, folder_id, offset, PAGE_SIZE
            );
            let resp = self.signed_get(&folders_url).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                debug!("4shared list children failed ({}): {}", status, &body[..body.len().min(200)]);
                break;
            }

            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(format!("Read folders body: {}", e)))?;
            let folders = Self::parse_folder_list(&body);
            let page_count = folders.len() as u32;

            for f in &folders {
                // Skip deleted/trashed entries
                if matches!(f.status.as_deref(), Some("deleted") | Some("trashed")) {
                    continue;
                }
                let name = f.name.clone().unwrap_or_default();
                if name.is_empty() { continue; }

                let entry_path = if normalized == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", normalized, name)
                };

                if let Some(ref id) = f.id {
                    Self::enforce_cache_limit(&mut self.folder_cache);
                    self.folder_cache.insert(entry_path.clone(), id.clone());
                }

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir: true,
                    size: 0,
                    modified: f.modified.clone(),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: std::collections::HashMap::new(),
                });
            }

            // FS-006: Stop if we got fewer items than page size (last page)
            if page_count < PAGE_SIZE {
                break;
            }
            offset += page_count;
        }

        // 2. List files with pagination (FS-006)
        offset = 0;
        loop {
            let files_url = format!(
                "{}/folders/{}/files?offset={}&limit={}",
                API_BASE, folder_id, offset, PAGE_SIZE
            );
            let resp = self.signed_get(&files_url).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                debug!("4shared list files failed ({}): {}", status, &body[..body.len().min(200)]);
                break;
            }

            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(format!("Read files body: {}", e)))?;
            let files = Self::parse_file_list(&body);
            let page_count = files.len() as u32;

            for f in &files {
                // Skip deleted/trashed/incomplete entries
                if matches!(f.status.as_deref(), Some("deleted") | Some("trashed") | Some("incomplete")) {
                    continue;
                }
                let name = f.name.clone().unwrap_or_default();
                if name.is_empty() { continue; }

                let entry_path = if normalized == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", normalized, name)
                };

                if let Some(ref id) = f.id {
                    Self::enforce_cache_limit(&mut self.file_cache);
                    self.file_cache.insert(entry_path.clone(), id.clone());
                }

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir: false,
                    size: f.size.unwrap_or(0) as u64,
                    modified: f.modified.clone(),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    // FS-011: Populate MIME type from API response
                    mime_type: f.mime_type.clone(),
                    metadata: std::collections::HashMap::new(),
                });
            }

            // FS-006: Stop if we got fewer items than page size (last page)
            if page_count < PAGE_SIZE {
                break;
            }
            offset += page_count;
        }

        self.current_path = normalized;
        self.current_folder_id = folder_id;

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&normalized).await?;
        self.current_path = normalized;
        self.current_folder_id = folder_id;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let parent = match self.current_path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(idx) => self.current_path[..idx].to_string(),
            None => "/".to_string(),
        };
        self.cd(&parent).await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let file_id = self.resolve_file_id(&resolved).await?;

        // FS-009: Use signed_get which includes retry logic
        let url = format!("{}/files/{}/download", API_BASE, file_id);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Download failed ({}): {}", status, body),
            ));
        }

        // FS-007: Streaming download with progress callback
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let total_size = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let file_id = self.resolve_file_id(&resolved).await?;
        self.download_bytes(&file_id).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(remote_path);
        let (parent_path, file_name) = Self::split_path(&normalized);
        let folder_id = self.resolve_folder_id(&parent_path).await?;

        // FS-007: Get file size for progress reporting
        let file_metadata = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::Other(format!("Read file metadata: {}", e)))?;
        let file_size = file_metadata.len();

        // Streaming upload: read file as a stream instead of loading into memory
        let file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::Other(format!("Open local file: {}", e)))?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let sign_url = format!("{}/files", UPLOAD_BASE);
        let extra = [
            ("folderId", folder_id.as_str()),
            ("fileName", file_name.as_str()),
        ];
        let auth = oauth1::authorization_header("POST", &sign_url, &self.credentials(), &extra);

        let url = format!(
            "{}/files?folderId={}&fileName={}",
            UPLOAD_BASE,
            folder_id,
            oauth1::percent_encode(&file_name)
        );

        // Note: streaming upload cannot use send_with_retry because the body stream
        // is consumed on first attempt. The OAuth-signed URL is still retry-safe for
        // the signing portion; transport-level retries require re-opening the file.
        let resp = self.client
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Upload failed ({}): {}", status, resp_body),
            ));
        }

        let upload_resp_id: Option<String> = resp.json::<FourSharedUploadResponse>().await
            .ok()
            .and_then(|r| r.id);

        if let Some(fid) = upload_resp_id {
            self.file_cache.insert(normalized, fid);
        }

        // FS-007: Report upload completion to progress callback
        if let Some(ref cb) = on_progress {
            cb(file_size, file_size);
        }

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(path);
        let (parent_path, folder_name) = Self::split_path(&normalized);
        let parent_id = self.resolve_folder_id(&parent_path).await?;

        let url = format!("{}/folders", API_BASE);
        let form = [
            ("parentId", parent_id.as_str()),
            ("name", folder_name.as_str()),
        ];
        let resp = self.signed_post_form(&url, &form).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(
                format!("Create folder failed ({}): {}", status, body),
            ));
        }

        if let Ok(body) = resp.text().await {
            if let Ok(folder) = serde_json::from_str::<FourSharedFolder>(&body) {
                if let Some(id) = folder.id {
                    self.folder_cache.insert(normalized, id);
                }
            }
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(path);
        let file_id = self.resolve_file_id(&normalized).await?;
        let url = format!("{}/files/{}", API_BASE, file_id);
        let resp = self.signed_delete(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(
                format!("Delete failed ({}): {}", status, body),
            ));
        }
        self.file_cache.remove(&normalized);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&normalized).await?;
        let url = format!("{}/folders/{}", API_BASE, folder_id);
        let resp = self.signed_delete(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(
                format!("Delete folder failed ({}): {}", status, body),
            ));
        }
        self.folder_cache.remove(&normalized);
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // 4shared DELETE /folders/{id} is recursive by default
        self.rmdir(path).await
    }

    async fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), ProviderError> {
        let old_normalized = self.resolve_path(old_path);
        let new_normalized = self.resolve_path(new_path);
        let (old_parent, old_name) = Self::split_path(&old_normalized);
        let (new_parent, new_name) = Self::split_path(&new_normalized);

        let is_cross_folder = old_parent != new_parent;

        // Try as file first, then as folder
        if let Ok(file_id) = self.resolve_file_id(&old_normalized).await {
            // Step 1: Move to new folder if cross-folder operation
            if is_cross_folder {
                let target_folder_id = self.resolve_folder_id(&new_parent).await?;
                // 4shared move API expects folderId as query param, not form body
                let sign_url = format!("{}/files/{}/move", API_BASE, file_id);
                let extra = [("folderId", target_folder_id.as_str())];
                let auth = oauth1::authorization_header("PUT", &sign_url, &self.credentials(), &extra);
                let full_url = format!(
                    "{}/files/{}/move?folderId={}",
                    API_BASE, file_id, oauth1::percent_encode(&target_folder_id)
                );
                let request = self.client
                    .put(&full_url)
                    .header("Authorization", &auth)
                    .build()
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
                let resp = send_with_retry(&self.client, request, &Self::retry_config())
                    .await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(
                        format!("Move file failed ({}): {}", status, &body[..body.len().min(300)]),
                    ));
                }
                info!("4shared moved file {} to folder {}", old_normalized, new_parent);
            }

            // Step 2: Rename if the name changed
            if old_name != new_name {
                let url = format!("{}/files/{}", API_BASE, file_id);
                let form = [("name", new_name.as_str())];
                let resp = self.signed_put_form(&url, &form).await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(
                        format!("Rename failed ({}): {}", status, body),
                    ));
                }
            }

            if let Some(id) = self.file_cache.remove(&old_normalized) {
                self.file_cache.insert(new_normalized, id);
            }
        } else {
            let folder_id = self.resolve_folder_id(&old_normalized).await?;

            // Step 1: Move to new parent folder if cross-folder operation
            if is_cross_folder {
                let target_folder_id = self.resolve_folder_id(&new_parent).await?;
                // 4shared move API expects folderId as query param, not form body
                let sign_url = format!("{}/folders/{}/move", API_BASE, folder_id);
                let extra = [("folderId", target_folder_id.as_str())];
                let auth = oauth1::authorization_header("PUT", &sign_url, &self.credentials(), &extra);
                let full_url = format!(
                    "{}/folders/{}/move?folderId={}",
                    API_BASE, folder_id, oauth1::percent_encode(&target_folder_id)
                );
                let request = self.client
                    .put(&full_url)
                    .header("Authorization", &auth)
                    .build()
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
                let resp = send_with_retry(&self.client, request, &Self::retry_config())
                    .await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(
                        format!("Move folder failed ({}): {}", status, &body[..body.len().min(300)]),
                    ));
                }
                info!("4shared moved folder {} to {}", old_normalized, new_parent);
            }

            // Step 2: Rename if the name changed
            if old_name != new_name {
                let url = format!("{}/folders/{}", API_BASE, folder_id);
                let form = [("name", new_name.as_str())];
                let resp = self.signed_put_form(&url, &form).await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(
                        format!("Rename folder failed ({}): {}", status, body),
                    ));
                }
            }

            if let Some(id) = self.folder_cache.remove(&old_normalized) {
                self.folder_cache.insert(new_normalized, id);
            }
        }

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let normalized = self.resolve_path(path);

        // Try as file
        if let Ok(file_id) = self.resolve_file_id(&normalized).await {
            let url = format!("{}/files/{}", API_BASE, file_id);
            let resp = self.signed_get(&url).await?;

            if resp.status().is_success() {
                let body = resp.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                let file: FourSharedFile = serde_json::from_str(&body)
                    .map_err(|e| ProviderError::ParseError(
                        format!("stat file parse: {}", e)
                    ))?;

                return Ok(RemoteEntry {
                    name: file.name.unwrap_or_default(),
                    path: normalized,
                    is_dir: false,
                    size: file.size.unwrap_or(0) as u64,
                    modified: file.modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    // FS-011: Populate MIME type from API response
                    mime_type: file.mime_type,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }

        // Try as folder
        let folder_id = self.resolve_folder_id(&normalized).await?;
        let url = format!("{}/folders/{}", API_BASE, folder_id);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(format!("Not found: {}", path)));
        }

        let body = resp.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let folder: FourSharedFolder = serde_json::from_str(&body)
            .map_err(|e| ProviderError::ParseError(
                format!("stat folder parse: {}", e)
            ))?;

        Ok(RemoteEntry {
            name: folder.name.unwrap_or_default(),
            path: normalized,
            is_dir: true,
            size: 0,
            modified: folder.modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        })
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
        Ok("4shared REST API v1.2 (OAuth 1.0a)".to_string())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/user", API_BASE);
        let resp = self.signed_get(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other("Failed to get storage info".to_string()));
        }

        let body = resp.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let user: FourSharedUser = serde_json::from_str(&body)
            .map_err(|e| ProviderError::ParseError(
                format!("storage_info parse: {}", e)
            ))?;

        Ok(StorageInfo {
            used: user.used_space.unwrap_or(0) as u64,
            total: user.total_space.unwrap_or(0) as u64,
            free: user.free_space.unwrap_or(0) as u64,
        })
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // FS-005: Apply resolve_path to the search path parameter
        let _resolved = self.resolve_path(path);
        info!("4shared find: searching for '{}' (scope: {})", pattern, _resolved);

        // FS-009: Use signed_get with retry for the search request.
        // The 4shared search API requires OAuth-signed query parameters.
        let base_url = format!("{}/files", API_BASE);
        let extra = [("searchName", pattern)];
        let auth = oauth1::authorization_header("GET", &base_url, &self.credentials(), &extra);

        // Build full URL with query parameter
        let url = format!(
            "{}/files?searchName={}",
            API_BASE,
            oauth1::percent_encode(pattern)
        );

        let request = self.client
            .get(&url)
            .header("Authorization", &auth)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let resp = send_with_retry(&self.client, request, &Self::retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Search failed ({}): {}", status, &body[..body.len().min(300)]
            )));
        }

        let body = resp.text().await
            .map_err(|e| ProviderError::ParseError(format!("Read search body: {}", e)))?;
        let files = Self::parse_file_list(&body);

        let entries = files.iter()
            .filter(|f| !matches!(f.status.as_deref(), Some("deleted") | Some("trashed") | Some("incomplete")))
            .filter_map(|f| {
                let name = f.name.as_ref()?;
                if name.is_empty() { return None; }
                Some(RemoteEntry {
                    name: name.clone(),
                    path: String::new(), // Search results don't have reliable full paths
                    is_dir: false,
                    size: f.size.unwrap_or(0) as u64,
                    modified: f.modified.clone(),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    // FS-011: Populate MIME type from API response
                    mime_type: f.mime_type.clone(),
                    metadata: {
                        let mut m = std::collections::HashMap::new();
                        if let Some(ref id) = f.id {
                            m.insert("id".to_string(), id.clone());
                        }
                        m
                    },
                })
            })
            .collect::<Vec<_>>();

        info!("4shared find '{}': {} results", pattern, entries.len());
        Ok(entries)
    }

    // FS-012: TODO — 4shared supports file share links via GET /v1_2/files/{id}/download
    //         (returns downloadPage URL). Implement create_share_link() in future.

    // FS-013: TODO — 4shared trash API: POST /v1_2/files/{id}/trash and
    //         POST /v1_2/folders/{id}/trash for soft-delete. Implement list_trash(),
    //         restore_from_trash(), permanent_delete() in future.

    // FS-014: TODO — 4shared does not support file versioning. No version API available.
}
