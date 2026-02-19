//! Google Drive Storage Provider
//!
//! Implements StorageProvider for Google Drive using the Drive API v3.
//! Uses OAuth2 for authentication.

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, ProviderConfig, StorageInfo,
    sanitize_api_error,
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
};

/// Google Workspace MIME type → export format mapping
const WORKSPACE_EXPORT_MAP: &[(&str, &str, &str)] = &[
    ("application/vnd.google-apps.document", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ".docx"),
    ("application/vnd.google-apps.spreadsheet", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ".xlsx"),
    ("application/vnd.google-apps.presentation", "application/vnd.openxmlformats-officedocument.presentationml.presentation", ".pptx"),
    ("application/vnd.google-apps.drawing", "application/pdf", ".pdf"),
    ("application/vnd.google-apps.jam", "application/pdf", ".pdf"),
];

/// Google Drive API base URL
const DRIVE_API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

/// Google Drive file metadata from API
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct DriveFile {
    id: String,
    name: String,
    mime_type: String,
    #[serde(default)]
    size: Option<String>,
    modified_time: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    trashed: bool,
}

/// Google Drive file list response
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFileList {
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

/// Google Drive provider configuration
#[derive(Debug, Clone)]
pub struct GoogleDriveConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl GoogleDriveConfig {
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config.extra.get("client_id")
            .ok_or_else(|| ProviderError::Other("Missing client_id".to_string()))?;
        let client_secret = config.extra.get("client_secret")
            .ok_or_else(|| ProviderError::Other("Missing client_secret".to_string()))?;

        Ok(Self::new(client_id, client_secret))
    }
}

/// Google Drive Storage Provider
pub struct GoogleDriveProvider {
    config: GoogleDriveConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_folder_id: String,
    current_path: String,
    /// Cache: path -> (folder_id, last_access_counter) — LRU eviction by access counter
    folder_cache: HashMap<String, (String, u64)>,
    /// Monotonic access counter for LRU eviction
    cache_access_counter: u64,
    /// Authenticated user email
    account_email: Option<String>,
}

