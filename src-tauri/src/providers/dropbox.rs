//! Dropbox Storage Provider
//!
//! Implements StorageProvider for Dropbox using the Dropbox API v2.
//! Uses OAuth2 for authentication.

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, ProviderConfig, StorageInfo, LockInfo,
    sanitize_api_error,
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
};

/// Dropbox API endpoints
const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";

/// Dropbox file metadata
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DropboxMetadata {
    #[serde(rename = ".tag")]
    tag: String,
    name: String,
    path_lower: Option<String>,
    path_display: Option<String>,
    id: Option<String>,
    #[serde(default)]
    size: u64,
    client_modified: Option<String>,
    server_modified: Option<String>,
}

/// List folder response
#[derive(Debug, Deserialize)]
struct ListFolderResult {
    entries: Vec<DropboxMetadata>,
    cursor: String,
    has_more: bool,
}

/// Dropbox provider configuration
#[derive(Debug, Clone)]
pub struct DropboxConfig {
    pub app_key: String,
    pub app_secret: String,
}

impl DropboxConfig {
    pub fn new(app_key: &str, app_secret: &str) -> Self {
        Self {
            app_key: app_key.to_string(),
            app_secret: app_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let app_key = config.extra.get("app_key")
            .or_else(|| config.extra.get("client_id"))
            .ok_or_else(|| ProviderError::Other("Missing app_key".to_string()))?;
        let app_secret = config.extra.get("app_secret")
            .or_else(|| config.extra.get("client_secret"))
            .ok_or_else(|| ProviderError::Other("Missing app_secret".to_string()))?;

        Ok(Self::new(app_key, app_secret))
    }
}

/// Dropbox Storage Provider
pub struct DropboxProvider {
    config: DropboxConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Authenticated user email
    account_email: Option<String>,
}

impl DropboxProvider {
    pub fn new(config: DropboxConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            connected: false,
            current_path: "".to_string(), // Dropbox root is ""
            account_email: None,
        }
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::dropbox(&self.config.app_key, &self.config.app_secret)
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
        self.oauth_manager.has_tokens(OAuthProvider::Dropbox)
    }

    /// Start OAuth flow (called via oauth2_start_auth command)
    #[allow(dead_code)]
    pub async fn start_auth(&self) -> Result<(String, String), ProviderError> {
        self.oauth_manager.start_auth_flow(&self.oauth_config()).await
    }

    /// Complete OAuth flow (called via oauth2_connect command)
    #[allow(dead_code)]
    pub async fn complete_auth(&self, code: &str, state: &str) -> Result<(), ProviderError> {
        self.oauth_manager.complete_auth_flow(&self.oauth_config(), code, state).await?;
        Ok(())
    }

    /// Normalize path for Dropbox API (empty string = root, paths start with /)
    fn normalize_path(&self, path: &str) -> String {
        let path = path.trim_matches('/');
        if path.is_empty() {
            "".to_string()
        } else {
            format!("/{}", path)
        }
    }

    /// Convert Dropbox metadata to RemoteEntry
    fn to_remote_entry(&self, meta: &DropboxMetadata) -> RemoteEntry {
        let is_dir = meta.tag == "folder";
        let path = meta.path_display.clone()
            .unwrap_or_else(|| meta.path_lower.clone().unwrap_or_default());

        RemoteEntry {
            name: meta.name.clone(),
            path,
            is_dir,
            size: meta.size,
            modified: meta.server_modified.clone(),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: HashMap::new(),
        }
    }

    /// Make API call with RPC style
    async fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> Result<T, ProviderError> {
        let url = format!("{}/{}", API_BASE, endpoint);
        
        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            
            // Check for path not found error
            if text.contains("path/not_found") {
                return Err(ProviderError::NotFound(sanitize_api_error(&text)));
            }

            return Err(ProviderError::Other(format!("API error {}: {}", status, sanitize_api_error(&text))));
        }

