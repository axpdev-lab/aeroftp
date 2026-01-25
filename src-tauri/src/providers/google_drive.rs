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
    StorageProvider, ProviderType, ProviderError, RemoteEntry, ProviderConfig,
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
};

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
    /// Cache: path -> folder_id
    folder_cache: HashMap<String, String>,
}

impl GoogleDriveProvider {
    pub fn new(config: GoogleDriveConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::new(),
            connected: false,
            current_folder_id: "root".to_string(),
            current_path: "/".to_string(),
            folder_cache: HashMap::new(),
        }
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::google(&self.config.client_id, &self.config.client_secret)
    }

    /// Get authorization header
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        let token = self.oauth_manager.get_valid_token(&self.oauth_config()).await?;
        HeaderValue::from_str(&format!("Bearer {}", token))
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
                return Err(ProviderError::Other(format!("API error {}: {}", status, text)));
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
        let query = format!(
            "name='{}' and '{}' in parents and trashed=false",
            name.replace("'", "\\'"), parent_id
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

    /// Resolve path to folder ID
    async fn resolve_path(&mut self, path: &str) -> Result<String, ProviderError> {
        // Check cache
        if let Some(id) = self.folder_cache.get(path) {
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

            // Check cache for intermediate paths
            if let Some(id) = self.folder_cache.get(&current_path) {
                current_id = id.clone();
                continue;
            }

            let file = self.find_by_name(part, &current_id).await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;

            if file.mime_type != "application/vnd.google-apps.folder" {
                return Err(ProviderError::InvalidPath(format!("{} is not a folder", part)));
            }

            current_id = file.id;
            self.folder_cache.insert(current_path.clone(), current_id.clone());
        }

        Ok(current_id)
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
    fn provider_type(&self) -> ProviderType {
        ProviderType::GoogleDrive
    }

    fn display_name(&self) -> String {
        "Google Drive".to_string()
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
        // Get file ID from path
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

        // Download file content
        let url = format!("{}/files/{}?alt=media", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!("Download failed: {}", response.status())));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        let _: () = tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::Other(format!("Write error: {}", e)))?;

        info!("Downloaded {} to {}", remote_path, local_path);
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

        let url = format!("{}/files/{}?alt=media", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        Ok(bytes.to_vec())
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let content = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

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

        // Create multipart upload
        let metadata = serde_json::json!({
            "name": file_name,
            "parents": [parent_id]
        });

        let boundary = "aeroftp_boundary";
        let mut body = Vec::new();
        
        // Metadata part
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
        body.extend_from_slice(metadata.to_string().as_bytes());
        body.extend_from_slice(b"\r\n");
        
        // File content part
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
            return Err(ProviderError::Other(format!("Upload failed: {}", text)));
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
            return Err(ProviderError::Other(format!("mkdir failed: {}", text)));
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

        let new_name = to.rsplit('/').next().unwrap_or(to);
        
        let metadata = serde_json::json!({
            "name": new_name
        });

        let url = format!("{}/files/{}", DRIVE_API_BASE, file.id);
        
        let response = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(metadata.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!("Rename failed: {}", response.status())));
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
        let permission = serde_json::json!({
            "role": "reader",
            "type": "anyone"
        });

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
            return Err(ProviderError::Other(format!("Failed to create share permission: {} - {}", status, text)));
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
}