impl GoogleDriveProvider {
    pub fn new(config: GoogleDriveConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            connected: false,
            current_folder_id: "root".to_string(),
            current_path: "/".to_string(),
            folder_cache: HashMap::new(),
            cache_access_counter: 0,
            account_email: None,
        }
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::google(&self.config.client_id, &self.config.client_secret)
    }

    /// Get authorization header
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        let token = self.oauth_manager.get_valid_token(&self.oauth_config()).await?;
        HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
            .map_err(|e| ProviderError::Other(format!("Invalid token: {}", e)))
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager.has_tokens(OAuthProvider::Google)
    }

    /// Start OAuth flow - returns URL to open (called via oauth2_start_auth command)
    #[allow(dead_code)]
    pub async fn start_auth(&self) -> Result<(String, String), ProviderError> {
        self.oauth_manager.start_auth_flow(&self.oauth_config()).await
    }

    /// Complete OAuth flow with code (called via oauth2_connect command)
    #[allow(dead_code)]
    pub async fn complete_auth(&self, code: &str, state: &str) -> Result<(), ProviderError> {
        self.oauth_manager.complete_auth_flow(&self.oauth_config(), code, state).await?;
        Ok(())
    }

    /// List files in a folder by ID
    async fn list_folder(&self, folder_id: &str) -> Result<Vec<DriveFile>, ProviderError> {
        let mut all_files = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "{}/files?q='{}'+in+parents+and+trashed=false&fields=files(id,name,mimeType,size,modifiedTime,parents),nextPageToken&pageSize=1000",
                DRIVE_API_BASE, folder_id
            );

            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("API error {}: {}", status, sanitize_api_error(&text))));
            }

            let list: DriveFileList = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            all_files.extend(list.files);

            match list.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        Ok(all_files)
    }

    /// Get file by ID (for future stat implementation)
    #[allow(dead_code)]
    async fn get_file(&self, file_id: &str) -> Result<DriveFile, ProviderError> {
        let url = format!(
            "{}/files/{}?fields=id,name,mimeType,size,modifiedTime,parents",
            DRIVE_API_BASE, file_id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::NotFound(file_id.to_string()));
        }

        response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))
    }

    /// Find file/folder by name in parent
    async fn find_by_name(&self, name: &str, parent_id: &str) -> Result<Option<DriveFile>, ProviderError> {
        let escaped_name = name.replace('\\', "\\\\").replace('\'', "\\'");
        let query = format!(
            "name='{}' and '{}' in parents and trashed=false",
            escaped_name, parent_id
        );

        let url = format!(
            "{}/files?q={}&fields=files(id,name,mimeType,size,modifiedTime,parents)",
            DRIVE_API_BASE, urlencoding::encode(&query)
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let list: DriveFileList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.files.into_iter().next())
    }

    /// P2-6: LRU eviction — remove least-recently-accessed half when cache exceeds max
    fn trim_cache_if_needed(&mut self) {
        const MAX_CACHE_SIZE: usize = 10_000;
        if self.folder_cache.len() > MAX_CACHE_SIZE {
            // Sort by access counter (ascending = least recently used first)
            let mut entries: Vec<(String, u64)> = self.folder_cache.iter()
                .map(|(k, (_, counter))| (k.clone(), *counter))
                .collect();
            entries.sort_by_key(|(_, counter)| *counter);
            // Remove oldest half
            let to_remove = entries.len() / 2;
            for (key, _) in entries.into_iter().take(to_remove) {
                self.folder_cache.remove(&key);
            }
        }
    }

    /// Resolve path to folder ID
    async fn resolve_path(&mut self, path: &str) -> Result<String, ProviderError> {
        // Check cache (update LRU counter on hit)
        if let Some((id, counter)) = self.folder_cache.get_mut(path) {
            self.cache_access_counter += 1;
            *counter = self.cache_access_counter;
            return Ok(id.clone());
        }

        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok("root".to_string());
        }

        let mut current_id = "root".to_string();
        let mut current_path = String::new();

        for part in path.split('/') {
            if part.is_empty() {
                continue;
            }

            current_path.push('/');
            current_path.push_str(part);

            // Check cache for intermediate paths (update LRU counter on hit)
            if let Some((id, counter)) = self.folder_cache.get_mut(&current_path) {
                self.cache_access_counter += 1;
                *counter = self.cache_access_counter;
                current_id = id.clone();
                continue;
            }

            let file = self.find_by_name(part, &current_id).await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;

            if file.mime_type != "application/vnd.google-apps.folder" {
                return Err(ProviderError::InvalidPath(format!("{} is not a folder", part)));
            }

            current_id = file.id;
            self.trim_cache_if_needed();
            self.cache_access_counter += 1;
            self.folder_cache.insert(current_path.clone(), (current_id.clone(), self.cache_access_counter));
        }

        Ok(current_id)
    }

    /// Check if MIME type is a Google Workspace type and return export MIME + extension
    fn workspace_export_info(mime_type: &str) -> Option<(&'static str, &'static str)> {
        WORKSPACE_EXPORT_MAP.iter()
            .find(|(ws_mime, _, _)| *ws_mime == mime_type)
            .map(|(_, export_mime, ext)| (*export_mime, *ext))
    }

    /// Download a file, auto-detecting Workspace files and exporting them
    async fn download_file_by_id(
        &self,
        file_id: &str,
        mime_type: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        let url = if let Some((export_mime, _)) = Self::workspace_export_info(mime_type) {
            // Workspace file: use export endpoint
            format!(
                "{}/files/{}/export?mimeType={}",
                DRIVE_API_BASE, file_id, urlencoding::encode(export_mime)
            )
        } else {
            // Regular file: use alt=media
            format!("{}/files/{}?alt=media", DRIVE_API_BASE, file_id)
        };

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Download failed {}: {}", status, sanitize_api_error(&text))));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        Ok(bytes.to_vec())
    }

    /// List files in the trash
    #[allow(dead_code)]
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut all_files = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "{}/files?q=trashed=true&fields=files(id,name,mimeType,size,modifiedTime,parents),nextPageToken&pageSize=1000",
                DRIVE_API_BASE
            );

            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("List trash error {}: {}", status, sanitize_api_error(&text))));
            }

            let list: DriveFileList = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            for file in &list.files {
                all_files.push(self.to_remote_entry(file, "/Trash"));
            }

            match list.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        info!("Listed {} trashed files", all_files.len());
        Ok(all_files)
    }

    /// Move a file to trash by path
    #[allow(dead_code)]
    pub async fn trash_file(&mut self, path: &str) -> Result<(), ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!("{}/files/{}", DRIVE_API_BASE, file.id);
        let body = serde_json::json!({ "trashed": true });

        let response = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Trash failed: {}", sanitize_api_error(&text))));
        }

        info!("Trashed: {}", path);
        Ok(())
    }

    /// Restore a file from trash by file ID
    #[allow(dead_code)]
    pub async fn restore_from_trash(&mut self, file_id: &str) -> Result<(), ProviderError> {
        let url = format!("{}/files/{}", DRIVE_API_BASE, file_id);
        let body = serde_json::json!({ "trashed": false });

        let response = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Restore from trash failed: {}", sanitize_api_error(&text))));
        }

        info!("Restored from trash: {}", file_id);
        Ok(())
    }

    /// Permanently delete a file by file ID (bypasses trash)
    #[allow(dead_code)]
    pub async fn permanent_delete(&mut self, file_id: &str) -> Result<(), ProviderError> {
        let url = format!("{}/files/{}", DRIVE_API_BASE, file_id);

        let response = self.client
            .delete(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() && response.status().as_u16() != 404 {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Permanent delete failed: {}", sanitize_api_error(&text))));
        }

        info!("Permanently deleted: {}", file_id);
        Ok(())
    }

    /// Convert DriveFile to RemoteEntry
    fn to_remote_entry(&self, file: &DriveFile, path_prefix: &str) -> RemoteEntry {
        let is_dir = file.mime_type == "application/vnd.google-apps.folder";
        let size = file.size.as_ref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        
        let path = if path_prefix == "/" {
            format!("/{}", file.name)
        } else {
            format!("{}/{}", path_prefix, file.name)
        };

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), file.id.clone());
        metadata.insert("mimeType".to_string(), file.mime_type.clone());
        if let Some((export_mime, ext)) = Self::workspace_export_info(&file.mime_type) {
            metadata.insert("exportMimeType".to_string(), export_mime.to_string());
            metadata.insert("exportExtension".to_string(), ext.to_string());
            metadata.insert("isWorkspaceFile".to_string(), "true".to_string());
        }

        RemoteEntry {
            name: file.name.clone(),
            path,
            is_dir,
            size,
            modified: file.modified_time.clone(),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: Some(file.mime_type.clone()),
            metadata,
        }
    }
}