        response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))
    }

    /// List deleted files in a folder (includes deleted entries)
    #[allow(dead_code)]
    pub async fn list_deleted(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let norm_path = self.normalize_path(path);

        let body = serde_json::json!({
            "path": norm_path,
            "recursive": false,
            "include_deleted": true,
            "include_has_explicit_shared_members": false,
            "include_mounted_folders": true
        });

        let mut result: ListFolderResult = self.rpc_call("files/list_folder", &body).await?;
        let mut all_entries = result.entries;

        while result.has_more {
            let continue_body = serde_json::json!({
                "cursor": result.cursor
            });
            result = self.rpc_call("files/list_folder/continue", &continue_body).await?;
            all_entries.extend(result.entries);
        }

        // Filter to only deleted entries
        let deleted: Vec<RemoteEntry> = all_entries.iter()
            .filter(|e| e.tag == "deleted")
            .map(|e| self.to_remote_entry(e))
            .collect();

        info!("Listed {} deleted entries in {}", deleted.len(), path);
        Ok(deleted)
    }

    /// Restore a deleted file by path and revision
    #[allow(dead_code)]
    pub async fn restore_file(&mut self, path: &str, rev: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "rev": rev
        });

        let _: serde_json::Value = self.rpc_call("files/restore", &body).await?;

        info!("Restored {} to revision {}", path, rev);
        Ok(())
    }

    /// Permanently delete a file (cannot be undone)
    #[allow(dead_code)]
    pub async fn permanent_delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let url = format!("{}/files/permanently_delete", API_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Permanent delete failed {}: {}", status, sanitize_api_error(&text))));
        }

        info!("Permanently deleted: {}", path);
        Ok(())
    }

    /// List folder with pagination
    async fn list_folder_all(&self, path: &str) -> Result<Vec<DropboxMetadata>, ProviderError> {
        let path = self.normalize_path(path);
        
        let body = serde_json::json!({
            "path": path,
            "recursive": false,
            "include_deleted": false,
            "include_has_explicit_shared_members": false,
            "include_mounted_folders": true
        });

        let mut result: ListFolderResult = self.rpc_call("files/list_folder", &body).await?;
        let mut all_entries = result.entries;

        while result.has_more {
            let continue_body = serde_json::json!({
                "cursor": result.cursor
            });
            result = self.rpc_call("files/list_folder/continue", &continue_body).await?;
            all_entries.extend(result.entries);
        }

        Ok(all_entries)
    }
}

