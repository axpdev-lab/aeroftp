//! 4shared Cloud Storage Provider
//!
//! Implements StorageProvider for 4shared using their REST API v1.2.
//! Uses OAuth 1.0a (HMAC-SHA1) for authentication.

use async_trait::async_trait;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo,
};
use super::oauth1::{self, OAuth1Credentials};
use super::types::FourSharedConfig;

/// 4shared API base URL
const API_BASE: &str = "https://api.4shared.com/v1_2";
/// 4shared upload URL
const UPLOAD_BASE: &str = "https://upload.4shared.com/v1_2";

// ============ Custom Deserializers ============

/// Deserialize a value that may be either a number or a string containing a number.
/// 4shared API sometimes returns Long fields as JSON strings.
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

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as i64))
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v as i64))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() {
                return Ok(None);
            }
            v.parse::<i64>().map(Some).map_err(de::Error::custom)
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
        Self {
            config,
            client: reqwest::Client::new(),
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
            consumer_secret: self.config.consumer_secret.clone(),
            token: self.config.access_token.clone(),
            token_secret: self.config.access_token_secret.clone(),
        }
    }

    /// Make a signed GET request
    async fn signed_get(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("GET", url, &self.credentials(), &[]);
        self.client
            .get(url)
            .header("Authorization", &auth)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed POST request (form-urlencoded body)
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

        self.client
            .post(url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed DELETE request
    async fn signed_delete(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let auth = oauth1::authorization_header("DELETE", url, &self.credentials(), &[]);
        self.client
            .delete(url)
            .header("Authorization", &auth)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Make a signed PUT request with JSON body
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

        self.client
            .put(url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
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

    /// Download file bytes from 4shared
    async fn download_bytes(&self, file_id: &str) -> Result<Vec<u8>, ProviderError> {
        let url = format!("{}/files/{}/download", API_BASE, file_id);
        let auth = oauth1::authorization_header("GET", &url, &self.credentials(), &[]);

        let resp = self.client
            .get(&url)
            .header("Authorization", &auth)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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

    /// Upload bytes to 4shared folder
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

        let resp = self.client
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/octet-stream")
            .body(content)
            .send()
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
                    Err(e) => info!(
                        "4shared: skipping folder entry {}: {} — raw: {}",
                        i, e, &item.to_string()[..item.to_string().len().min(200)]
                    ),
                }
            }
            return folders;
        }

        info!("4shared: could not parse folder list body: {}", &body[..body.len().min(300)]);
        Vec::new()
    }

    /// Parse file list response with per-entry fallback.
    /// Never fails — returns empty vec on completely unparseable body.
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
                    Err(e) => info!(
                        "4shared: skipping file entry {}: {} — raw: {}",
                        i, e, &item.to_string()[..item.to_string().len().min(200)]
                    ),
                }
            }
            return files;
        }

        info!("4shared: could not parse file list body: {}", &body[..body.len().min(300)]);
        Vec::new()
    }
}

#[async_trait]
impl StorageProvider for FourSharedProvider {
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

        // 1. List subfolders
        let folders_url = format!("{}/folders/{}/children", API_BASE, folder_id);
        let resp = self.signed_get(&folders_url).await?;

        if resp.status().is_success() {
            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(format!("Read folders body: {}", e)))?;
            let folders = Self::parse_folder_list(&body);

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
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            info!("4shared list children failed ({}): {}", status, &body[..body.len().min(200)]);
        }

        // 2. List files
        let files_url = format!("{}/folders/{}/files", API_BASE, folder_id);
        let resp = self.signed_get(&files_url).await?;

        if resp.status().is_success() {
            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(format!("Read files body: {}", e)))?;
            let files = Self::parse_file_list(&body);

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
                    mime_type: None,
                    metadata: std::collections::HashMap::new(),
                });
            }
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            info!("4shared list files failed ({}): {}", status, &body[..body.len().min(200)]);
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
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let file_id = self.resolve_file_id(&resolved).await?;
        let bytes = self.download_bytes(&file_id).await?;
        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::Other(format!("Write local file: {}", e)))?;
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
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let normalized = self.resolve_path(remote_path);
        let (parent_path, file_name) = Self::split_path(&normalized);
        let folder_id = self.resolve_folder_id(&parent_path).await?;

        let content = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::Other(format!("Read local file: {}", e)))?;

        if let Some(file_id) = self.upload_bytes(&folder_id, &file_name, content).await? {
            self.file_cache.insert(normalized, file_id);
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
        let (_, new_name) = Self::split_path(&new_normalized);

        // Try as file first, then as folder
        if let Ok(file_id) = self.resolve_file_id(&old_normalized).await {
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

            if let Some(id) = self.file_cache.remove(&old_normalized) {
                self.file_cache.insert(new_normalized, id);
            }
        } else {
            let folder_id = self.resolve_folder_id(&old_normalized).await?;
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
                    mime_type: None,
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
}