#[async_trait]
impl StorageProvider for GoogleDriveProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::GoogleDrive
    }

    fn display_name(&self) -> String {
        "Google Drive".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.is_authenticated() {
            return Err(ProviderError::AuthenticationFailed(
                "Not authenticated. Call start_auth() first.".to_string()
            ));
        }

        // Validate token by making a simple API call
        let _ = self.list_folder("root").await?;

        self.connected = true;
        self.current_folder_id = "root".to_string();
        self.current_path = "/".to_string();

        // Fetch user email from Google Drive about endpoint
        if let Ok(auth) = self.auth_header().await {
            if let Ok(resp) = self.client
                .get("https://www.googleapis.com/drive/v3/about?fields=user")
                .header(AUTHORIZATION, auth)
                .send().await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(email) = body["user"]["emailAddress"].as_str() {
                        self.account_email = Some(email.to_string());
                    }
                }
            }
        }

        info!("Connected to Google Drive");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.folder_cache.clear();
        info!("Disconnected from Google Drive");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let folder_id = if path == "." || path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(path).await?
        };

        let files = self.list_folder(&folder_id).await?;
        
        let path_prefix = if path == "." { &self.current_path } else { path };
        
        Ok(files.iter()
            .map(|f| self.to_remote_entry(f, path_prefix))
            .collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self.current_path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            format!("/{}", parts.join("/"))
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let folder_id = self.resolve_path(&new_path).await?;
        
        self.current_folder_id = folder_id;
        self.current_path = if new_path.is_empty() { "/".to_string() } else { new_path };
        
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            "root".to_string()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // For Workspace files, append the export extension to the local path
        let actual_local_path = if let Some((_, ext)) = Self::workspace_export_info(&file.mime_type) {
            if !local_path.ends_with(ext) {
                format!("{}{}", local_path, ext)
            } else {
                local_path.to_string()
            }
        } else {
            local_path.to_string()
        };

        let url = if let Some((export_mime, _)) = Self::workspace_export_info(&file.mime_type) {
            format!(
                "{}/files/{}/export?mimeType={}",
                DRIVE_API_BASE, file.id, urlencoding::encode(export_mime)
            )
        } else {
            format!("{}/files/{}?alt=media", DRIVE_API_BASE, file.id)
        };

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Download failed {}: {}", status, sanitize_api_error(&text))));
        }

        let mut stream = response.bytes_stream();
        let mut out_file = tokio::fs::File::create(&actual_local_path).await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            out_file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        }

        info!("Downloaded {} to {}", remote_path, actual_local_path);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            "root".to_string()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        self.download_file_by_id(&file.id, &file.mime_type).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let total_size = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?.len();

        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        const RESUMABLE_THRESHOLD: u64 = 5 * 1024 * 1024; // 5MB

        if total_size > RESUMABLE_THRESHOLD {
            // Resumable upload for large files — stream from file handle
            let metadata = serde_json::json!({
                "name": file_name,
                "parents": [parent_id]
            });

            // Step 1: Initiate resumable upload session
            let init_url = format!(
                "{}/files?uploadType=resumable",
                UPLOAD_API_BASE
            );

            let init_response = self.client
                .post(&init_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/json; charset=UTF-8")
                .header("X-Upload-Content-Length", total_size.to_string())
                .body(metadata.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !init_response.status().is_success() {
                let text = init_response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Resumable init failed: {}", sanitize_api_error(&text))));
            }

            let session_uri = init_response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ProviderError::Other("No upload session URI returned".to_string()))?
                .to_string();

            // Step 2: Upload in 10MB chunks from file handle
            const CHUNK_SIZE: usize = 10 * 1024 * 1024;
            let mut file = tokio::fs::File::open(local_path).await
                .map_err(|e| ProviderError::Other(format!("Open file error: {}", e)))?;
            let mut offset: u64 = 0;

            while offset < total_size {
                use tokio::io::AsyncReadExt;
                let remaining = (total_size - offset) as usize;
                let this_chunk = std::cmp::min(CHUNK_SIZE, remaining);
                let mut chunk = vec![0u8; this_chunk];
                file.read_exact(&mut chunk).await
                    .map_err(|e| ProviderError::Other(format!("Read chunk error: {}", e)))?;

                let end = offset + this_chunk as u64;
                let range = format!("bytes {}-{}/{}", offset, end - 1, total_size);

                let chunk_response = self.client
                    .put(&session_uri)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header("Content-Range", &range)
                    .body(chunk)
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                let status = chunk_response.status().as_u16();
                if status != 200 && status != 201 && status != 308 {
                    let text = chunk_response.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(format!("Chunk upload failed: {}", sanitize_api_error(&text))));
                }

                offset = end;
                if let Some(ref cb) = on_progress {
                    cb(offset, total_size);
                }
            }
        } else {
            // Simple multipart upload for small files (<=5MB, OK to buffer)
            let content = tokio::fs::read(local_path).await
                .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

            let metadata = serde_json::json!({
                "name": file_name,
                "parents": [parent_id]
            });

            let boundary = "aeroftp_boundary";
            let mut body = Vec::new();
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
            body.extend_from_slice(metadata.to_string().as_bytes());
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
            body.extend_from_slice(&content);
            body.extend_from_slice(format!("\r\n--{}--", boundary).as_bytes());

            let url = format!("{}/files?uploadType=multipart", UPLOAD_API_BASE);

            let response = self.client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, format!("multipart/related; boundary={}", boundary))
                .body(body)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Upload failed: {}", sanitize_api_error(&text))));
            }

            if let Some(ref cb) = on_progress {
                cb(total_size, total_size);
            }
        }

        info!("Uploaded {} to {}", local_path, remote_path);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, folder_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let metadata = serde_json::json!({
            "name": folder_name,
            "mimeType": "application/vnd.google-apps.folder",
            "parents": [parent_id]
        });

        let url = format!("{}/files", DRIVE_API_BASE);
        
        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(metadata.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("mkdir failed: {}", sanitize_api_error(&text))));
        }

        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!("{}/files/{}", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .delete(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() && response.status().as_u16() != 404 {
            return Err(ProviderError::Other(format!("Delete failed: {}", response.status())));
        }

        info!("Deleted: {}", path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Google Drive deletes folders with contents by default
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = from.trim_matches('/');
        let (from_parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let from_parent_id = if from_parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(from_parent_path).await?
        };

        let file = self.find_by_name(file_name, &from_parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        let to_path = to.trim_matches('/');
        let (to_parent_path, new_name) = if let Some(pos) = to_path.rfind('/') {
            (&to_path[..pos], &to_path[pos + 1..])
        } else {
            ("", to_path)
        };

        let to_parent_id = if to_parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(to_parent_path).await?
        };

        // Determine if this is a cross-folder move or a simple rename
        let is_move = from_parent_id != to_parent_id;

        let metadata = serde_json::json!({
            "name": new_name
        });

        let mut url = format!("{}/files/{}", DRIVE_API_BASE, file.id);

        if is_move {
            // Add move query parameters: addParents and removeParents
            url = format!(
                "{}?addParents={}&removeParents={}",
                url, urlencoding::encode(&to_parent_id), urlencoding::encode(&from_parent_id)
            );
        }

        let response = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(metadata.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Rename/move failed: {}", sanitize_api_error(&text))));
        }

        info!("Renamed {} to {}", from, to);
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        Ok(self.to_remote_entry(&file, parent_path))
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
        // No-op for REST API
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Google Drive API v3".to_string())
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        // Resolve path to file ID
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // Create "anyone with link can view" permission
        // GAP-A12: Support expires_in_secs via Google Drive expirationTime
        let mut permission = serde_json::json!({
            "role": "reader",
            "type": "anyone"
        });
        if let Some(secs) = _expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            permission["expirationTime"] = serde_json::Value::String(
                expires_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            );
        }

        let perm_url = format!("{}/files/{}/permissions", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .post(&perm_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(permission.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Failed to create share permission: {} - {}", status, sanitize_api_error(&text))));
        }

        // Get the web view link
        let file_url = format!("{}/files/{}?fields=webViewLink", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .get(&file_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            // Fallback to direct link
            return Ok(format!("https://drive.google.com/file/d/{}/view?usp=sharing", file.id));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct FileLink {
            web_view_link: Option<String>,
        }

        let link_response: FileLink = response.json().await
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        let url = link_response.web_view_link
            .unwrap_or_else(|| format!("https://drive.google.com/file/d/{}/view?usp=sharing", file.id));

        info!("Created share link for {}: {}", path, url);
        Ok(url)
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Resolve source file
        let from_path = from.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        // Resolve destination parent
        let to_path = to.trim_matches('/');
        let (to_parent, to_name) = if let Some(pos) = to_path.rfind('/') {
            (&to_path[..pos], &to_path[pos + 1..])
        } else {
            ("", to_path)
        };

        let to_parent_id = if to_parent.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(to_parent).await?
        };

        let metadata = serde_json::json!({
            "name": to_name,
            "parents": [to_parent_id]
        });

        let url = format!("{}/files/{}/copy", DRIVE_API_BASE, file.id);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(metadata.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Copy failed: {}", sanitize_api_error(&text))));
        }

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Google Drive API supports native search via q parameter
        // Search both filename and full-text content for comprehensive results
        let escaped = pattern.replace("'", "\\'");
        let query = format!(
            "(name contains '{}' or fullText contains '{}') and trashed=false",
            escaped, escaped
        );

        let mut all_entries = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "{}/files?q={}&fields=files(id,name,mimeType,size,modifiedTime,parents),nextPageToken&pageSize=200",
                DRIVE_API_BASE, urlencoding::encode(&query)
            );

            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Search failed: {}", sanitize_api_error(&text))));
            }

            let list: DriveFileList = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            for file in &list.files {
                all_entries.push(self.to_remote_entry(file, path));
            }

            match list.next_page_token {
                Some(token) if all_entries.len() < 500 => page_token = Some(token),
                _ => break,
            }
        }

        Ok(all_entries)
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/about?fields=storageQuota", DRIVE_API_BASE);

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("About failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Quota {
            limit: Option<String>,
            usage: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AboutResponse {
            storage_quota: Quota,
        }

        let about: AboutResponse = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let total = about.storage_quota.limit
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let used = about.storage_quota.usage
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<super::FileVersion>, ProviderError> {
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!(
            "{}/files/{}/revisions?fields=revisions(id,modifiedTime,size,lastModifyingUser)",
            DRIVE_API_BASE, file.id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("List revisions failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct User {
            display_name: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Revision {
            id: String,
            modified_time: Option<String>,
            size: Option<String>,
            last_modifying_user: Option<User>,
        }
        #[derive(Deserialize)]
        struct RevisionList {
            revisions: Vec<Revision>,
        }

        let list: RevisionList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.revisions.iter().map(|r| super::FileVersion {
            id: r.id.clone(),
            modified: r.modified_time.clone(),
            size: r.size.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0),
            modified_by: r.last_modifying_user.as_ref().and_then(|u| u.display_name.clone()),
        }).collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!(
            "{}/files/{}/revisions/{}?alt=media",
            DRIVE_API_BASE, file.id, version_id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Download revision failed: {}", sanitize_api_error(&text))));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!(
            "{}/files/{}?fields=thumbnailLink",
            DRIVE_API_BASE, file.id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Get thumbnail failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ThumbResponse {
            thumbnail_link: Option<String>,
        }

        let result: ThumbResponse = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        result.thumbnail_link
            .ok_or_else(|| ProviderError::NotFound("No thumbnail available".to_string()))
    }

    fn supports_permissions(&self) -> bool {
        true
    }

    async fn list_permissions(&mut self, path: &str) -> Result<Vec<super::SharePermission>, ProviderError> {
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let url = format!(
            "{}/files/{}/permissions?fields=permissions(id,role,type,emailAddress)",
            DRIVE_API_BASE, file.id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("List permissions failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Permission {
            role: String,
            #[serde(rename = "type")]
            perm_type: String,
            email_address: Option<String>,
        }
        #[derive(Deserialize)]
        struct PermList {
            permissions: Vec<Permission>,
        }

        let list: PermList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.permissions.iter().map(|p| super::SharePermission {
            role: p.role.clone(),
            target_type: p.perm_type.clone(),
            target: p.email_address.clone().unwrap_or_default(),
        }).collect())
    }

    async fn add_permission(
        &mut self,
        path: &str,
        permission: &super::SharePermission,
    ) -> Result<(), ProviderError> {
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        let mut body = serde_json::json!({
            "role": permission.role,
            "type": permission.target_type,
        });

        if !permission.target.is_empty() {
            body["emailAddress"] = serde_json::Value::String(permission.target.clone());
        }

        let url = format!("{}/files/{}/permissions", DRIVE_API_BASE, file.id);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Add permission failed: {}", sanitize_api_error(&text))));
        }

        Ok(())
    }

    fn supports_change_tracking(&self) -> bool {
        true
    }

    async fn get_change_token(&mut self) -> Result<String, ProviderError> {
        let url = format!("{}/changes/startPageToken", DRIVE_API_BASE);

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Get start page token failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct StartPageToken {
            start_page_token: String,
        }

        let result: StartPageToken = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(result.start_page_token)
    }

    async fn list_changes(&mut self, page_token: &str) -> Result<(Vec<super::ChangeEntry>, String), ProviderError> {
        let mut all_changes = Vec::new();
        let mut current_token = page_token.to_string();

        loop {
            let url = format!(
                "{}/changes?pageToken={}&fields=changes(fileId,file(name,mimeType,trashed),removed,time),newStartPageToken,nextPageToken&pageSize=1000",
                DRIVE_API_BASE, urlencoding::encode(&current_token)
            );

            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("List changes failed: {}", sanitize_api_error(&text))));
            }

            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ChangeFile {
                name: Option<String>,
                mime_type: Option<String>,
                trashed: Option<bool>,
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Change {
                file_id: Option<String>,
                file: Option<ChangeFile>,
                removed: Option<bool>,
                time: Option<String>,
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ChangeList {
                changes: Vec<Change>,
                new_start_page_token: Option<String>,
                next_page_token: Option<String>,
            }

            let list: ChangeList = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            for change in &list.changes {
                let removed = change.removed.unwrap_or(false)
                    || change.file.as_ref().and_then(|f| f.trashed).unwrap_or(false);
                let change_type = if removed { "deleted" } else { "modified" };

                all_changes.push(super::ChangeEntry {
                    file_id: change.file_id.clone().unwrap_or_default(),
                    name: change.file.as_ref().and_then(|f| f.name.clone()).unwrap_or_default(),
                    change_type: change_type.to_string(),
                    mime_type: change.file.as_ref().and_then(|f| f.mime_type.clone()),
                    timestamp: change.time.clone(),
                    removed,
                });
            }

            if let Some(new_token) = list.new_start_page_token {
                return Ok((all_changes, new_token));
            } else if let Some(next) = list.next_page_token {
                current_token = next;
            } else {
                return Ok((all_changes, current_token));
            }
        }
    }

    async fn remove_permission(&mut self, path: &str, target: &str) -> Result<(), ProviderError> {
        // First list permissions to find the matching permission ID
        let perms = self.list_permissions(path).await?;
        let _matching = perms.iter().find(|p| p.target == target)
            .ok_or_else(|| ProviderError::NotFound(format!("Permission for {} not found", target)))?;

        // Need the actual permission ID from the API
        let file_path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = file_path.rfind('/') {
            (&file_path[..pos], &file_path[pos + 1..])
        } else {
            ("", file_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // List with IDs
        let url = format!(
            "{}/files/{}/permissions?fields=permissions(id,emailAddress)",
            DRIVE_API_BASE, file.id
        );

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PermId {
            id: String,
            email_address: Option<String>,
        }
        #[derive(Deserialize)]
        struct PermIdList {
            permissions: Vec<PermId>,
        }

        let list: PermIdList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let perm = list.permissions.iter()
            .find(|p| p.email_address.as_deref() == Some(target))
            .ok_or_else(|| ProviderError::NotFound(format!("Permission for {} not found", target)))?;

        let delete_url = format!("{}/files/{}/permissions/{}", DRIVE_API_BASE, file.id, perm.id);

        let del_response = self.client
            .delete(&delete_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !del_response.status().is_success() {
            let text = del_response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Remove permission failed: {}", sanitize_api_error(&text))));
        }

        Ok(())
    }
}