#[async_trait]
impl StorageProvider for DropboxProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Dropbox
    }

    fn display_name(&self) -> String {
        "Dropbox".to_string()
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

        // Validate by getting account info
        let url = format!("{}/users/get_current_account", API_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            info!("Dropbox auth failed: HTTP {} - {}", status, body);
            return Err(ProviderError::AuthenticationFailed(
                format!("Invalid token (HTTP {})", status)
            ));
        }

        // Parse email from response
        if let Ok(body) = response.json::<serde_json::Value>().await {
            if let Some(email) = body["email"].as_str() {
                self.account_email = Some(email.to_string());
            }
        }

        self.connected = true;
        self.current_path = "".to_string();

        info!("Connected to Dropbox");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        info!("Disconnected from Dropbox");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let list_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else if path.starts_with('/') {
            path.trim_matches('/').to_string()
        } else {
            format!("{}/{}", self.current_path.trim_matches('/'), path)
        };

        let entries = self.list_folder_all(&list_path).await?;
        Ok(entries.iter().map(|e| self.to_remote_entry(e)).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_path.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_path))
        }
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.trim_matches('/').to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self.current_path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            parts.join("/")
        } else if self.current_path.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path, path)
        };

        // Verify path exists (skip for root)
        if !new_path.is_empty() {
            let db_path = self.normalize_path(&new_path);
            let body = serde_json::json!({
                "path": db_path
            });
            
            let meta: DropboxMetadata = self.rpc_call("files/get_metadata", &body).await?;
            if meta.tag != "folder" {
                return Err(ProviderError::InvalidPath(format!("{} is not a folder", path)));
            }
        }

        self.current_path = new_path;
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

        let path = self.normalize_path(remote_path);

        let arg = serde_json::json!({
            "path": path
        });

        let url = format!("{}/files/download", CONTENT_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Download failed: {}", sanitize_api_error(&text))));
        }

        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        }

        info!("Downloaded {} to {}", remote_path, local_path);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path = self.normalize_path(remote_path);
        
        let arg = serde_json::json!({
            "path": path
        });

        let url = format!("{}/files/download", CONTENT_BASE);
        
        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Download failed: {}", sanitize_api_error(&text))));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        Ok(bytes.to_vec())
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        let path = self.normalize_path(remote_path);
        let file_size = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::Other(format!("Metadata error: {}", e)))?.len();

        const UPLOAD_SESSION_THRESHOLD: u64 = 150 * 1024 * 1024; // 150MB

        if file_size > UPLOAD_SESSION_THRESHOLD {
            // Upload session for large files — read chunks from file, not all in memory
            const CHUNK_SIZE: u64 = 128 * 1024 * 1024; // 128MB
            let mut file = tokio::fs::File::open(local_path).await
                .map_err(|e| ProviderError::Other(format!("Open error: {}", e)))?;

            // Step 1: Start upload session with first chunk
            let first_chunk_size = std::cmp::min(CHUNK_SIZE, file_size) as usize;
            let mut first_chunk = vec![0u8; first_chunk_size];
            file.read_exact(&mut first_chunk).await
                .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

            let start_url = format!("{}/files/upload_session/start", CONTENT_BASE);
            let start_arg = serde_json::json!({
                "close": first_chunk_size as u64 >= file_size
            });

            let start_resp = self.client
                .post(&start_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", start_arg.to_string())
                .body(first_chunk)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !start_resp.status().is_success() {
                let text = start_resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Upload session start failed: {}", sanitize_api_error(&text))));
            }

            #[derive(Deserialize)]
            struct SessionStart {
                session_id: String,
            }
            let session: SessionStart = start_resp.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(ref progress) = on_progress {
                progress(first_chunk_size as u64, file_size);
            }

            // Step 2: Append remaining chunks (read from file, not memory)
            let mut offset = first_chunk_size as u64;
            while offset < file_size {
                let chunk_size = std::cmp::min(CHUNK_SIZE, file_size - offset) as usize;
                let mut chunk = vec![0u8; chunk_size];
                file.read_exact(&mut chunk).await
                    .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;
                let is_last = offset + chunk_size as u64 >= file_size;

                let append_url = format!("{}/files/upload_session/append_v2", CONTENT_BASE);
                let append_arg = serde_json::json!({
                    "cursor": {
                        "session_id": session.session_id,
                        "offset": offset
                    },
                    "close": is_last
                });

                let append_resp = self.client
                    .post(&append_url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header("Dropbox-API-Arg", append_arg.to_string())
                    .body(chunk)
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                if !append_resp.status().is_success() {
                    let text = append_resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(format!("Upload session append failed: {}", sanitize_api_error(&text))));
                }

                offset += chunk_size as u64;

                if let Some(ref progress) = on_progress {
                    progress(offset, file_size);
                }
            }

            // Step 3: Finish session
            let finish_url = format!("{}/files/upload_session/finish", CONTENT_BASE);
            let finish_arg = serde_json::json!({
                "cursor": {
                    "session_id": session.session_id,
                    "offset": file_size
                },
                "commit": {
                    "path": path,
                    "mode": "overwrite",
                    "autorename": false,
                    "mute": false
                }
            });

            let finish_resp = self.client
                .post(&finish_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", finish_arg.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !finish_resp.status().is_success() {
                let text = finish_resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Upload session finish failed: {}", sanitize_api_error(&text))));
            }
        } else {
            // Simple upload — stream file content without loading into memory
            let file = tokio::fs::File::open(local_path).await
                .map_err(|e| ProviderError::Other(format!("Open error: {}", e)))?;
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = reqwest::Body::wrap_stream(stream);

            let arg = serde_json::json!({
                "path": path,
                "mode": "overwrite",
                "autorename": false,
                "mute": false
            });

            let url = format!("{}/files/upload", CONTENT_BASE);

            let response = self.client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", arg.to_string())
                .header("Content-Length", file_size.to_string())
                .body(body)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Upload failed: {}", sanitize_api_error(&text))));
            }
        }

        info!("Uploaded {} to {}", local_path, remote_path);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "autorename": false
        });

        let _: serde_json::Value = self.rpc_call("files/create_folder_v2", &body).await?;
        
        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let _: serde_json::Value = self.rpc_call("files/delete_v2", &body).await?;
        
        info!("Deleted: {}", path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Dropbox delete removes folders with contents
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            self.normalize_path(from)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, from))
        };

        let to_path = if to.starts_with('/') {
            self.normalize_path(to)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, to))
        };

        let body = serde_json::json!({
            "from_path": from_path,
            "to_path": to_path
        });

        let _: serde_json::Value = self.rpc_call("files/move_v2", &body).await?;
        
        info!("Renamed {} to {}", from, to);
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let meta: DropboxMetadata = self.rpc_call("files/get_metadata", &body).await?;
        Ok(self.to_remote_entry(&meta))
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
        Ok("Dropbox API v2".to_string())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            self.normalize_path(from)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, from))
        };

        let to_path = if to.starts_with('/') {
            self.normalize_path(to)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, to))
        };

        let body = serde_json::json!({
            "from_path": from_path,
            "to_path": to_path
        });

        let _: serde_json::Value = self.rpc_call("files/copy_v2", &body).await?;

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let search_path = self.normalize_path(path);

        let body = serde_json::json!({
            "query": pattern,
            "options": {
                "path": if search_path.is_empty() { "" } else { search_path.as_str() },
                "max_results": 200,
                "file_status": "active"
            }
        });

        let url = format!("{}/files/search_v2", API_BASE);

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
            return Err(ProviderError::Other(format!("Search failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        struct MatchMetadata {
            metadata: DropboxMetadata,
        }
        #[derive(Deserialize)]
        struct SearchMatch {
            metadata: MatchMetadata,
        }
        #[derive(Deserialize)]
        struct SearchResult {
            matches: Vec<SearchMatch>,
        }

        let result: SearchResult = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(result.matches.iter()
            .map(|m| self.to_remote_entry(&m.metadata.metadata))
            .collect())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/users/get_space_usage", API_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Space usage failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        struct Allocation {
            allocated: u64,
        }
        #[derive(Deserialize)]
        struct SpaceUsage {
            used: u64,
            allocation: Allocation,
        }

        let usage: SpaceUsage = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(StorageInfo {
            used: usage.used,
            total: usage.allocation.allocated,
            free: usage.allocation.allocated.saturating_sub(usage.used),
        })
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        // Try to create a shared link
        // GAP-A12: Support expires_in_secs via Dropbox expires field (ISO 8601)
        let mut settings = serde_json::json!({
            "access": "viewer",
            "allow_download": true,
            "audience": "public",
            "requested_visibility": "public"
        });
        if let Some(secs) = _expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            settings["expires"] = serde_json::Value::String(
                expires_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            );
        }
        let body = serde_json::json!({
            "path": full_path,
            "settings": settings
        });

        // Dropbox API: sharing/create_shared_link_with_settings
        let url = format!("{}/sharing/create_shared_link_with_settings", API_BASE);
        
        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        // If link already exists, get it instead
        if status == 409 && text.contains("shared_link_already_exists") {
            // Get existing link
            let body = serde_json::json!({
                "path": full_path,
                "direct_only": false
            });

            let url = format!("{}/sharing/list_shared_links", API_BASE);
            
            let response = self.client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                return Err(ProviderError::Other("Failed to get existing share link".to_string()));
            }

            #[derive(Deserialize)]
            struct SharedLink {
                url: String,
            }
            #[derive(Deserialize)]
            struct SharedLinksResult {
                links: Vec<SharedLink>,
            }

            let result: SharedLinksResult = response.json().await
                .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

            if let Some(link) = result.links.first() {
                info!("Retrieved existing share link for {}: {}", path, link.url);
                return Ok(link.url.clone());
            }

            return Err(ProviderError::Other("No existing share link found".to_string()));
        }

        if !status.is_success() {
            if text.contains("missing_scope") {
                return Err(ProviderError::AuthenticationFailed(
                    "Dropbox token missing 'sharing.write' scope. Please disconnect and reconnect Dropbox to refresh permissions.".to_string()
                ));
            }
            return Err(ProviderError::Other(format!("Failed to create share link: {} - {}", status, sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        struct CreateLinkResponse {
            url: String,
        }

        let result: CreateLinkResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        info!("Created share link for {}: {}", path, result.url);
        Ok(result.url)
    }

    async fn remove_share_link(
        &mut self,
        path: &str,
    ) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        // First get the existing share link URL
        let body = serde_json::json!({
            "path": full_path,
            "direct_only": false
        });

        let url = format!("{}/sharing/list_shared_links", API_BASE);
        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::Other("Failed to list share links".to_string()));
        }

        #[derive(Deserialize)]
        struct SharedLink {
            url: String,
        }
        #[derive(Deserialize)]
        struct SharedLinksResult {
            links: Vec<SharedLink>,
        }

        let result: SharedLinksResult = response.json().await
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        let link_url = result.links.first()
            .ok_or_else(|| ProviderError::Other("No share link found to remove".to_string()))?;

        // Revoke the shared link
        let body = serde_json::json!({
            "url": link_url.url
        });

        let url = format!("{}/sharing/revoke_shared_link", API_BASE);
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
            return Err(ProviderError::Other(format!("Failed to revoke share link: {}", sanitize_api_error(&text))));
        }

        info!("Revoked share link for {}", path);
        Ok(())
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<super::FileVersion>, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "limit": 100
        });

        let url = format!("{}/files/list_revisions", API_BASE);

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
            return Err(ProviderError::Other(format!("List revisions failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        struct RevisionEntry {
            rev: String,
            size: u64,
            server_modified: String,
        }
        #[derive(Deserialize)]
        struct RevisionList {
            entries: Vec<RevisionEntry>,
        }

        let list: RevisionList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.entries.iter().map(|r| super::FileVersion {
            id: r.rev.clone(),
            modified: Some(r.server_modified.clone()),
            size: r.size,
            modified_by: None,
        }).collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let arg = serde_json::json!({
            "path": format!("rev:{}", version_id)
        });

        let url = format!("{}/files/download", CONTENT_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Download revision failed: {}", sanitize_api_error(&text))));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let _ = full_path; // Used for path resolution
        tokio::fs::write(local_path, &bytes).await
            .map_err(ProviderError::IoError)?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "rev": version_id
        });

        let _: serde_json::Value = self.rpc_call("files/restore", &body).await?;
        info!("Restored {} to revision {}", path, version_id);
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let arg = serde_json::json!({
            "path": full_path,
            "format": "jpeg",
            "size": "w256h256"
        });

        let url = format!("{}/files/get_thumbnail_v2", CONTENT_BASE);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::NotFound("No thumbnail available".to_string()));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Return as base64 data URI
        Ok(format!("data:image/jpeg;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)))
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(&mut self, path: &str, _timeout: u64) -> Result<LockInfo, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "entries": [{
                ".tag": "path_lookup",
                "path": full_path
            }]
        });

        let url = format!("{}/files/lock_file_batch", API_BASE);
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
            return Err(ProviderError::Other(format!("Lock failed: {}", sanitize_api_error(&text))));
        }

        #[derive(Deserialize)]
        struct LockContent {
            #[serde(default)]
            lock_holder_account_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct LockEntry {
            #[serde(default)]
            lock: Option<LockContent>,
        }
        #[derive(Deserialize)]
        struct LockResult {
            #[serde(rename = ".tag")]
            tag: String,
            #[serde(flatten)]
            entry: Option<LockEntry>,
        }
        #[derive(Deserialize)]
        struct LockBatchResponse {
            entries: Vec<LockResult>,
        }

        let result: LockBatchResponse = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        if let Some(entry) = result.entries.first() {
            if entry.tag == "success" {
                let owner = entry.entry.as_ref()
                    .and_then(|e| e.lock.as_ref())
                    .and_then(|l| l.lock_holder_account_id.clone());
                return Ok(LockInfo {
                    token: full_path.clone(),
                    owner,
                    timeout: 0,
                    exclusive: true,
                });
            }
        }

        Err(ProviderError::Other("Lock failed: no success entry".to_string()))
    }

    async fn unlock_file(&mut self, path: &str, _lock_token: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "entries": [{
                ".tag": "path_lookup",
                "path": full_path
            }]
        });

        let url = format!("{}/files/unlock_file_batch", API_BASE);
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
            return Err(ProviderError::Other(format!("Unlock failed: {}", sanitize_api_error(&text))));
        }

        info!("Unlocked file: {}", path);
        Ok(())
    }
}
